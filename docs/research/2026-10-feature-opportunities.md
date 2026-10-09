# Feature opportunities: research note (2026-10)

Scope: what is *missing* from pTask (3.42.2), not what is broken. Written to
decide which additions most raise its usefulness for the people who use it:
one operator, and the agents working the queue through MCP, `/sync` and the
CLI. It ranks the opportunities found and says why the top three should be
built, in order. #1 shipped as 3.43.0 (puretensor/ptask#122); the rest are
not in this tree.

## What pTask is, and what it already does well

pTask is a single-binary, self-hosted task system whose design centre is
*accountable work by agents under an operator*: an attributed journal (`pt
log`, `pt undo`), an atomic `task_claim`, a DAG that refuses to close a
blocked task, an approvals inbox only the operator can decide, a goal tree
that gives every task its why, deterministic session priming (`task_digest`,
`pt context`), distillation that deduplicates machine signals before they
become tasks, and a reaper for stale machine-made work. Most of what
mainstream trackers offer (quick-add, filter DSL, saved views, FTS, recurrence,
snooze, bulk, review, scoring with explanations, a calendar planner) is
already here. The gaps are in the loop *around* agent work: what happened,
who holds what, and whether a new task is really new.

## Method

1. **Operator evidence.** The fleet's own session reports and memory about
   pTask (July to October 2026), read for repeated friction rather than
   one-off bugs.
2. **State of the art.** Agent-native trackers (Beads `bd`, Backlog.md, Task
   Master, Linear for Agents), CLI managers (Taskwarrior 3, dstask,
   todo.txt), mainstream tools (Linear, GitHub Issues, Todoist, OmniFocus),
   one 2026 paper on multi-agent work allocation, and practitioner write-ups
   on agent-filed backlogs. Sources are listed at the end.
3. **Judged** on impact for the operator and agents, fit with pTask's
   principles (single binary, SQLite, attributed journal, nothing that
   changes state on by default, no new external services), and maintenance
   cost.

## Ranked opportunities

| # | Opportunity | Operator evidence | Precedent | Impact | Fit / cost | Decision |
|---|---|---|---|---|---|---|
| 1 | **Closure evidence and task notes**: `pt note`, `--note` on done/dismiss, carried into show/context/digest/export/cockpit; automated closers record their own evidence | Closure runs reported "pTask has no comment surface, so evidence is attached here before closure"; PT-1393 was closed by a dashboard click with no verification and read green for 16 days ("a task's status field is a claim, not evidence"); agents wrote evidence into descriptions, overwriting the original ask | Taskwarrior `annotate`, Beads `bd close -r`, GitHub/Linear comments; Anthropic's long-running-agent harness names agents marking work complete without testing | High: every close by every surface | Excellent: notes are journal events, so no migration, attribution and append-only come free | **Built** (3.43.0, puretensor/ptask#122) |
| 2 | **Claim ownership, release and leases**: `claimed_by`, `task_release`, optional lease TTL with heartbeat, an expiry sweep that ships off | Closure reports twice: "claims are released by closure or recorded as parked because pTask has no unclaim operation"; the docs admit a crashed agent's claim stays `in_progress` until someone notices; the reaper skips claimed tasks, so nothing recovers them | Beads `--claim` + `heartbeat` + `reclaim`; Linear agent sessions go stale after 30 min; Marcus treats any tool call as a heartbeat; arXiv 2606.19616 measures duplicate work without leases | High for parallel agent sweeps | Good: one small migration; the sweep changes state, so it is off by default | Next (not in this tree) |
| 3 | **Duplicate check at filing time and merge**: `task_add` / `pt add` report likely open duplicates; `pt merge` closes one into another, carrying labels and dependents | Backlog scans repeatedly listed hand- and agent-filed duplicates (several pairs and a trio in one pass); the operator rule that a closing pass must not open more than it closes, after a `+13 / −4` half hour | Beads `bd duplicates` / `bd duplicate --of`; GitHub `--duplicate-of`; Linear Triage Intelligence suggests duplicates at creation | High: attacks backlog inflation at the source | Good: lexical similarity over FTS, no model needed; merge is an attributed dismiss plus link moves. Notes stay on the closed task's journal; merge does not carry them. | Next (not in this tree) |
| 4 | Per-actor flow metrics: created vs closed per client, cycle-time p50/p90, untouched share | The `+13 / −4` flux chip shows counts but not *who* | Linear Insights; agent-backlog write-ups (342 opened vs 218 closed in a week) | Medium | Cheap (SQL over the journal) | Next (not in this tree) |
| 5 | Acceptance criteria and gated done (checklist; agents must tick all plus a note) | Same false-green lesson as #1 | Backlog.md `--ac` / definition of done; Beads `bd lint` | Medium | Opt-in per task, so no fleet-wide policy is imposed; #1 delivers the evidence half | Next (not in this tree) |
| 6 | Close-and-continue: `task_done` returns newly unblocked tasks, optional claim-next | Agent sweeps round-trip `task_next` after every close | Beads `--suggest-next` / `--claim-next` | Low to medium | Cheap | Next (not in this tree) |
| 7 | External gates (CI run, PR merged) as DAG nodes | Blocked work polled by hand | Beads `bd gate` | Medium | Needs a poller calling GitHub/Gitea: a new outbound dependency | Not now |
| 8 | ICS feed of deadlines and plan blocks | `pt plan --write` already puts the plan on the calendar | Todoist calendar feed | Low | A token in a subscribable URL is a long-lived credential in calendar apps | Not now |
| 9 | Templates / ephemeral tasks, veto hooks, contexts, history compaction | No repeated evidence | Beads molecules/wisps, Taskwarrior hooks and contexts, `bd admin compact` | Low | Hooks run code that changes state; compaction overlaps the fleet's memory system | Not now |

