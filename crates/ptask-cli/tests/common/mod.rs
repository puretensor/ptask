//! Shared harness for the black-box `pt` tests: every run gets a throwaway
//! HOME and PTASK_DB and inherits nothing else but PATH.
#![allow(dead_code)]

use std::process::{Command, Output, Stdio};

pub struct Pt {
    pub dir: tempfile::TempDir,
}

impl Pt {
    pub fn new() -> Self {
        Pt {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    pub fn command(&self, actor: &str, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_pt"));
        cmd.args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.dir.path())
            .env("PTASK_DB", self.dir.path().join("tasks.db"))
            .env("PTASK_ACTOR", actor)
            .env("COLUMNS", "120")
            .stdin(Stdio::null());
        cmd
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.run_as("test", args)
    }

    pub fn run_as(&self, actor: &str, args: &[&str]) -> Output {
        self.command(actor, args).output().unwrap()
    }

    pub fn ok(&self, args: &[&str]) -> String {
        self.ok_as("test", args)
    }

    pub fn ok_as(&self, actor: &str, args: &[&str]) -> String {
        let out = self.run_as(actor, args);
        assert!(
            out.status.success(),
            "pt {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// `pt --json <args>`, parsed.
    pub fn json(&self, args: &[&str]) -> serde_json::Value {
        let mut full = vec!["--json"];
        full.extend_from_slice(args);
        let out = self.ok(&full);
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("pt {full:?}: {e}\n{out}"))
    }
}
