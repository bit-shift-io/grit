# ARCHITECTURE.md — System Architecture & Codebase Map (Grit)

> **Purpose:** This document provides a structural map, architectural guidelines, and module breakdown for both human developers and AI assistants. Keep this file updated as key modules, traits, or data flows evolve.

---

## 1. Executive Overview

**Project Goal:** A fast, native, single-binary Git client written in Rust that bridges desktop UI performance and embedded local web convenience. It operates simultaneously as a native desktop application (`Iced` GUI) and an embedded web server daemon (`Axum` over WebSockets).

### Key Technology Stack
* **Language & Runtime:** Rust (latest stable), Tokio async runtime (`full` features)
* **Desktop UI:** `Iced` (v0.14+) with native rendering (`wgpu` / `winit`)
* **Web Server Daemon:** `Axum` (with `ws`, `tokio`, `http1` features)
* **Static Asset Embedding:** `rust-embed` (embeds static frontend assets into single compiled binary)
* **FileSystem Watching:** `notify` (v6+) monitoring `.git/` directory changes
* **CLI Engine:** `clap` (v4+ with `derive` macro support)
* **Serialization:** `serde` & `serde_json`
* **Logging/Tracing:** `tracing` & `tracing-subscriber`

---

## 2. Directory & Module Hierarchy

```text
.
├── Cargo.toml               # Project manifest and workspace settings
├── ARCHITECTURE.md          # System architecture, guidelines, and module breakdown
├── TASKS.md                 # Step-by-step roadmap for AI implementation
├── Notes.md                 # Design notes and comparative stack evaluations
├── web/                     # Web UI source files (embedded at build time)
│   └── dist/                # Pre-built HTML/CSS/JS frontend assets
└── src/
    ├── main.rs              # Entrypoint parsing CLI args and routing execution mode
    ├── git/                 # Git Engine subsystem
    │   ├── mod.rs           # Git CLI command execution logic and status queries
    │   ├── types.rs         # Core data models (RepoState, GitStatus, FileChange, GitAction)
    │   └── watcher.rs       # Debounced workspace file-system monitoring (.git/ directory)
    ├── server/              # Embedded Axum Web Server subsystem
    │   ├── mod.rs           # Axum router setup, shared AppState initialization
    │   ├── websocket.rs     # Real-time WebSocket connection handling and JSON protocol dispatch
    │   └── static_files.rs  # Embedded asset server using rust-embed
    └── ui/                  # Native Desktop GUI subsystem (Iced)
        ├── mod.rs           # Main Iced application entry and run loop
        ├── state.rs         # Application state and message dispatch enum
        └── components/      # UI Layout components
            ├── header.rs    # Branch selector, Push, Pull, and Fetch controls
            ├── staging.rs   # Split-view un-staged/staged file tree list
            ├── commit.rs    # Commit summary/description input form
            └── history.rs   # Scrollable commit log and revision list
```

---

## 3. Core Subsystems & Module Breakdown

### 3.1 CLI Entrypoint & Bootstrapping (`src/main.rs`)
* **`main.rs`**: Parses command-line arguments via `clap` (`--headless`, `--port`, `--path`).
* **Dual Execution Modes**:
  * **Headless Mode (`--headless`)**: Boots Tokio runtime and starts only the Axum WebSocket server daemon. Ideal for remote Linux boxes or SSH tunnels.
  * **GUI Mode (Default)**: Boots Tokio runtime, spawns the Axum server on a background task (`localhost:8080`), and launches the native `Iced` desktop window on the main thread.

### 3.2 Core Git Engine & Data Types (`src/git/`)
* **`src/git/types.rs`**: Strictly typed Rust representations of repository states.
  * `GitStatus`: Enum (`Modified`, `Untracked`, `Renamed`, `Deleted`, `Staged`).
  * `FileChange`: Struct tracking file path, `GitStatus`, and staging state.
  * `CommitInfo`: Struct holding commit hash, author, message, and timestamp.
  * `RepoState`: Aggregated state holding active branch, branches list, file changes, and commit history.
  * `GitAction`: Unified action enum (`Stage`, `Unstage`, `Commit`, `Push`, `Pull`, `CheckoutBranch`, `Revert`).
* **`src/git/mod.rs`**: Invokes local `git` CLI commands (`git status`, `git commit`, etc.) and captures standard error output (`stderr`) as custom structured `GitError`.
* **`src/git/watcher.rs`**: Monitors `.git/index`, `.git/HEAD`, and working directory changes using the `notify` crate with a 200ms debouncer to avoid event thrashing.

### 3.3 Axum Web Server & Embedded Assets (`src/server/`)
* **`src/server/mod.rs`**: Configures Axum router containing HTTP routes (`/health`) and WebSocket upgrade routes (`/ws`).
* **`src/server/websocket.rs`**: Handles bidirectional real-time communication. Converts inbound WebSocket JSON payloads into `GitAction` commands and broadcasts outbound updated `RepoState` payloads across all connected browser clients.
* **`src/server/static_files.rs`**: Reads compiled web bundle files directly from memory using `#[derive(RustEmbed)]` to serve web UI clients without external disk dependencies.

