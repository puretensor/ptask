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
            // CI's setup-python toolcache python3 loads libpython only
            // through LD_LIBRARY_PATH; the fake gcalendar runs under it.
            .env(
                "LD_LIBRARY_PATH",
                std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default(),
            )
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

    /// Whether `PT-N` (any status) still exists in this database.
    pub fn exists(&self, pt_id: &str) -> bool {
        self.run(&["show", pt_id]).status.success()
    }

    /// A throwaway `pt serve` on a free loopback port, sharing this
    /// database. Loopback with no token configured runs unauthenticated.
    pub fn serve(&self) -> Server {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = self
            .command("test", &["serve", "--bind", &format!("127.0.0.1:{port}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let server = Server {
            child,
            url: format!("http://127.0.0.1:{port}"),
        };
        for _ in 0..200 {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return server;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("pt serve did not come up on {}", server.url);
    }
}

pub struct Server {
    child: std::process::Child,
    pub url: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
