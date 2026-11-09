# Dispatch Mesh

Git-backed file synchronization with editing sessions, recorded agent intent, and recoverable conflicts.

**0.2 console preview.** This is the first engine milestone from the concept's delivery sequence. It is intended for disposable test folders, not yet as the sole copy of important data. A first tray host for Windows is in progress for 0.3 (see [Tray (preview)](#tray-preview)); guided account/repository setup, bundled Git, login startup, and the authenticated cloud MCP server are not implemented yet.

## Build and run

Requires Rust and Git on PATH. Tested on Windows with Git 2.35.1.

```powershell
cargo build --release
./target/release/dispatch-mesh.exe --help
./target/release/dispatch-mesh.exe --workspace C:/MeshDemo init --name Alice --email alice@example.com
./target/release/dispatch-mesh.exe --workspace C:/MeshDemo run
```

`init` creates the folder if necessary. Existing files become an editing session. In an Obsidian vault (a folder with `.obsidian/`), `init` also writes the per-device layout files `.obsidian/workspace.json` and `.obsidian/workspace-mobile.json` into `.meshignore`, adding only rules that are missing; the rest of `.obsidian/` keeps synchronizing. Local-only workspaces work without a remote or an AI provider. Ctrl+C captures saved changes and stops; it does not require a last network request to complete.

To connect a repository, add `--remote <URL-or-local-bare-repository-path>` to `init`. Use a dedicated test repository and a fresh folder to join an existing remote. Git Credential Manager or SSH must already provide access in this preview; Mesh never asks for or saves a token in the remote URL. Mesh creates device branches under `mesh/devices/` and permanent session commits on `main` (or the branch selected with `--branch`). Do not manually change Mesh's private Git repository.

```powershell
dispatch-mesh --workspace C:/MeshDemo status
dispatch-mesh --workspace C:/MeshDemo sync
dispatch-mesh --workspace C:/MeshDemo session finish
```

`run` watches files and reconciles missed events with periodic scans. Draft synchronization is attempted every three seconds after writes settle. Sessions span files and close after five minutes without local edits. `session finish` is optional and queues finalization; keep `run` active or invoke `sync` to summarize and publish. Saved edits made afterward enter another session. Incoming changes never reset the local editing timer. Status is JSON, including per-file recent-save presence and the unresolved conflicts: each with its path, whether it is binary, when it was recorded and, for the `current` version (in the folder) and the `other` version (kept), the contributors (author, device, last save, requested purposes of the intents covering the file), whether it came from shared history, whether it is a deletion, and its size.

Drafts are uploaded as ordinary Git files on the device branches. GitHub's ordinary file browser on `main` shows finalized sessions, so a newly saved file appears there after the five-minute pause and publication, not after its first draft upload. Other Mesh clients consume the drafts before then. To publish sooner, invoke `session finish` while `run` is active, or follow it with `sync` if Mesh is stopped.

On GitHub, select `mesh/devices/<device-id>` and browse `drafts/<session-id>/files/` to inspect current draft content. A version-two `state.json` index stores session metadata and Git blob references. Original versions live under `drafts/<session-id>/base/`; conflict inputs and selected resolutions have their own files under `conflicts/` and `resolutions/`. Empty files are real empty blobs, while a null reference records deletion. Git deduplicates identical content, and every referenced version is kept reachable through the draft tree for fetch and garbage collection. Neither text nor binary contents are embedded as integer arrays in new metadata.

Local state uses the same reference-based model with content-addressed SQLite BLOBs. Opening an existing workspace atomically migrates its version-one state and keeps the original JSON in a migration backup table. Session IDs, content fingerprints, and existing published commits remain unchanged. Version-one remote snapshots remain readable; new device snapshots use version two. Upgrade all collaborating clients before using version-two snapshots. Existing Git history is retained, including old snapshot commits; Mesh does not rewrite it to remove the earlier representation.

`status` reads a committed SQLite snapshot without taking the operation lock, so it remains available during synchronization. Genuine run-loop errors report their cause and are rate-limited; brief internal lock contention is retried quietly without losing pending scans.

The top-level `status` is one workspace summary. When several apply, the first in this order wins:

