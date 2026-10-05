//! Untrusted task text must never reach the operator's terminal as control
//! sequences. Titles arrive from fleet captures, email, Telegram, git commits
//! and agents; an OSC 52 in one would set the operator's clipboard, a CSI
//! could erase or hide rows, a bidi override could reorder a command line.
//!
//! Black-box: drives the built `pt` against a throwaway database seeded with
//! hostile text and checks every byte each renderer prints.

use std::process::{Command, Output, Stdio};

/// OSC 52 clipboard write, screen clear, carriage return, C1 CSI.
const HOSTILE: &str = "\x1b]52;c;eA==\x07\x1b[2J\r\u{9b}31m";

struct Pt {
    dir: tempfile::TempDir,
}

impl Pt {
    fn new() -> Self {
        Pt {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_as("test", args)
    }

    fn run_as(&self, actor: &str, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pt"))
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.dir.path())
            .env("PTASK_DB", self.dir.path().join("tasks.db"))
            .env("PTASK_ACTOR", actor)
            .env("COLUMNS", "120")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        self.ok_as("test", args)
    }

    fn ok_as(&self, actor: &str, args: &[&str]) -> String {
        let out = self.run_as(actor, args);
        assert!(
            out.status.success(),
            "pt {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
}

fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}'
    )
}

/// Length of the SGR sequence after an ESC, if it is one of the forms ui.rs
/// itself emits: reset, bold, dim, or a 24-bit foreground.
fn own_sgr_len(tail: &str) -> Option<usize> {
    for fixed in ["[0m", "[1m", "[2m"] {
        if tail.starts_with(fixed) {
            return Some(fixed.len());
        }
    }
    let body = tail.strip_prefix("[38;2;")?;
    let end = body.find('m')?;
    let channels: Vec<&str> = body[..end].split(';').collect();
    let ok = channels.len() == 3
        && channels.iter().all(|ch| {
            !ch.is_empty()
                && ch.len() <= 3
                && ch.bytes().all(|b| b.is_ascii_digit())
                && ch.parse::<u16>().is_ok_and(|v| v <= 255)
        });
    ok.then_some("[38;2;".len() + end + 1)
}

/// Colour off: no control character but '\n'. Colour on: additionally only
/// the SGR sequences ui.rs emits. Never a bidi control.
fn assert_terminal_safe(label: &str, text: &str, colour: bool) {
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if c == '\x1b' && colour {
            match own_sgr_len(&rest[1..]) {
                Some(n) => {
                    rest = &rest[1 + n..];
                    continue;
                }
                None => panic!(
                    "{label}: foreign escape sequence {:?} in:\n{text}",
                    &rest[..rest.len().min(16)]
                ),
            }
        }
        assert!(
            !(c.is_control() && c != '\n') && !is_bidi_control(c),
            "{label}: control character {c:?} reached the terminal in:\n{text}"
        );
        rest = &rest[c.len_utf8()..];
    }
}

fn check(pt: &Pt, label: &str, args: &[&str]) {
    for (colour, flag) in [(true, "--color=always"), (false, "--no-color")] {
        let mut full = vec![flag];
        full.extend_from_slice(args);
        let out = pt.run(&full);
        let stdout = String::from_utf8(out.stdout).unwrap();
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert!(
            !stdout.is_empty() || !stderr.is_empty(),
            "{label}: printed nothing"
        );
        assert_terminal_safe(&format!("{label} {flag} stdout"), &stdout, colour);
        assert_terminal_safe(&format!("{label} {flag} stderr"), &stderr, colour);
    }
}

