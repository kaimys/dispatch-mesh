# GitHub workflow verification findings

Date: 2026-11-04  
Application: Dispatch Mesh 0.2.4  
Test repository: [kaimys/dispatch-mesh-test](https://github.com/kaimys/dispatch-mesh-test)

## Result

All nine live workflow scenarios passed. The text and binary conflict scenarios were then rerun on 0.2.4 with the incoming contributor explicitly finalized first. All 44 automated tests passed, together with formatting and Clippy checks.

The [chronological case report](github-workflow-test-2026-11-04.md) retains timings, executable hashes, and the initial failed startup attempt. It also records the final comparison of both workspaces with GitHub `main`.

## Defects found and corrected

| Finding | Correction | Regression coverage |
|---|---|---|
| Starting `run` while an intent/control command held the workspace operation lock could terminate the runner. | Startup reads committed configuration without acquiring the writer lock. The capture loop retries contention. | `runner_startup_survives_a_concurrent_workspace_operation` in [CLI tests](../tests/cli.rs) |
| A selected conflict version could be invalidated when the incoming session published first. | Conflict records track the accepted view's contributing per-file revisions. Integration applies recorded selections to those original inputs independently of publication order and preserves the original snapshots. | Both selection choices with incoming-first publication, plus a three-author merged accepted view in [engine tests](../tests/engine.rs) |
| Already-integrated or rejected changes could produce empty permanent session commits. | Mark a closed session with no effective file changes as discarded, retaining its original draft, intent, and conflict records. Skip unnecessary summarization and publication. | `identical_concurrent_changes_do_not_add_an_empty_session_commit` in [engine tests](../tests/engine.rs) |

## Scenarios exercised

- **Continuous draft delivery:** Bob received Alice's first draft in 6.91 seconds, before finalization. A subsequent save retained the session identity, and incoming changes did not become Bob's local edits.
- **Finish and immediately continue:** follow-up edits entered a new session and did not alter the frozen snapshot.
- **Offline capture and restart:** a per-client transport failure left local capture working. Captured bytes survived terminating and restarting the test process, then synchronized after reconnection.
- **Compatible concurrent edits:** changes to separate lines of the same document merged and remained intact after both sessions finalized.
- **Ambiguous text conflicts:** exact base and competing versions were exported, an unrelated file continued syncing, and selecting the incoming version survived incoming-first publication.
- **Binary conflicts:** exact bytes were preserved, the accepted whole-file choice propagated, and that choice survived incoming-first publication.
- **Renames and deletions:** a rename containing spaces and accented characters, together with a separate deletion, propagated and finalized correctly.
- **Excluded content:** a nested `.env` containing only a synthetic test value did not enter capture, peer folders, or `main`.
- **Actual inactivity boundary:** two files shared one session. The full 300-second inactivity timer was used, followed by one finalized session commit; the case took 323.62 seconds including capture and publication.

## Scope and practical limits

Both clients ran on the same Windows machine with separate workspace directories, SQLite state, Git stores, device IDs, and author identities. They used the machine's existing GitHub authorization. This verifies real GitHub transport with isolated clients, not separate physical machines or separate GitHub accounts.

The test did not configure an AI provider. Message-generation behavior is separately covered by local mock-provider tests. The user's wiki workspace was not used or modified by the live harness.

Some GitHub operations required retries, including a bounded transport timeout. The cases verified eventual convergence and preserved data; their total durations are not throughput guarantees. Further performance profiling and testing on additional machines remain appropriate before desktop beta.

Previously published history, including earlier test attempts, was retained. The test repository intentionally contains synthetic test files, native device snapshots, and test commits. Local process logs, SQLite journals, Git objects, and exported conflict inputs were retained at the location in the chronological report. Owned runner processes were stopped after testing.

macOS/Linux hosts, tray controls, guided setup, startup registration, and cloud authorization remain outside this console-engine verification.

The initially empty GitHub repository selected the first pushed device branch as its default. After verification, its default branch was set to `main`. Guided repository setup should handle this explicitly; the runtime does not currently configure GitHub repository defaults.

## Reproduction

```powershell
python scripts/test_github_workflow.py --remote https://github.com/<owner>/<dedicated-test-repository>.git
```

Use the evidence directory reported by the harness with `--resume` to preserve an existing run. Repeated `--only-case text_conflict --only-case binary_conflict` options select the targeted conflict reruns. The harness writes synthetic data to the selected remote, restores its test transport overrides, and stops only its own runner processes.
