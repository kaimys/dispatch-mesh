# Dispatch workflow invariants

Every workflow in `dispatch/workflow/` and every agent working on Dispatch Mesh tickets follows these rules. They live here once; `CLAUDE.md` and `AGENTS.md` only point at this file. If an agent file and this file disagree, this file wins.

Board vocabulary: `Backlog` → `Refinement` → `In progress` → `Review` → `Done` → `Released`, plus `Rejected`. Ticket notes live in `dispatch/wiki/05_Requirements/Tickets/`; `dispatch/wiki` is a git-ignored path to the vault in each checkout.

## Source of truth

- The wiki is the only record of ticket state. There is no issue tracker and no team chat integration; questions for the requester are recorded in the ticket.
- Precedence when documents conflict: ADRs in `07_Engineering/Decisions/` (accepted and proposed both bind; only superseded decisions may be ignored) → `02_Product/Dispatch Mesh requirements and architecture.md` → `02_Product/Dispatch Mesh milestone plan.md` → the ticket → discovery notes. The product behavior in `AGENTS.md` is derived from these and must be kept consistent with them.
- UI work (tray, status window, any desktop-app view) follows `06_UX/Dispatch Mesh desktop app — design spec.md`: tokens, glyphs, components and per-view rules. Read that page, not the PDF or HTML export, which yield no text. Where the spec and a frame PNG disagree, the spec wins.

## The ticket freeze

- When a ticket leaves `In progress` (code now exists that depends on it), its **contract zone** is read-only: summary, user story / symptom, context, scope, out of scope, open questions and answers, acceptance criteria, and implementation plan. Stamp `frozen: <date>`.
- After the freeze, new information goes into the **record zone** (test plan, as-built notes, follow-ups) as dated entries. Annotate a wrong frozen statement beneath it with `> ⚠️ Correction <date>: …` instead of rewriting it. New scope becomes a new linked ticket.
- Why: if a spec can change after the code was built against it, a later spec↔code mismatch has two explanations and no way to tell them apart.

## Ownership and maintenance

- Every page carries `owner:`, a **person** with a page in `00_Start-Here/Team/`, never a team.
- Every derived page also carries `derived_from:` and `maintained_by:`. A workflow that creates a derived page must register its refresh; if no recurring job owns it, the workflow may not create it.
- Every wiki change gets an entry in `00_Start-Here/log.md` (`## [YYYY-MM-DD] <type> | <title>`), and new pages are listed in `00_Start-Here/index.md`.
- `01_Sources/` is immutable: read sources and write new pages from them; never edit or delete a source.

## Artifacts and review surface

Each workflow leaves its durable result in the ticket note. The note is where a human reviews it, not the chat transcript.

## Gates are gates

- `open_questions: 0` before leaving `Refinement`.
- `open_findings: 0` before the test plan is written. The code review runs before the freeze.
- `open_tests: 0` before leaving `Review`.
- No workflow moves a ticket across a gated boundary on its own. A human ends refinement and signs off the implementation plan.
- Mechanical gates are `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`, matching CI.

## Unset is not zero

An empty counter means *nobody has counted*, and it renders no badge. `0` means *counted, and clear*. New tickets leave `open_tests:` and `open_findings:` empty. Only the workflow that actually counted may write a number, and reviewers lower `open_tests` as they tick test-plan items.

## One code review and one test plan per story

- **One code review per story.** It ends when `open_findings` reaches `0`. Fixing blocking findings belongs to that review: the fix round clears `open_findings` to empty, and the re-check of those fixes writes the `0`. Once `open_findings` is `0`, it stays `0`; later code changes, including fixes for non-blocking findings or for failed tests, do not reset it or call for another review.
- **One test plan per story.** `/test-plan` runs once; it writes the plan, sets `open_tests` and stamps the freeze. When a check fails, fix the cause, record the failure and the fix as dated entries (test-plan results and as-built notes), run the check again on the fixed build, and tick it once it passes. When a human works through the plan, run all its checks first and collect the failures; fix them afterwards. A fix never blocks the human mid-plan: it runs in the background only when it cannot change the build under test. The fix resets no counter, no status changes, and the ticket stays in `Review`; a second test plan is not written.
- **No screen-reader tests.** Manual test plans contain no Narrator or other screen-reader checks (Kai, 2026-11-07). Accessible names, screen-reader-only text and keyboard order are still built and reviewed; they are not part of the manual pass.

## Direct status writes

Board automations fire only on drag and drop, never on frontmatter an agent writes. A workflow that writes `status: "Done"` itself must also stamp `completed: <date>`. It must never overwrite an existing `completed` date. `Released` does not restamp completion.

## Explicit small-bug exception

`/fix-bug` may omit independent code review only for a known, small-blast-radius fix, and only when the requester explicitly asks for the shortcut. Create the ticket before writing code, run the mechanical gates, verify the reproduction, and record the actual results and the omitted review. Leave `open_findings` empty. Complete only with no open questions or manual checks and a known `version_target`; then freeze the finalized contract and stamp `completed`. Stop and warn when the work grows, verification fails, or a human check is needed. Never run `/test-plan` with its review gate unmet, and never treat this as a general review toggle.

## Release policy

Recorded steps, in order:

1. Every in-scope ticket is ready (see `dispatch/workflow/release.md`).
2. Run the mechanical gates, plus the platform-specific validation the milestone plan's *Release discipline* section requires. New storage or transport formats need migration and compatibility tests.
3. Bump `version` in `Cargo.toml` and keep `Cargo.lock` consistent; update `README.md` when supported behavior or commands change.
4. `cargo build --release`, then write the release note in `08_Delivery-and-QA/Releases/`.
5. Packaging into `dist/` (previous builds: `dist/dispatch-mesh-<version>-windows-x64.zip`), git tags, and any publication are **not yet recorded policy**. Stop and ask the requester before packaging, tagging, pushing, or publishing.

Commit messages state the delivered behavior. Never force-push or rewrite shared history.