#[test]
fn hostile_task_text_never_reaches_the_terminal_raw() {
    let pt = Pt::new();
    let title = format!("Rotate key {HOSTILE} \u{202e}now");
    pt.ok(&["add", "--raw", &title]); // PT-1
    pt.ok(&["add", "--raw", &format!("Rotate key {HOSTILE} later")]); // PT-2
    pt.ok(&["add", "--raw", "Ship the fix"]); // PT-3
    pt.ok(&[
        "edit",
        "PT-1",
        "--label",
        &format!("ops{HOSTILE}"),
        "--desc",
        &format!("first line {HOSTILE}\n\tsecond \u{1b}[8mline"),
    ]);
    pt.ok(&["depend", "PT-3", "--on", "PT-1"]);
    pt.ok(&[
        "goal",
        "add",
        &format!("Goal {HOSTILE}"),
        "--why",
        &format!("because {HOSTILE}"),
    ]);
    pt.ok(&["goal", "link", "PT-1", "G-1"]);
    pt.ok(&["view", "save", "rot", "search: rotate"]);

    // The text is still shown — neutralised, not dropped.
    let shown = pt.ok(&["--no-color", "show", "PT-1"]);
    assert!(shown.contains("]52;c;eA=="), "{shown}");
    assert!(shown.contains('\u{fffd}'), "{shown}");

    for (label, args) in [
        ("add", vec!["add", "--raw", &title]),
        ("list", vec!["list"]),
        ("list -v", vec!["list", "-v"]),
        ("show", vec!["show", "PT-1"]),
        ("next", vec!["next"]),
        ("context", vec!["context", "PT-1"]),
        ("why", vec!["why", "PT-1"]),
        ("log", vec!["log", "PT-1"]),
        ("search", vec!["search", "rotate"]),
        ("review", vec!["review", "--stale-days", "0"]),
        ("delegate", vec!["delegate", "PT-1"]),
        ("depend", vec!["depend", "PT-3"]),
        ("priority", vec!["priority", "PT-2", "high"]),
        ("view show", vec!["view", "show", "rot"]),
        (
            "scoring diff",
            vec!["scoring", "run", "--diff", "--dry-run"],
        ),
        ("goal ls", vec!["goal", "ls"]),
        ("goal show", vec!["goal", "show", "G-1"]),
        // Core errors embed titles: the ambiguous-match list and the
        // blocker list both reach main's error print.
        ("done ambiguous", vec!["done", "rotate"]),
        ("done blocked", vec!["done", "PT-3"]),
        ("rm refused", vec!["rm", "PT-1"]),
    ] {
        check(&pt, label, &args);
    }
}

/// The reviewer's spoof: the stored (and digest-bound) payload wires
/// $50,000 to the attacker, but cursor-up + erase-line rewrite the preview on
/// screen into a $50 refund to the CFO.
const SPOOF_PAYLOAD: &str = "To: attacker@evil.example\nWire: $50,000 to acct 999\n\x1b[2A\r\x1b[2KTo: cfo@puretensor.ai\n\x1b[2KWire: $50 to acct 123 (refund)\n";

#[test]
fn approval_preview_cannot_spoof_the_bound_payload() {
    let pt = Pt::new();
    let payload = pt.dir.path().join("wire.txt");
    std::fs::write(&payload, SPOOF_PAYLOAD).unwrap();
    let agent = format!("agent{HOSTILE}");
    pt.ok_as(
        &agent,
        &[
            "approval",
            "request",
            "--kind",
            "spend",
            "--title",
            &format!("Refund customer {HOSTILE}"),
            "--note",
            "routine refund\x1b[8m",
            "--payload-file",
            payload.to_str().unwrap(),
        ],
    );

    for (colour, flag) in [(true, "--color=always"), (false, "--no-color")] {
        for (actor, args) in [
            ("operator", vec![flag, "approval", "show", "AP-1"]),
            ("operator", vec![flag, "approval", "list"]),
        ] {
            let out = pt.run_as(actor, &args);
            assert!(out.status.success(), "{args:?}");
            let stdout = String::from_utf8(out.stdout).unwrap();
            assert_terminal_safe(&format!("{args:?}"), &stdout, colour);
        }
    }

    let shown = pt.ok_as("operator", &["--no-color", "approval", "show", "AP-1"]);
    assert!(!shown.contains('\x1b') && !shown.contains('\r'), "{shown}");
    assert!(shown.contains("attacker@evil.example"), "{shown}");
    assert!(shown.contains("$50,000"), "{shown}");
    assert!(
        shown.contains("pt approval payload AP-1 | cat -v"),
        "a hazardous payload must carry the inspect warning:\n{shown}"
    );

    // The decision path prints the same page.
    let approved = pt.ok_as(
        "operator",
        &["--no-color", "approve", "AP-1", "--via", "dashboard"],
    );
    assert!(!approved.contains('\x1b') && !approved.contains('\r'));
    assert!(approved.contains("pt approval payload AP-1 | cat -v"));

    // An ordinary CRLF email body with a tab does not cry wolf.
    let email = pt.dir.path().join("email.txt");
    std::fs::write(&email, "Dear Alan,\r\n\tthe Q3 numbers are attached.\r\n").unwrap();
    pt.ok_as(
        "hal",
        &[
            "approval",
            "request",
            "--kind",
            "email",
            "--title",
            "Send Q3",
            "--payload-file",
            email.to_str().unwrap(),
        ],
    );
    let benign = pt.ok_as("operator", &["--no-color", "approval", "show", "AP-2"]);
    assert!(benign.contains("the Q3 numbers are attached."), "{benign}");
    assert!(!benign.contains("cat -v"), "{benign}");
    assert!(
        !benign.contains('\r') && !benign.contains('\u{fffd}'),
        "{benign}"
    );
}
