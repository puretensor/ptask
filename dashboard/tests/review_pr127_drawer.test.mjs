// Contract tests from the pre-merge review of PR #127 (acceptance criteria
// gate the close), cockpit half. On the PR head the drawer folds a task's
// acceptance criteria from GET /api/tasks/<id>/events, which the sidecar caps
// at the newest 60 events: a recurring task with criteria loses its checklist
// from the drawer after about fifteen occurrences, while `pt` still refuses
// the close on it. The drawer also interpolates the criterion number into
// innerHTML unescaped.
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

function loadCockpit(base) {
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

async function withCockpit(spec, body) {
  const dir = mkdtempSync(join(tmpdir(), "ptask-drawer-"));
  const sidecar = await startSidecar(dir, buildDb(dir, spec));
  try {
    await body(loadCockpit(sidecar.base));
  } finally {
    sidecar.stop();
    rmSync(dir, { recursive: true, force: true });
  }
}

// ---- PR #127 finding 2: the checklist survives a long history ----------------

const TASK = "6b1e9d42-3c8a-4f05-8e27-9a4c1d7b2e63";

function journal() {
  let i = 0;
  const events = [];
  const add = (event_type, payload, actor = "shell") =>
    events.push({ task_uuid: TASK, event_type, actor, ts: at(++i),
      payload: { task_uuid: TASK, ...payload, actor, source: "cli" } });
  return { events, add };
}

test("the drawer checklist survives a recurring task's history past 60 events", async () => {
  const { events, add } = journal();
  add("task.created", { pt_id: "PT-1" });
  add("task.criterion_added", { n: 1, text: "restore verified" });
  add("task.criterion_added", { n: 2, text: "offsite copy checked" });
  // Fifteen daily occurrences: both checked, advanced, checks reset.
  for (let day = 0; day < 15; day++) {
    add("task.criterion_checked", { n: 1 }, "hal");
    add("task.criterion_checked", { n: 2 }, "hal");
    add("task.recurrence_advanced", { pt_id: "PT-1", next_deadline: at(1440 * (day + 1)) });
    add("task.criteria_reset", { next_deadline: at(1440 * (day + 1)) });
  }
  // Today: 1 of 2 checked, so pt refuses the close.
  add("task.criterion_checked", { n: 1, evidence: "restore of last night's snapshot passed" }, "hal");
  assert.ok(events.length > 60, "the fixture is longer than the drawer's 60-event window");

  await withCockpit({ tasks: [taskRow(TASK, "PT-1", "Daily restore drill")], events }, async (cockpit) => {
    await cockpit.openDrawer(TASK);
    const html = cockpit.rendered();
    assert.ok(
      html.includes("restore verified") && html.includes("offsite copy checked"),
      `the drawer lost the acceptance criteria of a task with more than 60 events (requests: ${cockpit.requests.join(", ")})`,
    );
    assert.ok(html.includes("1/2"), "the drawer shows 1 of 2 criteria checked");
  });
});

// ---- PR #127 finding 7: the criterion number is escaped ----------------------

test("a criterion number from the journal is escaped in the drawer", async () => {
  const { events, add } = journal();
  add("task.created", { pt_id: "PT-1" });
  add("task.criterion_added", { n: "<img src=x onerror=alert(1)>", text: "escape probe" });
  await withCockpit({ tasks: [taskRow(TASK, "PT-1", "Escape probe")], events }, async (cockpit) => {
    await cockpit.openDrawer(TASK);
    const html = cockpit.rendered();
    assert.ok(!html.includes("<img"), "the drawer put raw markup from the journal into innerHTML via ${c.n}");
  });
});
