//! Magic-word directives for git integrations.
//!
//! Scans commit messages, branch names, and PR titles for transition keywords:
//!
//! - `Fixes PT-42`     mark PT-42 done on merge to default branch
//! - `Closes PT-42`    same as Fixes (Linear-style alias)
//! - `Ref PT-42`       reference only — no transition
//! - `Skip PT-42`      override; suppress any other directive for this PT-N
//!
//! Case-insensitive on the verb; PT-N is uppercased on emit. Multiple
//! directives in one string are returned in order of appearance.

use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Verb {
    Fixes,
    Closes,
    Ref,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directive {
    pub verb: Verb,
    pub pt_id: String,
}

impl Directive {
    pub fn closes_or_fixes(&self) -> bool {
        matches!(self.verb, Verb::Fixes | Verb::Closes)
    }
}

/// Parse all directives from a free-text blob. Returns them in input order.
pub fn parse(text: &str) -> Vec<Directive> {
    let mut found: Vec<(usize, Directive)> = Vec::new();
    // Iterate over the lowercased text but cut PT-N from the original so
    // the verb match is case-insensitive while PT-N stays canonical.
    let lower = text.to_ascii_lowercase();
    for verb in &[Verb::Fixes, Verb::Closes, Verb::Ref, Verb::Skip] {
        let needle = match verb {
            Verb::Fixes => "fixes ",
            Verb::Closes => "closes ",
            Verb::Ref => "ref ",
            Verb::Skip => "skip ",
        };
        let mut search = lower.as_str();
        let mut offset = 0usize;
        while let Some(pos) = search.find(needle) {
            // Confirm word-boundary: previous char (if any) is not alnum/_.
            let abs = offset + pos;
            let boundary_ok = abs == 0
                || lower.as_bytes()[abs - 1].is_ascii_whitespace()
                || matches!(
                    lower.as_bytes()[abs - 1],
                    b':' | b';' | b',' | b'.' | b'(' | b'[' | b'\n'
                );
            if !boundary_ok {
                let step = pos + needle.len();
                search = &search[step..];
                offset += step;
                continue;
            }
            let after = abs + needle.len();
            if let Some(pt) = extract_pt_id(&text[after..]) {
                found.push((
                    abs,
                    Directive {
                        verb: verb.clone(),
                        pt_id: pt,
                    },
                ));
            }
            let step = pos + needle.len();
            search = &search[step..];
            offset += step;
        }
    }
    found.sort_by_key(|(pos, _)| *pos);
    found.into_iter().map(|(_, d)| d).collect()
}

/// Resolve directives into a set of PT-Ns to close. Closes/Fixes mark, but
/// any matching Skip directive on the same PT-N suppresses the close.
pub fn pt_ids_to_close(directives: &[Directive]) -> Vec<String> {
    let skipped: HashSet<&str> = directives
        .iter()
        .filter(|d| d.verb == Verb::Skip)
        .map(|d| d.pt_id.as_str())
        .collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for d in directives {
        if d.closes_or_fixes()
            && !skipped.contains(d.pt_id.as_str())
            && seen.insert(d.pt_id.clone())
        {
            out.push(d.pt_id.clone());
        }
    }
    out
}

/// Read a PT-N token from the start of `s` (after the verb's trailing space).
/// Accepts `PT-N` and `pt-n` forms. Returns the canonical `PT-N` uppercased.
fn extract_pt_id(s: &str) -> Option<String> {
    let trimmed = s.trim_start();
    let upper = trimmed.to_ascii_uppercase();
    let rest = upper.strip_prefix("PT-")?;
    let mut end = 0;
    for (i, ch) in rest.char_indices() {
        if ch.is_ascii_digit() {
            end = i + ch.len_utf8();
        } else {
            break;
        }
    }
    if end == 0 {
        return None;
    }
    Some(format!("PT-{}", &rest[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixes_extracts_canonical_pt_n() {
        let d = parse("Fixes PT-42: do the thing");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].verb, Verb::Fixes);
        assert_eq!(d[0].pt_id, "PT-42");
    }

    #[test]
    fn case_insensitive_verb_uppercase_pt() {
        let d = parse("closes pt-3");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].verb, Verb::Closes);
        assert_eq!(d[0].pt_id, "PT-3");
    }

    #[test]
    fn multiple_directives_preserve_order() {
        let d = parse("Fixes PT-1, also Closes PT-2 and Ref PT-3");
        // Order of appearance.
        let pts: Vec<&str> = d.iter().map(|x| x.pt_id.as_str()).collect();
        assert_eq!(pts, vec!["PT-1", "PT-2", "PT-3"]);
    }

    #[test]
    fn mixed_verbs_preserve_input_order_not_verb_order() {
        let d = parse("Ref PT-3 first, then Closes PT-2, finally Fixes PT-1");
        let pairs: Vec<(&Verb, &str)> = d.iter().map(|x| (&x.verb, x.pt_id.as_str())).collect();
        assert_eq!(
            pairs,
            vec![
                (&Verb::Ref, "PT-3"),
                (&Verb::Closes, "PT-2"),
                (&Verb::Fixes, "PT-1"),
            ]
        );
    }

    #[test]
    fn requires_word_boundary_before_verb() {
        // `prefixes PT-1` should NOT match because 'fixes' is mid-word.
        let d = parse("prefixes PT-1");
        assert!(d.is_empty());
    }

    #[test]
    fn skip_suppresses_close() {
        let d = parse("Fixes PT-7 — wait, Skip PT-7 we're not done");
        let close = pt_ids_to_close(&d);
        assert!(close.is_empty());
    }

    #[test]
    fn close_dedups_within_one_message() {
        let d = parse("Fixes PT-9 / Closes PT-9");
        let close = pt_ids_to_close(&d);
        assert_eq!(close, vec!["PT-9"]);
    }

    #[test]
    fn ref_does_not_close() {
        let d = parse("Ref PT-50 background context");
        let close = pt_ids_to_close(&d);
        assert!(close.is_empty());
    }

    #[test]
    fn no_directive_at_all_is_empty() {
        let d = parse("ship the thing");
        assert!(d.is_empty());
    }

    #[test]
    fn extracts_only_digit_run() {
        let d = parse("Fixes PT-42abc nonsense");
        assert_eq!(d[0].pt_id, "PT-42");
    }
}
