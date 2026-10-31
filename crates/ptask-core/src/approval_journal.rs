//! Off-database, hash-chained approval journal.
//!
//! The approvals table alone cannot prove that a row was approved by the
//! operator: anything able to write the SQLite file can insert an
//! `approved` row. The journal is a second record, an append-only JSONL
//! file with a keyed hash chain (HMAC-SHA256 over the previous MAC and the
//! record fields), kept where the seats cannot write it and shipped
//! off-host. `consume` in `enforce` mode refuses a row unless the journal
//! holds a genuine, unconsumed `approve` record that matches the row's
//! digest (the action binding), kind, approver and expiry.
//!
//! Modes (`PTASK_APPROVAL_JOURNAL`):
//! * `off` (default; also unset, `0`, `false`, `no`): nothing is read or written; behaviour is unchanged.
//! * `shadow`: records are appended and a failing consume check is logged
//!   but allowed, so the journal can soak before it is authoritative.
//! * `enforce` (and any unrecognised value, so a typo fails closed): a consume without a matching journal record is refused
//!   (exit code 7) and a journal that cannot be written fails the call.
//!
//! The key file (`PTASK_APPROVAL_JOURNAL_KEY_FILE`, at least 16 bytes) and
//! journal path (`PTASK_APPROVAL_JOURNAL_PATH`) are never logged.

use crate::approvals::ApprovalError;
use crate::error::{Error, Result};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

type HmacSha256 = Hmac<Sha256>;

const SEP: char = '\u{1f}';
const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Off,
    Shadow,
    Enforce,
}

#[derive(Debug, Clone)]
pub struct Journal {
    pub mode: Mode,
    path: PathBuf,
    key_file: PathBuf,
}

/// One journal line. `ev` is request, approve, reject, withdraw, consume,
/// or consume_void (a consume whose database transaction did not commit).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Record {
    pub n: u64,
    pub ts: String,
    pub ev: String,
    pub uuid: String,
    pub digest: String,
    pub kind: String,
    pub expires_at: Option<String>,
    pub actor: String,
    pub prev: String,
    pub mac: String,
}

/// What `consume` needs to match against the journal.
#[derive(Debug, Clone)]
pub struct Claim<'a> {
    pub uuid: &'a str,
    pub digest: &'a str,
    pub kind: &'a str,
    pub expires_at: Option<&'a str>,
    pub decided_by: Option<&'a str>,
}

