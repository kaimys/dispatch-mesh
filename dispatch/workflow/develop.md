---
description: Implement a ticket — precondition checks, status move, code with tests, gates green, then hand off to /code-review in a fresh session.
argument-hint: <ticket id>
---

# Workflow: develop

Read `dispatch/invariants.md` first. `<ARGS>` is supplied by the invoking agent stub.
Tracker: none. Chat: none. If either is `none`, skip only that integration's lookups, writes and missing-side preconditions; continue the wiki work. With no chat, record questions for the requester in the ticket. A configured but unavailable integration is an error to report, not `none`.

Runs development for one ticket. `<ARGS>` is the ticket id.

## Start

1. Resolve the ticket (spec via `id: <ARGS>` in `dispatch/wiki/05_Requirements/Tickets`, task in none) and name the session `<ARGS> <short name>`.
2. **Pull the default branch first**, then branch off the freshly pulled tip. A stale base forces an avoidable merge later, and on a shared repo it happens within a day.
3. **Preconditions** — expected status is `Refinement` or the human-approved `Refinement` queue, acceptance criteria are final, `open_questions: 0`, and for anything non-trivial an approved `## Implementation plan` exists. If something is missing, say so and suggest `/refine` or `/implementation-plan`. **Do not code around a missing decision** — that is how a guess becomes a shipped behaviour nobody chose.
4. The explicit development invocation authorizes the move after those checks. Move to `In progress` in both systems, set `assignee:` to whoever is doing the work, and strip leftover `<!-- GUIDE: … -->` from sections you finalise.

## Implement

5. **Read before writing** — the affected code, the plan, the linked wiki pages. Make the smallest change that fully solves the ticket, and match the surrounding code's style, naming and structure rather than importing your own.
6. Follow the plan. If reality contradicts it, stop and update the plan in the ticket *first*, saying what changed — a plan silently abandoned mid-implementation leaves the review with no baseline.
7. **Tests are part of the change, not a follow-up.** Every new unit of behaviour gets a test at the layer that can assert it cheaply. Never weaken an existing assertion to make a change pass; if an assertion is wrong, that is its own visible decision.
8. Commit with the ticket reference so the release note can be generated from history: `<type>(<scope>): <what> (<ARGS>)`.

## Finish

9. **Gates, all green — no exceptions and no "unrelated failure" without evidence:**
   ```
   cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
   ```
   For user-facing changes, also run the app and confirm the affected surface actually behaves as the acceptance criteria say. A green suite is not a demonstration that the feature works.
10. **Update the wiki pages this change made wrong.** The docs go stale in exactly this step, every time.
11. **If this session fixed blocking findings (`open_findings` above `0`), clear `open_findings` — to empty, not to `0`.** The review is not finished until its re-check confirms the fixes, and only `/code-review` writes the `0`. Once `open_findings` is `0`, leave it: the story has had its one code review (see `dispatch/invariants.md`, *One code review and one test plan per story*), and later fixes do not reset it.
12. When code-complete and green, the next step is **`/code-review <ARGS>`** — and **not from this session.** The reviewer is not the author: the context that wrote the code is the one that will defend it, so recommend the review rather than running it here. The card's **Code review** chip is a fresh session by construction. A clean review then hands off to **`/test-plan <ARGS>`**, which writes the manual plan, sets `open_tests`, stamps the freeze and performs the move to `Review`. Do not make that move by hand; the gate belongs to that command.
