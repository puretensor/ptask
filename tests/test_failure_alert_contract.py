"""Contract tests for scripts/ptask_failure_alert.py pure helpers."""

from __future__ import annotations

import importlib.util
import os
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "ptask_failure_alert.py"
SPEC = importlib.util.spec_from_file_location("ptask_failure_alert", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class FirstNonemptyContractTests(unittest.TestCase):
  def test_returns_first_non_empty_in_order(self):
    with mock.patch.dict(
      os.environ,
      {
        "PTASK_TELEGRAM_BOT_TOKEN": "  ",
        "TELEGRAM_BOT_TOKEN": "tok",
        "PTASK_ACCOUNTABILITY_CHAT_ID": "1",
      },
      clear=True,
    ):
      self.assertEqual(MODULE.first_nonempty("PTASK_TELEGRAM_BOT_TOKEN", "TELEGRAM_BOT_TOKEN"), "tok")

  def test_all_missing_returns_empty_string(self):
    with mock.patch.dict(os.environ, {}, clear=True):
      self.assertEqual(MODULE.first_nonempty("A", "B", "C"), "")

  def test_strips_surrounding_whitespace(self):
    with mock.patch.dict(os.environ, {"A": "  value  "}, clear=True):
      self.assertEqual(MODULE.first_nonempty("A"), "value")


class ChatIdPrecedenceContractTests(unittest.TestCase):
  def test_digest_chat_list_uses_first_entry_only(self):
    with mock.patch.dict(
      os.environ,
      {
        "TELEGRAM_BOT_TOKEN": "tok",
        "PTASK_TELEGRAM_DIGEST_CHATS": " 42 , 99 ",
      },
      clear=True,
    ), mock.patch.object(MODULE.urllib.request, "urlopen") as urlopen:
      urlopen.return_value.__enter__ = lambda self: self
      urlopen.return_value.__exit__ = lambda *args: False
      urlopen.return_value.status = 200
      MODULE.send_failure_alert("unit", "host")
    request = urlopen.call_args.args[0]
    self.assertIn(b"chat_id=42", request.data)
    self.assertNotIn(b"chat_id=99", request.data)


if __name__ == "__main__":
  unittest.main()