| `status` | Meaning |
|---|---|
| `needs_attention` | Something needs you: an unresolved conflict, a capture error, a sync failure other than an unreachable remote (refused login or permission, rejected push, unexpected history, or any unclassified Git failure), an engine pause, or the last AI summary failing. |
| `offline` | The last sync could not reach the remote (DNS, connection refused, timeout, TLS handshake). Local capture continues; the next successful sync clears it. |
| `saved_locally` | Saved edits that are not yet uploaded as a draft. |
| `draft_uploaded` | Your open session is on your device branch, so other clients can already see it. |
| `waiting_for_finalization` | A closed session is waiting for summarization, conflict resolution, dependencies, or publication to `main`. |
| `synced` | All your sessions are published or discarded and nothing is waiting. |

For `needs_attention` and `offline`, `reason` names the cause; it never contains credentials, Git's raw error output, or provider response bodies. Each entry in `sessions` carries a `stage`: `saved_locally` → `draft_uploaded` → `waiting_for_finalization` → `published`, or `discarded`. Sessions received from other devices are uploaded by definition; a workspace without a remote has no upload stage. The raw `attention`, `offline`, `summary_failure`, `capture_error`, `local_changes_waiting`, and `recovery_pending` fields remain available. Earlier previews reported `waiting_to_sync`; that value no longer appears.

