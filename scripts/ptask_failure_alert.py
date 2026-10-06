#!/usr/bin/env python3
"""Send the systemd failure alert without placing credentials in argv."""

from __future__ import annotations

import os
import sys
import urllib.parse
import urllib.request


def first_nonempty(*names: str) -> str:
    for name in names:
        value = os.environ.get(name, "").strip()
        if value:
            return value
    return ""


def log_err(message: str) -> None:
    """Write to stderr, at journald priority err when stderr is the journal.

    Telegram is the only alert channel, so a failed send must at least reach
    `journalctl -p err`. systemd sets JOURNAL_STREAM=<dev>:<inode> for
    StandardError=journal and honours a leading "<3>" (sd-daemon SD_ERR) on
    each line; anywhere else (a terminal, a redirect) the line stays bare.
    """
    prefix = ""
    stream = os.environ.get("JOURNAL_STREAM", "")
    if stream:
        try:
            st = os.fstat(sys.stderr.fileno())
            if stream == f"{st.st_dev}:{st.st_ino}":
                prefix = "<3>"
        except (OSError, ValueError):
            pass
    print(f"{prefix}{message}", file=sys.stderr)


def send_failure_alert(unit: str, host: str) -> int:
    token = first_nonempty("PTASK_TELEGRAM_BOT_TOKEN", "TELEGRAM_BOT_TOKEN")
    chat_id = first_nonempty(
        "PTASK_ACCOUNTABILITY_CHAT_ID",
        "PTASK_TELEGRAM_DIGEST_CHATS",
        "TELEGRAM_CHAT_ID",
    ).split(",", 1)[0].strip()
    if not token or not chat_id:
        log_err(f"ptask failure alert is not configured; {unit} failed on {host}")
        return 64

    text = (
        f"🔴 ptask unit FAILED on {host}: {unit} - "
        f"journalctl --user -u {unit} -n 30"
    )
    body = urllib.parse.urlencode({"chat_id": chat_id, "text": text}).encode()
    request = urllib.request.Request(
        f"https://api.telegram.org/bot{token}/sendMessage",
        data=body,
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            if not 200 <= response.status < 300:
                raise RuntimeError("non-success response")
    except Exception:
        # Exception strings may contain Request.full_url, which contains the
        # credential. Keep the journal message deliberately generic.
        log_err(f"ptask failure alert delivery failed; {unit} failed on {host}")
        return 1
    return 0


def main() -> int:
    if len(sys.argv) != 3:
        log_err("usage: ptask-failure-alert UNIT HOST")
        return 64
    return send_failure_alert(sys.argv[1], sys.argv[2])


if __name__ == "__main__":
    raise SystemExit(main())
