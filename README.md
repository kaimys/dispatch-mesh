# Dispatch Mesh

Git-backed file synchronization with editing sessions, recorded agent intent, and recoverable conflicts.

**0.1 console preview.** This is the first engine milestone from the concept's delivery sequence. It is intended for disposable test folders, not yet as the sole copy of important data. The tray application, guided account/repository setup, bundled Git, login startup, and authenticated cloud MCP server are not implemented yet.

## Build and run

Requires Rust and Git on PATH. Tested on Windows with Git 2.35.1.

```powershell
cargo build --release
./target/release/dispatch-mesh.exe --help
./target/release/dispatch-mesh.exe --workspace C:/MeshDemo init --name Alice --email alice@example.com
./target/release/dispatch-mesh.exe --workspace C:/MeshDemo run
```

`init` creates the folder if necessary. Existing files become an editing session. Local-only workspaces work without a remote or an AI provider. Ctrl+C captures saved changes and stops; it does not require a last network request to complete.

To connect a repository, add `--remote <URL-or-local-bare-repository-path>` to `init`. Use a dedicated test repository and a fresh folder to join an existing remote. Git Credential Manager or SSH must already provide access in this preview; Mesh never asks for or saves a token in the remote URL. Mesh creates device branches under `mesh/devices/` and permanent session commits on `main` (or the branch selected with `--branch`). Do not manually change Mesh's private Git repository.

```powershell
dispatch-mesh --workspace C:/MeshDemo status
dispatch-mesh --workspace C:/MeshDemo sync
dispatch-mesh --workspace C:/MeshDemo session finish
```

`run` watches files and reconciles missed events with periodic scans. Draft synchronization is attempted every three seconds after writes settle. Sessions span files and close after five minutes without local edits. `session finish` is optional and queues finalization; keep `run` active or invoke `sync` to summarize and publish. Saved edits made afterward enter another session. Incoming changes never reset the local editing timer. Status is JSON, including per-file recent-save presence and unresolved conflict IDs.

## Agents and intent

```powershell
dispatch-mesh --workspace C:/MeshDemo intent begin --purpose "Clarify onboarding for new colleagues" --paths guide.md examples/setup.md
# Edit the named files, then use the returned intent ID:
dispatch-mesh --workspace C:/MeshDemo intent end <intent-id>
# Optional:
dispatch-mesh --workspace C:/MeshDemo session finish
```

Only one intent scope may be active per device. Capture occurs before opening a scope and before closing it. Exact relative paths constrain attribution; changes outside those paths have no recorded intent. If a path contains a mixture of scoped and unscoped edits in a session, its aggregate change loses intent attribution rather than claiming the purpose explains every edit. Filesystem watching cannot identify concurrent writers to the same path: agents should use a dedicated working folder/device context. Do not put secrets or entire conversations in `--purpose`.

## Local and hosted AI

Without a provider, Mesh uses factual local commit messages. Configure any compatible Chat Completions server:

```powershell
dispatch-mesh --workspace C:/MeshDemo provider set --base-url http://localhost:11434/v1 --model <installed-model>
dispatch-mesh --workspace C:/MeshDemo provider test
```

For a hosted provider, supply an HTTPS base URL and `--api-key-env MESH_AI_KEY`, with the credential already set in that environment variable. Keys are not stored in configuration. Add `--json-mode` only if the endpoint supports JSON-object mode.

By default the request includes filenames, sizes, and snapshot IDs, with text and intent withheld. **Add `--send-text` to explicitly allow transmission of text snapshots and recorded intent to that endpoint.** A snapshot may contain sensitive prose; generic secret detection is not implemented. Exclude private content before enabling it. Binary payloads are never sent for summarization. `provider test` uses synthetic input, not workspace files. `provider off` disables AI.

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

## Preview boundaries

- Plain regular files with UTF-8 paths; binary contents are supported. Symlinks, reparse points, submodules, executable file modes, Windows reserved names, and case-colliding paths are unsupported.
- Default maximum file size: 16 MiB, configurable in `.mesh/config.json`. Device manifests are JSON snapshots and keep settled sessions and conflict inputs; this is deliberately unoptimized and unsuitable for large binary collections. Retention and garbage collection are not yet implemented.
- `.mesh`, `.git`, `.env`, `.env.*`, `*.pem`, and `*.key` are excluded. `.gitignore` and `.meshignore` are honored. Use matching ignore policies on all clients; changing ignore rules for already synchronized files is not supported yet.
- Git credentials and commit identity are configured separately. This preview trusts collaborators authorized to write the remote repository; it does not implement cloud principal isolation or workspace permissions.
- Same-file dependencies can delay permanent publication until upstream sessions finish. Draft ordering uses explicit dependencies and a stable timestamp/ID order for independent sessions; distributed conflict display policy is experimental.
- Git network operations currently take a workspace operation lock. They are bounded but can temporarily delay capture and CLI actions. Separating the network worker from the capture journal is required before production use.
- Connecting an existing Git checkout does not yet import its local history automatically. Use a fresh folder with the remote option for an existing shared repository. Manual Git interoperability is outside the initial supported workflow.
- No tray/status window, desktop notifications, guided repository creation, bundled Git, installer, login startup, editor presence integration, or authenticated remote MCP yet. CLI status and conflict commands expose the underlying engine operations for this milestone.

## Verification

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Integration tests use temporary folders and local bare Git remotes. They exercise continuous draft delivery, local author attribution, session boundaries, offline capture, restart recovery, binary preservation and selection, text merging, intent scoping, ignore rules, and path validation. Provider tests use a local mock HTTP server; no API key or external AI request is needed.
