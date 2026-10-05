//! Todoist-style filter DSL — parser + SQL compiler.
//!
//! Grammar (subset of Todoist's; expanded in later phases):
//!
//! ```text
//! expr       := or_expr
//! or_expr    := and_expr ('|' and_expr)*
//! and_expr   := not_expr ('&' not_expr)*
//! not_expr   := '!' atom | atom
//! atom       := '(' expr ')' | term
//! term       := today
//!             | overdue
//!             | tomorrow
//!             | yesterday
//!             | no date
//!             | recurring
//!             | p1 | p2 | p3 | p4 | p5
//!             | @label
//!             | #project
//!             | due:        <phrase>
//!             | due before: <phrase>
//!             | due after:  <phrase>
//!             | search:     <keyword>
//!             | kind:       scout|ship
//! ```
//!
//! Examples:
//! - `today & p1`
//! - `(today | overdue) & #fleet`
//! - `@waiting & no date`
//! - `due before: next friday & !recurring`
//! - `search: ceph & @ops`

use crate::dates;
use crate::error::{Error, Result};
use jiff::Zoned;

/// Parsed filter AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),

    Today,
    Tomorrow,
    Yesterday,
    Overdue,
    NoDate,
    Recurring,
    Priority(i64), // pTask 1..=5 (native scale: 1=low .. 5=critical)
    Label(String),
    Project(String),
    DueOn(String), // ISO yyyy-mm-dd
    DueBefore(String),
    DueAfter(String),
    Search(String),
    /// `kind: scout` / `kind: ship` — investigation vs implementation.
    Kind(String),
}

/// Longest filter accepted, in bytes. Real filters are a few dozen bytes.
pub const MAX_FILTER_BYTES: usize = 2048;
/// Deepest `(` / `!` nesting accepted; each level is a recursive parser call.
pub const MAX_FILTER_NESTING: usize = 32;
/// Most terms accepted. The AST is compiled and dropped recursively, one
/// stack frame per node, and SQLite rejects expression trees deeper than 1000.
pub const MAX_FILTER_TERMS: usize = 256;

/// Compiled SQL fragment + bound parameter values (positional).
pub struct Sql {
    pub where_clause: String,
    pub params: Vec<rusqlite::types::Value>,
}

/// Public entry point: parse a filter DSL string into an AST.
///
/// Size, nesting and term count are capped here, so every caller (CLI,
/// saved views, `/list`, MCP, the bot) is protected: the parser, `to_sql`
/// and the AST's drop all recurse, and a hostile filter used to abort the
/// whole process with a stack overflow no panic guard can catch.
pub fn parse(input: &str) -> Result<Expr> {
    if input.len() > MAX_FILTER_BYTES {
        return Err(Error::Other(format!(
            "filter: {} bytes is over the {MAX_FILTER_BYTES}-byte limit",
            input.len()
        )));
    }
    let mut p = ParseCtx::new(input);
    p.skip_ws();
    let expr = p.parse_or()?;
    p.skip_ws();
    if p.peek().is_some() {
        return Err(Error::Other(format!(
            "filter: unexpected trailing input at byte {}: {:?}",
            p.pos,
            &input[p.pos..]
        )));
    }
    Ok(expr)
}

/// Compile an AST to a SQL WHERE-clause fragment + positional params.
/// The fragment is intended to be appended to a base query of the shape:
/// `SELECT ... FROM tasks t LEFT JOIN pt_extensions x ON x.task_uuid=t.id`.
pub fn to_sql(expr: &Expr, now: &Zoned) -> Result<Sql> {
    let mut params: Vec<rusqlite::types::Value> = Vec::new();
    let clause = compile(expr, now, &mut params)?;
    Ok(Sql {
        where_clause: clause,
        params,
    })
}

