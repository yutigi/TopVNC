import { build } from "esbuild";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { copyFileSync, existsSync } from "node:fs";

const workerDir = fileURLToPath(new URL("../", import.meta.url));
const spikeDir = fileURLToPath(new URL("../../", import.meta.url));
const exe = process.platform === "win32" ? ".exe" : "";
const tool = (name) => fileURLToPath(new URL(`../../.tools/bin/${name}${exe}`, import.meta.url));
const packager = tool("worker-build");
// Must equal the wasm-bindgen version in Cargo.lock.
const bindgen = tool("wasm-bindgen");
for (const [path, install] of [
  [packager, "cargo install worker-build --version 0.8.7 --locked --root .tools"],
  [bindgen, "cargo install wasm-bindgen-cli --version 0.2.129 --locked --root .tools"],
]) {
  if (!existsSync(path)) throw new Error(`Install the pinned tool from spike/: ${install}`);
}

function run(command, args, cwd, label) {
  const result = spawnSync(command, args, { cwd, stdio: "inherit" });
  if (result.error || result.status !== 0) throw new Error(`${label} failed`, { cause: result.error });
}

run(packager, ["--release"], workerDir, "Rust Worker packaging");
run("cargo", ["build", "--locked", "--release", "--target", "wasm32-unknown-unknown", "-p", "topvnc-spike-browser"],
  spikeDir, "Browser E2E WASM build");
run(bindgen, ["--target", "web", "--no-typescript", "--out-dir", "worker/build/browser-pkg",
  "target/wasm32-unknown-unknown/release/topvnc_spike_browser.wasm"], spikeDir, "Browser E2E bindings");
copyFileSync(`${workerDir}build/browser-pkg/topvnc_spike_browser_bg.wasm`, `${workerDir}public/topvnc_spike_browser_bg.wasm`);
await build({
  absWorkingDir: workerDir,
  entryPoints: ["probe.mjs"],
  outfile: "public/probe.bundle.js",
  bundle: true,
  format: "esm",
  target: ["safari17"],
  legalComments: "inline",
  sourcemap: false,
  minify: true,
});
