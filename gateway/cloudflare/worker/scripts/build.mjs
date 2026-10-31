// Packages the Rust Worker with the pinned worker-build, then checks that the
// browser client assets it serves exist.
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

const workerDir = fileURLToPath(new URL("../", import.meta.url));
const exe = process.platform === "win32" ? ".exe" : "";
const tool = (name) => fileURLToPath(new URL(`../.tools/bin/${name}${exe}`, import.meta.url));
// Must equal the wasm-bindgen version in this directory's Cargo.lock.
for (const [path, install] of [
  [tool("worker-build"), "cargo install worker-build --version 0.8.7 --locked --root .tools"],
  [tool("wasm-bindgen"), "cargo install wasm-bindgen-cli --version 0.2.129 --locked --root .tools"],
]) {
  if (!existsSync(path)) throw new Error(`Install the pinned tool from gateway/cloudflare/worker/: ${install}`);
}
const env = { ...process.env, PATH: `${fileURLToPath(new URL("../.tools/bin", import.meta.url))}${process.platform === "win32" ? ";" : ":"}${process.env.PATH}` };
const result = spawnSync(tool("worker-build"), ["--release"], { cwd: workerDir, stdio: "inherit", env });
if (result.error || result.status !== 0) throw new Error("Rust Worker packaging failed", { cause: result.error });
if (!existsSync(fileURLToPath(new URL("../../../../web/dist/index.html", import.meta.url)))) {
  throw new Error("Build the browser client first: npm run build in web/");
}
