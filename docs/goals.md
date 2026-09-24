# Goals

Every task should trace up to the mission. Goals sit above tasks: a tree of
`G-n` nodes, each with a title and a *why*. A worker that picks up a task
gets the why with the what — `pt show`, MCP, and `pt context` all carry the
chain.

Borrowed from Paperclip's "goal ancestry". pTask already had tasks, a
free-text `project` column, `parent_uuid` subtasks, and `task_links`. Nothing
sat above a project.

## The model

| Piece | Where | Role |
|---|---|---|
| Goal | `goals` | `id` uuid, unique `seq` for `G-n`, title, why, nullable `parent_id`, status `active` / `achieved` / `abandoned` |
| Direct link | `tasks.goal_id` | Optional FK to `goals.id` |
| Inheritance | `tasks.parent_uuid` | A subtask with no `goal_id` inherits its parent task's effective goal |

Human ids are `G-<n>`, minted from `pt_counters.goal_id` the same way tasks
mint `PT-<n>` and approvals mint `AP-<n>`.

Status defaults to `active`. `pt goal done G-n` sets `achieved`;
`pt goal abandon G-n` sets `abandoned`. `pt goal ls` lists active goals;
`--all` includes the rest.

There are no per-project default goals. `project` is free text (a tag, not an
entity) and too fragile to key a default on. Linking is always explicit.

## Inheritance order

`effective_goal(task)` resolves, in order:

1. **direct** — the task's own `goal_id`
2. **parent** — walk `parent_uuid` until a task with a `goal_id` is found
3. **none**

The returned chain is **leaf → root** (the linked goal first, then its
ancestors toward the mission). `pt show --json` and MCP put that chain on
`goal_chain` (`[{id, title, why}, ...]`) plus `goal_source`
(`direct` | `parent` | `none`).

`pt goal show G-n` uses the same walk for `chain` (ancestors, nearest first,
excluding self). `tasks` are those whose *effective* goal is this goal.
`rollup.{open,done}` counts tasks whose effective goal is this goal **or any
descendant**. Open means not `done` / `dismissed`.

`pt goal orphans` lists open tasks with no effective goal.

## Cycle guard

`pt goal set-parent G-n G-m` refuses self and any cycle (the new parent is
already a descendant of the node being moved).

A corrupted database can still contain a cycle — there is no trigger that
prevents one, because `pt show` must survive it. **Every parent-pointer walk
(task `parent_uuid` and goal `parent_id`) stops on a repeated node and caps
depth at 16.** A cycle is cut, not repeated; `pt show` always terminates.

Tree listing (`pt goal ls`) is a pre-order walk: parent before children,
siblings by `seq`. Depth is the real tree depth, even when inactive parents
are filtered out of the default listing.

## CLI

```
pt goal add TITLE [--why W] [--parent G-n]
pt goal ls [--all]
pt goal show G-n
pt goal link PT-n G-n
pt goal unlink PT-n
pt goal set-parent G-n G-m
pt goal done G-n
pt goal abandon G-n
pt goal orphans
```

All honour the global `--json`. Goal JSON:

```json
{
  "id": "G-1",
  "uuid": "...",
  "title": "...",
  "why": "...",
  "parent": "G-n or null",
  "status": "active",
  "created_at": "...",
  "updated_at": "..."
}
```

`ls` items add `depth`. `show` adds `chain`, `children`, `tasks`, and
`rollup`.

`pt show PT-n` in JSON is the task object plus `goal_chain` and
`goal_source`. Human `pt show` prints a short Why block when a chain exists.

Mutations are journaled with the actor: `goal.created`, `goal.updated`,
`task.goal_linked`, `task.goal_unlinked`.

## `pt context` — dispatch brief

`pt context PT-n` prints a Markdown brief for a worker mission:

- task title (and description, when set)
- a **Why** section listing the goal chain **root → leaf**, each with its why
- **Blockers**: open `depends_on` prerequisites with their PT ids and titles

When the task has no effective goal the Why section is omitted; the command
still succeeds. Use this as the mission preamble when dispatching a worker:
the *what* and the *why* in one page, plus anything that must finish first.

```
pt context PT-42
```

## MCP

`task_show`, `task_next`, and `task_claim` each carry `goal_chain` and
`goal_source` per task. New tools:

| Tool | Arguments |
|---|---|
| `goal_list` | optional `all` |
| `goal_show` | `id` (`G-n`) |
| `goal_link` | `{task, goal}` |