### 3.4 Native Desktop GUI (`src/ui/`)
* **`src/ui/state.rs`**: Defines the top-level `GritApp` struct implementing `iced::Application` / `iced::Element`. Tracks local view state and handles message processing.
* **`src/ui/components/`**: Modular Iced widget trees:
  * **`header.rs`**: Branch switcher dropdown and repository sync controls (Push / Pull / Fetch).
  * **`staging.rs`**: Dual list view for unstaged vs. staged changes with interactive action buttons.
  * **`commit.rs`**: Multi-line commit text entry form with "Commit" action submission.
  * **`history.rs`**: Visual scrollable commit log rendering commit metadata.
* **Async Task Handlers**: Heavy Git operations (`push`, `pull`, `fetch`) are offloaded to Tokio tasks via `iced::Task` / `iced::Command` to guarantee zero main UI thread blocking.

---

## 4. Primary Data & Event Flow

### System Startup (`main.rs`)
```
main.rs
  ├── Parse CLI Flags (clap: --headless, --port, --path)
  ├── Initialize Git Engine & Directory Watcher (.git/)
  ├── Spawn Tokio Background Runtime
  │     └── Axum Web Server (localhost:8080)
  │           ├── GET /health -> HTTP 200
  │           ├── GET /ws     -> Upgrades to WebSocket
  │           └── GET /*      -> Serves static assets via rust-embed
  └── IF NOT --headless:
        └── Launch Iced GUI Engine (Main Thread)
```

### File System Watcher & Real-Time Sync Loop
```
File System Event (.git/ index/HEAD modified)
  │
  ├──> notify::Watcher triggers debounced refresh event
  │
  ├──> Git Engine re-queries repository state (get_repository_status)
  │
  ├──> Broadcaster sends updated RepoState JSON payload via Axum WebSocket
  │      └── All connected browser clients update UI components instantly
  │
  └──> Iced Subscription receives local event
         └── Native desktop GUI state refreshes seamlessly
```

### Git Action Dispatch Flow
```
User Interaction (Desktop GUI Button OR Web Browser WS Message)
  │
  ├──> Dispatches GitAction (e.g., Stage("src/main.rs"))
  │
  ├──> Executed via std::process::Command calling local `git` CLI
  │
  ├──> Command succeeds / fails (returns GitResult<()>)
  │
  └──> Directory Watcher detects filesystem update -> Triggers Sync Loop
```

---

## 5. Architectural Invariants & Key Rules

1. **Single-Binary Portability**: No external static files, node runtimes, or client side daemons must be required to run the application in release mode. All web UI assets must be embedded into the binary executable at compile time using `rust-embed`.
2. **Unified Data Structures**: Both native `Iced` components and `Axum` WebSocket endpoints must consume identical data models defined in `src/git/types.rs`.
3. **Non-Blocking UI Thread**: The main thread handling `Iced` GUI rendering must never perform blocking I/O or execute long-running `git` processes directly. All `git` execution must run through async Tokio tasks or background threads.
4. **Git CLI Delegation**: Rely directly on standard local `git` CLI calls rather than complex native Git bindings (`git2-rs`/`libgit2`) to ensure full user SSH key, gpg signature, and local `.gitconfig` compatibility.

---

## 6. How to Extend

### Adding a New Git Operation
1. Add a new variant to `GitAction` in `src/git/types.rs`.
2. Implement the command execution logic in `src/git/mod.rs`.
3. Update `src/server/websocket.rs` to handle parsing the new JSON action variant.
4. Add corresponding UI triggers in `src/ui/components/` and `src/ui/state.rs`.

### Adding a New Web / Desktop UI Component
1. Build the widget layout in `src/ui/components/<component_name>.rs`.
2. Hook component messages into `Message` enum in `src/ui/state.rs`.
3. Mirror any corresponding web view elements in `web/` assets.

---

## 7. Validation & Testing

* **Build & Unit Verification**:
  ```bash
  cargo check
  cargo test
  ```
* **Headless Execution Verification**:
  ```bash
  cargo run -- --headless --port 8080 --path /path/to/repo
  ```
* **Full Dual-Mode Verification**:
  ```bash
  cargo run -- --path /path/to/repo
  ```
* **Release Packaging Verification**:
  ```bash
  cargo build --release
  ```

---

## 8. Key Files Quick Reference

| File | Purpose |
|------|---------|
| `Cargo.toml` | Project manifest and dependency declarations |
| `src/main.rs` | CLI argument parser and mode execution router |
| `src/git/types.rs` | Core Rust data models and state types |
| `src/git/mod.rs` | Git CLI process invocation and error wrapping |
| `src/git/watcher.rs` | Debounced file system watcher monitoring `.git/` changes |
| `src/server/mod.rs` | Axum router configuration and HTTP route definitions |
| `src/server/websocket.rs` | WebSocket JSON message protocol and broadcaster |
| `src/server/static_files.rs` | Embedded memory-served static web frontend assets |
| `src/ui/state.rs` | Top-level Iced desktop application state and messages |
| `src/ui/components/` | Native desktop widget layout panels |
| `TASKS.md` | AI-agent implementation task list and roadmap |
| `Notes.md` | Architectural comparison and design decisions |
