import { build } from "esbuild";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { existsSync } from "node:fs";

const workerDir = fileURLToPath(new URL("../", import.meta.url));
const packager = fileURLToPath(new URL(`../../.tools/bin/worker-build${process.platform === "win32" ? ".exe" : ""}`, import.meta.url));
if (!existsSync(packager)) {
  throw new Error("Install the pinned packager from spike/: cargo install worker-build --version 0.8.7 --locked --root .tools");
}
const result = spawnSync(packager, ["--release"], { cwd: workerDir, stdio: "inherit" });
if (result.error || result.status !== 0) throw new Error("Rust Worker packaging failed", { cause: result.error });
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
