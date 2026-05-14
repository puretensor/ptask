//! File-based source collectors.
//!
//! v0.8.8 — ports the in-distill collectors from `~/puretensor-tasks/ingest/distill.py`:
//!
//!   * [`voice_kb`] — parses `~/voice-kb/kb/*.md` markdown frontmatter for
//!     `action_items:` and falls back to scanning a `## Action Items` section.
//!   * [`cc_reports`] — scans `~/reports/cc/*.md` for `## Next Steps` /
//!     `## Actions` / `## TODO` / `## Tasks` / `## Follow-up` sections and
//!     collects bullet items.
//!
//! External-API collectors (Gmail, Drive, Telegram) need OAuth flows that
//! the operator must complete interactively; native ports stay queued for
//! v0.9.x while the existing Python collectors keep writing to `raw_items`
//! on their own schedules.

use anyhow::Result;
use jiff::Zoned;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Default look-back window for `collect_*` calls, mirrors `RECENT_DAYS`.
pub const DEFAULT_RECENT_DAYS: i64 = 30;

/// One staged item heading toward `raw_items`. Field set matches the
/// Python `items.append(...)` rows in `distill.py` so the v0.9.0 cutover
/// is a schema-equivalent swap.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RawItem {
    pub text: String,
    pub source_type: String,
    pub source_file: String,
    pub source_date: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_commitment_score: Option<f64>,
}

pub fn default_voice_kb_path() -> PathBuf {
    if let Ok(p) = std::env::var("VOICE_KB_PATH") {
        return PathBuf::from(p);
    }
    home()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("voice-kb/kb")
}

pub fn default_reports_path() -> PathBuf {
    if let Ok(p) = std::env::var("REPORTS_PATH") {
        return PathBuf::from(p);
    }
    home()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("reports/cc")
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

// ----- voice KB --------------------------------------------------------------

/// Collect raw items from the voice KB tree.
pub fn collect_voice_kb(days: i64) -> Result<Vec<RawItem>> {
    collect_voice_kb_at(&default_voice_kb_path(), days, &Zoned::now())
}

/// Test-friendly variant. `dir` is the voice-kb directory, `anchor` is the
/// "now" anchor used for the look-back window.
pub fn collect_voice_kb_at(dir: &Path, days: i64, anchor: &Zoned) -> Result<Vec<RawItem>> {
    if !dir.exists() {
        warn!(target: "ptask::collectors", path = %dir.display(), "voice kb missing");
        return Ok(Vec::new());
    }
    let cutoff_str = if days > 0 {
        Some(
            anchor
                .saturating_sub(jiff::Span::new().days(days))
                .strftime("%Y%m%d")
                .to_string(),
        )
    } else {
        None
    };

    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("md"))
        .collect();
    files.sort();
    files.reverse();

    if let Some(cutoff) = &cutoff_str {
        files.retain(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .map(|stem| stem.get(..8).unwrap_or("00000000") >= cutoff.as_str())
                .unwrap_or(false)
        });
    }

    let mut items = Vec::new();
    let n_files = files.len();
    for fp in files {
        let raw = match std::fs::read_to_string(&fp) {
            Ok(s) => s,
            Err(e) => {
                debug!(target: "ptask::collectors", file = %fp.display(), error = ?e, "read failed");
                continue;
            }
        };
        let stem = fp.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let date_str = stem.chars().take(8).collect::<String>();
        let action_items = parse_action_items(&raw);
        for txt in action_items {
            let trimmed = txt.trim();
            if trimmed.is_empty() {
                continue;
            }
            items.push(RawItem {
                text: trimmed.to_string(),
                source_type: "voice_memo".to_string(),
                source_file: fp.display().to_string(),
                source_date: date_str.clone(),
                base_commitment_score: Some(0.70),
            });
        }
    }
    info!(target: "ptask::collectors", items = items.len(), files = n_files, "voice kb collected");
    Ok(items)
}

/// Extract `action_items` from a markdown file with optional `---` YAML
/// frontmatter. Falls back to scanning a `## Action Items` heading.
pub fn parse_action_items(raw: &str) -> Vec<String> {
    let (front, body) = split_frontmatter(raw);
    let mut from_yaml = parse_action_items_from_yaml(front);
    if from_yaml.is_empty() && contains_action_items_header(body) {
        from_yaml.extend(extract_section_bullets(body, &["action items"]));
    }
    from_yaml
}

