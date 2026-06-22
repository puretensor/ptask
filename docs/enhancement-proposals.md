# pTask — Enhancement Proposals

> Origin: operator friction during 2026-06-22 session. Creating PT-653 with `p4`
> (intending "urgent") silently landed it at **low**, then required an SSH into
> tensor-core + absolute-path binary call to fix. This doc grounds "why pTask
> feels spartan / always has issues" in the actual code and proposes fixes,
> prioritized by operator pain.

Baseline reviewed: **v1.6.0** (`~/ptask`, 6-crate workspace, ~14.4K LOC Rust).
The engine is not actually spartan — it has distill/scoring/accountability/DAG.
The problem is the **operator-facing surface**: confusing priority semantics, a
remote CLI missing every verb except add/list/done, and several commands the
master-plan promised but never shipped.

---

## P0 — Correctness bugs (these are the "always having issues")

### 1. Priority-scale collision (THE root cause of the PT-653 mis-fire)
`quickadd.rs` uses Todoist's **inverted** convention and maps it onto pTask's native scale:

| You type (quick-add) | Todoist meaning | pTask value stored | `pt list` displays |
|----------------------|-----------------|--------------------|--------------------|
| `p1` | highest | 4 (urgent) | **p4** |
| `p2` | high | 3 (high) | p3 |
| `p3` | medium | 2 (normal) | p2 |
| `p4` | low | **1 (low)** | **p1** |
| `p5` | — (out of range) | *not parsed at all* | stays as title text |

So the **same token means opposite things** depending on surface:
- In quick-add (`pt add`/`pt remote add`), `p4` = **low**.
- In the display, `pt priority`, and `--priority` flag, the scale is `p1=low … p5=critical`, so `p4` = **urgent**.

That inversion is exactly why `pt remote add "...p4"` produced a **low** task. And **critical (p5) is unreachable via quick-add entirely** — out of Todoist's 1–4 range, so it falls through as a title word.

**Fix (recommended):** unify on ONE scale everywhere — native `1..5` where `p1=low … p5=critical` — matching the display, `pt priority`, and `--priority`. Drop the Todoist inversion in quick-add. Keep word aliases (`low|normal|high|urgent|critical`) working in quick-add too (currently the parser only does `pN`). Internal-only tool, so the break is acceptable and worth it. Add a one-release deprecation note in `--help`.

**Cheaper interim fix:** if we keep Todoist semantics, at minimum (a) make `p5`/`critical` reachable in quick-add, and (b) print the interpreted priority on add so silent inversion is impossible to miss (see #6).

### 2. The ptask *skill* doc contradicts the *tool*
`~/.claude/skills/ptask/SKILL.md` documents quick-add as "`p1` (low) … `p5` (critical)" — the **opposite** of what `quickadd.rs` actually does, and it references a `p5` token the parser can't even read. Whatever we decide in #1, the skill doc and `pt add --help` must be made to agree with the binary. This doc drift is half the reason HAL keeps mis-setting priority.

---

## P1 — Thin remote surface (the daily friction)

`pt remote` exposes only **add / list / done**. Every other mutation forces an SSH
into the canonical host (tensor-core) and an absolute-path call
(`~/.cargo/bin/pt …`, because `pt` isn't even on the non-interactive PATH there).
That round-trip is the single biggest "pTask is painful" complaint.

### 3. Add the missing verbs to `pt remote`
The sync protocol already carries arbitrary mutations; the client just doesn't expose them. Add:
- `pt remote priority PT-N <level>`  ← would have made the PT-653 fix one local command
- `pt remote edit PT-N [--title|--deadline|--clear-deadline|--desc]`
- `pt remote reopen PT-N` (un-done)
- `pt remote show PT-N` (single-task detail; see #5)
- `pt remote next` (DAG-ready list from anywhere)

### 4. Put `pt` on PATH on the canonical host
Non-interactive `ssh tensor-core 'pt …'` fails (`command not found`). Symlink
`~/.cargo/bin/pt` → `/usr/local/bin/pt` (or ship via the same profile.d that sets
`PTASK_SYNC_URL`) so the SSH fallback path actually works without absolute paths.

---

## P2 — Commands the master-plan promised but never shipped

The North Star in `docs/master-plan.md` advertises `pt add/list/done/next/edit/show/rm/review`. Actually shipped: add/list/done/next/edit. Missing:

### 5. `pt show PT-N` — single-task detail
Today you can only `list`. There's no way to see one task's full record: description,
deadline, labels/project, dependencies, score breakdown, event history. This is
table-stakes and is in the North Star.

### 6. `pt add` should echo the parsed interpretation
`QuickAdd` already has a `Display` impl emitting `p=urgent`/date/label bits — it's
just not surfaced on create. Print "Parsed: p=low, due=…, @label, #project" on every
`add` (or behind `--explain`). This alone makes silent mis-parses like PT-653
impossible to miss at the moment of creation.

### 7. `pt reopen` / `pt rm` for tasks
`rm` exists only for *views*. There's no CLI verb to reopen a wrongly-completed task
or to delete/dismiss one (the `dismissed` status exists in the model but has no verb).

### 8. `pt review` — the weekly triage loop
Listed as a v1.0 success criterion ("Friday Telegram conversation that triages the
inbox, flags stale items, produces a weekly summary"). Accountability timers exist,
but the interactive `review` surface was never built.

---

## P3 — Status model & workflow richness

### 9. Richer status vocabulary + transitions
Status is a free-text string defaulting to `pending`; effective vocabulary is
`pending|done|dismissed|blocked`. There's no `in_progress` (mark "working on it now")
and no `snooze`/`defer` (hide until a wake date). Both are core to a real triage
loop and were a stated Linear-grade goal. Add `pt start PT-N` and
`pt snooze PT-N <date>`.

### 10. Dependencies from the CLI
The DAG engine and `pt next` exist, but there's no verb to *create* a dependency
(`pt depend PT-A --on PT-B`). Today edges can only arrive via import/sync, so `next`
is underused because operators can't express blockers.

---

## Suggested sequencing

1. **#1 + #2 together** — unify the priority scale and fix the skill/`--help` docs. Highest pain, smallest diff, kills the recurring mis-priority class of bug.
2. **#3 + #4** — `pt remote priority` + `pt remote edit` + PATH fix. Removes the SSH-to-canonical-host tax.
3. **#6** — parsed-interpretation echo on add. Cheap, high-signal safety net.
4. **#5, #7** — `show` / `reopen` / task `rm`. Round out the promised CLI.
5. **#8, #9, #10** — review loop, status richness, CLI dependencies. Larger, schedule deliberately.

Items 1–3 are a half-day of work and would eliminate ~80% of the recurring friction.
