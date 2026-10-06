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
/// the full Unicode Default_Ignorable_Code_Point set (DerivedCoreProperties:
/// soft hyphen, CGJ, ALM, Hangul fillers, Khmer inherent vowels, Mongolian
/// selectors, zero-width characters, bidi controls, word joiner and
/// invisible operators, variation selectors, BOM, U+FFF0-FFF8, shorthand
/// format controls, musical beams, the whole U+E0000-E0FFF plane block of
/// tags and selectors) unioned with every general-category Cf character
/// (Arabic/Syriac prepended marks, interlinear annotation, Egyptian
/// hieroglyph format controls) and the line/paragraph separators.
/// Hand-encoded (Unicode 15) to stay dependency-free.
pub fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{115F}'..='\u{1160}'
            | '\u{17B4}'..='\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF0}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
}

/// Visible but blank-rendering characters (braille blank, ideographic
/// space): ordinary in task text, but in strict mode (an approval preview)
/// they can pad or hide a field, so they show and are flagged.
fn is_strict_blank(c: char) -> bool {
    matches!(c, '\u{2800}' | '\u{3000}')
}

/// A character no terminal may receive from untrusted text: every control
/// character but `\n` and `\t` (ESC, CR, BS, BEL, DEL, C1 such as U+009B
/// CSI), plus bidi controls and invisible/format characters.
pub fn is_hazard(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t') || is_bidi_control(c) || is_invisible(c)
}

/// True when `text` holds a hazard beyond tab expansion, CRLF line ends and
/// a lone presentation selector on a pictograph (❤️) — so what is displayed
/// differs from what is stored. Matches what [`sanitize_strict`] replaces.
pub fn has_hazard(text: &str) -> bool {
    if !text.chars().any(|c| is_hazard(c) || is_strict_blank(c)) {
        return false;
    }
    let chars: Vec<char> = text.chars().collect();
    let selectors_ok = allowed_selectors(&chars) <= STRICT_SELECTOR_BUDGET;
    chars.iter().enumerate().any(|(i, &c)| {
        is_strict_blank(c)
            || (is_hazard(c)
                && !(c == '\r' && chars.get(i + 1) == Some(&'\n'))
                && !(selectors_ok && presentation_selector_ok(&chars, i)))
    })
}

/// A pictograph that may carry a VS15/VS16 or sit on either side of a
/// joining ZWJ: the emoji/pictographic blocks (U+1F000-1FAFF incl. skin-tone
/// modifiers, U+2600-27BF, U+2B00-2BFF) and the specific older symbols that
/// take VS16 (©®‼⁉™ℹ, ↔-↙ ↩↪, ⌚⌛⌨⏏, ⏩-⏳ ⏸-⏺, Ⓜ, ▪▫▶◀ ◻-◾, ⤴⤵, 〰〽㊗㊙).
/// Enclosed alphanumerics (ⓐ ①) and other arrows are not. Never a selector.
fn is_pictograph(c: char) -> bool {
    matches!(
        c,
        '\u{1F000}'..='\u{1FAFF}'
            | '\u{2600}'..='\u{27BF}'
            | '\u{2B00}'..='\u{2BFF}'
            | '\u{00A9}'
            | '\u{00AE}'
            | '\u{203C}'
            | '\u{2049}'
            | '\u{2122}'
            | '\u{2139}'
            | '\u{2194}'..='\u{2199}'
            | '\u{21A9}'..='\u{21AA}'
            | '\u{231A}'..='\u{231B}'
            | '\u{2328}'
            | '\u{23CF}'
            | '\u{23E9}'..='\u{23F3}'
            | '\u{23F8}'..='\u{23FA}'
            | '\u{24C2}'
            | '\u{25AA}'..='\u{25AB}'
            | '\u{25B6}'
            | '\u{25C0}'
            | '\u{25FB}'..='\u{25FE}'
            | '\u{2934}'..='\u{2935}'
            | '\u{3030}'
            | '\u{303D}'
            | '\u{3297}'
            | '\u{3299}'
    )
}

