from __future__ import annotations

import contextlib
import importlib.util
import io
import os
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).parents[1] / "ptask_failure_alert.py"
SPEC = importlib.util.spec_from_file_location("ptask_failure_alert", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class Response:
    status = 200

    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False


class FailureAlertTests(unittest.TestCase):
    def test_missing_credentials_fails_without_network(self):
        with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(
            MODULE.urllib.request, "urlopen"
        ) as urlopen:
            self.assertEqual(MODULE.send_failure_alert("unit", "host"), 64)
            urlopen.assert_not_called()

    def test_token_is_kept_out_of_argv_and_output(self):
        token = "123456789:" + "A" * 35
        stderr = io.StringIO()
        with mock.patch.dict(
            os.environ,
            {"TELEGRAM_BOT_TOKEN": token, "TELEGRAM_CHAT_ID": "1"},
            clear=True,
        ), mock.patch.object(
            MODULE.urllib.request,
            "urlopen",
            side_effect=RuntimeError(f"failed https://api.telegram.org/bot{token}"),
        ), contextlib.redirect_stderr(stderr):
            self.assertEqual(MODULE.send_failure_alert("unit", "host"), 1)
        self.assertNotIn(token, stderr.getvalue())

    def test_success_posts_without_spawning_a_tokenized_process(self):
        token = "123456789:" + "B" * 35
        with mock.patch.dict(
            os.environ,
            {"TELEGRAM_BOT_TOKEN": token, "TELEGRAM_CHAT_ID": "1"},
            clear=True,
        ), mock.patch.object(
            MODULE.urllib.request, "urlopen", return_value=Response()
        ) as urlopen:
            self.assertEqual(MODULE.send_failure_alert("unit", "host"), 0)
        request = urlopen.call_args.args[0]
        self.assertIn(token, request.full_url)

    def test_prefixed_credentials_and_digest_chat_precedence(self):
        token = "123456789:" + "C" * 35
        with mock.patch.dict(
            os.environ,
            {
                "PTASK_TELEGRAM_BOT_TOKEN": token,
                "PTASK_TELEGRAM_DIGEST_CHATS": "42,43",
                "TELEGRAM_BOT_TOKEN": "wrong",
                "TELEGRAM_CHAT_ID": "99",
            },
            clear=True,
        ), mock.patch.object(
            MODULE.urllib.request, "urlopen", return_value=Response()
        ) as urlopen:
            self.assertEqual(MODULE.send_failure_alert("unit", "host"), 0)
        request = urlopen.call_args.args[0]
        self.assertIn(token, request.full_url)
        self.assertIn(b"chat_id=42", request.data)

    def _failed_send_stderr(self, journal_stream, fstat_ids):
        """Fail a send with stderr on an fd whose (dev, ino) is fstat_ids."""

        class FdStringIO(io.StringIO):
            def fileno(self):
                return 2

        stderr = FdStringIO()
        env = {"TELEGRAM_BOT_TOKEN": "123:" + "D" * 35, "TELEGRAM_CHAT_ID": "1"}
        if journal_stream is not None:
            env["JOURNAL_STREAM"] = journal_stream
        stat = mock.Mock(st_dev=fstat_ids[0], st_ino=fstat_ids[1])
        with mock.patch.dict(os.environ, env, clear=True), mock.patch.object(
            MODULE.urllib.request, "urlopen", side_effect=OSError("down")
        ), mock.patch.object(MODULE.os, "fstat", return_value=stat), contextlib.redirect_stderr(
            stderr
        ):
            self.assertEqual(MODULE.send_failure_alert("ptask-backup.service", "host1"), 1)
        return stderr.getvalue()

    def test_failed_send_logs_at_err_priority_on_the_journal(self):
        out = self._failed_send_stderr("8:4242", (8, 4242))
        self.assertTrue(out.startswith("<3>"), out)
        self.assertIn("ptask-backup.service failed on host1", out)

    def test_failed_send_has_no_priority_prefix_off_the_journal(self):
        # Interactive run, and a JOURNAL_STREAM inherited by a redirected stderr.
        for stream in (None, "8:4242"):
            out = self._failed_send_stderr(stream, (9, 1))
            self.assertFalse(out.startswith("<"), out)
            self.assertIn("delivery failed", out)

    def test_systemd_and_ansible_never_expand_token_into_argv(self):
        root = Path(__file__).parents[2]
        unit = (root / "scripts/systemd/ptask-failure-alert@.service").read_text()
        self.assertIn(
            "ExecStart=%h/.local/libexec/ptask-failure-alert %i %H", unit
        )
        self.assertNotIn("TELEGRAM_BOT_TOKEN", unit.split("ExecStart=", 1)[1])
        self.assertNotIn("curl", unit.split("ExecStart=", 1)[1])
        ansible = (root / "scripts/ansible/ptask.yml").read_text()
        self.assertIn('src: "{{ playbook_dir }}/../ptask_failure_alert.py"', ansible)
        self.assertIn(
            'dest: "{{ ptask_libexec_dir }}/ptask-failure-alert"', ansible
        )


if __name__ == "__main__":
    unittest.main()