impl Journal {
    /// Read the configuration from the environment. `None` when off.
    pub fn from_env() -> Option<Journal> {
        let mode = match std::env::var("PTASK_APPROVAL_JOURNAL")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "" | "off" | "0" | "false" | "no" => return None,
            "shadow" => Mode::Shadow,
            "enforce" => Mode::Enforce,
            // Anything else, including a typo of "enforce", fails closed.
            other => {
                tracing::warn!(
                    "PTASK_APPROVAL_JOURNAL={other:?} is not recognised; treating it as enforce"
                );
                Mode::Enforce
            }
        };
        let path = std::env::var_os("PTASK_APPROVAL_JOURNAL_PATH").map(PathBuf::from);
        let key_file = std::env::var_os("PTASK_APPROVAL_JOURNAL_KEY_FILE").map(PathBuf::from);
        match (path, key_file) {
            (Some(path), Some(key_file)) => Some(Journal {
                mode,
                path,
                key_file,
            }),
            // Misconfigured: enforce must not silently degrade to off.
            _ if mode == Mode::Enforce => Some(Journal {
                mode,
                path: PathBuf::new(),
                key_file: PathBuf::new(),
            }),
            _ => {
                tracing::warn!("approval journal shadow mode needs path and key file; disabled");
                None
            }
        }
    }

    pub fn new(mode: Mode, path: impl Into<PathBuf>, key_file: impl Into<PathBuf>) -> Journal {
        Journal {
            mode,
            path: path.into(),
            key_file: key_file.into(),
        }
    }

    fn key(&self) -> Result<Vec<u8>> {
        let bytes = std::fs::read(&self.key_file).map_err(|e| unverified(format!("key: {e}")))?;
        let trimmed: Vec<u8> = bytes
            .iter()
            .copied()
            .filter(|b| !b.is_ascii_whitespace())
            .collect();
        if trimmed.len() < 16 {
            return Err(unverified("key file shorter than 16 bytes".into()));
        }
        Ok(trimmed)
    }

    fn mac(key: &[u8], r: &Record) -> String {
        let mut m = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
        let body = [
            r.n.to_string(),
            r.ts.clone(),
            r.ev.clone(),
            r.uuid.clone(),
            r.digest.clone(),
            r.kind.clone(),
            r.expires_at.clone().unwrap_or_else(|| "-".into()),
            r.actor.clone(),
            r.prev.clone(),
        ]
        .join(&SEP.to_string());
        m.update(body.as_bytes());
        hex::encode(m.finalize().into_bytes())
    }

    /// Parse and verify the whole chain. Err on any broken link, bad MAC,
    /// gap or unparsable line.
    fn read_chain(&self, key: &[u8], text: &str) -> Result<Vec<Record>> {
        let mut out: Vec<Record> = Vec::new();
        let mut prev = GENESIS.to_string();
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let r: Record = serde_json::from_str(line)
                .map_err(|_| unverified(format!("journal line {} unparsable", i + 1)))?;
            if r.n != out.len() as u64 + 1 || r.prev != prev || Self::mac(key, &r) != r.mac {
                return Err(unverified(format!(
                    "journal chain broken at record {}",
                    r.n
                )));
            }
            prev = r.mac.clone();
            out.push(r);
        }
        Ok(out)
    }

    /// Verify the chain and return the number of records.
    pub fn verify(&self) -> Result<usize> {
        let key = self.key()?;
        let text = std::fs::read_to_string(&self.path)
            .map_err(|e| unverified(format!("journal read: {e}")))?;
        Ok(self.read_chain(&key, &text)?.len())
    }

    /// Run `f` with the journal exclusively locked and the chain verified.
    fn locked<T>(
        &self,
        f: impl FnOnce(&[u8], &mut std::fs::File, Vec<Record>) -> Result<T>,
    ) -> Result<T> {
        let key = self.key()?;
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&self.path)
            .map_err(|e| unverified(format!("journal open: {e}")))?;
        file.lock()
            .map_err(|e| unverified(format!("journal lock: {e}")))?;
        let mut text = String::new();
        file.read_to_string(&mut text)
            .map_err(|e| unverified(format!("journal read: {e}")))?;
        let chain = self.read_chain(&key, &text)?;
        f(&key, &mut file, chain)
    }

    fn append_in(
        key: &[u8],
        file: &mut std::fs::File,
        chain: &[Record],
        mut r: Record,
    ) -> Result<()> {
        r.n = chain.len() as u64 + 1;
        r.prev = chain
            .last()
            .map(|c| c.mac.clone())
            .unwrap_or_else(|| GENESIS.to_string());
        r.mac = Self::mac(key, &r);
        let mut line = serde_json::to_string(&r).map_err(|e| unverified(e.to_string()))?;
        line.push('\n');
        file.write_all(line.as_bytes())
            .and_then(|_| file.sync_data())
            .map_err(|e| unverified(format!("journal write: {e}")))
    }

    fn record(ev: &str, claim: &Claim, actor: &str, ts: &str) -> Record {
        Record {
            n: 0,
            ts: ts.to_string(),
            ev: ev.to_string(),
            uuid: claim.uuid.to_string(),
            digest: claim.digest.to_string(),
            kind: claim.kind.to_string(),
            expires_at: claim.expires_at.map(str::to_string),
            actor: actor.to_string(),
            prev: String::new(),
            mac: String::new(),
        }
    }

    /// Append an event record. In shadow mode failures are logged only.
    pub fn append(&self, ev: &str, claim: &Claim, actor: &str, ts: &str) -> Result<()> {
        let res = self.locked(|key, file, chain| {
            Self::append_in(key, file, &chain, Self::record(ev, claim, actor, ts))
        });
        self.soften(res)
    }

    /// Check that the journal authorises consuming `claim`, then append the
    /// consume record under the same lock so a replay is refused across
    /// processes. Shadow mode logs a refusal and still records the consume
    /// when the chain is intact.
    ///
    /// `Ok(true)` means a consume line was appended. `Ok(false)` is shadow
    /// mode softening a failure that wrote nothing. Callers that void a
    /// consume when their own database transaction rolls back must void
    /// only after `Ok(true)`: a refusal leaves any earlier committed
    /// consume in place. A shadow refusal appends a consume only when none
    /// is open; an already-open consume returns `Ok(false)` and writes
    /// nothing, so a rolled-back retry cannot void it.
    pub fn check_and_record_consume(&self, claim: &Claim, actor: &str, ts: &str) -> Result<bool> {
        let res = self.locked(|key, file, chain| {
            let verdict = authorise(&chain, claim);
            match verdict {
                Ok(()) => {
                    Self::append_in(key, file, &chain, Self::record("consume", claim, actor, ts))?;
                    Ok(true)
                }
                Err(e) if self.mode == Mode::Shadow => {
                    tracing::warn!(
                        approval = claim.uuid,
                        "approval journal (shadow) would refuse consume: {e}"
                    );
                    // An open consume already committed. Another line and
                    // Ok(true) would arm ConsumeVoidGuard; a rolled-back
                    // retry would then void it and reopen the approval.
                    if consume_is_open(&chain, claim.uuid) {
                        Ok(false)
                    } else {
                        Self::append_in(
                            key,
                            file,
                            &chain,
                            Self::record("consume", claim, actor, ts),
                        )?;
                        Ok(true)
                    }
                }
                Err(e) => Err(e),
            }
        });
        match res {
            Ok(appended) => Ok(appended),
            Err(e) if self.mode == Mode::Shadow => {
                tracing::warn!("approval journal (shadow) error ignored: {e}");
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    /// Whether the journal currently authorises `claim` (an approve is
    /// present, unconsumed, and matches). Does not append. Shadow mode
    /// logs a refusal and returns `Ok`. Used by `payload` and `verify`,
    /// which release or attest before `consume` latches.
    pub fn authorises(&self, claim: &Claim) -> Result<()> {
        let res = self.locked(|_key, _file, chain| authorise(&chain, claim));
        match res {
            Err(e) if self.mode == Mode::Shadow => {
                tracing::warn!(
                    approval = claim.uuid,
                    "approval journal (shadow) would refuse release: {e}"
                );
                Ok(())
            }
            other => other,
        }
    }

    /// Append `consume_void` when `uuid` has an unvoided consume.
    ///
    /// `consume` writes the journal line before the SQLite commit, because
    /// the journal is the authority and must not lag a committed latch.
    /// If that transaction then rolls back, the consume line would brick
    /// the still-approved row forever (`authorise` treats consume as
    /// sticky). Only a holder of the journal key can append the void, and
    /// the void does not look at the database: clearing `consumed_at` in
    /// SQLite must not undo a consume that did commit.
    ///
    /// No-op when this uuid has no open consume (the check failed before
    /// any line was written, or a void is already in place).
    pub fn void_uncommitted_consume(&self, claim: &Claim, actor: &str, ts: &str) -> Result<()> {
        let res = self.locked(|key, file, chain| {
            if !consume_is_open(&chain, claim.uuid) {
                return Ok(());
            }
            Self::append_in(
                key,
                file,
                &chain,
                Self::record("consume_void", claim, actor, ts),
            )
        });
        self.soften(res)
    }

    fn soften(&self, res: Result<()>) -> Result<()> {
        match res {
            Err(e) if self.mode == Mode::Shadow => {
                tracing::warn!("approval journal (shadow) error ignored: {e}");
                Ok(())
            }
            other => other,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn unverified(msg: String) -> Error {
    Error::Approval(ApprovalError::Unverified(msg))
}

/// The journal must hold an `approve` record for this row that matches the
/// action digest, kind, approver and expiry, and no earlier consume.
fn authorise(chain: &[Record], claim: &Claim) -> Result<()> {
    let mut approve: HashMap<&str, &Record> = HashMap::new();
    let mut consumed = false;
    for r in chain.iter().filter(|r| r.uuid == claim.uuid) {
        match r.ev.as_str() {
            "approve" => {
                approve.insert("a", r);
            }
            "consume" => consumed = true,
            // A consume whose database transaction rolled back. Only the
            // key holder can write this; it does not clear a consume that
            // has no matching void.
            "consume_void" => consumed = false,
            "reject" | "withdraw" => {
                approve.remove("a");
            }
            _ => {}
        }
    }
    let Some(a) = approve.get("a") else {
        return Err(unverified(
            "no journaled approval for this row (not decided through the operator channel)".into(),
        ));
    };
    if consumed {
        return Err(Error::Approval(ApprovalError::AlreadyConsumed));
    }
    if a.digest != claim.digest || a.kind != claim.kind {
        return Err(unverified(
            "row action binding differs from the journaled approval".into(),
        ));
    }
    if a.expires_at.as_deref() != claim.expires_at {
        return Err(unverified(
            "row expiry differs from the journaled approval".into(),
        ));
    }
    if claim.decided_by.is_some_and(|d| d != a.actor) {
        return Err(unverified(
            "row approver differs from the journaled approver".into(),
        ));
    }
    Ok(())
}

/// True when the latest consume for `uuid` has not been voided.
fn consume_is_open(chain: &[Record], uuid: &str) -> bool {
    let mut consumed = false;
    for r in chain.iter().filter(|r| r.uuid == uuid) {
        match r.ev.as_str() {
            "consume" => consumed = true,
            "consume_void" => consumed = false,
            _ => {}
        }
    }
    consumed
}