## Why these three

They close one loop, in order. An agent claims work (#2 makes the claim
owned, releasable and recoverable), works it and records what it found and
how it verified the result (#1 makes that evidence part of the close), and
does not file a second copy of work that already exists (#3). Each answers a
complaint the operator or the agents made more than once, each has direct
precedent in the agent-native tools that emerged in 2025-2026, and each fits
pTask's existing machinery: the journal, the atomic claim, FTS and the
link table. None adds a service, a network call or a background job that
changes state unless the operator turns it on.

Sequenced as separate PRs, one feature each. This tree ships only
puretensor/ptask#122 (notes and closure evidence, 3.43.0). #2 (claim
ownership, heartbeat leases, release and reclaim) and #3 (duplicate check
at filing time, `pt dupes`, `pt merge`) are the next two; they are not built
here. A merge is an attributed dismiss plus link moves — labels and
dependents transfer; notes stay on the closed task and are not carried.
#4 (per-actor flux), #6 (close-and-continue) and #5 (opt-in acceptance
criteria) follow the same loop and are also unbuilt in this tree.

The remaining three are stopped on purpose, not for lack of time. #7 needs a
poller calling GitHub or Gitea, which is the outbound dependency the brief
rules out; the git webhook already closes tasks on `Closes PT-n`, which covers
the commonest case. #8 would put a long-lived token in a calendar
subscription URL, and `pt plan --write` already puts the plan on the
calendar. #9 has no repeated evidence of need, and its hooks would run code
that changes state.

## Sources

- Beads CLI reference: <https://raw.githubusercontent.com/steveyegge/beads/main/docs/CLI_REFERENCE.md>; claim heartbeat and reclaim: <https://beads.gascity.com/cli-reference/heartbeat>, <https://beads.gascity.com/cli-reference/reclaim>
- Linear for Agents (session staleness): <https://linear.app/developers/agents>; Triage Intelligence: <https://linear.app/docs/triage-intelligence>; Insights: <https://linear.app/docs/insights>
- Marcus agent recovery (heartbeat by any tool call): <https://marcus.readthedocs.io/en/latest/systems/coordination/34-agent-recovery-system.html>
- Lease-based allocation and duplicate work among agents: <https://arxiv.org/abs/2606.19616>
- GitHub `gh issue close --duplicate-of`: <https://manpages.opensuse.org/Tumbleweed/gh/gh-issue-close.1.en.html>
- Backlog.md (acceptance criteria, definition of done): <https://raw.githubusercontent.com/MrLesk/Backlog.md/main/README.md>
- Anthropic, effective harnesses for long-running agents: <https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents>
- Taskwarrior docs (annotate, hooks, contexts): <https://taskwarrior.org/docs/>
- Todoist calendar feed: <https://todoist.com/help/articles/208789889>
- Agent-filed backlogs: <https://github.com/Morrison-Lab/ai-config/issues/3134>, <https://allen.hutchison.org/2026/08/25/the-backlog-was-a-coping-mechanism/>
