//! Terminal safety for untrusted text.
//!
//! Task, goal and approval text arrives from fleet captures, email,
//! Telegram, git and agents. Every surface that prints it to a terminal (the
//! CLI, the TUI) or embeds it in a message meant for one (core errors) goes
//! through here, so one predicate decides what a terminal may never see.

use std::borrow::Cow;

/// Visible, width-1 stand-in for a character the terminal would act on or
/// that would hide itself.
pub const STAND_IN: char = '\u{FFFD}';

/// Visible marker for a line break folded into a single-line slot (U+2424
/// SYMBOL FOR NEWLINE), so a title cannot forge an output line.
pub const LINE_MARK: char = '\u{2424}';

/// Bidi embeddings, overrides, isolates and marks: they reorder what follows.
pub fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}'
    )
}

/// Characters that render as nothing (or reflow lines) and so hide text:
/// zero-width and format characters, the line/paragraph separators, the
/// soft hyphen, the Mongolian vowel separator, BOM and the tag block.
pub fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{E0000}'..='\u{E007F}'
    )
}

/// A character no terminal may receive from untrusted text: every control
/// character but `\n` and `\t` (ESC, CR, BS, BEL, DEL, C1 such as U+009B
/// CSI), plus bidi controls and invisible/format characters.
pub fn is_hazard(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t') || is_bidi_control(c) || is_invisible(c)
}

/// True when `text` holds a hazard beyond tab expansion and CRLF line ends —
/// so what is displayed differs from what is stored.
pub fn has_hazard(text: &str) -> bool {
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' && chars.peek() == Some(&'\n') {
            continue;
        }
        if is_hazard(c) {
            return true;
        }
    }
    false
}

/// A character that may sit on either side of a joining ZWJ in an emoji
/// sequence: pictographs (including skin-tone modifiers), dingbats, misc
/// symbols and the text/emoji variation selectors.
fn is_emoji_part(c: char) -> bool {
    matches!(
        c,
        '\u{1F000}'..='\u{1FAFF}'
            | '\u{2190}'..='\u{21FF}'
            | '\u{2300}'..='\u{23FF}'
            | '\u{2460}'..='\u{24FF}'
            | '\u{25A0}'..='\u{27BF}'
            | '\u{2900}'..='\u{297F}'
            | '\u{2B00}'..='\u{2BFF}'
            | '\u{3030}'
            | '\u{303D}'
            | '\u{3297}'
            | '\u{3299}'
            | '\u{00A9}'
            | '\u{00AE}'
            | '\u{203C}'
            | '\u{2049}'
            | '\u{2122}'
            | '\u{2139}'
            | '\u{FE0E}'
            | '\u{FE0F}'
    )
}

/// Shared walk behind [`sanitize`], [`one_line`] and [`sanitize_strict`].
/// `emoji_zwj` lets a U+200D through when both neighbours are emoji parts:
/// it only joins glyphs (👨‍👩‍👧, 🏳️‍🌈) and hides nothing in a task list.
fn clean(text: &str, fold_lines: bool, emoji_zwj: bool) -> Cow<'_, str> {
    let needs = |c: char| c == '\t' || (fold_lines && c == '\n') || is_hazard(c);
    if !text.chars().any(needs) {
        return Cow::Borrowed(text);
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\t' => out.push(' '),
            '\r' if chars.get(i + 1) == Some(&'\n') => {
                if fold_lines {
                    out.push(LINE_MARK);
                    i += 1;
                }
            }
            '\r' | '\n' | '\u{2028}' | '\u{2029}' if fold_lines => out.push(LINE_MARK),
            '\u{200D}'
                if emoji_zwj
                    && i > 0
                    && is_emoji_part(chars[i - 1])
                    && chars.get(i + 1).copied().is_some_and(is_emoji_part) =>
            {
                out.push(c)
            }
            c if is_hazard(c) => out.push(STAND_IN),
            c => out.push(c),
        }
        i += 1;
    }
    if out == text {
        return Cow::Borrowed(text);
    }
    Cow::Owned(out)
}

/// Multi-line safe text for general display: hazards become U+FFFD, a tab a
/// space, a CRLF line end `\n`. Newlines survive, and so does a ZWJ inside
/// an emoji sequence. Borrows when there is nothing to change.
pub fn sanitize(text: &str) -> Cow<'_, str> {
    clean(text, false, true)
}

/// Like [`sanitize`] but every ZWJ shows as U+FFFD too: for text whose exact
/// bytes matter (an approval preview), where nothing invisible may pass.
pub fn sanitize_strict(text: &str) -> Cow<'_, str> {
    clean(text, false, false)
}

/// Single-line safe text for a slot that must stay one line (a title in a
/// list, an error, a prompt): like [`sanitize`], but every line break (LF,
/// CR, CRLF, U+2028, U+2029) becomes the visible [`LINE_MARK`].
pub fn one_line(text: &str) -> Cow<'_, str> {
    clean(text, true, true)
}