fn compile(expr: &Expr, now: &Zoned, params: &mut Vec<rusqlite::types::Value>) -> Result<String> {
    use rusqlite::types::Value;
    Ok(match expr {
        Expr::And(l, r) => format!(
            "({} AND {})",
            compile(l, now, params)?,
            compile(r, now, params)?
        ),
        Expr::Or(l, r) => format!(
            "({} OR {})",
            compile(l, now, params)?,
            compile(r, now, params)?
        ),
        // Atoms read nullable columns (no deadline, no project, no
        // pt_extensions row for a task without a pt_id), and NOT NULL is
        // NULL, which WHERE drops: `!#fleet` lost every project-less task.
        // An atom that is unknown for a row is false for it, so its
        // negation is true.
        Expr::Not(inner) => format!("(NOT COALESCE(({}), 0))", compile(inner, now, params)?),

        Expr::Today => {
            params.push(Value::Text(now.date().to_string()));
            format!("substr(t.deadline,1,10) = ?{}", params.len())
        }
        Expr::Tomorrow => {
            let d = now.date().checked_add(jiff::Span::new().days(1)).unwrap();
            params.push(Value::Text(d.to_string()));
            format!("substr(t.deadline,1,10) = ?{}", params.len())
        }
        Expr::Yesterday => {
            let d = now.date().checked_sub(jiff::Span::new().days(1)).unwrap();
            params.push(Value::Text(d.to_string()));
            format!("substr(t.deadline,1,10) = ?{}", params.len())
        }
        Expr::Overdue => {
            params.push(Value::Text(now.date().to_string()));
            let today = params.len();
            params.push(Value::Text(dates::format_iso(now)));
            let now_iso = params.len();
            // `julianday()` yields NULL on anything it cannot parse, and a
            // NULL comparison is never true — so an unparseable deadline
            // ("in two weeks", "90 days (for evaluation)") used to drop out
            // of `overdue` silently. Surface it instead: a deadline we can't
            // read is a deadline we can't prove is in the future.
            format!(
                "(t.deadline IS NOT NULL AND t.status NOT IN ('done','dismissed') AND \
                 (julianday(t.deadline) IS NULL OR \
                  (length(t.deadline) = 10 AND substr(t.deadline,1,10) < ?{today}) OR \
                  (length(t.deadline) > 10 \
                   AND julianday(t.deadline) < julianday(?{now_iso}))))"
            )
        }
        Expr::NoDate => "t.deadline IS NULL".to_string(),
        Expr::Recurring => "t.id IN (SELECT task_uuid FROM pt_recurrence)".to_string(),
        Expr::Priority(p) => {
            params.push(Value::Integer(*p));
            format!("t.priority = ?{}", params.len())
        }
        Expr::Label(name) => {
            let json_string = serde_json::to_string(name)
                .map_err(|e| Error::Other(format!("label serialise: {}", e)))?;
            params.push(Value::Text(format!("%{}%", escape_like(&json_string))));
            format!("x.labels LIKE ?{} ESCAPE '\\'", params.len())
        }
        Expr::Project(name) => {
            params.push(Value::Text(name.clone()));
            format!("x.project = ?{}", params.len())
        }
        Expr::DueOn(d) => {
            params.push(Value::Text(d.clone()));
            format!("substr(t.deadline,1,10) = ?{}", params.len())
        }
        Expr::DueBefore(d) => {
            params.push(Value::Text(d.clone()));
            format!("substr(t.deadline,1,10) < ?{}", params.len())
        }
        Expr::DueAfter(d) => {
            params.push(Value::Text(d.clone()));
            format!("substr(t.deadline,1,10) > ?{}", params.len())
        }
        Expr::Kind(k) => {
            params.push(Value::Text(k.clone()));
            format!("COALESCE(t.kind,'ship') = ?{}", params.len())
        }
        Expr::Search(kw) => {
            // LIKE wildcards escaped same way as tasks::resolve.
            let pat = format!("%{}%", escape_like(&kw.to_ascii_lowercase()));
            params.push(Value::Text(pat));
            format!(
                "(lower(t.title) LIKE ?{n} ESCAPE '\\' OR lower(t.description) LIKE ?{n} ESCAPE '\\')",
                n = params.len()
            )
        }
    })
}

pub(crate) fn escape_like(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

// ---- hand-rolled recursive-descent parser ----
//
// The grammar is small enough that a winnow combinator chain would obscure
// rather than clarify. Each method returns a parsed Expr and advances `pos`.

struct ParseCtx<'a> {
    input: &'a str,
    pos: usize,
    /// Current `(` / `!` nesting.
    depth: usize,
    /// Terms parsed so far.
    terms: usize,
}

