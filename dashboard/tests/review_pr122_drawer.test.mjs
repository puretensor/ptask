// Contract test from the pre-merge review of PR #122 (closure evidence and
// task notes), cockpit half: the task drawer must show a task's notes however
// long its history is. On the PR head the drawer builds its Notes trail from
// GET /api/tasks/<id>/events, which the sidecar caps at the newest 60 events,
// so a note older than that silently disappears from the drawer.
//
// Harness: a scratch SQLite file in the sidecar's schema (built by python3,
// as tests/test_server.py does), the shipped sidecar (server.py) serving it on
// a free loopback port, and the shipped cockpit script (www/index.html)
// evaluated with a stub DOM whose fetch() reaches that sidecar. Whatever the
// drawer requests, the real sidecar answers, so a fix may fetch more events or
// add a sidecar read route. The sidecar's reads are SQL: this job has no `pt`
// binary, and PTASK_BIN points at a stub that fails loudly.
//
// Never touches a live database: PTASK_DB and HOME are a throwaway directory.

import test from "node:test";
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync, chmodSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const DASH = join(dirname(fileURLToPath(import.meta.url)), "..");

// ---- scratch database in the sidecar's schema --------------------------------

const BUILD_DB = `
import json, sqlite3, sys
sys.path.insert(0, sys.argv[1])
import server  # the sidecar's own task columns, as tests/test_server.py uses
db, spec = sys.argv[2], json.load(open(sys.argv[3]))
con = sqlite3.connect(db)
con.execute("CREATE TABLE tasks (%s)" % ", ".join(server.TASK_COLS))
con.execute("CREATE TABLE pt_extensions (task_uuid TEXT, pt_id TEXT)")
con.execute("CREATE TABLE task_labels (task_uuid TEXT, label TEXT)")
con.execute("""CREATE TABLE pt_event_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT, uuid TEXT NOT NULL UNIQUE,
    task_uuid TEXT, event_type TEXT NOT NULL, payload TEXT NOT NULL,
    ts TEXT NOT NULL, actor TEXT)""")
for t in spec["tasks"]:
    row = {c: t.get(c) for c in server.TASK_COLS}
    con.execute("INSERT INTO tasks (%s) VALUES (%s)" % (",".join(row), ",".join("?" * len(row))),
                list(row.values()))
    con.execute("INSERT INTO pt_extensions VALUES (?, ?)", (t["id"], t["pt_id"]))
for i, e in enumerate(spec["events"]):
    con.execute("INSERT INTO pt_event_log (uuid, task_uuid, event_type, payload, ts, actor)"
                " VALUES (?, ?, ?, ?, ?, ?)",
                ("fixture-%d" % i, e["task_uuid"], e["event_type"], json.dumps(e["payload"]),
                 e["ts"], e["actor"]))
con.commit()
`;

// A throwaway environment for python3: nothing of the caller's but PATH and,
// when set, LD_LIBRARY_PATH (a toolcache or pyenv python3 built as a shared
// library cannot load libpython without it).
function pyEnv(vars) {
  const env = { PATH: process.env.PATH, ...vars };
  if (process.env.LD_LIBRARY_PATH) env.LD_LIBRARY_PATH = process.env.LD_LIBRARY_PATH;
  return env;
}

function buildDb(dir, spec) {
  const db = join(dir, "tasks.db");
  const specPath = join(dir, "spec.json");
  writeFileSync(specPath, JSON.stringify(spec));
  const r = spawnSync("python3", ["-c", BUILD_DB, DASH, db, specPath], {
    env: pyEnv({ HOME: dir, PTASK_DB: db }),
    encoding: "utf8",
  });
  assert.equal(r.status, 0, `building the scratch db failed: ${r.stderr}`);
  return db;
}

// Journal timestamps one minute apart, in pt's format.
const at = (i) => new Date(Date.UTC(2026, 8, 1, 9, 0) + i * 60_000).toISOString().replace("Z", "+00:00");

function taskRow(id, pt_id, title) {
  return {
    id, pt_id, title, description: "", priority: 2, status: "pending",
    created_at: at(0), updated_at: at(0), deadline: null, source_type: "cli",
    task_type: "task", priority_score: 0.5, score_urgency: 0, score_dependency: 0,
    score_neglect: 0, escalation_level: 0, dismissal_count: 0, last_reminded: null,
    cluster_keywords: null, ai_reasoning: null, project: null,
  };
}

