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
        self.serve_with(&[])
    }

    /// [`Pt::serve`] with extra environment for the server process.
    ///
    /// The port is picked by binding `:0` and releasing it, so another
    /// process on the host can take it before the child binds. A TCP
    /// accept on the port is therefore not readiness: the server is up
    /// only once the socket listening there belongs to this child. A child
    /// that exits first (lost the port) is retried on a fresh one; any
    /// other failure panics with what the server printed.
    pub fn serve_with(&self, envs: &[(&str, &str)]) -> Server {
        let log = self.dir.path().join("serve.log");
        for _attempt in 0..5 {
            let port = std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            let mut cmd = self.command("test", &["serve", "--bind", &format!("127.0.0.1:{port}")]);
            for (k, v) in envs {
                cmd.env(k, v);
            }
            let mut child = cmd
                .stdout(Stdio::null())
                .stderr(std::fs::File::create(&log).unwrap())
                .spawn()
                .unwrap();
            for _ in 0..400 {
                if let Some(status) = child.try_wait().unwrap() {
                    let out = std::fs::read_to_string(&log).unwrap_or_default();
                    if out.contains("in use") {
                        break; // another process took the port: try a new one
                    }
                    panic!("pt serve exited ({status}) before listening:\n{out}");
                }
                if listens_on(child.id(), port) {
                    return Server {
                        child,
                        url: format!("http://127.0.0.1:{port}"),
                        log,
                    };
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        panic!(
            "pt serve did not come up:\n{}",
            std::fs::read_to_string(&log).unwrap_or_default()
        );
    }
}

/// Whether process `pid` owns the loopback socket listening on `port`:
/// the listener's inode in /proc/net/tcp is one of the process's fds.
fn listens_on(pid: u32, port: u16) -> bool {
    let local = format!("0100007F:{port:04X}");
    let Ok(tcp) = std::fs::read_to_string("/proc/net/tcp") else {
        return false;
    };
    let Some(inode) = tcp.lines().skip(1).find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        // f[1] local address, f[3] state (0A = LISTEN), f[9] inode
        (f.len() > 9 && f[1] == local && f[3] == "0A").then(|| f[9].to_string())
    }) else {
        return false;
    };
    let want = format!("socket:[{inode}]");
    std::fs::read_dir(format!("/proc/{pid}/fd"))
        .map(|fds| {
            fds.flatten().any(|fd| {
                std::fs::read_link(fd.path())
                    .map(|t| t.to_string_lossy() == want)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

pub struct Server {
    child: std::process::Child,
    pub url: String,
    log: std::path::PathBuf,
}

impl Server {
    /// What the server has written to stderr so far.
    pub fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Stop the server and return everything it wrote to stderr.
    pub fn stop(mut self) -> String {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.log()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // A test failing with a server up shows what the server said.
        if std::thread::panicking() {
            eprintln!("pt serve stderr:\n{}", self.log());
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
