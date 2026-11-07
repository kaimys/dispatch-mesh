---
description: Steer development, code review and test plan across several tickets with subagents until a stated goal is reached — ticket branches stacked, review loop capped at three rounds, humans review the result together.
argument-hint: <goal, e.g. "develop all stories in Refinement with an implementation plan until they have a test plan">
---

# Workflow: orchestrate

Read `dispatch/invariants.md` first. `<ARGS>` is supplied by the invoking agent stub.
Tracker: none. Chat: none.

Steers tickets through `develop → code review (≤ 3 rounds) → test plan` until the goal in `<ARGS>` is reached. **This session is the orchestrator only:** it selects tickets, cuts branches, launches subagents, reads their results and the ticket frontmatter, and decides the next step. It never edits code, tests or tickets itself, and it never reviews.

| Role | Agent | Fresh context | Model / effort |
|---|---|---|---|
| Development, fix rounds, test plan | `dispatch-developer` | every launch | as in `.claude/agents/dispatch-developer.md` |
| Code review | `dispatch-reviewer` | every launch | as in `.claude/agents/dispatch-reviewer.md` (stronger model, more effort) |

Every launch is a new subagent, so the reviewer is never the author and each fix round is a different session from the review it answers, as `develop.md` and `code-review.md` require.

## 1. Understand the goal

1. Confirm today's date with `date`.
2. Turn `<ARGS>` into a **selection** and a **terminal stage**. The default reading, and the example this workflow was built for: *"develop all stories in Refinement with an implementation plan until they have a test plan"* selects tickets with `type: story`, `status: "Refinement"`, `open_questions: 0` and a filled `## Implementation plan` (more than a `<!-- GUIDE -->` placeholder), and runs each to a written test plan (`open_tests` set, `frozen` stamped, status `Review`). A goal may also name ticket ids, other types or an earlier terminal stage (`until reviewed`). A goal that needs a stage this workflow does not run (refinement, planning, release) is out of scope: say so and stop.
3. The human's goal is the authorization `/develop` needs to start each selected ticket; it does not waive any gate. A ticket that matches the type and status but fails a precondition (open questions, no plan) is **skipped and reported**, never worked around.
4. **Order** the selection the way the board does: by `rank` ascending (the lowest number is the top card), unranked tickets after them by title.
5. Show the ordered list, the skipped tickets with the reason, and the base of the first branch, then proceed. Ask only when the goal is genuinely ambiguous.

## 2. Prepare the run worktree

The run never commits in the checkout the human works in.

6. Create a dedicated worktree beside the repository, detached at the first base: `git worktree add --detach ../<repo>-run-<YYYYMMDD> <base>`. If it exists from an interrupted run, reuse it (see *Resume*).
   - **First base:** the default branch, fresh. `git fetch origin`, then use `main` if `origin/main` is its ancestor, `origin/main` if `main` is its ancestor, and stop and ask if they diverged. A goal that names a starting branch overrides this.
7. `dispatch/wiki` is a git-ignored junction to the vault, so a new worktree lacks it. Create the same link in the run worktree, pointing at the target the main checkout's link resolves to (Windows: `cmd //c mklink /J dispatch\wiki <target>`; elsewhere `ln -s`). The vault is shared, so ticket edits land in the real tickets, which is where the humans review them.
8. Switch this session into the run worktree (`EnterWorktree` with `path`), so every subagent works there. Pass its absolute path in every brief as well.

## 3. Per ticket, in order

Keep a run table in your context: ticket, branch, base, review rounds, head sha, outcome.

9. **Cut the branch.** `git switch -c <id lowercase>-<short-slug> <base>`. The base is the previous ticket's branch, or the first base for the first ticket. Stacking is the point: each branch builds on the last, so the stack merges without conflicts in order. A previous ticket that ended blocked still serves as the base; its state is in the report.
10. **Develop.** Launch `dispatch-developer` with task `develop`, the ticket, worktree, branch and base. Wait for its notification.
    - `outcome: blocked`, or `<base>..HEAD` has no commits: mark the ticket **blocked at development**, note why, and continue with the next ticket on the same base (the empty branch adds nothing to the stack; delete it if it has no commits).
11. **Review loop**, `round` from 1 to 3:
    1. Launch `dispatch-reviewer` with the ticket, worktree, branch, base and round. Wait.
    2. Read `open_findings` from the ticket frontmatter yourself; the ticket is authoritative over the RESULT block. If they disagree, trust the ticket and note it.
    3. `open_findings: 0` → the review is complete; go to step 12.
    4. `decision_needed: yes` → stop the loop: a finding needs a human decision, and another round would guess. Mark **blocked at review (decision)**.
    5. `open_findings` above `0` and `round < 3` → launch `dispatch-developer` with task `fix-findings`. Wait. If it reports `blocked`, mark **blocked at fix** and stop the loop. Otherwise increment `round` and repeat.
    6. `open_findings` above `0` after round 3 → stop: **blocked after 3 reviews**. The ticket stays in `In progress` with the review entries as its record. Do not run the test plan.
12. **Test plan**, only with `open_findings: 0` and when the terminal stage includes it: launch `dispatch-developer` with task `test-plan`. Wait. Confirm in the ticket that `open_tests` is set, `frozen` is stamped and the status is `Review`; otherwise mark **blocked at test plan** with the agent's note.
13. Record the outcome, the final head sha and the round count in the run table. The next ticket's base is this branch.

Launch one subagent at a time and wait for it: every step depends on the previous step's commits and ticket state. Do not poll; the completion notification wakes you. Never act on a result you have not received.

## 4. Finish

14. The goal is reached when every selected ticket has either reached the terminal stage or ended blocked with a reason. Do not push, merge, open pull requests or move cards: the humans review the stack together.
15. Report:
    - the run worktree path and the **stack in merge order**: `main ← <branch 1> ← <branch 2> …`, each with its ticket, outcome, rounds, head sha and `open_tests`;
    - every blocked ticket with the blocking findings or the decision it needs, and which later branches sit on top of it;
    - skipped tickets and why.
    Merging happens after human review, bottom of the stack first, so each merge is a fast-forward of the reviewed commits. A fix to a lower branch after review means rebasing the branches above it; say which.
16. Leave the run worktree in place for the review. Remove it (`git worktree remove`) only when the human asks; first remove the `dispatch/wiki` junction with `cmd //c rmdir dispatch\wiki` (or `rm` of the symlink), never by deleting its contents: the junction points at the real vault.

## Resume

An interrupted run is resumable from the tickets and git alone; there is no separate state file. Re-derive the selection, then for each ticket in order: an existing branch with commits above its base and status `In progress` resumes at the review loop (count the `## Code review` entries whose heading says `stacked on` as used rounds, and if `open_findings` is empty after a fix round, the next step is a re-check); `open_findings: 0` without a test plan resumes at step 12; a ticket in `Review` with a `frozen` stamp is done. Continue the stack from the last existing branch.

## Never

- Never let the orchestrator write code or tickets, run the gates in place of an agent, or review.
- Never skip a gate to keep the run moving, write `open_findings` or `open_tests` yourself, or move a card a workflow does not move.
- Never commit in the human's checkout, push, force-push, or rewrite a branch another ticket of the run is stacked on.
