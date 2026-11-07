# Claude Code instructions

Read `AGENTS.md` for the repository guide, product invariants and verification commands. It applies to Claude Code as written.

For ticket work (any `/refine`, `/develop`, `/code-review`, … command), read `dispatch/invariants.md` first. The commands in `.claude/commands/` are stubs that load the canonical workflows from `dispatch/workflow/`. `/orchestrate <goal>` steers several tickets through develop, code review and test plan with the subagents `dispatch-developer` and `dispatch-reviewer`; `.claude/agents/` holds their stubs (model, effort), `dispatch/agents/` their canonical instructions.

Claude-specific: `.claude/settings.json` wires the Dispatch run-lifecycle hooks (`dispatch/scripts/run-state.mjs`). They do nothing outside a Dispatch chip launch.
