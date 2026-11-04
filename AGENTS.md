# Dispatch Mesh agent guide

## Purpose and current scope

Dispatch Mesh synchronizes folders through Git while hiding Git mechanics from end users. It tracks editing sessions, records agent intent, and preserves recoverable conflicts. Read `README.md` for the supported behavior and CLI examples before changing the engine.

The current application is a Rust console preview. Tray controls, guided account/repository setup, bundled Git, and login startup are future work. macOS and Linux builds precede the authenticated cloud MCP milestone, which is deferred to version 2.0. Do not present planned features as implemented.

## Repository map

- `src/main.rs`: CLI commands, watcher, capture loop, and synchronization scheduling.
- `src/engine.rs`: workspace operations, sessions, integration, conflict resolution, and publication.
- `src/model.rs`: configuration and durable domain types.
- `src/storage.rs`: SQLite persistence and migration.
- `src/codec.rs`: metadata encoding with content references.
- `src/transport.rs`: device manifests and native Git blobs.
- `src/git.rs`: Git subprocess operations and path/object validation.
- `src/ai.rs`: Chat Completions summaries, recorded intent, validation, and local fallback.
- `tests/`: CLI, engine, provider, and storage/transport integration tests.
- `scripts/test_github_workflow.py`: optional live GitHub verification using isolated clients.
- `reports/`: recorded verification results and known limitations.

`dispatch/wiki` is the project's Obsidian vault (product concept documents and tickets), synchronized by Mesh itself. It is ignored by this repository. Do not assume it is disposable or include it in source-repository commits.

## Behavioral invariants

- Drafts synchronize continuously on `mesh/devices/<device-id>`. Finalized sessions become permanent commits on `main` or the configured shared branch. Preserve immutable shared history; do not replace this with force-pushed amendments to shared commits.
- A session spans related changes across multiple files. Five minutes without local edits closes it automatically. `session finish` is optional; subsequent edits enter another session.
- Incoming changes must not become local authorship or reset the local inactivity timer. Preserve capture while network operations or AI requests are pending.
- Never discard uncaptured edits or original conflict inputs. Preserve transaction boundaries, recovery journals, pending capture, and migration compatibility.
- Conflict selections must survive publication order changes. Retain the contributing input revisions; reconcile later distinct edits separately. Unrelated files should continue synchronizing.
- Text uses clean three-way merging where possible. Binary conflicts select an entire competing file. Closed sessions with no effective changes must not produce empty permanent commits.
- New metadata references native Git blobs or local SQLite BLOBs; do not embed file payloads as integer arrays in JSON. Referenced remote blobs must remain reachable and must be validated before use.
- Recorded intent is the requested purpose, not evidence of completion. Scope it to the declared relative paths and include it locally in commit bodies even when AI fails. Mixed or unrelated edits must not inherit that purpose.
- AI text sharing is opt-in. Never send binary payloads, silently switch providers, persist API keys, or expose credentials/response bodies in diagnostics. Provider failures must retain a factual local fallback.
- Manual Git operations inside Mesh's private store are outside the supported user workflow. Users should not need Git CLI knowledge for future setup and desktop features.

## Development and verification

Requires Rust and Git on `PATH`. Typical commands, run from the repository root:

```powershell
cargo build --release
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
./target/release/dispatch-mesh.exe --help
```

Use focused tests while developing, then run the relevant checks before delivery. Changes to synchronization, durability, attribution, or conflict ordering should have regression coverage that verifies observable behavior. Integration tests use temporary folders and local bare Git remotes; provider tests use a mock server and require no API key. Some engine tests take several minutes on Windows.

Keep `Cargo.toml` and `Cargo.lock` consistent when changing the version. Update `README.md` when supported behavior or commands change. Build outputs belong in ignored `target/` and `dist/` directories.

## Live verification and user data

Use disposable folders and a dedicated remote for live testing. The designated test repository is `https://github.com/kaimys/dispatch-mesh-test.git`; do not substitute the user's wiki remote.

```powershell
python scripts/test_github_workflow.py --remote https://github.com/kaimys/dispatch-mesh-test.git
```

The harness writes synthetic files and commits to the remote. It retains evidence, supports `--resume` and repeated `--only-case` options, and includes a real five-minute idle test. It simulates offline operation by changing only a test client's transport configuration. Restore transport overrides and stop only processes owned by the test. Do not change global credentials or networking to simulate failures.

An initially empty GitHub repository may select the first device branch as its default. Verify the default branch when setting up a fresh remote; shared content is finalized on `main` regardless of GitHub's browser default.

Never delete `.mesh`, reset remote history, overwrite real user files, or remove recovery data as a troubleshooting shortcut. Before replacing a Windows executable, check whether it is running; coordinate stopping a user-owned runner rather than killing it indiscriminately. Do not enable AI sharing or contact a live provider merely to run tests.

## Reporting changes

Explain what changed, why, and how it was verified. Distinguish local automated checks from real remote tests and platform coverage. Report failures and remaining limitations honestly, retain useful evidence, and avoid claiming two clients on one Windows machine validate separate hosts or accounts. Use meaningful commit messages describing the resulting behavior.

## Dispatch ticket workflows

Tickets live in the Obsidian vault at `dispatch/wiki` and are worked through the Dispatch board. For any ticket work, read `dispatch/invariants.md` first. The canonical workflows are `dispatch/workflow/*.md`. Codex invokes them as `$refine <id>`, `$develop <id>` and so on through the stubs in `.codex/skills/`, and Claude Code invokes them through `.claude/commands/`.

Codex-specific: `.codex/hooks.json` wires the Dispatch run-lifecycle hooks. Codex runs them only after you accept the hook-trust prompt in an interactive `codex` session. Editing that file un-trusts the changed entries, so re-accept the prompt afterwards.