/// Characters serde_json leaves raw but a terminal would act on: DEL, C1,
/// bidi controls and invisible/format characters.
fn json_needs_escape(c: char) -> bool {
    ('\u{7F}'..='\u{9F}').contains(&c) || is_bidi_control(c) || is_invisible(c)
}

/// Re-escape serialized JSON so it is terminal-safe: serde_json escapes C0
/// controls only, so DEL, C1, bidi and invisible characters are rewritten as
/// `\uXXXX` (surrogate pairs above the BMP). Those characters can only occur
/// inside JSON strings, so the result is valid JSON with the same value.
pub fn json_terminal_safe(json: &str) -> Cow<'_, str> {
    if !json.chars().any(json_needs_escape) {
        return Cow::Borrowed(json);
    }
    let mut out = String::with_capacity(json.len() + 16);
    for c in json.chars() {
        if json_needs_escape(c) {
            let mut units = [0u16; 2];
            for u in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{:04x}", u));
            }
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOSTILE: &str = "\x1b]52;c;eA==\x07\x1b[2J\r\u{9b}31m";

    #[test]
    fn emoji_zwj_sequences_survive_general_display() {
        for seq in [
            "👨\u{200d}👩\u{200d}👧",               // family
            "🏳\u{fe0f}\u{200d}🌈",                  // rainbow flag (VS16 before ZWJ)
            "❤\u{fe0f}\u{200d}🔥",                  // heart on fire (BMP base)
            "🧑\u{1f3fd}\u{200d}💻",                // skin tone before ZWJ
            "ok \u{2764}\u{fe0f} \u{263a}\u{fe0e}", // variation selectors
        ] {
            let title = format!("ship {seq} today");
            assert_eq!(sanitize(&title), title, "{seq:?}");
            assert_eq!(one_line(&title), title, "{seq:?}");
        }
        // A ZWJ that joins no emoji still shows: letters, edges, doubled.
        for s in [
            "a\u{200d}b",
            "\u{200d}👩",
            "👨\u{200d}",
            "👨\u{200d}\u{200d}👩",
        ] {
            assert!(sanitize(s).contains(STAND_IN), "{s:?}");
            assert!(one_line(s).contains(STAND_IN), "{s:?}");
        }
    }

    #[test]
    fn sanitize_keeps_newlines_and_neutralises_hazards() {
        assert!(matches!(
            sanitize("plain · ünïcode 日本\nnext"),
            Cow::Borrowed(_)
        ));
        assert_eq!(
            sanitize(HOSTILE),
            "\u{fffd}]52;c;eA==\u{fffd}\u{fffd}[2J\u{fffd}\u{fffd}31m"
        );
        assert_eq!(sanitize("a\r\nb\n\tc"), "a\nb\n c");
        for c in [
            '\u{202e}',
            '\u{2066}',
            '\u{2069}',
            '\u{200b}',
            '\u{200d}',
            '\u{2060}',
            '\u{2064}',
            '\u{feff}',
            '\u{ad}',
            '\u{180e}',
            '\u{e0041}',
            '\u{e007f}',
            '\u{2028}',
            '\u{2029}',
            '\u{7f}',
            '\u{85}',
        ] {
            assert_eq!(sanitize(&format!("a{c}b")), "a\u{fffd}b", "{:?}", c);
            assert!(has_hazard(&format!("a{c}b")), "{:?}", c);
        }
        assert!(!has_hazard("Dear Alan,\r\n\tthe numbers.\n"));
    }

    #[test]
    fn one_line_folds_every_line_break_to_a_visible_mark() {
        let forged = "nl real\n  - PT-99 forged entry\r\n  ✔ done PT-42\rx\u{2028}y\u{2029}z";
        let out = one_line(forged);
        assert!(!out.contains(['\n', '\r', '\u{2028}', '\u{2029}']), "{out}");
        assert_eq!(out.matches(LINE_MARK).count(), 5, "{out}");
        assert_eq!(one_line("a\tb\x1b"), "a b\u{fffd}");
        assert!(matches!(one_line("clean"), Cow::Borrowed(_)));
    }

    #[test]
    fn json_escapes_what_serde_leaves_raw_and_round_trips() {
        let value = serde_json::json!({
            "t": "del\u{7f} c1\u{9b} rlo\u{202e} zw\u{200b} tag\u{e0041} ls\u{2028} ok ünï"
        });
        let raw = serde_json::to_string(&value).unwrap();
        let safe = json_terminal_safe(&raw);
        assert!(
            safe.chars()
                .all(|c| !json_needs_escape(c) && !c.is_control()),
            "{safe}"
        );
        assert!(safe.contains("\\udb40\\udc41"), "{safe}");
        assert!(safe.contains("ünï"));
        let back: serde_json::Value = serde_json::from_str(&safe).unwrap();
        assert_eq!(back, value);
    }
}
