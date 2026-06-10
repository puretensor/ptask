//! Saved Views — named filter DSL strings stored in `pt_views`.
//!
//! Phase-2 scope: name + filter DSL only. `grouping` and `sort_by` columns
//! exist in the schema and are reserved for the TUI (v0.3.0) and view
//! switching UX. They're written as NULL here and can be populated later.

use crate::dates;
use crate::error::Result;
use crate::storage::Db;
use rusqlite::params;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct View {
    pub id: i64,
    pub name: String,
    pub filter_dsl: String,
    pub grouping: Option<String>,
    pub sort_by: Option<String>,
    pub created_at: String,
}

/// Insert a new saved view, or `UNIQUE` constraint error if `name` exists.
pub fn create(db: &Db, name: &str, filter_dsl: &str) -> Result<View> {
    // Validate the DSL parses before saving — fail fast.
    let _ast = crate::filter::parse(filter_dsl)?;

    let now = dates::format_iso(&dates::now_in_operator_tz()?);
    let conn = db.get()?;
    conn.execute(
        "INSERT INTO pt_views (name, filter_dsl, grouping, sort_by, created_at)
         VALUES (?1, ?2, NULL, NULL, ?3)",
        params![name, filter_dsl, now],
    )?;
    let id: i64 = conn.last_insert_rowid();
    Ok(View {
        id,
        name: name.to_string(),
        filter_dsl: filter_dsl.to_string(),
        grouping: None,
        sort_by: None,
        created_at: now,
    })
}

pub fn get(db: &Db, name: &str) -> Result<View> {
    let conn = db.get()?;
    let row = conn.query_row(
        "SELECT id, name, filter_dsl, grouping, sort_by, created_at
         FROM pt_views WHERE name = ?1",
        [name],
        row_to_view,
    );
    match row {
        Ok(v) => Ok(v),
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            Err(crate::Error::Other(format!("view not found: {}", name)))
        }
        Err(e) => Err(e.into()),
    }
}

pub fn list(db: &Db) -> Result<Vec<View>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id, name, filter_dsl, grouping, sort_by, created_at
         FROM pt_views ORDER BY name ASC",
    )?;
    let rows = stmt.query_map([], row_to_view)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn delete(db: &Db, name: &str) -> Result<bool> {
    let conn = db.get()?;
    let n = conn.execute("DELETE FROM pt_views WHERE name = ?1", [name])?;
    Ok(n > 0)
}

fn row_to_view(r: &rusqlite::Row<'_>) -> rusqlite::Result<View> {
    Ok(View {
        id: r.get(0)?,
        name: r.get(1)?,
        filter_dsl: r.get(2)?,
        grouping: r.get(3)?,
        sort_by: r.get(4)?,
        created_at: r.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        // V008 bootstraps the production-shape legacy schema — no stub.
        (dir, Db::open(&path).unwrap())
    }

    #[test]
    fn create_and_get_roundtrip() {
        let (_dir, db) = fresh_db();
        let v = create(&db, "fleet-today", "today & #fleet").unwrap();
        assert_eq!(v.name, "fleet-today");
        let fetched = get(&db, "fleet-today").unwrap();
        assert_eq!(fetched.filter_dsl, "today & #fleet");
    }

    #[test]
    fn create_rejects_invalid_dsl() {
        let (_dir, db) = fresh_db();
        let err = create(&db, "bad", "this is not valid dsl").unwrap_err();
        // The error came from filter::parse, not the insert.
        assert!(format!("{}", err).contains("filter:"));
    }

    #[test]
    fn list_orders_by_name() {
        let (_dir, db) = fresh_db();
        create(&db, "b-view", "p1").unwrap();
        create(&db, "a-view", "today").unwrap();
        let views = list(&db).unwrap();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].name, "a-view");
        assert_eq!(views[1].name, "b-view");
    }

    #[test]
    fn delete_returns_whether_anything_removed() {
        let (_dir, db) = fresh_db();
        create(&db, "v1", "p1").unwrap();
        assert!(delete(&db, "v1").unwrap());
        assert!(!delete(&db, "v1").unwrap());
    }

    #[test]
    fn duplicate_name_errors() {
        let (_dir, db) = fresh_db();
        create(&db, "dup", "p1").unwrap();
        assert!(create(&db, "dup", "p2").is_err());
    }

    #[test]
    fn get_missing_returns_error() {
        let (_dir, db) = fresh_db();
        assert!(get(&db, "nope").is_err());
    }
}
