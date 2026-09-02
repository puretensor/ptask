//! Named, scoped API tokens (`pt_api_tokens`, V009).
//!
//! Replaces the single shared `PTASK_API_TOKEN` as the way machine clients
//! authenticate: each consumer (hal, puresentinel, nexus, dashboard,
//! per-host shell) gets its own token with a scope, so credentials are
//! individually revocable and every server-side mutation is attributable
//! to a client_id. The plain token is shown once at creation; only
//! hex(sha256(token)) is stored. The env token remains a legacy fallback
//! credential (client_id "legacy-env") until rotation completes.

use crate::error::{Error, Result};
use crate::storage::Db;
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};

/// Ordered permission levels; each implies the ones before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    Read,
    Capture,
    Write,
    Admin,
}

impl Scope {
    pub fn parse(s: &str) -> Option<Scope> {
        match s.trim().to_ascii_lowercase().as_str() {
            "read" => Some(Scope::Read),
            "capture" => Some(Scope::Capture),
            "write" => Some(Scope::Write),
            "admin" => Some(Scope::Admin),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Capture => "capture",
            Scope::Write => "write",
            Scope::Admin => "admin",
        }
    }
}

/// A resolved caller identity.
#[derive(Debug, Clone)]
pub struct Identity {
    pub client_id: String,
    pub scope: Scope,
}

#[derive(Debug, Clone)]
pub struct TokenInfo {
    pub client_id: String,
    pub scopes: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

pub fn hash_token(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    hex::encode(h.finalize())
}

/// Mint a new token for `client_id` with `scope`. Returns the PLAIN token —
/// the only time it is ever available; store it with the consumer.
pub fn create(db: &Db, client_id: &str, scope: Scope) -> Result<String> {
    if client_id.trim().is_empty() {
        return Err(Error::Other("client_id must not be empty".into()));
    }
    let token = format!(
        "pt_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let now = crate::dates::format_iso(&crate::dates::now_in_operator_tz()?);
    let conn = db.get()?;
    conn.execute(
        "INSERT INTO pt_api_tokens (token_hash, client_id, scopes, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![hash_token(&token), client_id.trim(), scope.as_str(), now],
    )?;
    Ok(token)
}

/// Resolve a presented plain token to an identity. `None` = unknown or
/// revoked. Touches `last_used_at` on success.
pub fn resolve(db: &Db, presented: &str) -> Result<Option<Identity>> {
    let hash = hash_token(presented);
    let conn = db.get()?;
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT client_id, scopes FROM pt_api_tokens
             WHERE token_hash = ?1 AND revoked_at IS NULL",
            [&hash],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((client_id, scopes)) = row else {
        return Ok(None);
    };
    let scope = Scope::parse(&scopes).unwrap_or(Scope::Read);
    // Bookkeeping only: a write that cannot land (litestream restore, full disk,
    // a long writer holding the lock past busy_timeout) must not turn every
    // authenticated call — including pure reads and the MCP gate — into a 401.
    let now = crate::dates::format_iso(&crate::dates::now_in_operator_tz()?);
    if let Err(e) = conn.execute(
        "UPDATE pt_api_tokens SET last_used_at = ?1 WHERE token_hash = ?2",
        params![now, hash],
    ) {
        tracing::warn!(target: "ptask::tokens", error = %e, "last_used_at touch skipped");
    }
    Ok(Some(Identity { client_id, scope }))
}

pub fn list(db: &Db) -> Result<Vec<TokenInfo>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT client_id, scopes, created_at, last_used_at, revoked_at
         FROM pt_api_tokens ORDER BY created_at",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(TokenInfo {
            client_id: r.get(0)?,
            scopes: r.get(1)?,
            created_at: r.get(2)?,
            last_used_at: r.get(3)?,
            revoked_at: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Revoke every active token for `client_id`. Returns how many were revoked.
pub fn revoke(db: &Db, client_id: &str) -> Result<usize> {
    let now = crate::dates::format_iso(&crate::dates::now_in_operator_tz()?);
    let conn = db.get()?;
    let n = conn.execute(
        "UPDATE pt_api_tokens SET revoked_at = ?1
         WHERE client_id = ?2 AND revoked_at IS NULL",
        params![now, client_id],
    )?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        (dir, Db::open(&path).unwrap())
    }

    #[test]
    fn create_resolve_revoke_roundtrip() {
        let (_dir, db) = fresh_db();
        let plain = create(&db, "hal", Scope::Write).unwrap();
        assert!(plain.starts_with("pt_"));

        let id = resolve(&db, &plain).unwrap().expect("token resolves");
        assert_eq!(id.client_id, "hal");
        assert_eq!(id.scope, Scope::Write);
        assert!(id.scope >= Scope::Capture);

        assert!(resolve(&db, "pt_wrong").unwrap().is_none());

        assert_eq!(revoke(&db, "hal").unwrap(), 1);
        assert!(resolve(&db, &plain).unwrap().is_none(), "revoked = unknown");
        let infos = list(&db).unwrap();
        assert_eq!(infos.len(), 1);
        assert!(infos[0].revoked_at.is_some());
    }

    #[test]
    fn scope_ordering_matches_privilege() {
        assert!(Scope::Admin > Scope::Write);
        assert!(Scope::Write > Scope::Capture);
        assert!(Scope::Capture > Scope::Read);
        assert_eq!(Scope::parse("WRITE"), Some(Scope::Write));
        assert_eq!(Scope::parse("nope"), None);
    }
}