Each session also reports `device` (the id of the device it was made on; compare it with the top-level `device` to tell your own sessions from received ones), `started_at` and `last_saved` (Unix seconds), `title` (the summary it was added to shared history with, `null` until then) and `purposes` (the requested purposes of its recorded intents). The workspace reports `has_remote`, `last_sync` (when a sync with the remote last succeeded, to the minute; `null` if never, and always `null` without a remote) and `saved_copies` (each displaced file's `id`, original `path` and `created_at`; the file is `.mesh/recovery/<id>/content`).

Only one runner may own a workspace. A second `run`, or a second embedded engine, stops with "Mesh is already running for this workspace" and starts nothing; try again once the first one has stopped. Every other command keeps working while a runner is active.

Desktop hosts embed the same run loop in-process through `dispatch_mesh::runner::Runner::start`. The returned handle publishes the `status` snapshot plus the runner's current capture, sync and summary errors whenever they change, and `stop()` saves local work without waiting for pending network or AI requests, just as Ctrl+C does for `run`. The tray host below is built on it; it is not yet a stable public API.

Git always runs with `LC_ALL=C`, so failures are classified the same way on any system language. On an HTTP 429 from the AI provider, Mesh separates exhausted quota or billing from a temporary rate limit (including the provider's retry delay) when the provider says which it is.

## Agents and intent

```powershell
dispatch-mesh --workspace C:/MeshDemo intent begin --purpose "Clarify onboarding for new colleagues" --paths guide.md examples/setup.md
# Edit the named files, then use the returned intent ID:
dispatch-mesh --workspace C:/MeshDemo intent end <intent-id>
# Optional:
dispatch-mesh --workspace C:/MeshDemo session finish
```

Only one intent scope may be active per device. Capture occurs before opening a scope and before closing it. Exact relative paths constrain attribution; changes outside those paths have no recorded intent. If a path contains a mixture of scoped and unscoped edits in a session, its aggregate change loses intent attribution rather than claiming the purpose explains every edit. Filesystem watching cannot identify concurrent writers to the same path: agents should use a dedicated working folder/device context. Do not put secrets or entire conversations in `--purpose`.

Recorded purposes appear in the commit body under `Recorded user intent (requested purpose)`, grouped by their associated files. Mesh adds this section locally for both AI-generated messages and local fallback messages, so it does not depend on an AI provider being available or repeating the purpose. Unrelated, mixed-attribution, and reverted changes do not inherit it. The purpose records what was requested, not proof of what was achieved. The title remains a summary of the changes. `status` reports the configured `commit_message_mode`.

## Local and hosted AI

Without a provider, Mesh uses factual local commit messages. Configure any compatible Chat Completions server:

```powershell
dispatch-mesh --workspace C:/MeshDemo provider set --base-url http://localhost:11434/v1 --model <installed-model>
dispatch-mesh --workspace C:/MeshDemo provider test
```

For a hosted provider, supply an HTTPS base URL and `--api-key-env MESH_AI_KEY`, with the credential already set in that environment variable. Keys are not stored in configuration. Add `--json-mode` only if the endpoint supports JSON-object mode.

Both an API base such as `https://api.openai.com/v1` and a full endpoint such as `https://api.openai.com/v1/chat/completions` are accepted. Mesh resolves the route once. Set the key in the same terminal before starting `run`; environment changes made later are not inherited by an already running process. Use `provider test` from that terminal to verify credentials and response formatting with synthetic input. Provider failures report the missing variable, HTTP status, timeout, or response-format problem without echoing response bodies or credentials.

By default, file contents remain private and Mesh generates factual file-operation messages locally (`local_metadata` mode). These distinguish added, modified, and deleted files without asking a model to infer changes from filenames or sizes. Recorded purpose is still included locally.

**Add `--send-text` to enable AI change summaries and explicitly allow transmission of before/after text snapshots and recorded intent to that endpoint.** Requests include authoritative file-operation and existence fields, so a modification to an existing document is distinguishable from creating a new file. A snapshot may contain sensitive prose; generic secret detection is not implemented. Exclude private content before enabling it. Binary payloads are never sent for summarization. `provider test` always uses synthetic input to check the endpoint, even in metadata-only mode. `provider off` disables AI.

AI requests have timeouts, bounded retries, response validation, and a factual fallback. There is no automatic provider switching. Summaries are tied to frozen session content and the integrated history base; if that base advances, Mesh discards the outdated message and schedules a new one. The run loop can capture while a summary request is pending.

## Conflicts and recovery

```powershell
dispatch-mesh --workspace C:/MeshDemo conflicts list
dispatch-mesh --workspace C:/MeshDemo conflicts show <conflict-id>
dispatch-mesh --workspace C:/MeshDemo conflicts export <conflict-id> --output C:/ConflictReview
dispatch-mesh --workspace C:/MeshDemo conflicts resolve <conflict-id> --choose accepted
# Or: --choose incoming
```

The export directory must be new and outside the workspace. Records preserve exact base, accepted, and incoming bytes and synchronize with device manifests. Ordinary clean three-way text merges are automatic. Ambiguous text and binary conflicts preserve the accepted version until someone chooses a version. Binary resolution only selects a whole competing file. The preview exposes version selection for text too; an interactive text-resolution editor and AI conflict proposals remain future work. Unrelated paths continue to synchronize, but finalization of a session containing unresolved files waits.

Mesh persists local state transactionally in `.mesh/state.sqlite` and uses a write-ahead journal when applying incoming content. Do not delete `.mesh`: it holds drafts, pending edits, conflict inputs, and private Git objects. Displaced files remain in `.mesh/recovery/<id>/content` with a `record.json` identifying their original path; no automatic cleanup is performed in this preview. If local edits cannot be separated from received peer text, their bytes are saved in pending-capture state and synchronization pauses for attention. `session finish` checkpoints the current draft and captures those dependent edits into the next session. This conservative fallback avoids attributing another author's work to the local user.

Conflict records retain the exact input revisions that contributed to the competing view. A recorded selection continues to govern those inputs if the clients publish in a different order; later, different edits are reconciled separately. A closed session with no effective changes after integration or resolution is marked discarded instead of adding an empty permanent commit. Its original snapshots, intent, and resolution records remain recoverable. Update collaborating clients together when adopting these workflow fixes.

### Storage growth

Nothing Mesh stores is pruned in this preview. Plan disk space accordingly.

| Location | What accumulates |
|---|---|
| `.mesh/state.sqlite` | Local state, plus a content-addressed `blobs` table holding every captured version, conflict input, pending capture, and the original state of migrated workspaces. Identical content is stored once. |
| `.mesh/objects.git` | Mesh's private Git store: every draft snapshot, device-branch commit, and published commit fetched or created on this device. |
| `.mesh/recovery/<id>/` | Files displaced while applying incoming content. Each is kept indefinitely. |
| Remote device branches `mesh/devices/<device-id>` | One commit per draft upload, with every referenced version kept reachable, including settled sessions and conflict inputs. |
| Remote `main` | One immutable commit per finalized session. History is never rewritten. |

### Recovery procedures

Run `status` first; `reason` tells you which case applies. Never delete `.mesh` or reset Mesh's Git store to recover: both hold the only copies of drafts and conflict inputs.

- **`offline`.** Nothing to do: keep working. Edits are captured locally and upload on the next successful sync. If it persists, check the network, proxy, and VPN, then run `sync`.
- **`needs_attention`, "N unresolved conflicts".** Open the tray's Conflicts tab, or use `conflicts list`, `conflicts show` or `conflicts export`, then `conflicts resolve <id> --choose accepted|incoming`. Unrelated files keep syncing meanwhile.
- **`needs_attention`, "Remote refused access".** Credentials or repository permissions are wrong. Fix the sign-in in Git Credential Manager or your SSH agent, confirm you can write the repository, then run `sync`.
- **`needs_attention`, "Remote rejected the update".** Usually a publication race with another client. The next cycle fetches and retries; run `sync` if it does not clear.
- **`needs_attention`, "Shared history was rewritten outside Mesh" or "The shared branch was deleted outside Mesh".** Someone changed `main` with Git directly. Mesh pauses synchronization and keeps local work. Restore the shared branch on the remote (it must contain the commits Mesh last saw), then run `sync`.
- **`needs_attention`, "Local capture waiting: …".** A file could not be read or is unsupported (see *Preview boundaries*), for example a symlink or a case collision. Fix or restore the named file; no deletion is recorded meanwhile.
- **`needs_attention`, pending-capture message ("Edits depend on a received draft …").** Run `session finish`. It checkpoints the current draft, and the dependent edits enter the next session.
- **`needs_attention`, "AI summary failed: …".** Publication continues with a factual local message. Fix the API key, quota, or endpoint named in the message. The state clears with the next summary that does not fail, including one made after you remove the provider.
- **Restoring a displaced file.** Open `.mesh/recovery/<id>/record.json` to find the original path, then copy `.mesh/recovery/<id>/content` back to that path (or anywhere else). The copy is an ordinary edit and enters a new session.

## Tray (preview)

`dispatch-mesh-tray.exe` is a separate Windows program that keeps one or more workspaces syncing from the notification area, without a console window. It is built next to the CLI and runs the same engine for each workspace; the CLI stays an ordinary console program.

```powershell
cargo build --release
./target/release/dispatch-mesh-tray.exe --workspace C:/MeshDemo --workspace D:/Shared/Handbook
```

- Each `--workspace` must already be set up with `dispatch-mesh init`. Adding workspaces from the window comes with guided setup.
- The icon shows the most urgent state of all workspaces: needs attention, offline, working (saved on this device, shared as draft, finishing up), or up to date. The tooltip names the state, and for attention and offline the reason. State is told apart by the icon's badge shape, not by color alone.
- Left-click opens the status window, laid out after the desktop app design: the workspace name and folder, then one card with the state, the reason or what the state means, a reassurance line when offline and for the attention reasons that have one (for example that both versions of a conflicting file are kept), the actions and the last successful sync. With several workspaces, it lists the most urgent first, and the workspace name leads back to that list.
- The **Activity** tab lists every session still on its way, then the ten settled sessions saved most recently, each group newest by last save. Each row shows the author ("You" for this device's sessions), the number of files, start and last save, the full summary title once published or else the files, any requested purpose, and a segmented stage track (saved on this device → shared as draft → finishing up → added to shared history) with the stage named next to it. Hovering a row shows its author and changed files. The **Conflicts** tab counts unresolved conflicts and lists them, each with its path, when it was recorded, and who made the current version (in the folder) and the other version (kept): "You" for this device, "Shared history" when no session contributed. Saved copies list each displaced file with **Show in folder**; restoring one stays a manual copy (see *Recovery procedures*).
- **Review and choose** on a conflict widens the window to compare the two versions side by side. A text conflict shows both with line numbers and the differing lines marked (each side up to its first 1,000 lines, with a line saying how many it has; when the line comparison takes too long, the window says changes could not be marked), each side's author, time and requested purpose, and **Show common base**; a file over 1 MB gets the whole-file view instead. A binary conflict shows each side's size, type and author, with an image preview for PNG, JPEG, GIF and WebP up to 10 MB. A side that deleted the file says so. **Keep current version** and **Use other version** do what `conflicts resolve --choose accepted|incoming` does, even while the tray's engine runs; both versions stay in history. The window then opens the next conflict, or returns to the list. If you saved the file again after the conflict was recorded, the window says your edit is safe and disables both buttons until the file matches the current version again (the engine refuses to resolve over a newer edit; reconciling it is planned separately). **Back** (or Escape) restores the compact size. A maximized window is left as it is.
- **Finish session** in the window, and one menu item per workspace with an open session, closes the session just like `session finish`. Under **Needs attention** for conflicts, **Open conflicts** switches to the Conflicts tab. A failed sync says that Mesh retries automatically.
- Closing the window only hides it; Mesh keeps syncing. **Quit** in the right-click menu captures saved changes and stops, without waiting for the network, just like Ctrl+C for `run`.
- If a CLI `run` or another tray already runs a workspace, the window says so and offers **Retry**; the other workspaces keep running. CLI commands such as `status`, `sync`, `session finish` and `intent` keep working while the tray runs.
- The window bundles the IBM Plex Sans and Mono fonts (SIL Open Font License, `tray/ui/fonts/OFL.txt`).
- Not yet: writing a combined version of a text conflict, presence and pause, notifications other than a one-time hint after the first close, login startup and an installer. On Linux, building needs the WebKitGTK and AppIndicator development packages (see `.github/workflows/ci.yml`).

## Preview boundaries

- Plain regular files with UTF-8 paths; binary contents are supported. Symlinks, reparse points, submodules, executable file modes, Windows reserved names, and case-colliding paths are unsupported.
- Default maximum file size: 16 MiB, configurable in `.mesh/config.json`. Metadata references native Git blobs and deduplicated local SQLite BLOBs. Settled sessions and conflict inputs are retained, and reconstructing a device snapshot is bounded by a 128 MiB preview memory limit. Large-file storage, retention, and application-level cleanup remain future work.
- `.mesh`, `.git`, `.env`, `.env.*`, `*.pem`, and `*.key` are excluded. `.gitignore`, `.ignore` and `.meshignore` inside the workspace are honored. They synchronize like other files, so every device follows the same rules. A rule file always synchronizes, even when a rule matches it; only an excluded folder keeps it out. Rules from folders above the workspace are not part of the shared rules.
- **Stop synchronizing a file** by adding a rule for it to `.meshignore`, even after it has been synchronized. The file stays on every device's disk. `main` records its removal, attributed to the device that added the rule, and later edits on any device stay local. A device that hasn't received the rule yet can still send edits; they are dropped, and an open conflict on the file is settled as "removed" with both inputs kept. Removing the rule again makes each device's copy an ordinary new change: identical copies merge, differing copies become an ordinary conflict. For an existing Obsidian vault, add these two lines to `.meshignore`:

  ```
  .obsidian/workspace.json
  .obsidian/workspace-mobile.json
  ```

  **Update every device to 0.3.0 before adding such a rule.** Older versions treat the received removal as a deletion and move their local copy into `.mesh/recovery` (restore it as described under *Recovery procedures*).
- Git credentials and commit identity are configured separately. This preview trusts collaborators authorized to write the remote repository; it does not implement cloud principal isolation or workspace permissions.
- Same-file dependencies can delay permanent publication until upstream sessions finish. Draft ordering uses explicit dependencies and a stable timestamp/ID order for independent sessions; distributed conflict display policy is experimental.
- Git network operations run on a separate worker and release the workspace operation lock while waiting on the remote. The worker reloads durable state before continuing, preserving edits captured during the request. One network cycle runs at a time; local merge/apply phases can still briefly hold the operation lock.
- Connecting an existing Git checkout does not yet import its local history automatically. Use a fresh folder with the remote option for an existing shared repository. Manual Git interoperability is outside the initial supported workflow.
- The tray host shows status and session detail, finishes sessions and reviews and resolves conflicts by choosing a whole version; notifications, guided repository creation, bundled Git, installer, login startup, editor presence integration, and authenticated remote MCP are not available yet. CLI status and conflict commands expose the underlying engine operations for this milestone.

## Verification

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Integration tests use temporary folders and local bare Git remotes. They exercise continuous draft delivery, local author attribution, session boundaries, offline capture, restart recovery, binary preservation and selection, text merging, intent scoping, ignore rules, and path validation. Storage tests cover old-workspace migration, native-file transport, blob reachability after Git garbage collection, recovery-field persistence, and rejection of unshared object references. Provider tests use a local mock HTTP server; no API key or external AI request is needed.

For a live GitHub verification run, use a dedicated test repository:

```powershell
python scripts/test_github_workflow.py --remote https://github.com/<owner>/<test-repository>.git
```

The harness writes synthetic data to `main` and Mesh device branches through two isolated Windows clients, retains local evidence, and writes a report under `dispatch/wiki/08_Delivery-and-QA/Reports/`. It simulates offline operation by changing only a test client's private Git transport, exercises process termination/restart, and includes a real five-minute inactivity test. It stops only the runner processes it owns. `--resume <evidence-folder>` reuses a run; repeated `--only-case <name>` arguments select targeted reruns. Live AI calls are not part of this harness.