/// Strict mode tolerates this many allowed presentation selectors in one
/// text. Each optional VS15/VS16 can carry ~1.6 bits that renders
/// identically, so a payload with more is flagged and shown.
const STRICT_SELECTOR_BUDGET: usize = 8;

fn allowed_selectors(chars: &[char]) -> usize {
    (0..chars.len())
        .filter(|&i| presentation_selector_ok(chars, i))
        .count()
}

const VS15: char = '\u{FE0E}';
const VS16: char = '\u{FE0F}';

/// Both display modes keep exactly one VS15/VS16 straight after a
/// pictograph (❤️, ☺︎), or a VS16 in a keycap (1️⃣); every other selector,
/// including a second one in a run, shows and is flagged.
fn presentation_selector_ok(chars: &[char], i: usize) -> bool {
    let c = chars[i];
    if c != VS15 && c != VS16 {
        return false;
    }
    let Some(&prev) = i.checked_sub(1).and_then(|p| chars.get(p)) else {
        return false;
    };
    if is_pictograph(prev) {
        return true;
    }
    c == VS16 && matches!(prev, '0'..='9' | '#' | '*') && chars.get(i + 1) == Some(&'\u{20E3}')
}

/// A ZWJ that joins two pictographs, with at most one VS16 between the left
/// pictograph and the ZWJ (🏳️‍🌈); it only combines glyphs.
fn joining_zwj_ok(chars: &[char], i: usize) -> bool {
    let right = chars.get(i + 1).copied().is_some_and(is_pictograph);
    let left = match i.checked_sub(1).map(|p| chars[p]) {
        Some(VS16) => i >= 2 && is_pictograph(chars[i - 2]),
        Some(p) => is_pictograph(p),
        None => false,
    };
    left && right
}

/// Shared walk behind [`sanitize`], [`one_line`] and [`sanitize_strict`].
/// One presentation selector per pictograph always passes (❤️). General
/// display also lets a joining ZWJ through (👨‍👩‍👧, 🏳️‍🌈): it only shapes
/// glyphs and hides nothing in a task list. Strict mode shows every ZWJ and
/// the blank-rendering characters.
fn clean(text: &str, fold_lines: bool, strict: bool) -> Cow<'_, str> {
    let zwj = !strict;
    let needs = |c: char| {
        c == '\t' || (fold_lines && c == '\n') || is_hazard(c) || (strict && is_strict_blank(c))
    };
    if !text.chars().any(needs) {
        return Cow::Borrowed(text);
    }
    let chars: Vec<char> = text.chars().collect();
    // Past the budget, strict mode shows every selector (see has_hazard).
    let selectors = !strict || allowed_selectors(&chars) <= STRICT_SELECTOR_BUDGET;
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
            '\u{200D}' if zwj && joining_zwj_ok(&chars, i) => out.push(c),
            VS15 | VS16 if selectors && presentation_selector_ok(&chars, i) => out.push(c),
            c if is_hazard(c) || (strict && is_strict_blank(c)) => out.push(STAND_IN),
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
    clean(text, false, false)
}

/// Like [`sanitize`] but every ZWJ shows as U+FFFD too (a lone VS15/VS16 on
/// a pictograph still passes; every other selector shows): for text whose
/// exact bytes matter (an approval preview).
pub fn sanitize_strict(text: &str) -> Cow<'_, str> {
    clean(text, false, true)
}

