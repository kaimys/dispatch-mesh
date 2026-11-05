---
description: Ship a version — test pass, bump, release note generated from the board, tag, promote the shipped tickets, announce.
argument-hint: [version]
---

# Workflow: release

Read `dispatch/invariants.md` first. `<ARGS>` is supplied by the invoking agent stub.
Tracker: none. Chat: none. If either is `none`, skip only that integration's lookups, writes and missing-side preconditions; continue the wiki work. With no chat, record questions for the requester in the ticket. A configured but unavailable integration is an error to report, not `none`.

Releases one version. `<ARGS>` is the target version (e.g. `v1.4.0`); without it, use the next planned version from the Release Plan.

**The project's recorded release order is load-bearing.** Follow its documented build, migration, promotion and publication sequence, preserving the reasons it gives. If the project has no recorded release policy, stop before changing versions, production, tags or publications and ask the requester; never invent backend, mirror or deployment steps for a project that did not name them.

## Scope the release

1. **The release scope is what the board says it is:** every ticket whose `version_target` matches, in `dispatch/wiki/05_Requirements/Tickets`. Anything shipped without a ticket is invisible here — which is the practical argument for no-ticket-no-merge.
2. Verify each in-scope ticket is actually ready: at `Review` or beyond with `open_tests: 0` and `open_findings` empty or `0` — empty is tolerated here, unlike in `/test-plan`, for legacy tickets or an explicitly recorded `/fix-bug` shortcut that omitted review; it never means reviewed. Include already-completed shortcut tickets in the release scope, verify their recorded results and target version, and do not require a second trip through review. **List the ones that aren't and stop.** Either they get finished, or they get moved to the next version — both are the user's call, not yours.
3. Confirm today's date with `date`.

## Prove it

4. Full test pass on a **non-production** environment: the automated gates
   ```
   cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
   ```
   plus the manual plans of the in-scope tickets if they haven't been signed off yet.
5. Fix-or-defer any failure explicitly. A release note that lists a ticket whose tests never passed is worse than a delayed release.

## Cut it

6. Bump the version in the project's manifest(s), consistently — one source of truth, everything else derived.
7. **Write the release note** `dispatch/wiki/08_Delivery-and-QA/Releases/<version>.md` from `dispatch/wiki/00_Start-Here/Templates/release-note.md`: `version:` and `date:` frontmatter exactly as the template shows — **the Release Plan tab parses these**, and a malformed one silently drops the release from the board. Contents: what shipped as **links to the tickets** (they carry the detail — don't restate it), build metadata, and what deliberately did not make it.
8. Build, tag and publish per the project's process.
9. **Write the GitHub release text** `dispatch/wiki/08_Delivery-and-QA/Releases/GitHub/<version> GitHub release.md`: a paste-ready body for the GitHub release of the tag, written for users rather than for the board.
   - **Frontmatter:** only `owner:` and `release: "[[<version>]]"`. Never add `version:` or `date:`: the Release Plan would count the page as a second release.
   - **Body:**
     - one-line summary;
     - *What's new*, grouped by ticket;
     - *Breaking changes*, when there are any;
     - *Upgrading*: migration and compatibility;
     - *Not in this release*;
     - *Download*: package contents and SHA-256 of each artifact;
     - the build line: tag, commit, toolchain, CI.

     Behavior is described in user terms. Unlike the release note, this text summarizes, because GitHub readers cannot follow wikilinks.
   - **Links are absolute URLs into the public wiki repository**, `https://github.com/kaimys/dispatch-mesh-llm-wiki/blob/main/<vault path>`. The vault path is the page's path relative to `dispatch/wiki`, URL-encoded: spaces become `%20`, and the em dash in ADR titles becomes `%E2%80%94`. Link the shipped and deferred tickets, the governing ADRs and the milestone section. Never use wikilinks.
   - **Verify every link before handing the text over:** the file must exist in that repository. Use the directory listing's `html_url` from `gh api repos/kaimys/dispatch-mesh-llm-wiki/contents/<dir>`. A page the vault has not yet synchronized there (typically this release's own note) is left out and named in the report, not linked to a 404.
   - Producing the text publishes nothing. Creating the GitHub release object or uploading assets is publication under the release policy: ask first.

## Land it

10. **Promote the shipped tickets:** write status → `Released`, then mirror the tracker. Do **not** stamp `completed` here and never overwrite one already there — completion belongs to `Done`, which the team reaches on its own (including via `/fix-bug`), and a date invented at release time dates the forecast from the release rather than from the work. Board automations fire on a drag, never on frontmatter an agent writes, so whatever the `Released` drag would have mirrored is this step's job to do by hand. A tracker failure is a reported partial synchronization failure; retain the record and retry that operation on the existing issue, not a new ticket. Anything deferred gets its `version_target` moved forward — not silently dropped.
11. Announce in none with a link to the release note.
12. Report: version, ticket count, anything deferred and why, the path of the GitHub release text, and any link it had to leave out.
