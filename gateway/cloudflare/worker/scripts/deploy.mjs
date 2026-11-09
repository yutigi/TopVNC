// Manual entry point: npm run deploy[:check] -- [--config /path/to/local.toml].
// No tokens are created, uploaded or stored by this helper.
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { basename, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { assertPublishable, deploymentConfig } from "./deployment-config.mjs";
import { verifyAccess } from "./verify-access.mjs";

const workerDir = fileURLToPath(new URL("../", import.meta.url)).replace(/[\\/]$/, "");
const rootDir = resolve(workerDir, "../../..");
const webDir = resolve(rootDir, "web");
const npmCli = process.env.npm_execpath;
const args = process.argv.slice(2);
let checkOnly = false;
let configPath = resolve(workerDir, "wrangler.local.toml");
for (let i = 0; i < args.length; i++) {
  if (args[i] === "--check") checkOnly = true;
  else if (args[i] === "--config" && args[i + 1] && !args[i + 1].startsWith("--")) configPath = resolve(args[++i]);
  else if (args[i] === "--help") {
    console.log("npm run deploy[:check] -- [--config /absolute/path/to/wrangler.local.toml]\nCheck builds without publishing. Deploy publishes only a clean, current main checkout.");
    process.exit(0);
  } else { console.error(`Unknown or incomplete option: ${args[i]}`); process.exit(1); }
}

function run(command, argv, cwd = rootDir, capture = false) {
  const result = spawnSync(command, argv, { cwd, encoding: "utf8", stdio: capture ? "pipe" : "inherit" });
  if (result.error || result.status !== 0) throw new Error(`${command === process.execPath ? "Node/npm/Wrangler" : command} failed${capture ? "; check installation, repository access and network connectivity" : "; resolve the error above"} and retry.`);
  return capture ? result.stdout.trim() : undefined;
}
const npm = (argv, cwd) => run(process.execPath, [npmCli, ...argv], cwd);
function publishState(checkedHead) {
  const head = run("git", ["rev-parse", "HEAD"], rootDir, true);
  const state = {
    branch: run("git", ["branch", "--show-current"], rootDir, true),
    status: run("git", ["status", "--porcelain"], rootDir, true), head,
    remoteHead: head, checkedHead,
  };
  assertPublishable(state); // Reject local mistakes without making a network request.
  state.remoteHead = run("git", ["ls-remote", "origin", "refs/heads/main"], rootDir, true).split(/\s/)[0];
  assertPublishable(state);
  return head;
}

let temporary;
let temporaryDir;
try {
  if (!npmCli || !existsSync(npmCli)) throw new Error("Run this helper through npm run deploy or npm run deploy:check.");
  if (!existsSync(configPath)) throw new Error("Local config not found. Copy wrangler.example.toml to wrangler.local.toml and fill in the existing app values, or pass --config /absolute/path/to/your/existing/wrangler.local.toml.");
  const checkedHead = checkOnly ? undefined : publishState();
  if (process.platform === "win32") run("chcp.com", ["65001"], rootDir, true);
  // Suppress Wrangler's informational binding summary containing personal values.
  process.env.WRANGLER_LOG = "error";
  process.env.WRANGLER_SEND_METRICS = "false";
  console.log("Installing pinned Worker and web dependencies...");
  npm(["ci"], workerDir);
  npm(["ci"], webDir);
  const { unstable_readConfig } = await import("wrangler");
  const config = deploymentConfig(unstable_readConfig({ config: configPath }), workerDir, webDir);
  mkdirSync(resolve(workerDir, ".wrangler"), { recursive: true });
  temporaryDir = mkdtempSync(resolve(workerDir, ".wrangler/manual-deploy-"));
  temporary = resolve(temporaryDir, "wrangler.json");
  writeFileSync(temporary, JSON.stringify(config, null, 2), { mode: 0o600 });
  console.log(`Checking ${config.name} at ${config.vars.APP_ORIGIN}...`);
  await verifyAccess(config.vars.APP_ORIGIN, config.vars.ACCESS_ISSUER);
  run("rustup", ["target", "add", "wasm32-unknown-unknown"]);
  const lock = readFileSync(resolve(rootDir, "Cargo.lock"), "utf8");
  const version = lock.match(/name = "wasm-bindgen"\r?\nversion = "([^"]+)"/)?.[1];
  if (!version) throw new Error("Cannot find the pinned wasm-bindgen version in Cargo.lock.");
  const bindgen = resolve(rootDir, `.tools/bin/wasm-bindgen${process.platform === "win32" ? ".exe" : ""}`);
  if (!existsSync(bindgen) || run(bindgen, ["--version"], rootDir, true) !== `wasm-bindgen ${version}`) {
    run("cargo", ["install", "wasm-bindgen-cli", "--version", version, "--locked", "--root", resolve(rootDir, ".tools")]);
  }
  run("cargo", ["fmt", "--all", "--", "--check"]);
  run("cargo", ["test", "--locked", "--workspace"]);
  run("cargo", ["clippy", "--locked", "--workspace", "--all-targets", "--", "-D", "warnings"]);
  npm(["test"], webDir);
  npm(["run", "build"], webDir);
  npm(["run", "test:wasm"], webDir);
  npm(["test"], workerDir);
  const wrangler = resolve(workerDir, "node_modules/wrangler/bin/wrangler.js");
  run(process.execPath, [wrangler, "deploy", "--dry-run", "--config", temporary], workerDir);
  if (checkOnly) console.log("All deployment checks passed. Nothing was published.");
  else {
    await verifyAccess(config.vars.APP_ORIGIN, config.vars.ACCESS_ISSUER);
    const head = publishState(checkedHead); // Publish only the commit these checks built.
    console.log(`Publishing main commit ${head}...`);
    run(process.execPath, [wrangler, "deploy", "--config", temporary, "--message", `Manual main ${head}`], workerDir);
    await verifyAccess(config.vars.APP_ORIGIN, config.vars.ACCESS_ISSUER);
    console.log(`Deployed ${head} to ${config.vars.APP_ORIGIN}. Verify your paired device session next.`);
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
} finally {
  if (temporaryDir) {
    // Wrangler can create a cache alongside its config. Remove only this run's
    // generated directory after verifying its absolute parent and name.
    const target = realpathSync(temporaryDir);
    const parent = realpathSync(resolve(workerDir, ".wrangler"));
    if (dirname(target) !== parent || !basename(target).startsWith("manual-deploy-")) {
      console.error("Refusing to clean a temporary directory outside the Worker's .wrangler directory.");
      process.exitCode = 1;
    } else rmSync(target, { recursive: true, force: true });
  }
}