fn split_frontmatter(raw: &str) -> (&str, &str) {
    if !raw.starts_with("---") {
        return ("", raw);
    }
    let after = &raw[3..];
    if let Some(end) = after.find("\n---") {
        let yaml = &after[..end];
        let body_start = end + "\n---".len();
        let body = &after[body_start..];
        // Skip newline after closing `---` if present.
        let body = body.strip_prefix('\n').unwrap_or(body);
        (yaml, body)
    } else {
        ("", raw)
    }
}

/// Tiny `action_items:` list parser. Handles the YAML shapes the operator's
/// pipeline actually emits — `- "text"` and `- text` per line, indented under
/// `action_items:`. Anything more exotic is silently skipped (and the
/// fallback section scanner picks it up).
fn parse_action_items_from_yaml(yaml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_list = false;
    let mut list_indent: Option<usize> = None;
    for line in yaml.lines() {
        if !in_list {
            if let Some(rest) = line.split_once(':') {
                if rest.0.trim() == "action_items" {
                    in_list = true;
                    let trailing = rest.1.trim();
                    if !trailing.is_empty() && !trailing.starts_with('#') {
                        // Single-line form: `action_items: ["a", "b"]`.
                        out.extend(parse_inline_list(trailing));
                        in_list = false;
                    }
                }
            }
            continue;
        }
        // In-list state.
        let indent = line.chars().take_while(|c| *c == ' ').count();
        let trimmed = &line[indent..];
        if trimmed.is_empty() {
            continue;
        }
        if !trimmed.starts_with('-') {
            // A new key at lower indent terminates the list.
            if list_indent.is_some_and(|li| indent < li) || list_indent.is_none() {
                in_list = false;
            }
            continue;
        }
        if list_indent.is_none() {
            list_indent = Some(indent);
        }
        let item = trimmed.trim_start_matches('-').trim();
        if let Some(s) = unquote_yaml_scalar(item) {
            out.push(s);
        }
    }
    out
}

fn parse_inline_list(s: &str) -> Vec<String> {
    let s = s.trim();
    if !s.starts_with('[') || !s.ends_with(']') {
        return Vec::new();
    }
    let inner = &s[1..s.len() - 1];
    let mut out = Vec::new();
    for part in inner.split(',') {
        if let Some(v) = unquote_yaml_scalar(part.trim()) {
            out.push(v);
        }
    }
    out
}

fn unquote_yaml_scalar(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        Some(s[1..s.len() - 1].to_string())
    } else {
        Some(s.to_string())
    }
}

fn contains_action_items_header(body: &str) -> bool {
    body.lines().any(|l| {
        let lo = l.to_ascii_lowercase();
        l.starts_with('#') && lo.contains("action items")
    })
}

// ----- CC reports ------------------------------------------------------------

const REPORT_HEADING_KEYWORDS: &[&str] = &["next step", "action", "todo", "task", "follow"];

pub fn collect_cc_reports(days: i64) -> Result<Vec<RawItem>> {
    collect_cc_reports_at(&default_reports_path(), days, &Zoned::now())
}

pub fn collect_cc_reports_at(dir: &Path, days: i64, anchor: &Zoned) -> Result<Vec<RawItem>> {
    if !dir.exists() {
        warn!(target: "ptask::collectors", path = %dir.display(), "reports dir missing");
        return Ok(Vec::new());
    }
    let cutoff_str = anchor
        .saturating_sub(jiff::Span::new().days(days.max(0)))
        .strftime("%Y-%m-%d")
        .to_string();

    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("md"))
        .collect();
    files.sort();
    files.reverse();
    files.retain(|p| {
        p.file_stem()
            .and_then(|s| s.to_str())
            .map(|stem| stem.get(..10).unwrap_or("0000-00-00") >= cutoff_str.as_str())
            .unwrap_or(false)
    });

    let mut items = Vec::new();
    let n_files = files.len();
    for fp in files {
        let Ok(content) = std::fs::read_to_string(&fp) else {
            continue;
        };
        let stem = fp.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let date_str = stem.chars().take(10).collect::<String>();
        let bullets = extract_section_bullets(&content, REPORT_HEADING_KEYWORDS);
        for text in bullets {
            if text.len() > 8 {
                items.push(RawItem {
                    text,
                    source_type: "cc_report".to_string(),
                    source_file: fp.display().to_string(),
                    source_date: date_str.clone(),
                    base_commitment_score: None,
                });
            }
        }
    }
    info!(target: "ptask::collectors", items = items.len(), files = n_files, "cc reports collected");
    Ok(items)
}

