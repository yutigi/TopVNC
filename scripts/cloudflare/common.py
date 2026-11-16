"""Repository paths and bounded command execution for the setup helpers."""
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import threading

ROOT = Path(__file__).resolve().parents[2]
WORKER = ROOT / "gateway/cloudflare/worker"
CONFIG = WORKER / "wrangler.local.toml"
EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")


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
    # npm and Wrangler start their own child processes. On POSIX a new session
    # lets a timeout signal the whole group; on Windows taskkill walks the tree.
    group = {} if os.name == "nt" else {"start_new_session": True}
    try:
        # These workflows never prompt; a prompt hidden behind the pipe would hang.
        process = subprocess.Popen(command, cwd=WORKER, env=env, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                   text=True, encoding="utf-8", errors="replace", **group)
    except OSError:
        print(f"{label} could not start.")
        return 1
    # Show progress as it happens: a build, tests and upload take minutes.
    reader = threading.Thread(target=_forward, args=(process.stdout,), daemon=True)
    reader.start()
    try:
        code = process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        _stop_tree(process)
        _finish(reader, process.stdout)
        print(f"{label} timed out after {timeout} seconds; it and its child processes were stopped.")
        return 1
    except KeyboardInterrupt:
        _stop_tree(process)
        raise
    _finish(reader, process.stdout)
    return code


def _forward(stream):
    for line in stream:
        print(EMAIL.sub("[email redacted]", line), end="", flush=True)


def _finish(reader, stream):
    # A leftover descendant could hold the pipe open; never wait on it forever.
    reader.join(timeout=5)
    if not reader.is_alive():
        stream.close()


def _stop_tree(process):
    # Killing only the direct child (npm) would leave node and Wrangler
    # running, still able to publish after the helper reports a timeout.
    if os.name == "nt":
        try:
            subprocess.run(["taskkill", "/T", "/F", "/PID", str(process.pid)],
                           stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL, check=False, timeout=30)
        except (OSError, subprocess.TimeoutExpired):
            pass
    else:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except OSError:
            pass
    if process.poll() is None:
        process.kill()
    process.wait()
