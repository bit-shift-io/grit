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
