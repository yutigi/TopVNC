"""Repository paths and bounded command execution for the setup helpers."""
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
WORKER = ROOT / "gateway/cloudflare/worker"
CONFIG = WORKER / "wrangler.local.toml"


def run_wrangler(arguments, timeout):
    cli = WORKER / "node_modules/wrangler/bin/wrangler.js"
    if not cli.is_file() or not CONFIG.is_file():
        print("Install Worker dependencies with npm ci and create wrangler.local.toml first.")
        return 1
    return _run(["node", str(cli), *arguments, "--config", str(CONFIG)], timeout, "Wrangler")


def run_deployment(timeout):
    # The npm entry point owns branch checks, builds, tests, config filtering,
    # and the final pre-publish recheck. Never publish through raw Wrangler.
    npm = "npm.cmd" if os.name == "nt" else "npm"
    return _run([npm, "run", "deploy", "--", "--config", str(CONFIG)], timeout, "Deployment helper")


def _run(command, timeout, label):
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    log = ROOT / "out/cloudflare/wrangler.log"
    log.parent.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, WRANGLER_SEND_METRICS="false", WRANGLER_LOG_PATH=str(log))
    try:
        result = subprocess.run(command, cwd=WORKER, env=env, capture_output=True,
                                text=True, encoding="utf-8", errors="replace", timeout=timeout)
    except (OSError, subprocess.TimeoutExpired):
        # Exceptions may carry captured output; don't print their raw contents.
        print(f"{label} could not start or timed out.")
        return 1
    safe = re.sub(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}",
                  "[email redacted]", result.stdout + "\n" + result.stderr)
    print(safe[-9000:])
    return result.returncode
