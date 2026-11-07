---
description: Plan a multi-ticket run, have the human confirm it, then steer development, code review and test plan with subagents until the goal is reached — ticket branches stacked, review rounds bounded, humans review the result together.
argument-hint: <goal, e.g. "develop all stories in Refinement with an implementation plan until they have a test plan">
---

# Workflow: orchestrate

Read `dispatch/invariants.md` first. `<ARGS>` is supplied by the invoking agent stub.
Tracker: none. Chat: none.

Steers tickets through `develop → code review (bounded) → test plan` until the goal in `<ARGS>` is reached. It runs in two phases: **plan** (read-only, ends with the human's confirmation) and **run**. **This session is the orchestrator only:** it plans, selects tickets, cuts branches, launches subagents, reads their results and the ticket frontmatter, asks the human, and decides the next step. It never edits code, tests or tickets itself, and it never reviews.

| Role | Agent | Instructions | Model / effort |
|---|---|---|---|
| Development, fix rounds, test plan | `dispatch-developer` | `dispatch/agents/developer.md` | Claude Code: `.claude/agents/dispatch-developer.md`; Codex: `.codex/agents/dispatch-developer.toml` |
| Code review | `dispatch-reviewer` | `dispatch/agents/reviewer.md` | Claude Code: `.claude/agents/dispatch-reviewer.md`; Codex: `.codex/agents/dispatch-reviewer.toml` (stronger model or more effort than the developer) |

Every launch is a new subagent, never a continued one, so the reviewer is never the author and each fix round is a different session from the review it answers, as `develop.md` and `code-review.md` require. Launch the named custom agent, not a generic one: the agent's own instructions and model come from its definition, and the brief carries only the ticket-specific facts the agent file lists.

### Tool notes

The steps below are tool-neutral. Where they need a tool-specific mechanism:

| Step | Claude Code | Codex |
|---|---|---|
| Ask the human a discrete question | `AskUserQuestion` | ask in the conversation, numbered options with the recommendation first, and end the turn |
| Run the session in the run worktree | `EnterWorktree` with `path` | Codex cannot switch the session's directory: start the session in the run worktree (`codex -C <run worktree>`) for the run phase, or, if it already runs elsewhere, prefix every command with `cd <run worktree> &&` and pass the path in every brief |
| Launch a subagent | `Agent` with `subagent_type` | spawn the custom agent by name |
| Wait for it | the completion notification wakes you; do not poll | wait for the agent's result before the next step |

Subagents inherit the session's permissions. A run with frequent approval prompts stalls; say in the plan which permission mode the run needs.

## Bounds

The run cannot loop. Per ticket it launches at most **1 development + `max_reviews` reviews + (`max_reviews` − 1) fix rounds + 1 test plan** agents; with the default `max_reviews: 3` that is 7. Only the human can add to it, at most one extra fix round and re-check per ticket, or one reviewer launch to record a ruling (see *Asking the human*). Inside that budget the loop also stops early:

- **Blocking bar.** The reviewer may count a finding as blocking only on one of four grounds, and must name it: a numbered acceptance criterion that fails (`AC <n>`), an invariant or linked ADR that is broken (`invariant` / `ADR-<n>`), a defect inside this ticket's change with a concrete failure scenario a user would hit (`defect`), or red gates (`gates`). Everything else — hardening, style, extra tests the criteria do not demand, doc drift, pre-existing issues outside `<base>..HEAD` — is non-blocking and goes to the ticket's follow-ups. When in doubt, non-blocking.
- **Narrow re-checks.** Rounds after the first verify the earlier blocking findings and read only the fix diff. A new blocking finding is allowed only when the fix itself introduced it.
- **Narrow fixes.** A fix round fixes the blocking findings and nothing else, so it does not hand the next round new surface to review.
- **Progress or stop.** If a re-check does not lower the blocking count, the loop stops: another round would not converge.

When the loop stops with blocking findings left, the human decides (see *Asking the human*); the orchestrator never lowers a counter or skips the gate.

## Phase 1 — Plan and confirm

Read-only. Before the human confirms, create no worktree, branch or commit, launch no subagent, and edit no ticket.

1. Confirm today's date with `date`.
2. **Interpret the goal.** Turn `<ARGS>` into a **selection** and a **terminal stage**. The default reading, and the example this workflow was built for: *"develop all stories in Refinement with an implementation plan until they have a test plan"* selects tickets with `type: story`, `status: "Refinement"`, `open_questions: 0` and a filled `## Implementation plan` (more than a `<!-- GUIDE -->` placeholder), and runs each to a written test plan (`open_tests` set, `frozen` stamped, status `Review`). A goal may also name ticket ids, other types or an earlier terminal stage (`until reviewed`). A goal that needs a stage this workflow does not run (refinement, planning, release) is out of scope: say so and stop.
3. The confirmed plan is the authorization `/develop` needs to start each selected ticket; it waives no gate. A ticket that matches the type and status but fails a precondition (open questions, no plan) is **skipped and reported**, never worked around.
4. **Order** the selection the way the board does: by `rank` ascending (the lowest number is the top card), unranked tickets after them by title.
5. **Read enough to see the risks**: each selected ticket's scope, acceptance criteria, implementation plan (files it touches, size estimate) and links; `git log` / `git branch` for unmerged branches that touch the same files; the first base (step 11). Read plans, not code; this is a briefing, not a review.
6. **Write the plan** in chat, brief — one screen:
   - **Goal as understood:** the selection rule and the terminal stage, in one or two lines.
   - **Tickets in order:** id, title, rank, branch name, size from its plan; then the skipped tickets with the reason.
   - **Stack:** `<first base> ← <branch 1> ← <branch 2> …`, and why this order (rank, or a dependency that overrides it).
   - **Settings:** `max_reviews` (default 3), mode `attended` (default: ask the human at decision points) or `unattended` (mark blocked and carry on), the agent tool and the models per role, the permission mode the run needs, and the agent budget from *Bounds* (per ticket and in total).
   - **Caveats and risks**, concrete and specific to this run, each with its consequence and mitigation. Look at least for: tickets whose plans touch the same files; a ticket that depends on one outside the run or later in the stack; unmerged branches (including the human's own work in progress) that touch the same code and will conflict at merge time; tickets needing a human or the running app to verify (the agents cannot judge visuals — that lands in the test plan); plans that are stale against the code; very large plans; long Windows test times and a cold `target/` in the new worktree; that ticket edits land in the live, synchronized vault immediately; and that a blocked ticket mid-stack holds up the merge of every branch above it.
   - **Questions**, if any: only what the plan genuinely depends on.
7. **Ask, then wait.** Put discrete questions to the human (see *Tool notes*; self-contained, options with a recommendation first); open-ended ones in prose. Then end the turn and wait for an explicit confirmation of the plan. Changes requested by the human are folded into the plan and shown again; the run starts only on a clear "go" for the plan as last shown.

## Asking the human

In `attended` mode the orchestrator stops and asks — naming the ticket and the finding or question so the dialog stands on its own — when:

- a reviewer reports `decision_needed: yes`, or a developer reports `blocked` on a question;
- the review loop stops with blocking findings left (cap reached or no progress). Options: *one more fix round and re-check* (at most once per ticket), *accept named findings as non-blocking* (a human ruling), *leave the ticket blocked and continue*, *stop the run*;
- the repository or a ticket is in a state the plan did not foresee (diverged base, unexpected commits, a ticket changed by someone else meanwhile).

The answer is passed in the next agent's brief as **Human ruling** with the human's words and the date; that agent records it in the ticket (developer: as-built entry; reviewer: the review entry). A ruling that downgrades findings is applied by a `dispatch-reviewer` launch, the only role that writes `open_findings`, scoped to recording the ruling and re-checking nothing else. In `unattended` mode the same situations mark the ticket blocked with the reason, and the run continues with the next ticket.

## Phase 2 — Run

### Prepare the run worktree

The run never commits in the checkout the human works in.

8. Create a dedicated worktree beside the repository, detached at the first base: `git worktree add --detach ../<repo>-run-<YYYYMMDD> <base>`. If it exists from an interrupted run, reuse it (see *Resume*).
9. `dispatch/wiki` is a git-ignored junction to the vault, so a new worktree lacks it. Create the same link in the run worktree, pointing at the target the main checkout's link resolves to (Windows: `cmd //c mklink /J dispatch\wiki <target>`; elsewhere `ln -s`).
10. Move this session into the run worktree (see *Tool notes*), so every subagent works there. Pass its absolute path in every brief as well.
11. **First base:** the default branch, fresh. `git fetch origin`, then use `main` if `origin/main` is its ancestor, `origin/main` if `main` is its ancestor, and ask if they diverged. A confirmed plan that names a starting branch overrides this. If the base moved since the plan was confirmed, say so before cutting the first branch.

### Per ticket, in order

Keep a run table in your context: ticket, branch, base, review rounds, blocking counts per round, head sha, outcome.

12. **Cut the branch.** `git switch -c <id lowercase>-<short-slug> <base>`. The base is the previous ticket's branch, or the first base for the first ticket. Each branch builds on the last, so the stack merges without conflicts in order. A previous ticket that ended blocked still serves as the base; its state is in the report.
13. **Develop.** Launch `dispatch-developer` with task `develop`, the ticket, worktree, branch and base. Wait for its result.
    - `outcome: blocked`, or `<base>..HEAD` has no commits: ask (attended) or mark **blocked at development** (unattended). A ticket that stays blocked adds nothing to the stack; delete its branch if it has no commits and continue on the same base.
14. **Review loop**, `round` from 1 to `max_reviews`:
    1. Launch `dispatch-reviewer` with the ticket, worktree, branch, base, round and — from round 2 — the commit the previous review saw, so it can scope itself to the fix diff. Wait.
    2. Read `open_findings` from the ticket frontmatter yourself; the ticket is authoritative over the RESULT block. If they disagree, trust the ticket and note it.
    3. `open_findings: 0` → the review is complete; go to step 15.
    4. `decision_needed: yes` → stop the loop and ask (see *Asking the human*).
    5. From round 2: `open_findings` not lower than the previous round's → stop the loop (**no progress**) and ask.
    6. `open_findings` above `0` and `round < max_reviews` → launch `dispatch-developer` with task `fix-findings`. Wait. A `blocked` report stops the loop and asks. Otherwise increment `round` and repeat.
    7. `open_findings` above `0` after the last round → stop (**review cap reached**) and ask.
    A ticket left with blocking findings stays in `In progress` with the review entries as its record, and gets no test plan.
15. **Test plan**, only with `open_findings: 0` and when the terminal stage includes it: launch `dispatch-developer` with task `test-plan`. Wait. Confirm in the ticket that `open_tests` is set, `frozen` is stamped and the status is `Review`; otherwise ask or mark **blocked at test plan**.
16. Record the outcome, the final head sha and the rounds in the run table, and post one line of progress (`US00006 → Review, 2 rounds, 5 tests`). The next ticket's base is this branch.

Launch one subagent at a time and wait for it: every step depends on the previous step's commits and ticket state. Never act on a result you have not received.

## Finish

17. The goal is reached when every selected ticket has either reached the terminal stage or ended blocked with a reason. Do not push, merge, open pull requests or move cards: the humans review the stack together.
18. Report:
    - the run worktree path and the **stack in merge order**: `main ← <branch 1> ← <branch 2> …`, each with its ticket, outcome, review rounds with blocking counts, head sha and `open_tests`;
    - every blocked ticket with the blocking findings or the decision it needs, and which later branches sit on top of it;
    - the human rulings made during the run, skipped tickets and why, and which planned risks materialized.
    Merging happens after human review, bottom of the stack first, so each merge is a fast-forward of the reviewed commits. A fix to a lower branch after review means rebasing the branches above it; say which.
19. Leave the run worktree in place for the review. Remove it (`git worktree remove`) only when the human asks; first remove the `dispatch/wiki` junction with `cmd //c rmdir dispatch\wiki` (or `rm` of the symlink), never by deleting its contents: the junction points at the real vault.

## Resume

An interrupted run is resumable from the tickets and git alone; there is no separate state file. Show the human the re-derived state as a short plan and get confirmation before continuing (Phase 1, steps 6–7). For each ticket in order: an existing branch with commits above its base and status `In progress` resumes at the review loop (the `## Code review` entries whose heading says `stacked on` are the used rounds, and if `open_findings` is empty after a fix round, the next step is a re-check); `open_findings: 0` without a test plan resumes at step 15; a ticket in `Review` with a `frozen` stamp is done. Continue the stack from the last existing branch.

## Never

- Never start the run without the human's confirmation of the plan as last shown.
- Never let the orchestrator write code or tickets, run the gates in place of an agent, or review.
- Never skip a gate to keep the run moving, write `open_findings` or `open_tests` yourself, or move a card a workflow does not move.
- Never exceed the bounds; another round needs the human's explicit ruling.
- Never commit in the human's checkout, push, force-push, or rewrite a branch another ticket of the run is stacked on.
