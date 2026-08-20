# Implementation Tasks: Grit AI Agent Roadmap

This document serves as an actionable, phase-by-phase task list for an AI coding agent (Cursor, Cline, or Claude Dev) to build the Grit application from scratch.

---

## Pre-Requisites & Project Setup
- [x] Initialize Cargo workspace with `Cargo.toml`.
- [x] Add dependencies:
  - `tokio` (features: `full`)
  - `iced` (version `0.14` or latest)
  - `axum` (features: `ws`, `tokio`, `http1`)
  - `tower-http` (features: `fs`, `cors`, `trace`)
  - `serde` & `serde_json` (features: `derive`)
  - `notify` (v6+)
  - `rust-embed`
  - `clap` (features: `derive`)
  - `tracing` & `tracing-subscriber`

---

## Phase 1: Core Git Engine & Data Types (`src/git/`)
- [x] **Task 1.1: Define Core Data Structures** (`src/git/types.rs`)
  - Create `GitStatus` enum (`Modified`, `Untracked`, `Renamed`, `Deleted`, `Staged`).
  - Create `FileChange` struct (`path: String`, `status: GitStatus`, `is_staged: bool`).
  - Create `CommitInfo` struct (`hash: String`, `author: String`, `message: String`, `timestamp: i64`).
  - Create `RepoState` struct (`current_branch: String`, `branches: Vec<String>`, `changes: Vec<FileChange>`, `history: Vec<CommitInfo>`).
  - Create `GitAction` enum (`Stage(String)`, `Unstage(String)`, `Commit(String)`, `Push`, `Pull`, `CheckoutBranch(String)`, `Revert(String)`).

- [x] **Task 1.2: Implement Git CLI Command Execution** (`src/git/mod.rs`)
  - Implement `get_repository_status(repo_path: &Path) -> Result<RepoState, GitError>`.
  - Implement `execute_action(repo_path: &Path, action: GitAction) -> Result<(), GitError>`.
  - Ensure standard error output (`stderr`) from `git` CLI calls is properly captured and wrapped into a custom `GitError`.

- [x] **Task 1.3: Implement Workspace File Watcher** (`src/git/watcher.rs`)
  - Set up `notify::RecommendedWatcher` to monitor `.git/index`, `.git/HEAD`, and working directory files.
  - Implement a debounced channel (e.g. 200ms threshold) pushing refresh events to Tokio async channels.

---

## Phase 2: Axum Web Daemon & Embedded Assets (`src/server/`)
- [x] **Task 2.1: Axum Route & State Setup** (`src/server/mod.rs`)
  - Initialize `AppState` holding shared repository state (`Arc<RwLock<RepoState>>`) and event broadcast channel (`tokio::sync::broadcast`).
  - Build Axum router with HTTP health check `/health` and WebSocket route `/ws`.

- [x] **Task 2.2: WebSocket Real-Time State Stream** (`src/server/websocket.rs`)
  - Implement WebSocket handshake handler.
  - Listen for client-sent `GitAction` JSON messages and dispatch them to the Git Engine.
  - Broadcast updated `RepoState` JSON payloads across all active WebSocket connections when changes are detected by the file watcher.

- [x] **Task 2.3: Single-Binary Asset Embedding** (`src/server/static_files.rs`)
  - Implement `#[derive(RustEmbed)]` pointing to the `web/dist` folder.
  - Add custom Axum static file handler to serve `index.html`, JavaScript, and CSS bundles directly from memory.

---

## Phase 3: Native Desktop Application (`src/ui/`)
- [x] **Task 3.1: Initialize Iced Application State** (`src/ui/state.rs`)
  - Create `GritApp` struct implementing `iced::Application` or `iced::element`.
  - Define UI messages (`Message::StageFile(String)`, `Message::CommitPressed`, `Message::PullPressed`, `Message::StateUpdated(RepoState)`).

- [x] **Task 3.2: Build Core UI Layout Panels** (`src/ui/components/`)
  - **Header Panel:** Branch drop-down selector + Push / Pull / Fetch buttons.
  - **Staging View Panel:** Split view separating *Unstaged Changes* and *Staged Changes* with action buttons.
  - **Commit Panel:** Text input for commit subject/body + "Commit" action button.
  - **History Panel:** Scrollable list rendering commit hashes, authors, and messages.

- [x] **Task 3.3: Wire Async Actions & Background Tasks**
  - Integrate Iced `Subscription` to listen to internal channel updates from the Git Engine / file watcher.
  - Use `iced::Task` / `iced::Command` to run heavy `git push` or `git pull` calls on background threads without blocking the main UI loop.

---

## Phase 4: CLI Entrypoint & Dual-Mode Execution (`src/main.rs`)
- [x] **Task 4.1: CLI Argument Parsing**
  - Use `clap` to parse `--headless` (flag), `--port` (default: 8080), and `--path` (default: current working directory).

- [x] **Task 4.2: Mode Routing**
  - If `--headless` is set: start Tokio runtime and launch Axum server exclusively.
  - If default mode: start Axum background server on Tokio, then immediately launch `iced::run` on the main thread.

---

## Phase 5: Verification & Testing
- [x] **Task 5.1: Integration Testing**
  - Verify file staging and unstaging updates both desktop GUI and active browser tab simultaneously.
  - Test `git commit` execution across clean and dirty repository trees.
- [x] **Task 5.2: Release Build Verification**
  - Run `cargo build --release`.
  - Confirm output binary is fully self-contained and operates correctly when moved to a clean directory without external frontend assets.

