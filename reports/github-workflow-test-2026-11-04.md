# GitHub workflow test report

Date: 2026-11-04

Remote: https://github.com/kaimys/dispatch-mesh-test.git
Run: `run-20261104-9364dcbc`
Evidence directory: `C:\Users\kai\AppData\Local\Temp\dispatch-mesh-live-fs5pfto5`
Current build: `dispatch-mesh 0.2.4`, SHA-256 `eb69d6f4261392749436a3b9f1ac1cf3831cc5a67eadbd18e565be526c8fd058`

Scope: two isolated Windows clients on one machine, real GitHub transport, synthetic text/binary data, local commit messages. Alice and Bob use distinct Mesh device/author identities and the machine's existing GitHub authorization. No wiki files or live AI provider were used.

Attempts are listed chronologically; a later passing rerun supersedes an earlier failure of the same case. Initial failures remain as evidence of defects found during testing.

| Case | Result | Seconds | Build hash | Evidence |
|---|---|---:|---|---|
| live_drafts | FAIL | 100.12 | initial | Timed out: live draft on Bob; {'alice': {'attention': None, 'unresolved': 0}, 'bob': {'attention': None, 'unresolved': 0}} |
| live_drafts | PASS | 34.42 | 3f2e6680e12a | Draft reached Bob in 6.91s, with one stable editing session and no incoming-edit attribution. Intent survived finalization. |
| finish_then_edit | PASS | 32.06 | 3f2e6680e12a | Immediate follow-up edits entered a separate session; the frozen session did not absorb them. |
| offline_restart | PASS | 42.8 | 3f2e6680e12a | Per-client transport failure did not block capture. Durable edits survived process termination/restart and synchronized after reconnect. |
| compatible_edits | PASS | 106.59 | 3f2e6680e12a | Independent changes to the first and last lines merged and remained intact after both sessions published. |
| text_conflict | PASS | 132.73 | 3f2e6680e12a | Exact base and competing versions exported intact; unrelated drafts continued; 'incoming' selection converged and survived finalization. |
| binary_conflict | PASS | 163.48 | 3f2e6680e12a | Exact base and competing versions exported intact; unrelated drafts continued; 'accepted' selection converged and survived finalization. |
| renames_deletes | PASS | 214.17 | 3f2e6680e12a | Rename with spaces/Unicode and deletion propagated with content preserved, then finalized. |
| ignored_content | PASS | 44.69 | 3f2e6680e12a | A nested .env containing only a synthetic value was excluded from capture, peer delivery, and main history. |
| real_idle_boundary | PASS | 323.62 | 3f2e6680e12a | Two files shared session 56c7a692-4243-416c-b653-f075536a3c68; automatic publication followed the real 300-second inactivity timer, with one final session commit. |
| text_conflict | PASS | 187.02 | eb69d6f42613 | Exact base and competing versions exported intact; unrelated drafts continued; 'incoming' selection survived incoming-contributor-first publication. |
| binary_conflict | PASS | 202.36 | eb69d6f42613 | Exact base and competing versions exported intact; unrelated drafts continued; 'accepted' selection survived incoming-contributor-first publication. |

Process logs, SQLite journals, Git objects, and exported conflict inputs remain in the evidence directory. All owned runner processes are stopped and test transport overrides are restored at completion. Existing remote history is retained.

This run does not validate separate physical machines or GitHub accounts, macOS/Linux hosts, tray UI, guided setup, cloud authorization, or live AI output quality.

## Final consistency check

Both clients and GitHub `main` converged to `e3f000c9b01b864ace8ec607e915a8a3f9c498b3`. All 13 shared files matched GitHub byte-for-byte. The compatible-edit history contains both Mesh Test Alice and Mesh Test Bob. All nine latest scenario results are PASS.
