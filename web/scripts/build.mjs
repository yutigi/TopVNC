// Builds the browser client into web/dist: the Rust/WASM session crate, its
// wasm-bindgen loader, and the page and Worker bundles. The Cloudflare Worker
// serves web/dist as static assets.
import { build } from "esbuild";
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";

const webDir = fileURLToPath(new URL("../", import.meta.url));
const rootDir = fileURLToPath(new URL("../../", import.meta.url));
const exe = process.platform === "win32" ? ".exe" : "";
// Must equal the wasm-bindgen version in the root Cargo.lock.
const BINDGEN_VERSION = "0.2.128";
const bindgen = fileURLToPath(new URL(`../../.tools/bin/wasm-bindgen${exe}`, import.meta.url));
if (!existsSync(bindgen)) {
  throw new Error(`Install the pinned tool from the repository root: cargo install wasm-bindgen-cli --version ${BINDGEN_VERSION} --locked --root .tools`);
}

function run(command, args, cwd, label) {
  const result = spawnSync(command, args, { cwd, stdio: "inherit" });
  if (result.error || result.status !== 0) throw new Error(`${label} failed`, { cause: result.error });
}

const version = spawnSync(bindgen, ["--version"], { encoding: "utf8" }).stdout.trim();
if (version !== `wasm-bindgen ${BINDGEN_VERSION}`) throw new Error(`Expected wasm-bindgen ${BINDGEN_VERSION}, found ${version}`);

run("cargo", ["build", "--locked", "--release", "--target", "wasm32-unknown-unknown", "-p", "topvnc-web"], rootDir, "Browser client WASM build");
rmSync(`${webDir}build/pkg`, { recursive: true, force: true });
run(bindgen, ["--target", "web", "--no-typescript", "--out-dir", "web/build/pkg",
  "target/wasm32-unknown-unknown/release/topvnc_web.wasm"], rootDir, "Browser client bindings");

const dist = `${webDir}dist/`;
rmSync(dist, { recursive: true, force: true });
mkdirSync(dist, { recursive: true });
for (const file of ["index.html", "style.css", "_headers"]) copyFileSync(`${webDir}${file}`, `${dist}${file}`);
copyFileSync(`${webDir}build/pkg/topvnc_web_bg.wasm`, `${dist}topvnc_web_bg.wasm`);
await build({
  absWorkingDir: webDir,
  entryPoints: { app: "app.mjs", "session-worker": "session-worker.mjs" },
  outdir: "dist",
  bundle: true,
  format: "esm",
  // The oldest Safari with OffscreenCanvas 2D; older browsers fall back to
  // the page thread but need the same syntax.
  target: ["safari16.4"],
  legalComments: "inline",
  sourcemap: false,
  minify: true,
});
