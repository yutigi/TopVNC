"""Offline tests for the migrated Cloudflare helpers."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
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
    def test_single_and_multi_host_paths(self):
        self.assertEqual(validate.probe_paths({}), ["/", "/ws"])
        variables = {"HOSTS_JSON": json.dumps([{"id": "gaming-pc"}, {"id": "office-pc"}]),
                     "LEGACY_HOST_ID": "gaming-pc"}
        self.assertEqual(validate.probe_paths(variables),
                         ["/", "/api/hosts", "/ws/gaming-pc", "/ws/office-pc", "/ws"])
        for hosts in ([], [{"id": "../secret"}], [{"id": "pc"}, {"id": "pc"}],
                      [{"id": "a" * 49}], [{"id": f"pc-{i}"} for i in range(33)]):
            with self.subTest(hosts=hosts), self.assertRaises(ValueError):
                validate.probe_paths({"HOSTS_JSON": json.dumps(hosts)})

    def test_redirect_requires_real_https_access_hostname(self):
        self.assertTrue(validate.access_redirect(302, "https://team.cloudflareaccess.com/login"))
        for location in ("http://team.cloudflareaccess.com/login", "/login",
                         "https://team.cloudflareaccess.com.evil.test/login",
                         "https://evil.test/?next=.cloudflareaccess.com/login",
                         "https://user@team.cloudflareaccess.com/login",
                         "https://[broken", "https://team.cloudflareaccess.com:bad/login"):
            with self.subTest(location=location):
                self.assertFalse(validate.access_redirect(302, location))
        self.assertFalse(validate.access_redirect(200, "https://team.cloudflareaccess.com/login"))

    def test_failed_probe_prevents_dry_run(self):
        response = Mock(code=200, headers={})
        opener = Mock()
        opener.open.return_value = response
        with patch.object(validate.CONFIG.__class__, "read_text", return_value=
                          '[vars]\nAPP_ORIGIN = "https://app.example.test"'), \
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
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = root / "wrangler.local.toml"
            with patch.object(common, "ROOT", root), patch.object(common, "WORKER", root), \
                 patch.object(common, "CONFIG", config), \
                 patch.object(common.subprocess, "run") as run, \
                 contextlib.redirect_stdout(io.StringIO()) as output:
                run.return_value = Mock(returncode=1, stdout="", stderr="owner@example.com: main required")
                self.assertEqual(common.run_deployment(timeout=1800), 1)
                self.assertEqual(run.call_args.args[0],
                                 ["npm.cmd" if common.os.name == "nt" else "npm",
                                  "run", "deploy", "--", "--config", str(config)])
                self.assertEqual(run.call_args.kwargs["cwd"], root)
                self.assertEqual(run.call_args.kwargs["timeout"], 1800)
                self.assertNotIn("owner@example.com", output.getvalue())
                self.assertIn("main required", output.getvalue())

    def test_account_check_propagates_failure(self):
        with patch.object(account, "run_wrangler", side_effect=[0, 1, 0]) as run, \
             patch.object(sys, "argv", ["check-cloudflare.py"]), \
             contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(account.main(), 1)
            self.assertEqual(run.call_count, 3)

    def test_wrangler_paths_redaction_exit_code_and_timeout(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = root / "wrangler.local.toml"
            config.touch()
            cli = root / "node_modules/wrangler/bin/wrangler.js"
            cli.parent.mkdir(parents=True)
            cli.touch()
            with patch.object(common, "ROOT", root), patch.object(common, "WORKER", root), \
                 patch.object(common, "CONFIG", config), \
                 patch.object(common.subprocess, "run") as run, \
                 contextlib.redirect_stdout(io.StringIO()) as output:
                run.return_value = Mock(returncode=7, stdout="owner@example.com", stderr="")
                self.assertEqual(common.run_wrangler(["deploy", "--dry-run"], 35), 7)
                self.assertNotIn("owner@example.com", output.getvalue())
                self.assertIn("[email redacted]", output.getvalue())
                self.assertEqual(run.call_args.kwargs["env"]["WRANGLER_LOG_PATH"],
                                 str(root / "out/cloudflare/wrangler.log"))
                self.assertEqual(run.call_args.args[0],
                                 ["node", str(cli), "deploy", "--dry-run", "--config", str(config)])
                run.side_effect = subprocess.TimeoutExpired("node", 35, output="private data")
                self.assertEqual(common.run_wrangler(["whoami"], 35), 1)
                self.assertNotIn("private data", output.getvalue())


if __name__ == "__main__":
    unittest.main()
