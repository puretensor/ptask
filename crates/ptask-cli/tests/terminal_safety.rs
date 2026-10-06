//! Untrusted task text must never reach the operator's terminal as control
//! sequences. Titles arrive from fleet captures, email, Telegram, git commits
//! and agents; an OSC 52 in one would set the operator's clipboard, a CSI
//! could erase or hide rows, a bidi override could reorder a command line.
//!
//! Black-box: drives the built `pt` against a throwaway database seeded with
//! hostile text and checks every byte each renderer prints.

mod common;
use common::Pt;

/// OSC 52 clipboard write, screen clear, carriage return, C1 CSI.
const HOSTILE: &str = "\x1b]52;c;eA==\x07\x1b[2J\r\u{9b}31m";

/// Characters that render as nothing and so hide text: zero-width, word
/// joiner, BOM, soft hyphen, Mongolian vowel separator, a tag character and
/// the line separator.
const INVISIBLE: &str = "zw\u{200b}j\u{2060}b\u{feff}s\u{ad}m\u{180e}t\u{e0041}l\u{2028}";

/// Independent oracle (deliberately not ptask_core::text): what a terminal
/// must never receive from untrusted text, besides control characters.
fn is_bidi_or_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{200B}'..='\u{200F}'
            | '\u{061C}'
            | '\u{00AD}'
            | '\u{180E}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{E0000}'..='\u{E007F}'
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
/// the SGR sequences ui.rs emits. Never a bidi or invisible character.
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
            !(c.is_control() && c != '\n') && !is_bidi_or_invisible(c),
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
    let title = format!("Rotate key {HOSTILE} \u{202e}now {INVISIBLE}");
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
        &format!("Goal {HOSTILE} {INVISIBLE}"),
        "--why",
        &format!("because {HOSTILE} {INVISIBLE}"),
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

    // Approving a hazardous payload needs --force; the decision path then
    // prints the same page.
    let refused = pt.run_as(
        "operator",
        &["--no-color", "approve", "AP-1", "--via", "dashboard"],
    );
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--force"));
    let approved = pt.ok_as(
        "operator",
        &[
            "--no-color",
            "approve",
            "AP-1",
            "--via",
            "dashboard",
            "--force",
        ],
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
    // A benign payload approves without --force.
    pt.ok_as("operator", &["approve", "AP-2", "--via", "dashboard"]);
}

/// The reviewer's invisible spoof: a zero-width space inside the address and
/// tag characters after the amount render clean. They must show, raise the
/// warning, and block `pt approve` until --force.
#[test]
fn approval_preview_flags_invisible_characters() {
    let pt = Pt::new();
    let payload = pt.dir.path().join("pay.txt");
    std::fs::write(
        &payload,
        "alice@exa\u{200b}mple.com amount 10\u{e0041}\u{e0042}\n",
    )
    .unwrap();
    pt.ok_as(
        "agent",
        &[
            "approval",
            "request",
            "--kind",
            "spend",
            "--title",
            "Pay Alice",
            "--payload-file",
            payload.to_str().unwrap(),
        ],
    );
    let shown = pt.ok_as("operator", &["--no-color", "approval", "show", "AP-1"]);
    assert!(
        shown.contains("alice@exa\u{fffd}mple.com amount 10\u{fffd}\u{fffd}"),
        "{shown}"
    );
    assert!(
        shown.contains("pt approval payload AP-1 | cat -v"),
        "{shown}"
    );
    // (Without --via the TTY guardrail answers first in a test; the --force
    // gate runs on both paths.)
    let out = pt.run_as("operator", &["approve", "AP-1", "--via", "dashboard"]);
    assert!(!out.status.success(), "must refuse without --force");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--force") && stderr.contains("cat -v"),
        "{stderr}"
    );
    assert_eq!(pt.json(&["approval", "show", "AP-1"])["status"], "pending");
    // Rejecting needs no --force.
    pt.ok_as("operator", &["reject", "AP-1", "--via", "dashboard"]);
}