---

## Phase 6: UX Essentials (README, Diffs, Nuke)
- [x] **Task 6.1: Write a Simple README** (`README.md`)
  - Replace the placeholder with a short, friendly README describing Grit, build/run commands, and key features.
- [x] **Task 6.2: Add Nuke Action Variant** (`src/git/types.rs`)
  - Add `Nuke` to the `GitAction` enum and include it in the JSON round-trip test.
- [x] **Task 6.3: Add File Diff Retrieval + Nuke Execution** (`src/git/mod.rs`)
  - Implement `get_file_diff(repo_path, path)` returning the unified diff string (staged + unstaged + untracked).
  - Handle `GitAction::Nuke` in `execute_action`: `git fetch --all`, `git reset --hard origin/<current-branch>`, `git clean -fdx`, falling back to re-cloning if the working tree is missing `.git`.
  - Add unit tests covering diff output and a nuke that discards local changes.
- [x] **Task 6.4: Expose Diff via HTTP** (`src/server/mod.rs`)
  - Add a `/diff` route that accepts a `path` query param and returns the file diff as plain text.
- [x] **Task 6.5: Wire Diff & Nuke Messages into UI State** (`src/ui/state.rs`)
  - Add `Message::ShowDiff(String)`, `Message::DiffLoaded(String, String)`, and `Message::NukePressed`.
  - Store selected diff text on the active `RepoTab` and refresh after nuke.
  - Add state-machine tests for diff load and nuke dispatch.
- [x] **Task 6.6: Make Staging Rows Clickable for Diffs** (`src/ui/components/staging.rs`)
  - Emit `Message::ShowDiff(path)` when a change row is clicked.
- [x] **Task 6.7: Add Diff Panel Component** (`src/ui/components/diff.rs`, `src/ui/components/mod.rs`)
  - Render the selected file's diff in a monospace scrollable panel; show a hint when no diff is selected.
- [x] **Task 6.8: Add Nuke Button to Header** (`src/ui/components/header.rs`)
  - Add a red "Nuke" button emitting `Message::NukePressed`.
- [x] **Task 6.9: Enrich Web UI** (`web/dist/index.html`, `web/dist/app.js`)
  - Render git status (branch, changes list with stage/unstage buttons), fetch/display file diffs, and add a Nuke button with confirm.

---

## Phase 7: Sync Desktop Tabs to the Web Interface
- [x] **Task 7.1: Shared Tab Registry Types** (`src/server/registry.rs`, `src/server/mod.rs`)
  - Create `WebTab { id, name, repo_path, state: RepoState }`, `WebState { active, tabs }`, and `TabRegistry` wrapping a `tokio::sync::watch` channel so the desktop GUI and web server share one tab list.
  - Add `TabRegistry::new/set/snapshot/subscribe/update_state` plus a `with_single_tab(path)` helper for headless boot.
  - Add unit tests for snapshot, update_state, and watch notifications.

- [x] **Task 7.2: Multi-Tab Server State** (`src/server/mod.rs`)
  - Refactor `AppState` to hold `registry: TabRegistry` and `broadcast: broadcast::Sender<WebState>` (replacing the single `repo_path`/`state`).
  - Add `refresh_tab(app, tab_id)` and `refresh_all(app)` that recompute status via `get_repository_status` and push results into the registry.
  - Rework `boot`/`run`/`sync_loop` to relay registry changes to the broadcast channel and spawn watchers for seeded tabs.
  - Update existing server tests to build `AppState` from a registry and assert on `WebState`.

- [x] **Task 7.3: Tab-Aware WebSocket Protocol** (`src/server/websocket.rs`)
  - Send the current `WebState` snapshot on connect instead of a bare `RepoState`.
  - Accept client messages shaped `{ "tab": <id>, "action": <GitAction> }`, resolve the repo path from the registry, dispatch the action, then `refresh_tab` for that id.
  - Update WebSocket tests to send tab-scoped actions and assert on `WebState`.

- [x] **Task 7.4: Tab-Scoped Diff Endpoint** (`src/server/mod.rs`)
  - Change `/diff` to accept a `tab` query param (`/diff?tab=<id>&path=<file>`) and resolve the repo path from the registry.
  - Update the existing diff endpoint test.

- [x] **Task 7.5: Desktop Pushes Tabs to Registry** (`src/ui/state.rs`)
  - Add `registry: Option<TabRegistry>` to `GritApp` and a `sync_registry()` helper that publishes the repo tabs + active index.
  - Call `sync_registry()` after `OpenTab`, `CloseTab`, `OpenNewRepo`, and `TabStateUpdated`.
  - Change `run()` to accept a `TabRegistry`, attach it, and publish tabs on startup.
  - Add tests that the registry reflects tab add/remove/active/state changes (registry `None` in existing tests keeps them passing).

- [x] **Task 7.6: Share Registry Across Modes** (`src/main.rs`)
  - Create one `TabRegistry` in `main()`, seed a single tab from `--path` in headless mode, and pass clones to both `server::run` and `ui::state::run`.
  - Keep CLI-parsing tests intact.

- [x] **Task 7.7: Web Tab Bar UI** (`web/dist/index.html`, `web/dist/app.js`, `web/dist/style.css`)
  - Render a tab bar listing every repo tab from `WebState`; track the active tab client-side (defaulting to `WebState.active`).
  - Scope stage/unstage, diff loading, and nuke actions to the active tab id.
  - Add tab bar styles and keep dark-mode support.