/// Single-line safe text for a slot that must stay one line (a title in a
/// list, an error, a prompt): like [`sanitize`], but every line break (LF,
/// CR, CRLF, U+2028, U+2029) becomes the visible [`LINE_MARK`].
pub fn one_line(text: &str) -> Cow<'_, str> {
    clean(text, true, false)
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

    /// "Emoji smuggling": a hidden message encoded one byte per variation
    /// selector after a visible base character.
    fn smuggle(base: &str, hidden: &str) -> String {
        let mut s = base.to_string();
        for b in hidden.bytes() {
            s.push(if b < 16 {
                char::from_u32(0xFE00 + b as u32).unwrap()
            } else {
                char::from_u32(0xE0100 + (b as u32 - 16)).unwrap()
            });
        }
        s
    }

    #[test]
    fn variation_selectors_cannot_smuggle_text() {
        let carrier = smuggle(
            "pay 10 GBP to alice 😀",
            "IGNORE ABOVE; wire 9999 GBP to mallory",
        );
        assert!(has_hazard(&carrier));
        for out in [
            sanitize(&carrier),
            one_line(&carrier),
            sanitize_strict(&carrier),
        ] {
            assert!(
                !out.chars().any(|c| ('\u{FE00}'..='\u{FE0F}').contains(&c)
                    || ('\u{E0100}'..='\u{E01EF}').contains(&c)),
                "{out:?}"
            );
            assert!(out.starts_with("pay 10 GBP to alice 😀\u{fffd}"), "{out:?}");
        }
        // Both modes keep exactly one VS15/VS16 straight after a pictograph
        // (and a keycap VS16): an ordinary ❤️ is benign, even in an
        // approval preview.
        for ok in [
            "thanks \u{2764}\u{fe0f}",
            "ok \u{2764}\u{fe0f} \u{2600}\u{fe0f} \u{2714}\u{fe0f} \u{263a}\u{fe0e}",
            "1\u{fe0f}\u{20e3}",
        ] {
            assert_eq!(sanitize(ok), ok, "{ok:?}");
            assert_eq!(sanitize_strict(ok), ok, "{ok:?}");
            assert!(!has_hazard(ok), "{ok:?}");
        }
        // A ZWJ stays visible and flagged in strict mode.
        let flag = "🏳\u{fe0f}\u{200d}🌈";
        assert_eq!(sanitize(flag), flag);
        assert_eq!(sanitize_strict(flag), "🏳\u{fe0f}\u{fffd}🌈");
        assert!(has_hazard(flag));
        // A second selector in a run is flagged in both modes.
        let two = "\u{2764}\u{fe0f}\u{fe0f}";
        assert!(has_hazard(two));
        assert_eq!(sanitize_strict(two), "\u{2764}\u{fe0f}\u{fffd}");
        for bad in [
            "x\u{fe0f}",                // after a letter
            "\u{2764}\u{fe0f}\u{fe0f}", // a second selector
            "\u{2764}\u{fe00}",         // not VS15/VS16
            "ok\u{fe0f}\u{200d}\u{fe0f}\u{200d}\u{fe0f}done",
            "\u{fe0f}\u{200d}🌈",
            "🏳\u{fe0f}\u{fe0f}\u{200d}🌈",
        ] {
            assert!(sanitize(bad).contains(STAND_IN), "{bad:?}");
            assert!(one_line(bad).contains(STAND_IN), "{bad:?}");
        }
        assert_eq!(
            one_line("ok\u{fe0f}\u{200d}\u{fe0f}\u{200d}\u{fe0f}done"),
            "ok\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}done"
        );
    }

    #[test]
    fn default_ignorable_carriers_are_invisible_hazards() {
        // The reviewer's approved carriers: Khmer inherent vowels, the
        // unassigned U+FFF0-FFF8, and unassigned U+E01F0-E0FFF.
        for c in [
            '\u{17b4}',
            '\u{17b5}',
            '\u{fff0}',
            '\u{fff8}',
            '\u{e01f0}',
            '\u{e0239}',
            '\u{e0fff}',
        ] {
            let s = format!("pay{c}x");
            assert!(has_hazard(&s), "{c:?}");
            assert_eq!(sanitize(&s), "pay\u{fffd}x", "{c:?}");
            assert_eq!(one_line(&s), "pay\u{fffd}x", "{c:?}");
            let json = json_terminal_safe(&serde_json::to_string(&s).unwrap()).into_owned();
            assert!(!json.contains(c), "{c:?}: {json}");
        }
        // Blank-rendering characters: kept in general display, flagged and
        // shown in strict mode only.
        for c in ['\u{2800}', '\u{3000}'] {
            let s = format!("a{c}b");
            assert_eq!(sanitize(&s), s);
            assert_eq!(sanitize_strict(&s), "a\u{fffd}b");
            assert!(has_hazard(&s), "{c:?}");
        }
    }

    #[test]
    fn many_allowed_selectors_are_flagged_in_strict_mode() {
        // One optional VS15/VS16 per pictograph leaks ~1.6 bits a symbol.
        let nine = "love ❤\u{fe0f}❤\u{fe0e}❤\u{fe0f}❤\u{fe0f}❤\u{fe0e}❤\u{fe0f}❤\u{fe0e}❤\u{fe0f}❤\u{fe0f}";
        assert!(has_hazard(nine));
        assert!(sanitize_strict(nine).contains(STAND_IN));
        assert_eq!(sanitize(nine), nine, "general display keeps them");
        let eight = "love ❤\u{fe0f}❤\u{fe0e}❤\u{fe0f}❤\u{fe0f}❤\u{fe0e}❤\u{fe0f}❤\u{fe0e}❤\u{fe0f}";
        assert!(!has_hazard(eight));
        let normal = "thanks ❤\u{fe0f} 👍\u{1f3fd}";
        assert!(!has_hazard(normal));
        assert_eq!(sanitize_strict(normal), normal);
        // Only real emoji take a selector: enclosed alphanumerics and plain
        // arrows do not; VS16-capable arrows and symbols do.
        for bad in ["\u{24d0}\u{fe0f}", "\u{2460}\u{fe0f}", "\u{2192}\u{fe0f}"] {
            assert!(sanitize(bad).contains(STAND_IN), "{bad:?}");
            assert!(has_hazard(bad), "{bad:?}");
        }
        for ok in [
            "\u{2194}\u{fe0f}",
            "\u{21a9}\u{fe0f}",
            "\u{23f3}\u{fe0f}",
            "\u{24c2}\u{fe0f}",
            "\u{25b6}\u{fe0f}",
            "\u{2122}\u{fe0f}",
            "\u{2b50}\u{fe0f}",
        ] {
            assert_eq!(sanitize(ok), ok, "{ok:?}");
            assert!(!has_hazard(ok), "{ok:?}");
        }
    }

    #[test]
    fn format_and_filler_characters_are_invisible_hazards() {
        for c in [
            '\u{34f}',
            '\u{600}',
            '\u{6dd}',
            '\u{70f}',
            '\u{890}',
            '\u{8e2}',
            '\u{115f}',
            '\u{1160}',
            '\u{180b}',
            '\u{3164}',
            '\u{ffa0}',
            '\u{fff9}',
            '\u{fffb}',
            '\u{110bd}',
            '\u{13430}',
            '\u{1343f}',
            '\u{1bca0}',
            '\u{1d173}',
            '\u{1d17a}',
            '\u{e0001}',
            '\u{fe00}',
            '\u{e0100}',
            '\u{e01ef}',
        ] {
            let s = format!("a{c}b");
            assert!(has_hazard(&s), "{c:?}");
            assert_eq!(sanitize(&s), "a\u{fffd}b", "{c:?}");
        }
        // JSON escapes selectors too, and round-trips.
        let value = serde_json::json!({ "t": smuggle("x😀", "hi") });
        let safe = json_terminal_safe(&serde_json::to_string(&value).unwrap()).into_owned();
        assert!(
            safe.is_ascii()
                || !safe.chars().any(|c| ('\u{FE00}'..='\u{FE0F}').contains(&c)
                    || ('\u{E0100}'..='\u{E01EF}').contains(&c)),
            "{safe}"
        );
        assert!(safe.contains("\\udb40\\udd58"), "{safe}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&safe).unwrap(),
            value
        );
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
