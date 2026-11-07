# Claude Code instructions

Read `AGENTS.md` for the repository guide, product invariants and verification commands. It applies to Claude Code as written.

For ticket work (any `/refine`, `/develop`, `/code-review`, … command), read `dispatch/invariants.md` first. The commands in `.claude/commands/` are stubs that load the canonical workflows from `dispatch/workflow/`. `/orchestrate <goal>` steers several tickets through develop, code review and test plan with the subagents in `.claude/agents/` (`dispatch-developer`, `dispatch-reviewer`); it is Claude-only, since it relies on Claude Code subagents.

Claude-specific: `.claude/settings.json` wires the Dispatch run-lifecycle hooks (`dispatch/scripts/run-state.mjs`). They do nothing outside a Dispatch chip launch.