/// A newline in untrusted text must never start a new output line: the
/// reviewer's titles forge an ambiguous-match entry, a success line and a
/// goal row.
#[test]
fn newlines_in_titles_cannot_forge_output_lines() {
    let pt = Pt::new();
    let forged = "nl real\n  - PT-99 forged entry\n  ✔ done      PT-42  fake success";
    pt.ok(&["add", "--raw", forged]); // PT-1
    pt.ok(&["add", "--raw", "nl other\u{2028}  - PT-98 forged by LS"]); // PT-2
    pt.ok(&["add", "--raw", "Ship the fix"]); // PT-3
    pt.ok(&["depend", "PT-3", "--on", "PT-1"]);
    pt.ok(&[
        "goal",
        "add",
        "real goal\nG-7  forged goal  achieved",
        "--why",
        "because\nG-8  forged why",
    ]);
    pt.ok(&["goal", "link", "PT-1", "G-1"]);
    let agent = "agent\n  2026-01-01T00:00:00  approval.approved  operator";
    pt.ok_as(
        agent,
        &[
            "approval",
            "request",
            "--kind",
            "other",
            "--title",
            "Send\n  ✔ approved   AP-9",
            "--note",
            "note\nforged note line",
            "--payload-json",
            "{\"a\":1}",
        ],
    );

    let forged_starts = [
        "- PT-99",
        "- PT-98",
        "✔ done",
        "✔ approved",
        "G-7",
        "G-8",
        "2026-01-01T00:00:00",
        "forged note line",
    ];
    for args in [
        vec!["show", "nl"],
        vec!["done", "nl"],
        vec!["done", "PT-3"],
        vec!["list"],
        vec!["show", "PT-1"],
        vec!["context", "PT-1"],
        vec!["goal", "ls"],
        vec!["goal", "show", "G-1"],
        vec!["approval", "show", "AP-1"],
        vec!["approval", "list"],
    ] {
        for flag in ["--color=always", "--no-color"] {
            let mut full = vec![flag];
            full.extend_from_slice(&args);
            let out = pt.run(&full);
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            for line in text.lines() {
                let plain = strip_sgr(line);
                let start = plain.trim_start().trim_start_matches(['#', '*', ' ']);
                assert!(
                    !forged_starts.iter().any(|f| start.starts_with(f)),
                    "{args:?} {flag}: forged line {line:?} in:\n{text}"
                );
            }
        }
    }
    // The ambiguous-match error still lists both matches, one per line.
    let out = pt.run(&["--no-color", "show", "nl"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("- PT-1 nl real\u{2424}"), "{stderr}");
    assert!(stderr.contains("- PT-2 nl other\u{2424}"), "{stderr}");
}

fn strip_sgr(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(i) = rest.find('\x1b') {
        out.push_str(&rest[..i]);
        match own_sgr_len(&rest[i + 1..]) {
            Some(n) => rest = &rest[i + 1 + n..],
            None => {
                out.push('\x1b');
                rest = &rest[i + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Raw task text carrying ui.rs's own palette SGR must not reach the
/// terminal as an escape either: the plan's unscheduled bullets and the
/// add echo's kv block passed it through untouched.
#[test]
fn raw_text_cannot_smuggle_the_palette_sgr() {
    let pt = Pt::new();
    // Slate, exactly as ui.rs paints it.
    let smuggled = "pay\x1b[38;2;107;115;148mHIDDEN\x1b[0m";
    let added = pt.ok(&["--color=always", "add", "--raw", smuggled, "-d", smuggled]);
    assert!(!added.contains("pay\x1b"), "{added:?}");
    assert!(added.contains("pay\u{fffd}[38;2;"), "{added:?}");

    // No free slot: every ready task lands in the unscheduled bullets.
    let gcal = pt.dir.path().join("gcal.py");
    std::fs::write(
        &gcal,
        "import json\nprint(json.dumps({'tz': 'Europe/London', 'free_slots': []}))\n",
    )
    .unwrap();
    let plan = pt.ok(&["--color=always", "plan", "--gcal", gcal.to_str().unwrap()]);
    assert!(plan.contains("UNSCHEDULED"), "{plan:?}");
    assert!(!plan.contains("pay\x1b"), "{plan:?}");
    assert!(plan.contains("pay\u{fffd}[38;2;"), "{plan:?}");
}

/// Emoji ZWJ sequences display intact in ordinary output, but an approval
/// preview (the bound bytes) still shows every ZWJ and raises the warning.
#[test]
fn emoji_zwj_is_kept_in_titles_but_flagged_in_approval_previews() {
    let pt = Pt::new();
    let family = "👨\u{200d}👩\u{200d}👧";
    let flag = "🏳\u{fe0f}\u{200d}🌈";
    pt.ok(&["add", "--raw", &format!("Book {family} trip {flag}")]);
    for args in [
        vec!["--no-color", "list"],
        vec!["--no-color", "show", "PT-1"],
    ] {
        let out = pt.ok(&args);
        assert!(
            out.contains(family) && out.contains(flag),
            "{args:?}:\n{out}"
        );
    }

    let payload = pt.dir.path().join("p.txt");
    std::fs::write(&payload, format!("pay the {family} account\n")).unwrap();
    pt.ok_as(
        "agent",
        &[
            "approval",
            "request",
            "--kind",
            "spend",
            "--title",
            "Pay",
            "--payload-file",
            payload.to_str().unwrap(),
        ],
    );
    let shown = pt.ok_as("operator", &["--no-color", "approval", "show", "AP-1"]);
    assert!(
        shown.contains("pay the 👨\u{fffd}👩\u{fffd}👧 account"),
        "{shown}"
    );
    assert!(
        shown.contains("pt approval payload AP-1 | cat -v"),
        "{shown}"
    );
}