/// Walk a markdown body, return bullet lines under any heading whose text
/// (lowercased) contains one of `keywords`. Stops a section at the next
/// heading that doesn't match.
fn extract_section_bullets(body: &str, keywords: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_section = false;
    for line in body.lines() {
        let lower = line.to_ascii_lowercase();
        if line.starts_with('#') {
            let matches = keywords.iter().any(|k| lower.contains(k));
            in_section = matches;
            continue;
        }
        if !in_section {
            continue;
        }
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            let text = rest.trim().to_string();
            if !text.is_empty() {
                out.push(text);
            }
        }
    }
    out
}

// ----- tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;

    fn z(s: &str) -> Zoned {
        s.parse::<Timestamp>()
            .unwrap()
            .to_zoned(jiff::tz::TimeZone::UTC)
    }

    #[test]
    fn parse_action_items_from_yaml_list() {
        let md = "---\naction_items:\n  - \"buy bread\"\n  - call alex\n---\n# body\n";
        let items = parse_action_items(md);
        assert_eq!(items, vec!["buy bread", "call alex"]);
    }

    #[test]
    fn parse_action_items_inline_list() {
        let md = "---\naction_items: [\"buy bread\", \"call alex\"]\n---\n";
        let items = parse_action_items(md);
        assert_eq!(items, vec!["buy bread", "call alex"]);
    }

    #[test]
    fn parse_action_items_falls_back_to_section() {
        let md = "---\ntitle: x\n---\nIntro.\n\n## Action Items\n- buy bread\n* call alex\n";
        let items = parse_action_items(md);
        assert_eq!(items, vec!["buy bread", "call alex"]);
    }

    #[test]
    fn extract_section_bullets_stops_at_next_heading() {
        let md = "## Next Steps\n- a\n- b\n\n## Other\n- c\n";
        let out = extract_section_bullets(md, REPORT_HEADING_KEYWORDS);
        assert_eq!(out, vec!["a", "b"]);
    }

    #[test]
    fn collect_voice_kb_filters_by_date_prefix() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("20260101-old.md"),
            "---\naction_items:\n  - old item\n---\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("20260513-recent.md"),
            "---\naction_items:\n  - recent item\n---\n",
        )
        .unwrap();
        let anchor = z("2026-05-14T00:00:00Z");
        let items = collect_voice_kb_at(dir.path(), 30, &anchor).unwrap();
        let titles: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(titles, vec!["recent item"]);
    }

    #[test]
    fn collect_voice_kb_keeps_all_with_zero_days() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("20200101-ancient.md"),
            "---\naction_items:\n  - ancient item\n---\n",
        )
        .unwrap();
        let anchor = z("2026-05-14T00:00:00Z");
        let items = collect_voice_kb_at(dir.path(), 0, &anchor).unwrap();
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn collect_voice_kb_handles_missing_dir() {
        let anchor = z("2026-05-14T00:00:00Z");
        let items = collect_voice_kb_at(Path::new("/nonexistent/voice/kb"), 30, &anchor).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn collect_cc_reports_picks_up_next_steps_bullets() {
        let dir = tempfile::tempdir().unwrap();
        let body = "# Session\n\n## Summary\nstuff\n\n## Next Steps\n\
                    - Item one that is sufficiently long\n\
                    - Item two with detail\n\n## Notes\n- skip me\n";
        std::fs::write(dir.path().join("2026-05-13_session.md"), body).unwrap();
        let anchor = z("2026-05-14T00:00:00Z");
        let items = collect_cc_reports_at(dir.path(), 30, &anchor).unwrap();
        let titles: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(
            titles,
            vec!["Item one that is sufficiently long", "Item two with detail"]
        );
        assert_eq!(items[0].source_type, "cc_report");
        assert_eq!(items[0].source_date, "2026-05-13");
    }

    #[test]
    fn collect_cc_reports_filters_by_iso_date_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let body = "## Next Steps\n- some item that is long enough\n";
        std::fs::write(dir.path().join("2024-01-01_ancient.md"), body).unwrap();
        std::fs::write(dir.path().join("2026-05-13_recent.md"), body).unwrap();
        let anchor = z("2026-05-14T00:00:00Z");
        let items = collect_cc_reports_at(dir.path(), 30, &anchor).unwrap();
        // 30-day window from 2026-05-14 should include 2026-05-13 only.
        assert_eq!(items.len(), 1);
        assert!(items[0].source_file.contains("2026-05-13"));
    }
}
