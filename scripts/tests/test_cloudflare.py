"""Offline tests for the migrated Cloudflare helpers."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch

HELPERS = Path(__file__).resolve().parents[1] / "cloudflare"
sys.path.insert(0, str(HELPERS))
import common


def load(name):
    spec = importlib.util.spec_from_file_location(name, HELPERS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


validate = load("validate-cloudflare")
worker = load("run-worker")
account = load("check-cloudflare")


class CloudflareTests(unittest.TestCase):
    def test_single_and_multi_host_paths_match_the_deploy_check(self):
        # Same routes as gateway/cloudflare/worker/scripts/verify-access.mjs.
        self.assertEqual(validate.probe_paths({}), ["/", "/api/hosts", "/ws"])
        variables = {"HOSTS_JSON": json.dumps([{"id": "gaming-pc"}, {"id": "office-pc"}]),
                     "LEGACY_HOST_ID": "gaming-pc"}
        # A registry never routes /ws, even when it migrates a legacy pairing.
        self.assertEqual(validate.probe_paths(variables),
                         ["/", "/api/hosts", "/ws/gaming-pc", "/ws/office-pc"])
        for hosts in ([], [{"id": "../secret"}], [{"id": "pc"}, {"id": "pc"}],
                      [{"id": "a" * 49}], [{"id": f"pc-{i}"} for i in range(33)]):
            with self.subTest(hosts=hosts), self.assertRaises(ValueError):
                validate.probe_paths({"HOSTS_JSON": json.dumps(hosts)})

    def test_redirect_requires_the_configured_access_team_login(self):
        team = validate.access_team("https://team.cloudflareaccess.com")
        self.assertEqual(team, "team.cloudflareaccess.com")
        login = "https://team.cloudflareaccess.com/cdn-cgi/access/login/app.example.test"
        self.assertTrue(validate.access_redirect(302, login, team))
        for location in ("http://team.cloudflareaccess.com/cdn-cgi/access/login/x", "/login",
                         "https://team.cloudflareaccess.com/login",
                         "https://other.cloudflareaccess.com/cdn-cgi/access/login/x",
                         "https://team.cloudflareaccess.com.evil.test/cdn-cgi/access/login/x",
                         "https://evil.test/?next=.cloudflareaccess.com/cdn-cgi/access/login/x",
                         "https://user@team.cloudflareaccess.com/cdn-cgi/access/login/x",
                         "https://[broken", "https://team.cloudflareaccess.com:bad/login"):
            with self.subTest(location=location):
                self.assertFalse(validate.access_redirect(302, location, team))
        self.assertFalse(validate.access_redirect(200, login, team))
        self.assertFalse(validate.access_redirect(302, login, None))
        for issuer in (None, 7, "", "http://team.cloudflareaccess.com",
                       "https://team.cloudflareaccess.com.evil.test", "https://evil.test",
                       "https://team.cloudflareaccess.com/path", "https://user@team.cloudflareaccess.com"):
            with self.subTest(issuer=issuer):
                self.assertIsNone(validate.access_team(issuer))

    def test_missing_access_issuer_prevents_probes(self):
        opener = Mock()
        with patch.object(validate.CONFIG.__class__, "read_text", return_value=
                          '[vars]\nAPP_ORIGIN = "https://app.example.test"'), \
             patch.object(validate.urllib.request, "build_opener", return_value=opener), \
             patch.object(validate, "run_wrangler") as run, \
             patch.object(sys, "argv", ["validate-cloudflare.py"]), \
             contextlib.redirect_stdout(io.StringIO()), self.assertRaises(SystemExit):
            validate.main()
        opener.open.assert_not_called()
        run.assert_not_called()

    def test_failed_probe_prevents_dry_run(self):
        response = Mock(code=200, headers={})
        opener = Mock()
        opener.open.return_value = response
        with patch.object(validate.CONFIG.__class__, "read_text", return_value=
                          '[vars]\nAPP_ORIGIN = "https://app.example.test"\n'
                          'ACCESS_ISSUER = "https://team.cloudflareaccess.com"'), \
             patch.object(validate.urllib.request, "build_opener", return_value=opener), \
             patch.object(validate, "run_wrangler") as run, \
             patch.object(sys, "argv", ["validate-cloudflare.py"]), \
             contextlib.redirect_stdout(io.StringIO()), self.assertRaises(SystemExit):
            validate.main()
        run.assert_not_called()
        response.close.assert_called_once()

    def test_worker_requires_explicit_mode(self):
        with patch.object(worker, "run_wrangler", return_value=7) as run, \
             patch.object(worker, "run_deployment", return_value=9) as deploy:
            for arguments in ([], ["--dry-run", "--publish"]):
                with patch.object(sys, "argv", ["run-worker.py", *arguments]), \
                     contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                    worker.main()
            run.assert_not_called()
            deploy.assert_not_called()
            with patch.object(sys, "argv", ["run-worker.py", "--dry-run"]):
                self.assertEqual(worker.main(), 7)
            run.assert_called_once_with(["deploy", "--dry-run"], timeout=120)
            deploy.assert_not_called()
            run.reset_mock()
            with patch.object(sys, "argv", ["run-worker.py", "--publish"]):
                self.assertEqual(worker.main(), 9)
            deploy.assert_called_once_with(timeout=1800)
            run.assert_not_called()

    def test_publish_uses_guarded_npm_workflow_and_propagates_failure(self):
        with patch.object(common, "_run", return_value=1) as run:
            self.assertEqual(common.run_deployment(timeout=1800), 1)
        run.assert_called_once_with(["npm.cmd" if common.os.name == "nt" else "npm",
                                     "run", "deploy", "--", "--config", str(common.CONFIG)],
                                    1800, "Deployment helper")

    def test_account_check_propagates_failure(self):
        with patch.object(account, "run_wrangler", side_effect=[0, 1, 0]) as run, \
             patch.object(sys, "argv", ["check-cloudflare.py"]), \
             contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(account.main(), 1)
            self.assertEqual(run.call_count, 3)

    def test_wrangler_paths_and_exit_code(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = root / "wrangler.local.toml"
            cli = root / "node_modules/wrangler/bin/wrangler.js"
            with patch.object(common, "WORKER", root), patch.object(common, "CONFIG", config), \
                 patch.object(common, "_run", return_value=7) as run, \
                 contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(common.run_wrangler(["whoami"], 35), 1)  # Not installed yet.
                run.assert_not_called()
                config.touch()
                cli.parent.mkdir(parents=True)
                cli.touch()
                self.assertEqual(common.run_wrangler(["deploy", "--dry-run"], 35), 7)
            run.assert_called_once_with(
                ["node", str(cli), "deploy", "--dry-run", "--config", str(config)], 35, "Wrangler")

    def test_streams_redacted_output_from_the_worker_directory(self):
        child = ("import os, sys; print('owner@example.com: main required', flush=True); "
                 "sys.stderr.write('admin@example.org\\n'); print(os.getcwd()); "
                 "print(os.environ['WRANGLER_LOG_PATH'], os.environ['WRANGLER_SEND_METRICS']); "
                 "raise SystemExit(7)")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            with patch.object(common, "ROOT", root), patch.object(common, "WORKER", root), \
                 contextlib.redirect_stdout(io.StringIO()) as output:
                self.assertEqual(common._run([sys.executable, "-I", "-c", child], 30, "Test"), 7)
        text = output.getvalue()
        self.assertIn("[email redacted]: main required", text)
        self.assertNotIn("owner@example.com", text)
        self.assertNotIn("admin@example.org", text)
        self.assertIn(str(root), text)
        self.assertIn(f"{root / 'out/cloudflare/wrangler.log'} false", text)

    def test_timeout_stops_every_descendant_holding_the_output(self):
        # Like npm -> node -> wrangler: the grandchild shares the output pipe
        # and would keep the pipe (and a publish) alive if only npm were killed.
        grandchild = "import time; print('grandchild ready', flush=True); time.sleep(8)"
        child = ("import subprocess, sys, time; "
                 f"subprocess.Popen([sys.executable, '-I', '-c', {grandchild!r}]); time.sleep(60)")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            started = time.monotonic()
            with patch.object(common, "ROOT", root), patch.object(common, "WORKER", root), \
                 contextlib.redirect_stdout(io.StringIO()) as output:
                self.assertEqual(common._run([sys.executable, "-I", "-c", child], 3, "Test"), 1)
            elapsed = time.monotonic() - started
        self.assertIn("grandchild ready", output.getvalue())
        self.assertIn("Test timed out after 3 seconds", output.getvalue())
        # A surviving grandchild holds the pipe until it exits 8 s after starting.
        self.assertLess(elapsed, 7)


if __name__ == "__main__":
    unittest.main()