// ---- the shipped sidecar ------------------------------------------------------

function freePort() {
  return new Promise((resolve, reject) => {
    const s = createServer();
    s.once("error", reject);
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address();
      s.close(() => resolve(port));
    });
  });
}

async function startSidecar(dir, db) {
  const stubPt = join(dir, "pt");
  writeFileSync(stubPt, "#!/bin/sh\necho 'no pt binary in this contract test' >&2\nexit 1\n");
  chmodSync(stubPt, 0o755);
  const port = await freePort();
  const child = spawn("python3", [join(DASH, "server.py")], {
    env: pyEnv({
      HOME: dir, PTASK_DB: db, PTASK_BIN: stubPt,
      PTASK_DASH_BIND: `127.0.0.1:${port}`, PTASK_ACTOR: "dashboard",
    }),
    stdio: ["ignore", "ignore", "pipe"],
  });
  let stderr = "";
  child.stderr.on("data", (d) => { stderr += d; });
  const base = `http://127.0.0.1:${port}`;
  for (let i = 0; i < 100; i++) {
    try {
      const r = await fetch(`${base}/healthz`);
      if (r.ok) return { base, stop: () => child.kill() };
    } catch { /* not up yet */ }
    await new Promise((r) => setTimeout(r, 100));
  }
  child.kill();
  throw new Error(`sidecar did not come up: ${stderr}`);
}

// ---- the shipped cockpit script, with a stub DOM ------------------------------

// A value that tolerates any use: property reads, calls, `new`, string use.
function anything() {
  return new Proxy(function () {}, {
    get(_t, k) {
      if (k === Symbol.toPrimitive) return () => "";
      if (k === Symbol.iterator) return function* () {};
      if (k === "then" || typeof k === "symbol") return undefined;
      if (k === "length") return 0;
      return anything();
    },
    apply: () => anything(),
    construct: () => anything(),
    set: () => true,
  });
}

function element(name) {
  const own = {
    name, innerHTML: "", textContent: "", value: "", hidden: false, className: "",
    dataset: {}, style: { setProperty() {} },
    classList: { add() {}, remove() {}, toggle() {}, contains: () => false },
    children: [], offsetHeight: 0,
  };
  return new Proxy(own, {
    get: (t, k) => (k in t ? t[k] : typeof k === "symbol" ? undefined : anything()),
    set: (t, k, v) => { t[k] = v; return true; },
  });
}

function storage() {
  const m = new Map();
  return { getItem: (k) => (m.has(k) ? m.get(k) : null), setItem: (k, v) => m.set(k, String(v)), removeItem: (k) => m.delete(k) };
}

// `refuse(path)` answers a request with a 404 instead, as a server without
// that route would.
function loadCockpit(base, refuse = () => false) {
  const html = readFileSync(join(DASH, "www", "index.html"), "utf8");
  const src = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)].map((m) => m[1]).at(-1);
  assert.ok(src, "the cockpit's inline script is in index.html");
  const elements = new Map();
  const el = (key) => {
    if (!elements.has(key)) elements.set(key, element(key));
    return elements.get(key);
  };
  const doc = element("document");
  Object.assign(doc, {
    querySelector: (s) => el(s), querySelectorAll: () => [], getElementById: (id) => el(`#${id}`),
    createElement: (t) => element(t), documentElement: element("html"), body: element("body"),
    activeElement: null, addEventListener() {}, removeEventListener() {},
  });
  let live = false; // the startup load() never settles; only the drawer's requests go out
  const requests = [];
  const vars = {};
  const overrides = {
    document: doc,
    window: element("window"),
    navigator: element("navigator"),
    location: { hash: "", search: "", pathname: "/", href: `${base}/`, origin: base },
    history: { replaceState() {}, pushState() {} },
    localStorage: storage(), sessionStorage: storage(),
    fetch: (p, opts) => {
      if (!live) return new Promise(() => {});
      requests.push(String(p));
      if (refuse(String(p))) {
        return Promise.resolve(new Response('{"error":"not found"}', { status: 404 }));
      }
      return fetch(new URL(String(p), base), opts);
    },
    setTimeout: (fn, ms) => (live ? setTimeout(fn, ms) : 0),
    setInterval: () => 0, clearTimeout() {}, clearInterval() {},
    requestAnimationFrame: () => 0,
    getComputedStyle: () => ({ getPropertyValue: () => "" }),
    matchMedia: () => ({ matches: false, addEventListener() {}, addListener() {} }),
    EventSource: class { addEventListener() {} close() {} },
    alert() {}, confirm: () => false, prompt: () => null,
  };
  const exported = {};
  const scope = new Proxy({}, {
    has: (_t, k) => k === "__exports__" || k in overrides || k in vars || !(k in globalThis),
    get: (_t, k) => {
      if (k === Symbol.unscopables) return undefined;
      if (k === "__exports__") return exported;
      if (k in overrides) return overrides[k];
      if (k in vars) return vars[k];
      return anything();
    },
    set: (_t, k, v) => { vars[k] = v; return true; },
  });
  // Function declarations are hoisted to the top of the block, so the drawer
  // is exported before any top-level statement runs.
  const run = new Function(
    "__scope__",
    `with (__scope__) {\n__exports__.openDrawer = openDrawer;\n${src}\n;__exports__.evaluated = true;\n}`,
  );
  run(scope);
  assert.ok(exported.evaluated, "the cockpit script evaluated to the end under the stub DOM");
  return {
    async openDrawer(uuid) {
      live = true;
      await exported.openDrawer(uuid);
    },
    // Everything the drawer rendered, wherever it put it.
    rendered: () => [...elements.values()].map((e) => `${e.innerHTML}\n${e.textContent}`).join("\n"),
    requests,
  };
}