impl<'a> ParseCtx<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            pos: 0,
            depth: 0,
            terms: 0,
        }
    }

    /// Enter one `(` / `!` level.
    fn descend(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > MAX_FILTER_NESTING {
            return Err(Error::Other(format!(
                "filter: nested deeper than {MAX_FILTER_NESTING} levels at byte {}",
                self.pos
            )));
        }
        Ok(())
    }

    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }

    /// Peek whether the upcoming bytes (case-insensitive) match `s`.
    fn lookahead(&self, s: &str) -> bool {
        let rest = &self.input[self.pos..];
        rest.as_bytes()
            .get(..s.len())
            .map(|b| b.eq_ignore_ascii_case(s.as_bytes()))
            .unwrap_or(false)
    }

    fn parse_or(&mut self) -> Result<Expr> {
        let mut left = self.parse_and()?;
        loop {
            self.skip_ws();
            if self.peek() == Some('|') {
                self.pos += 1;
                let right = self.parse_and()?;
                left = Expr::Or(Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr> {
        let mut left = self.parse_not()?;
        loop {
            self.skip_ws();
            if self.peek() == Some('&') {
                self.pos += 1;
                let right = self.parse_not()?;
                left = Expr::And(Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr> {
        self.skip_ws();
        if self.peek() == Some('!') {
            self.pos += 1;
            self.descend()?;
            let inner = self.parse_atom()?;
            self.depth -= 1;
            Ok(Expr::Not(Box::new(inner)))
        } else {
            self.parse_atom()
        }
    }

    fn parse_atom(&mut self) -> Result<Expr> {
        self.skip_ws();
        if self.peek() == Some('(') {
            self.pos += 1;
            self.descend()?;
            let inner = self.parse_or()?;
            self.skip_ws();
            if self.peek() != Some(')') {
                return Err(Error::Other(format!(
                    "filter: expected ')' at byte {}",
                    self.pos
                )));
            }
            self.pos += 1;
            self.depth -= 1;
            Ok(inner)
        } else {
            self.parse_term()
        }
    }

    fn parse_term(&mut self) -> Result<Expr> {
        self.skip_ws();
        self.terms += 1;
        if self.terms > MAX_FILTER_TERMS {
            return Err(Error::Other(format!(
                "filter: more than {MAX_FILTER_TERMS} terms"
            )));
        }
        // Order matters: longer keywords before shorter overlapping ones.
        for (kw, expr) in &[
            ("today", Expr::Today),
            ("tomorrow", Expr::Tomorrow),
            ("yesterday", Expr::Yesterday),
            ("overdue", Expr::Overdue),
            ("no date", Expr::NoDate),
            ("no deadline", Expr::NoDate),
            ("recurring", Expr::Recurring),
        ] {
            if self.match_keyword(kw) {
                return Ok(expr.clone());
            }
        }
        // due before: / due after: / due:   (order: longest first)
        if self.match_keyword("due before:") {
            let phrase = self.consume_phrase();
            return Ok(Expr::DueBefore(self.resolve_date(&phrase)?));
        }
        if self.match_keyword("due after:") {
            let phrase = self.consume_phrase();
            return Ok(Expr::DueAfter(self.resolve_date(&phrase)?));
        }
        if self.match_keyword("due:") {
            let phrase = self.consume_phrase();
            return Ok(Expr::DueOn(self.resolve_date(&phrase)?));
        }
        if self.match_keyword("kind:") {
            let phrase = self.consume_phrase();
            let kind: crate::tasks::TaskKind = phrase.trim().parse()?;
            return Ok(Expr::Kind(kind.as_str().to_string()));
        }
        if self.match_keyword("search:") {
            let phrase = self.consume_phrase();
            // An empty keyword compiles to LIKE '%%' -- every task.
            if phrase.is_empty() {
                return Err(Error::Other(format!(
                    "filter: empty search: at byte {}",
                    self.pos
                )));
            }
            return Ok(Expr::Search(phrase));
        }
        // p1..p5 — native pTask scale (p1=low .. p5=critical), no inversion.
        if self.lookahead("p") {
            let save = self.pos;
            self.pos += 1;
            let rest = &self.input[self.pos..];
            if let Some(first) = rest.chars().next()
                && let Some(n) = first.to_digit(10)
                && (1..=5).contains(&n)
            {
                self.pos += first.len_utf8();
                return Ok(Expr::Priority(n as i64));
            }
            self.pos = save;
        }
        // @label  / #project
        match self.peek() {
            Some('@') => {
                self.pos += 1;
                let name = self.consume_ident();
                if name.is_empty() {
                    return Err(Error::Other(format!(
                        "filter: empty @label at byte {}",
                        self.pos
                    )));
                }
                return Ok(Expr::Label(name));
            }
            Some('#') => {
                self.pos += 1;
                let name = self.consume_ident();
                if name.is_empty() {
                    return Err(Error::Other(format!(
                        "filter: empty #project at byte {}",
                        self.pos
                    )));
                }
                return Ok(Expr::Project(name));
            }
            _ => {}
        }
        Err(Error::Other(format!(
            "filter: unrecognised term at byte {}: {:?}",
            self.pos,
            self.peek_word()
        )))
    }

    /// Match a literal keyword on a word boundary. Advances on success.
    fn match_keyword(&mut self, kw: &str) -> bool {
        if !self.lookahead(kw) {
            return false;
        }
        // Boundary check: char after kw must not be alphanumeric, except for
        // keywords that themselves end in `:` (where any following char is fine).
        let after = self.pos + kw.len();
        let boundary_ok = kw.ends_with(':')
            || self.input[after..]
                .chars()
                .next()
                .map(|c| !c.is_alphanumeric() && c != '_')
                .unwrap_or(true);
        if boundary_ok {
            self.pos = after;
            true
        } else {
            false
        }
    }

    /// Consume a `@label` / `#project` name: everything up to whitespace or
    /// an operator character, so `domain:mgmt`, `v1.2` and `team/ops` are
    /// whole names (the CLI and MCP write `domain:<x>` labels).
    fn consume_ident(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_whitespace() || matches!(c, '&' | '|' | '(' | ')' | '!') {
                break;
            }
            self.pos += c.len_utf8();
        }
        self.input[start..self.pos].to_string()
    }

    /// Consume the rest of the phrase up to the next boolean operator
    /// (`&` / `|` / `)`) or end of input. Trims surrounding whitespace.
    /// Used for `due:`, `due before:`, `due after:`, `search:` payloads.
    fn consume_phrase(&mut self) -> String {
        self.skip_ws();
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == '&' || c == '|' || c == ')' {
                break;
            }
            self.pos += c.len_utf8();
        }
        self.input[start..self.pos].trim().to_string()
    }

    /// For error messages.
    fn peek_word(&self) -> &str {
        let rest = &self.input[self.pos..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '&' || c == '|' || c == ')')
            .unwrap_or(rest.len());
        &rest[..end]
    }

    /// Resolve a date phrase to ISO yyyy-mm-dd using the dates module.
    fn resolve_date(&self, phrase: &str) -> Result<String> {
        let now = dates::now_in_operator_tz()?;
        let z = dates::parse_at(phrase, now)?;
        Ok(z.date().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::ToSql;

    fn ast(s: &str) -> Expr {
        parse(s).unwrap()
    }

    fn anchor() -> Zoned {
        let tz = jiff::tz::TimeZone::get(dates::OPERATOR_TZ).unwrap();
        jiff::civil::date(2026, 5, 13)
            .at(12, 0, 0, 0)
            .to_zoned(tz)
            .unwrap()
    }

    fn bind_refs(values: &[rusqlite::types::Value]) -> Vec<&dyn ToSql> {
        values.iter().map(|v| v as &dyn ToSql).collect()
    }

    #[test]
    fn keywords_today_overdue() {
        assert!(matches!(ast("today"), Expr::Today));
        assert!(matches!(ast("overdue"), Expr::Overdue));
        assert!(matches!(ast("no date"), Expr::NoDate));
        assert!(matches!(ast("recurring"), Expr::Recurring));
    }

    #[test]
    fn priority_p1_through_p5() {
        // Native pTask scale: p1=low(1) .. p5=critical(5)
        for (input, expected) in &[("p1", 1), ("p2", 2), ("p3", 3), ("p4", 4), ("p5", 5)] {
            match ast(input) {
                Expr::Priority(n) => assert_eq!(n, *expected),
                other => panic!("input {input} parsed as {:?}", other),
            }
        }
    }

    #[test]
    fn label_and_project_tokens() {
        assert!(matches!(ast("@home"), Expr::Label(ref s) if s == "home"));
        assert!(matches!(ast("#fleet"), Expr::Project(ref s) if s == "fleet"));
    }

    #[test]
    fn label_and_project_names_run_to_the_next_operator() {
        // PARSE-3: names stopped at the first char outside [alnum _ -], so
        // the system's own `domain:<x>` labels (pt add --label domain:mgmt,
        // MCP labels_add) were unfilterable: "unexpected trailing input".
        for name in ["domain:mgmt", "v1.2", "team/ops", "a_b-c"] {
            assert_eq!(ast(&format!("@{name}")), Expr::Label(name.into()));
            assert_eq!(ast(&format!("#{name}")), Expr::Project(name.into()));
        }
        assert_eq!(
            ast("(@domain:mgmt&#infra/core)|!@x"),
            Expr::Or(
                Box::new(Expr::And(
                    Box::new(Expr::Label("domain:mgmt".into())),
                    Box::new(Expr::Project("infra/core".into())),
                )),
                Box::new(Expr::Not(Box::new(Expr::Label("x".into())))),
            )
        );
        assert!(parse("@").is_err());
        assert!(parse("# & p1").is_err());
    }

    #[test]
    fn colon_label_filter_lists_the_labelled_task() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::Db::open(dir.path().join("labels.db")).unwrap();
        let ext = crate::Extensions {
            labels: vec!["domain:mgmt".into()],
            ..Default::default()
        };
        let task = crate::tasks::create_with_extensions(
            &db,
            crate::NewTask::minimal("quarterly board pack"),
            ext,
            &crate::event_log::EventCtx::test(),
        )
        .unwrap();
        crate::tasks::create(
            &db,
            crate::NewTask::minimal("unlabelled"),
            &crate::event_log::EventCtx::test(),
        )
        .unwrap();
        let expr = parse("@domain:mgmt").unwrap();
        let rows = crate::tasks::list_with_filter(&db, Some(&expr), None, None, 10).unwrap();
        assert_eq!(
            rows.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            [task.id.as_str()]
        );
    }

    #[test]
    fn kind_token_parses_and_rejects_junk() {
        assert!(matches!(ast("kind: scout"), Expr::Kind(ref k) if k == "scout"));
        assert!(matches!(ast("kind: implement"), Expr::Kind(ref k) if k == "ship"));
        assert!(parse("kind: sideways").is_err());
    }

    #[test]
    fn precedence_and_binds_tighter_than_or() {
        // a & b | c  -->  (a & b) | c
        let e = ast("today & p1 | overdue");
        match e {
            Expr::Or(l, r) => {
                assert!(matches!(*l, Expr::And(_, _)));
                assert!(matches!(*r, Expr::Overdue));
            }
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn parens_force_or_first() {
        let e = ast("(today | overdue) & p1");
        match e {
            Expr::And(l, r) => {
                assert!(matches!(*l, Expr::Or(_, _)));
                assert!(matches!(*r, Expr::Priority(1)));
            }
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn not_negates() {
        let e = ast("!recurring");
        assert!(matches!(e, Expr::Not(inner) if matches!(*inner, Expr::Recurring)));
    }

    #[test]
    fn due_before_with_natural_phrase() {
        let e = ast("due before: tomorrow");
        match e {
            Expr::DueBefore(d) => {
                // dates::parse uses live "now", so just sanity-check shape.
                assert_eq!(d.len(), 10);
                assert!(d.starts_with("20"));
            }
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn search_consumes_keyword() {
        let e = ast("search: ceph");
        assert!(matches!(e, Expr::Search(ref s) if s == "ceph"));
    }

    /// A migrated store whose rows leave every column an atom reads NULL
    /// somewhere: no pt_id (so no pt_extensions row), no project, no
    /// deadline, an unreadable deadline, NULL description and legacy status.
    fn nullable_fixture() -> (tempfile::TempDir, crate::Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::Db::open(dir.path().join("filter.db")).unwrap();
        db.with_conn(|c| {
            c.execute_batch(
                "INSERT INTO tasks (id, title, description, priority, status, created_at,
                                    updated_at, deadline, pt_id, project, kind, due_at) VALUES
                   ('full', 'ceph mon', 'ceph quorum', 5, 'pending', 'x', 'x', '2026-05-13',
                    'PT-1', 'fleet', 'scout', '2026-05-13T10:00:00+01:00'),
                   ('bare', 'bare', NULL, 2, 'pending', 'x', 'x', NULL, NULL, NULL, 'ship', NULL),
                   ('nulls', 'nulls', NULL, 3, NULL, 'x', 'x', '2020-01-01T00:00:00Z', 'PT-3',
                    NULL, 'ship', NULL),
                   ('junk', 'junk', '', 1, 'pending', 'x', 'x', 'in two weeks', 'PT-4', NULL,
                    'ship', NULL);
                 INSERT INTO task_labels (task_uuid, label) VALUES ('full', 'ops');
                 INSERT INTO pt_recurrence (task_uuid, rrule, mode, original_input,
                                            next_occurrence)
                   VALUES ('full', 'FREQ=DAILY', 'fixed', 'every day', '2026-05-13');",
            )?;
            Ok(())
        })
        .unwrap();
        (dir, db)
    }

    #[test]
    fn every_atom_or_its_negation_covers_every_row() {
        // PARSE-2: `!A` compiled to `NOT (A)`, and A is NULL -- not false -- on
        // a row missing the column it reads, so `!#fleet` dropped every
        // project-less task and `#fleet | !#fleet` returned part of the table.
        let (_dir, db) = nullable_fixture();
        let count = |f: &str| {
            let expr = parse(f).unwrap();
            crate::tasks::list_with_filter(&db, Some(&expr), None, None, 100)
                .unwrap()
                .len()
        };
        let all = crate::tasks::list_with_filter(&db, None, None, None, 100)
            .unwrap()
            .len();
        assert_eq!(all, 4);
        for atom in [
            "today",
            "tomorrow",
            "yesterday",
            "overdue",
            "no date",
            "recurring",
            "p1",
            "p5",
            "@ops",
            "#fleet",
            "due: 2026-05-13",
            "due before: 2026-05-13",
            "due after: 2026-05-13",
            "search: ceph",
            "kind: scout",
        ] {
            assert_eq!(count(&format!("{atom} | !{atom}")), all, "{atom} | !{atom}");
            assert_eq!(count(&format!("{atom} & !{atom}")), 0, "{atom} & !{atom}");
        }
    }

    #[test]
    fn empty_search_is_an_error_not_a_match_all() {
        // CLI-15: `pt bulk "search: $TERM" --done` with an empty TERM matched
        // `%%` -- every task -- while empty `@` / `#` were already rejected.
        for f in [
            "search:",
            "search:   ",
            "search: & p1",
            "p1 & search:",
            "(search: )",
        ] {
            assert!(parse(f).is_err(), "{f:?} must not parse");
        }
    }

    #[test]
    fn search_stops_at_operator() {
        let e = ast("search: ceph & @ops");
        match e {
            Expr::And(l, r) => {
                assert!(matches!(*l, Expr::Search(ref s) if s == "ceph"));
                assert!(matches!(*r, Expr::Label(ref s) if s == "ops"));
            }
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn trailing_garbage_is_error() {
        assert!(parse("today garbage").is_err());
    }

    /// Run `f` on a thread with a tokio-worker-sized (2 MiB) stack, where a
    /// recursion bomb aborts the whole process instead of failing a test.
    fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(f)
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn deeply_nested_filter_is_an_error_not_a_stack_overflow() {
        // SRV-1: one GET /list with 1000 nested parens aborted `pt serve`.
        on_small_stack(|| {
            let bomb = format!("{}today{}", "(".repeat(200_000), ")".repeat(200_000));
            assert!(parse(&bomb).is_err());
            // Short enough to pass the length cap, still too deep.
            let deep = format!("{}today{}", "(".repeat(40), ")".repeat(40));
            assert!(parse(&deep).is_err());
            let negated = format!("{}today{}", "!(".repeat(40), ")".repeat(40));
            assert!(parse(&negated).is_err());
        });
    }

    #[test]
    fn long_flat_chain_is_an_error_not_a_stack_overflow() {
        // A flat `p5&p5&…` chain parses iteratively but compiles and drops
        // recursively, one frame per term.
        on_small_stack(|| {
            assert!(parse(&vec!["p5"; 100_000].join("&")).is_err());
            assert!(parse(&vec!["p5"; MAX_FILTER_TERMS + 1].join("|")).is_err());
        });
    }

    #[test]
    fn filters_at_the_limits_still_parse_compile_and_run() {
        on_small_stack(|| {
            let conn = rusqlite::Connection::open_in_memory().unwrap();
            conn.execute_batch(
                "CREATE TABLE tasks (id TEXT, title TEXT, description TEXT, priority INTEGER,
                                     deadline TEXT, due_at TEXT, status TEXT, kind TEXT);
                 CREATE TABLE pt_extensions (task_uuid TEXT, labels TEXT, project TEXT);
                 CREATE TABLE pt_recurrence (task_uuid TEXT);
                 INSERT INTO tasks (id, title, priority) VALUES ('a', 'x', 5);",
            )
            .unwrap();
            let terms = vec!["p5"; MAX_FILTER_TERMS].join("&");
            let nested = format!(
                "{}today{}",
                "(".repeat(MAX_FILTER_NESTING),
                ")".repeat(MAX_FILTER_NESTING)
            );
            for f in [terms, nested] {
                let sql = to_sql(&parse(&f).unwrap(), &anchor()).unwrap();
                let query = format!(
                    "SELECT COUNT(*) FROM tasks t LEFT JOIN pt_extensions x ON x.task_uuid = t.id
                     WHERE {}",
                    sql.where_clause
                );
                let params = bind_refs(&sql.params);
                conn.query_row(&query, params.as_slice(), |r| r.get::<_, i64>(0))
                    .unwrap();
            }
        });
    }

    #[test]
    fn compile_today_emits_substring_match() {
        let sql = to_sql(&ast("today"), &anchor()).unwrap();
        assert_eq!(sql.where_clause, "substr(t.deadline,1,10) = ?1");
        assert_eq!(sql.params.len(), 1);
    }

    #[test]
    fn compile_complex_combines_parens() {
        let sql = to_sql(&ast("(today | overdue) & p1"), &anchor()).unwrap();
        assert!(sql.where_clause.contains(" OR "));
        assert!(sql.where_clause.contains(" AND "));
        assert!(sql.where_clause.contains("priority"));
    }

    #[test]
    fn compile_label_escapes_like_wildcards() {
        let sql = to_sql(&ast("@a_b"), &anchor()).unwrap();
        assert!(sql.where_clause.contains("labels LIKE"));
        assert!(sql.where_clause.contains("ESCAPE"));
        assert!(matches!(
            &sql.params[0],
            rusqlite::types::Value::Text(s) if s == "%\"a\\_b\"%"
        ));
    }

    #[test]
    fn label_like_treats_underscore_literally() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE pt_extensions (task_uuid TEXT, labels TEXT NOT NULL DEFAULT '[]');
             INSERT INTO pt_extensions (task_uuid, labels) VALUES
               ('literal', '[\"a_b\"]'),
               ('wildcard-lookalike', '[\"axb\"]');",
        )
        .unwrap();
        let sql = to_sql(&ast("@a_b"), &anchor()).unwrap();
        let query = format!(
            "SELECT task_uuid FROM pt_extensions x WHERE {} ORDER BY task_uuid",
            sql.where_clause
        );
        let params = bind_refs(&sql.params);
        let rows: Vec<String> = conn
            .prepare(&query)
            .unwrap()
            .query_map(params.as_slice(), |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(rows, vec!["literal"]);
    }

    #[test]
    fn compile_search_lowercases_and_escapes() {
        let sql = to_sql(&ast("search: CEPH"), &anchor()).unwrap();
        assert!(sql.where_clause.contains("lower(t.title)"));
        assert!(matches!(
            &sql.params[0],
            rusqlite::types::Value::Text(s) if s == "%ceph%"
        ));
    }

    #[test]
    fn compile_no_date() {
        let sql = to_sql(&ast("no date"), &anchor()).unwrap();
        assert_eq!(sql.where_clause, "t.deadline IS NULL");
    }

    #[test]
    fn compile_recurring_subquery() {
        let sql = to_sql(&ast("recurring"), &anchor()).unwrap();
        assert!(sql.where_clause.contains("pt_recurrence"));
    }

    #[test]
    fn overdue_does_not_treat_today_date_only_as_overdue() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE tasks (title TEXT, deadline TEXT, status TEXT);
             CREATE TABLE pt_extensions (task_uuid TEXT, labels TEXT);
             INSERT INTO tasks (title, deadline, status) VALUES
               ('yesterday date-only', '2026-05-12', 'pending'),
               ('today date-only', '2026-05-13', 'pending'),
               ('earlier today time', '2026-05-13T10:00:00+01:00', 'pending'),
               ('later today time', '2026-05-13T18:00:00+01:00', 'pending'),
               ('past mixed offset', '2026-05-13T10:30:00Z', 'pending'),
               ('future mixed offset', '2026-05-13T11:30:00Z', 'pending'),
               ('done yesterday', '2026-05-12', 'done'),
               ('dismissed yesterday', '2026-05-12', 'dismissed');",
        )
        .unwrap();
        let sql = to_sql(&ast("overdue"), &anchor()).unwrap();
        let query = format!(
            "SELECT title FROM tasks t LEFT JOIN pt_extensions x ON 1=0 WHERE {} ORDER BY title",
            sql.where_clause
        );
        let params = bind_refs(&sql.params);
        let rows: Vec<String> = conn
            .prepare(&query)
            .unwrap()
            .query_map(params.as_slice(), |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                "earlier today time",
                "past mixed offset",
                "yesterday date-only"
            ]
        );
    }

    #[test]
    fn overdue_surfaces_unparseable_deadline() {
        // Regression: `julianday()` returns NULL on anything it can't read, so
        // a free-text deadline fell out of `overdue` entirely and the task was
        // invisible. Six such rows exist in the live store ("in two weeks",
        // "90 days (for evaluation)"). A deadline we can't read must surface.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE tasks (title TEXT, deadline TEXT, status TEXT);
             CREATE TABLE pt_extensions (task_uuid TEXT, labels TEXT);
             INSERT INTO tasks (title, deadline, status) VALUES
               ('free text long', 'in two weeks', 'pending'),
               ('free text ten', 'tomorrow!!', 'pending'),
               ('done free text', '90 days (for evaluation)', 'done'),
               ('future date-only', '2026-05-20', 'pending');",
        )
        .unwrap();
        let sql = to_sql(&ast("overdue"), &anchor()).unwrap();
        let query = format!(
            "SELECT title FROM tasks t LEFT JOIN pt_extensions x ON 1=0 WHERE {} ORDER BY title",
            sql.where_clause
        );
        let params = bind_refs(&sql.params);
        let rows: Vec<String> = conn
            .prepare(&query)
            .unwrap()
            .query_map(params.as_slice(), |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec!["free text long", "free text ten"],
            "unparseable deadlines must surface as overdue, and only those"
        );
    }

    #[test]
    fn overdue_datetime_comparison_handles_offsets_across_calendar_dates() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE tasks (title TEXT, deadline TEXT, status TEXT);
             CREATE TABLE pt_extensions (task_uuid TEXT, labels TEXT);
             INSERT INTO tasks (title, deadline, status) VALUES
               ('future with previous UTC date', '2026-05-12T23:30:00Z', 'pending'),
               ('past with next local date', '2026-05-14T00:00:00+02:00', 'pending');",
        )
        .unwrap();

        let tz = jiff::tz::TimeZone::get(dates::OPERATOR_TZ).unwrap();
        let just_after_midnight = jiff::civil::date(2026, 5, 13)
            .at(0, 15, 0, 0)
            .to_zoned(tz.clone())
            .unwrap();
        let sql = to_sql(&ast("overdue"), &just_after_midnight).unwrap();
        let query = format!(
            "SELECT title FROM tasks t LEFT JOIN pt_extensions x ON 1=0 WHERE {} ORDER BY title",
            sql.where_clause
        );
        let params = bind_refs(&sql.params);
        let rows: Vec<String> = conn
            .prepare(&query)
            .unwrap()
            .query_map(params.as_slice(), |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(rows.is_empty(), "future instants were overdue: {rows:?}");

        let just_before_midnight = jiff::civil::date(2026, 5, 13)
            .at(23, 45, 0, 0)
            .to_zoned(tz)
            .unwrap();
        let sql = to_sql(&ast("overdue"), &just_before_midnight).unwrap();
        let query = format!(
            "SELECT title FROM tasks t LEFT JOIN pt_extensions x ON 1=0 WHERE {} ORDER BY title",
            sql.where_clause
        );
        let params = bind_refs(&sql.params);
        let rows: Vec<String> = conn
            .prepare(&query)
            .unwrap()
            .query_map(params.as_slice(), |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(
            rows.contains(&"past with next local date".to_string()),
            "past cross-date instant was not overdue: {rows:?}"
        );
    }
}
