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

/// Compiled SQL fragment + bound parameter values (positional).
pub struct Sql {
    pub where_clause: String,
    pub params: Vec<rusqlite::types::Value>,
}

/// Public entry point: parse a filter DSL string into an AST.
pub fn parse(input: &str) -> Result<Expr> {
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
        Expr::Not(inner) => format!("(NOT ({}))", compile(inner, now, params)?),

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
                "(t.deadline IS NOT NULL AND t.status != 'done' AND \
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

fn escape_like(input: &str) -> String {
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
}

impl<'a> ParseCtx<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
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
            let inner = self.parse_atom()?;
            Ok(Expr::Not(Box::new(inner)))
        } else {
            self.parse_atom()
        }
    }

    fn parse_atom(&mut self) -> Result<Expr> {
        self.skip_ws();
        if self.peek() == Some('(') {
            self.pos += 1;
            let inner = self.parse_or()?;
            self.skip_ws();
            if self.peek() != Some(')') {
                return Err(Error::Other(format!(
                    "filter: expected ')' at byte {}",
                    self.pos
                )));
            }
            self.pos += 1;
            Ok(inner)
        } else {
            self.parse_term()
        }
    }

    fn parse_term(&mut self) -> Result<Expr> {
        self.skip_ws();
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

    /// Consume an identifier (letters/digits/`_`/`-`). Trims trailing whitespace.
    fn consume_ident(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                self.pos += c.len_utf8();
            } else {
                break;
            }
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
               ('done yesterday', '2026-05-12', 'done');",
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
