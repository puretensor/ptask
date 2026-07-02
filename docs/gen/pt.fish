# Print an optspec for argparse to handle cmd's options that are independent of any subcommand.
function __fish_pt_global_optspecs
	string join \n db= h/help V/version
end

function __fish_pt_needs_command
	# Figure out if the current invocation already has a command.
	set -l cmd (commandline -opc)
	set -e cmd[1]
	argparse -s (__fish_pt_global_optspecs) -- $cmd 2>/dev/null
	or return
	if set -q argv[1]
		# Also print the command, so this can be used to figure out what it is.
		echo $argv[1]
		return 1
	end
	return 0
end

function __fish_pt_using_subcommand
	set -l cmd (__fish_pt_needs_command)
	test -z "$cmd"
	and return 1
	contains -- $cmd[1] $argv
end

complete -c pt -n "__fish_pt_needs_command" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_needs_command" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_needs_command" -s V -l version -d 'Print version'
complete -c pt -n "__fish_pt_needs_command" -f -a "add" -d 'Create a new task'
complete -c pt -n "__fish_pt_needs_command" -f -a "list" -d 'List tasks'
complete -c pt -n "__fish_pt_needs_command" -f -a "done" -d 'Mark a task done (by PT-N or title substring)'
complete -c pt -n "__fish_pt_needs_command" -f -a "priority" -d 'Promote/demote a task\'s priority (critical|urgent|high|normal|low or 1..=5)'
complete -c pt -n "__fish_pt_needs_command" -f -a "edit" -d 'Edit task fields'
complete -c pt -n "__fish_pt_needs_command" -f -a "reopen" -d 'Reopen a completed/dismissed task (status → pending)'
complete -c pt -n "__fish_pt_needs_command" -f -a "show" -d 'Show one task\'s full row + side-table detail'
complete -c pt -n "__fish_pt_needs_command" -f -a "dismiss" -d 'Dismiss a task (soft close, status → dismissed; reversible via reopen)'
complete -c pt -n "__fish_pt_needs_command" -f -a "rm" -d 'Delete a task permanently (hard delete + tombstone)'
complete -c pt -n "__fish_pt_needs_command" -f -a "next" -d 'Show ready-to-start tasks (all dependencies done)'
complete -c pt -n "__fish_pt_needs_command" -f -a "view" -d 'Manage saved views'
complete -c pt -n "__fish_pt_needs_command" -f -a "tui" -d 'Launch the terminal UI (ratatui)'
complete -c pt -n "__fish_pt_needs_command" -f -a "serve" -d 'Run the HTTP server (sync API, capture, webhooks, metrics)'
complete -c pt -n "__fish_pt_needs_command" -f -a "bot" -d 'Run the Telegram bot (Bot API long-poll)'
complete -c pt -n "__fish_pt_needs_command" -f -a "branch" -d 'Print a Linear-style branch name for a task'
complete -c pt -n "__fish_pt_needs_command" -f -a "distill" -d 'Run the distillation pipeline (Python subprocess shim until v0.9)'
complete -c pt -n "__fish_pt_needs_command" -f -a "accountability" -d 'Run one accountability cycle (escalation + Telegram/email)'
complete -c pt -n "__fish_pt_needs_command" -f -a "scoring" -d 'Recompute composite priority scores for all active tasks'
complete -c pt -n "__fish_pt_needs_command" -f -a "remote" -d 'Talk to a remote canonical `pt serve` (no local DB)'
complete -c pt -n "__fish_pt_needs_command" -f -a "log" -d 'Show a task\'s attributed event history (who did what, via which surface)'
complete -c pt -n "__fish_pt_needs_command" -f -a "undo" -d 'Reverse the most recent undoable mutation (done/dismiss/create)'
complete -c pt -n "__fish_pt_needs_command" -f -a "token" -d 'Manage named scoped API tokens (create/list/revoke)'
complete -c pt -n "__fish_pt_needs_command" -f -a "backfill" -d 'One-shot backfill PT-N for any tasks lacking one'
complete -c pt -n "__fish_pt_needs_command" -f -a "gen-manpage" -d 'Generate the `pt(1)` manpage to stdout'
complete -c pt -n "__fish_pt_needs_command" -f -a "gen-completions" -d 'Generate shell completions (bash/zsh/fish) to stdout'
complete -c pt -n "__fish_pt_needs_command" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c pt -n "__fish_pt_using_subcommand add" -s p -l priority -d 'Priority override (low|normal|high|urgent|critical or 1..=5). If omitted, uses quick-add priority or "normal"' -r
complete -c pt -n "__fish_pt_using_subcommand add" -s d -l description -d 'Description override' -r
complete -c pt -n "__fish_pt_using_subcommand add" -l deadline -d 'Deadline override (ISO date, e.g. 2026-05-20)' -r
complete -c pt -n "__fish_pt_using_subcommand add" -l reason -d 'Why this task was created — stored as ai_reasoning' -r
complete -c pt -n "__fish_pt_using_subcommand add" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand add" -l raw -d 'Disable quick-add parsing — treat the title literally'
complete -c pt -n "__fish_pt_using_subcommand add" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand list" -s s -l status -d 'Filter by status (or `all`)' -r
complete -c pt -n "__fish_pt_using_subcommand list" -s p -l priority -d 'Filter by priority' -r
complete -c pt -n "__fish_pt_using_subcommand list" -s n -l limit -d 'Max rows' -r
complete -c pt -n "__fish_pt_using_subcommand list" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand list" -s v -l verbose -d 'Show description and UUID'
complete -c pt -n "__fish_pt_using_subcommand list" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand done" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand done" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand priority" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand priority" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand edit" -l deadline -d 'Set deadline to an ISO date/datetime, e.g. 2026-06-16' -r
complete -c pt -n "__fish_pt_using_subcommand edit" -l title -d 'Replace the title' -r
complete -c pt -n "__fish_pt_using_subcommand edit" -l desc -d 'Replace the description' -r
complete -c pt -n "__fish_pt_using_subcommand edit" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand edit" -l clear-deadline -d 'Clear the deadline'
complete -c pt -n "__fish_pt_using_subcommand edit" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand reopen" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand reopen" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand show" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand show" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand dismiss" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand dismiss" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand rm" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand rm" -s y -l yes -d 'Skip the confirmation prompt'
complete -c pt -n "__fish_pt_using_subcommand rm" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand next" -s n -l limit -d 'Max ready tasks to show' -r
complete -c pt -n "__fish_pt_using_subcommand next" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand next" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand view; and not __fish_seen_subcommand_from save list show rm help" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand view; and not __fish_seen_subcommand_from save list show rm help" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand view; and not __fish_seen_subcommand_from save list show rm help" -f -a "save" -d 'Save a filter DSL string under a name'
complete -c pt -n "__fish_pt_using_subcommand view; and not __fish_seen_subcommand_from save list show rm help" -f -a "list" -d 'List saved views'
complete -c pt -n "__fish_pt_using_subcommand view; and not __fish_seen_subcommand_from save list show rm help" -f -a "show" -d 'Run a saved view\'s filter and print matching tasks'
complete -c pt -n "__fish_pt_using_subcommand view; and not __fish_seen_subcommand_from save list show rm help" -f -a "rm" -d 'Delete a saved view'
complete -c pt -n "__fish_pt_using_subcommand view; and not __fish_seen_subcommand_from save list show rm help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c pt -n "__fish_pt_using_subcommand view; and __fish_seen_subcommand_from save" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand view; and __fish_seen_subcommand_from save" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand view; and __fish_seen_subcommand_from list" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand view; and __fish_seen_subcommand_from list" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand view; and __fish_seen_subcommand_from show" -s n -l limit -d 'Override row limit' -r
complete -c pt -n "__fish_pt_using_subcommand view; and __fish_seen_subcommand_from show" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand view; and __fish_seen_subcommand_from show" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand view; and __fish_seen_subcommand_from rm" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand view; and __fish_seen_subcommand_from rm" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand view; and __fish_seen_subcommand_from help" -f -a "save" -d 'Save a filter DSL string under a name'
complete -c pt -n "__fish_pt_using_subcommand view; and __fish_seen_subcommand_from help" -f -a "list" -d 'List saved views'
complete -c pt -n "__fish_pt_using_subcommand view; and __fish_seen_subcommand_from help" -f -a "show" -d 'Run a saved view\'s filter and print matching tasks'
complete -c pt -n "__fish_pt_using_subcommand view; and __fish_seen_subcommand_from help" -f -a "rm" -d 'Delete a saved view'
complete -c pt -n "__fish_pt_using_subcommand view; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c pt -n "__fish_pt_using_subcommand tui" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand tui" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand serve" -l bind -d 'Bind address. Default 127.0.0.1:9501 (leaves :9500 for legacy Python FastAPI during the parallel-ops window)' -r
complete -c pt -n "__fish_pt_using_subcommand serve" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand serve" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand bot" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand bot" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand branch" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand branch" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand distill" -l days -d 'Days of history for the Python pipeline to ingest (defaults to 60, matching the legacy systemd unit)' -r
complete -c pt -n "__fish_pt_using_subcommand distill" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand distill" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand accountability; and not __fish_seen_subcommand_from run help" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand accountability; and not __fish_seen_subcommand_from run help" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand accountability; and not __fish_seen_subcommand_from run help" -f -a "run" -d 'Run the state machine + dispatch once'
complete -c pt -n "__fish_pt_using_subcommand accountability; and not __fish_seen_subcommand_from run help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c pt -n "__fish_pt_using_subcommand accountability; and __fish_seen_subcommand_from run" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand accountability; and __fish_seen_subcommand_from run" -l dry-run -d 'Don\'t actually send anything; log what would have been dispatched'
complete -c pt -n "__fish_pt_using_subcommand accountability; and __fish_seen_subcommand_from run" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand accountability; and __fish_seen_subcommand_from help" -f -a "run" -d 'Run the state machine + dispatch once'
complete -c pt -n "__fish_pt_using_subcommand accountability; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c pt -n "__fish_pt_using_subcommand scoring; and not __fish_seen_subcommand_from run help" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand scoring; and not __fish_seen_subcommand_from run help" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand scoring; and not __fish_seen_subcommand_from run help" -f -a "run" -d 'Recompute the four score_* columns + priority_score for every task with status NOT IN (\'done\', \'dismissed\')'
complete -c pt -n "__fish_pt_using_subcommand scoring; and not __fish_seen_subcommand_from run help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c pt -n "__fish_pt_using_subcommand scoring; and __fish_seen_subcommand_from run" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand scoring; and __fish_seen_subcommand_from run" -l dry-run -d 'Compute and log scores but don\'t write them back to the DB'
complete -c pt -n "__fish_pt_using_subcommand scoring; and __fish_seen_subcommand_from run" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand scoring; and __fish_seen_subcommand_from help" -f -a "run" -d 'Recompute the four score_* columns + priority_score for every task with status NOT IN (\'done\', \'dismissed\')'
complete -c pt -n "__fish_pt_using_subcommand scoring; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c pt -n "__fish_pt_using_subcommand remote; and not __fish_seen_subcommand_from add list done priority edit reopen show next dismiss version help" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand remote; and not __fish_seen_subcommand_from add list done priority edit reopen show next dismiss version help" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand remote; and not __fish_seen_subcommand_from add list done priority edit reopen show next dismiss version help" -f -a "add" -d '`pt remote add "..."` — create a task on the canonical host without opening a local DB. Uses PTASK_SYNC_URL (default http://100.121.42.54:9501)'
complete -c pt -n "__fish_pt_using_subcommand remote; and not __fish_seen_subcommand_from add list done priority edit reopen show next dismiss version help" -f -a "list" -d '`pt remote list` — fetch the live task set from the canonical host'
complete -c pt -n "__fish_pt_using_subcommand remote; and not __fish_seen_subcommand_from add list done priority edit reopen show next dismiss version help" -f -a "done" -d '`pt remote done <query>` — mark a task done by PT-N or title substring'
complete -c pt -n "__fish_pt_using_subcommand remote; and not __fish_seen_subcommand_from add list done priority edit reopen show next dismiss version help" -f -a "priority" -d '`pt remote priority <query> <level>` — set priority on the canonical host'
complete -c pt -n "__fish_pt_using_subcommand remote; and not __fish_seen_subcommand_from add list done priority edit reopen show next dismiss version help" -f -a "edit" -d '`pt remote edit <query> --deadline <iso> | --clear-deadline`'
complete -c pt -n "__fish_pt_using_subcommand remote; and not __fish_seen_subcommand_from add list done priority edit reopen show next dismiss version help" -f -a "reopen" -d '`pt remote reopen <query>` — flip a done/dismissed task back to pending'
complete -c pt -n "__fish_pt_using_subcommand remote; and not __fish_seen_subcommand_from add list done priority edit reopen show next dismiss version help" -f -a "show" -d '`pt remote show <query>` — print one task\'s full row + detail (read-only)'
complete -c pt -n "__fish_pt_using_subcommand remote; and not __fish_seen_subcommand_from add list done priority edit reopen show next dismiss version help" -f -a "next" -d '`pt remote next [-n N]` — DAG-ready tasks from the canonical host'
complete -c pt -n "__fish_pt_using_subcommand remote; and not __fish_seen_subcommand_from add list done priority edit reopen show next dismiss version help" -f -a "dismiss" -d '`pt remote dismiss <query>` — soft-close a task (reversible via reopen)'
complete -c pt -n "__fish_pt_using_subcommand remote; and not __fish_seen_subcommand_from add list done priority edit reopen show next dismiss version help" -f -a "version" -d '`pt remote version` — compare this client\'s version against the canonical server\'s `GET /version`. Exits non-zero on skew'
complete -c pt -n "__fish_pt_using_subcommand remote; and not __fish_seen_subcommand_from add list done priority edit reopen show next dismiss version help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from add" -l url -d 'Override the canonical endpoint' -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from add" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from add" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from list" -s s -l status -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from list" -s p -l priority -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from list" -s n -l limit -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from list" -l url -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from list" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from list" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from done" -l url -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from done" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from done" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from priority" -l url -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from priority" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from priority" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from edit" -l deadline -d 'Set deadline to an ISO date/datetime, e.g. 2026-06-30' -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from edit" -l title -d 'Replace the title' -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from edit" -l desc -d 'Replace the description' -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from edit" -l url -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from edit" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from edit" -l clear-deadline -d 'Clear the deadline'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from edit" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from reopen" -l url -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from reopen" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from reopen" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from show" -l url -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from show" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from show" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from next" -s n -l limit -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from next" -l url -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from next" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from next" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from dismiss" -l url -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from dismiss" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from dismiss" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from version" -l url -d 'Override the canonical endpoint' -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from version" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from version" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from help" -f -a "add" -d '`pt remote add "..."` — create a task on the canonical host without opening a local DB. Uses PTASK_SYNC_URL (default http://100.121.42.54:9501)'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from help" -f -a "list" -d '`pt remote list` — fetch the live task set from the canonical host'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from help" -f -a "done" -d '`pt remote done <query>` — mark a task done by PT-N or title substring'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from help" -f -a "priority" -d '`pt remote priority <query> <level>` — set priority on the canonical host'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from help" -f -a "edit" -d '`pt remote edit <query> --deadline <iso> | --clear-deadline`'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from help" -f -a "reopen" -d '`pt remote reopen <query>` — flip a done/dismissed task back to pending'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from help" -f -a "show" -d '`pt remote show <query>` — print one task\'s full row + detail (read-only)'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from help" -f -a "next" -d '`pt remote next [-n N]` — DAG-ready tasks from the canonical host'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from help" -f -a "dismiss" -d '`pt remote dismiss <query>` — soft-close a task (reversible via reopen)'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from help" -f -a "version" -d '`pt remote version` — compare this client\'s version against the canonical server\'s `GET /version`. Exits non-zero on skew'
complete -c pt -n "__fish_pt_using_subcommand remote; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c pt -n "__fish_pt_using_subcommand log" -s n -l limit -d 'Max events to show (newest first)' -r
complete -c pt -n "__fish_pt_using_subcommand log" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand log" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand undo" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand undo" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand token; and not __fish_seen_subcommand_from create list revoke help" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand token; and not __fish_seen_subcommand_from create list revoke help" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand token; and not __fish_seen_subcommand_from create list revoke help" -f -a "create" -d 'Mint a token for a client. Prints the plain token ONCE — store it with the consumer; only its hash is kept'
complete -c pt -n "__fish_pt_using_subcommand token; and not __fish_seen_subcommand_from create list revoke help" -f -a "list" -d 'List all tokens (client, scope, created/last-used/revoked)'
complete -c pt -n "__fish_pt_using_subcommand token; and not __fish_seen_subcommand_from create list revoke help" -f -a "revoke" -d 'Revoke every active token for a client id'
complete -c pt -n "__fish_pt_using_subcommand token; and not __fish_seen_subcommand_from create list revoke help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c pt -n "__fish_pt_using_subcommand token; and __fish_seen_subcommand_from create" -l scope -d 'Scope: read | capture | write | admin (each implies the previous)' -r
complete -c pt -n "__fish_pt_using_subcommand token; and __fish_seen_subcommand_from create" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand token; and __fish_seen_subcommand_from create" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand token; and __fish_seen_subcommand_from list" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand token; and __fish_seen_subcommand_from list" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand token; and __fish_seen_subcommand_from revoke" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand token; and __fish_seen_subcommand_from revoke" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand token; and __fish_seen_subcommand_from help" -f -a "create" -d 'Mint a token for a client. Prints the plain token ONCE — store it with the consumer; only its hash is kept'
complete -c pt -n "__fish_pt_using_subcommand token; and __fish_seen_subcommand_from help" -f -a "list" -d 'List all tokens (client, scope, created/last-used/revoked)'
complete -c pt -n "__fish_pt_using_subcommand token; and __fish_seen_subcommand_from help" -f -a "revoke" -d 'Revoke every active token for a client id'
complete -c pt -n "__fish_pt_using_subcommand token; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c pt -n "__fish_pt_using_subcommand backfill" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand backfill" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand gen-manpage" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand gen-manpage" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand gen-completions" -l db -d 'Override the SQLite path (default: $PTASK_DB or ~/puretensor-tasks/tasks.db)' -r
complete -c pt -n "__fish_pt_using_subcommand gen-completions" -s h -l help -d 'Print help'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "add" -d 'Create a new task'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "list" -d 'List tasks'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "done" -d 'Mark a task done (by PT-N or title substring)'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "priority" -d 'Promote/demote a task\'s priority (critical|urgent|high|normal|low or 1..=5)'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "edit" -d 'Edit task fields'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "reopen" -d 'Reopen a completed/dismissed task (status → pending)'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "show" -d 'Show one task\'s full row + side-table detail'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "dismiss" -d 'Dismiss a task (soft close, status → dismissed; reversible via reopen)'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "rm" -d 'Delete a task permanently (hard delete + tombstone)'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "next" -d 'Show ready-to-start tasks (all dependencies done)'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "view" -d 'Manage saved views'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "tui" -d 'Launch the terminal UI (ratatui)'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "serve" -d 'Run the HTTP server (sync API, capture, webhooks, metrics)'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "bot" -d 'Run the Telegram bot (Bot API long-poll)'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "branch" -d 'Print a Linear-style branch name for a task'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "distill" -d 'Run the distillation pipeline (Python subprocess shim until v0.9)'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "accountability" -d 'Run one accountability cycle (escalation + Telegram/email)'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "scoring" -d 'Recompute composite priority scores for all active tasks'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "remote" -d 'Talk to a remote canonical `pt serve` (no local DB)'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "log" -d 'Show a task\'s attributed event history (who did what, via which surface)'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "undo" -d 'Reverse the most recent undoable mutation (done/dismiss/create)'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "token" -d 'Manage named scoped API tokens (create/list/revoke)'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "backfill" -d 'One-shot backfill PT-N for any tasks lacking one'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "gen-manpage" -d 'Generate the `pt(1)` manpage to stdout'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "gen-completions" -d 'Generate shell completions (bash/zsh/fish) to stdout'
complete -c pt -n "__fish_pt_using_subcommand help; and not __fish_seen_subcommand_from add list done priority edit reopen show dismiss rm next view tui serve bot branch distill accountability scoring remote log undo token backfill gen-manpage gen-completions help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c pt -n "__fish_pt_using_subcommand help; and __fish_seen_subcommand_from view" -f -a "save" -d 'Save a filter DSL string under a name'
complete -c pt -n "__fish_pt_using_subcommand help; and __fish_seen_subcommand_from view" -f -a "list" -d 'List saved views'
complete -c pt -n "__fish_pt_using_subcommand help; and __fish_seen_subcommand_from view" -f -a "show" -d 'Run a saved view\'s filter and print matching tasks'
complete -c pt -n "__fish_pt_using_subcommand help; and __fish_seen_subcommand_from view" -f -a "rm" -d 'Delete a saved view'
complete -c pt -n "__fish_pt_using_subcommand help; and __fish_seen_subcommand_from accountability" -f -a "run" -d 'Run the state machine + dispatch once'
complete -c pt -n "__fish_pt_using_subcommand help; and __fish_seen_subcommand_from scoring" -f -a "run" -d 'Recompute the four score_* columns + priority_score for every task with status NOT IN (\'done\', \'dismissed\')'
complete -c pt -n "__fish_pt_using_subcommand help; and __fish_seen_subcommand_from remote" -f -a "add" -d '`pt remote add "..."` — create a task on the canonical host without opening a local DB. Uses PTASK_SYNC_URL (default http://100.121.42.54:9501)'
complete -c pt -n "__fish_pt_using_subcommand help; and __fish_seen_subcommand_from remote" -f -a "list" -d '`pt remote list` — fetch the live task set from the canonical host'
complete -c pt -n "__fish_pt_using_subcommand help; and __fish_seen_subcommand_from remote" -f -a "done" -d '`pt remote done <query>` — mark a task done by PT-N or title substring'
complete -c pt -n "__fish_pt_using_subcommand help; and __fish_seen_subcommand_from remote" -f -a "priority" -d '`pt remote priority <query> <level>` — set priority on the canonical host'
complete -c pt -n "__fish_pt_using_subcommand help; and __fish_seen_subcommand_from remote" -f -a "edit" -d '`pt remote edit <query> --deadline <iso> | --clear-deadline`'
complete -c pt -n "__fish_pt_using_subcommand help; and __fish_seen_subcommand_from remote" -f -a "reopen" -d '`pt remote reopen <query>` — flip a done/dismissed task back to pending'
complete -c pt -n "__fish_pt_using_subcommand help; and __fish_seen_subcommand_from remote" -f -a "show" -d '`pt remote show <query>` — print one task\'s full row + detail (read-only)'
complete -c pt -n "__fish_pt_using_subcommand help; and __fish_seen_subcommand_from remote" -f -a "next" -d '`pt remote next [-n N]` — DAG-ready tasks from the canonical host'
complete -c pt -n "__fish_pt_using_subcommand help; and __fish_seen_subcommand_from remote" -f -a "dismiss" -d '`pt remote dismiss <query>` — soft-close a task (reversible via reopen)'
complete -c pt -n "__fish_pt_using_subcommand help; and __fish_seen_subcommand_from remote" -f -a "version" -d '`pt remote version` — compare this client\'s version against the canonical server\'s `GET /version`. Exits non-zero on skew'
complete -c pt -n "__fish_pt_using_subcommand help; and __fish_seen_subcommand_from token" -f -a "create" -d 'Mint a token for a client. Prints the plain token ONCE — store it with the consumer; only its hash is kept'
complete -c pt -n "__fish_pt_using_subcommand help; and __fish_seen_subcommand_from token" -f -a "list" -d 'List all tokens (client, scope, created/last-used/revoked)'
complete -c pt -n "__fish_pt_using_subcommand help; and __fish_seen_subcommand_from token" -f -a "revoke" -d 'Revoke every active token for a client id'