async function withCockpit(spec, body, refuse) {
  const dir = mkdtempSync(join(tmpdir(), "ptask-drawer-"));
  const sidecar = await startSidecar(dir, buildDb(dir, spec));
  try {
    await body(loadCockpit(sidecar.base, refuse));
  } finally {
    sidecar.stop();
    rmSync(dir, { recursive: true, force: true });
  }
}

// ---- PR #122 finding 6 --------------------------------------------------------

const TASK = "0f5c2a8e-1b7d-4c3e-9a6f-2d8b7e1c4a90";

test("the drawer shows a note older than the task's newest 60 events", async () => {
  const OLD_NOTE = "restore drill passed on the second replica";
  const events = [
    { task_uuid: TASK, event_type: "task.created", actor: "shell", ts: at(1),
      payload: { task_uuid: TASK, pt_id: "PT-1", actor: "shell", source: "cli" } },
    { task_uuid: TASK, event_type: "task.noted", actor: "hal", ts: at(2),
      payload: { task_uuid: TASK, pt_id: "PT-1", note: OLD_NOTE, actor: "hal", source: "mcp" } },
  ];
  // 70 later events (priority edits) push the note out of the newest 60.
  for (let i = 0; i < 70; i++) {
    events.push({ task_uuid: TASK, event_type: "task.updated", actor: "shell", ts: at(3 + i),
      payload: { task_uuid: TASK, priority: 2 + (i % 3), actor: "shell", source: "cli" } });
  }
  await withCockpit({ tasks: [taskRow(TASK, "PT-1", "Keep the replica drill green")], events }, async (cockpit) => {
    await cockpit.openDrawer(TASK);
    assert.ok(
      cockpit.rendered().includes(OLD_NOTE),
      `the drawer dropped a note older than the newest 60 events (requests: ${cockpit.requests.join(", ")})`,
    );
  });
});

// ---- a server without the notes route (pt serve before 3.46.1) -------------

test("the drawer keeps its history and notes when the server has no notes route", async () => {
  const NOTE = "cert served on lhr";
  const events = [
    { task_uuid: TASK, event_type: "task.created", actor: "shell", ts: at(1),
      payload: { task_uuid: TASK, pt_id: "PT-1", actor: "shell", source: "cli" } },
    { task_uuid: TASK, event_type: "task.noted", actor: "hal", ts: at(2),
      payload: { task_uuid: TASK, pt_id: "PT-1", note: NOTE, actor: "hal", source: "mcp" } },
  ];
  await withCockpit(
    { tasks: [taskRow(TASK, "PT-1", "Renew the staging certificate")], events },
    async (cockpit) => {
      await cockpit.openDrawer(TASK);
      const shown = cockpit.rendered();
      assert.ok(
        !shown.includes("history unavailable"),
        `a missing notes route cost the drawer its history: ${shown.match(/history unavailable[^\n]*/)}`,
      );
      assert.ok(shown.includes("task.created"), "the history rendered");
      assert.ok(shown.includes(NOTE), "the notes fell back to the events");
    },
    (path) => path.endsWith("/notes"),
  );
});
