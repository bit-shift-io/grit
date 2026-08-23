# Architectural Notes: Grit Project

> **Historical document.** Written during early design; details below reflect
> the state at that time. In particular, all `localhost:8080` mentions predate
> the current default daemon port **5000** (`--port` still overrides it).

## 1. Context & Motivation
SourceGit is an excellent C#/AvaloniaUI desktop Git client with complex UI components (visual commit graphs, syntax-highlighted diffs, tabbed workspaces). When evaluating a port to a Rust-based stack, direct client-side web rendering faces severe browser sandbox constraints: browsers cannot execute local `git` CLI binaries, manage SSH keys, or access filesystem directories freely.

To bridge the gap between desktop performance and browser convenience, we designed **Grit**: a single-binary application combining a native Rust desktop GUI with an embedded local web server daemon.

---

## 2. Framework & Architecture Comparison

| Stack Strategy | Desktop Capability | Browser / Web Capability | Complexity / Footprint | Selected? |
| :--- | :--- | :--- | :--- | :--- |
| **Rust + Flutter** | Native rendering via Skia/Impeller | High sandbox friction (Wasm FS limitations) | Moderate setup; requires Dart/Rust FFI bindings | ❌ No |
| **Rust + Tauri v2** | Native OS window with Webview | High web compatibility (Monaco diffs, Canvas graphs) | Requires JS frontend build toolchain | ❌ Alternative |
| **Rust + Iced + Axum** | Pure Rust native app (`wgpu` / `winit`) | Served locally over WebSocket to any browser tab | Single Rust codebase; uniform state model | ✅ **Selected** |

---

## 3. Key Design Decisions

1. **Scope Reduction for V1:**
   * **Included:** Baseline Git operations (`stage`, `unstage`, `commit`, `push`, `pull`, `branch`, `revert`, history list).
   * **Deferred:** Complex multi-branch visual DAG render canvas, side-by-side Monaco code diffs, interactive rebase dialogs.
2. **Single-Binary Strategy:**
   * Uses `rust-embed` to embed Web assets (HTML/CSS/JS or Iced Wasm) directly into the Rust binary at compile time.
   * Eliminates multi-file installation or external daemon management.
3. **Dual Execution Modes:**
   * **GUI Mode (Default):** Spawns an Iced desktop window AND starts an Axum WebSocket background server on `localhost:8080`. *(historical port; now 5000)*
   * **Headless Mode (`--headless`):** Skips OS window creation entirely; runs as a background service ideal for headless Linux boxes or SSH tunnels.
4. **State Synchronization:**
   * Uses the `notify` crate to watch `.git/` directory changes.
   * State changes trigger WebSocket pushes to all connected web clients while updating Iced's UI state.




# Implementation Plan: Grit
**Single-Binary Cross-Platform Git Client (Native Desktop & Local Web UI in Rust)**

---

## 1. Executive Summary & Concept

**Grit** is a high-performance, single-binary Git client written in Rust. It delivers both a **Native Desktop UI** (via Iced and `wgpu`) and an integrated **Local Web UI** (via an Axum daemon and Web/Wasm interface) contained entirely within a single executable.

### Key Features
* **Core Git Workflow:** Focused on essential operations—`stage`, `unstage`, `commit`, `push`, `pull`, `branch`, `revert`, and basic history view.
* **Dual Execution Modes:**
  * **Desktop Mode (Default):** Launches a native OS window while serving the web interface locally on `localhost:8080`. *(historical port; now 5000)*
  * **Headless Mode (`--headless`):** Skips GUI creation and runs exclusively as a background daemon for browser access.
* **Single-Binary Deployment:** All static web assets/Wasm bundles are baked directly into the binary using `rust-embed`.

---

## 2. Architecture Overview

```text
                     +-------------------------------------------------+
                     |              Single Rust Binary                 |
                     |                                                 |
                     |   +-----------------------------------------+   |
                     |   |         Core Git Engine (Rust)          |   |
                     |   |  - std::process::Command / git2-rs      |   |
                     |   |  - Directory watcher (notify crate)     |   |
                     |   +-------------------+---------------------+   |
                     |                       |                         |
                     |          +------------+------------+            |
                     |          |                         |            |
                     |          v                         v            |
                     |  +---------------+       +------------------+   |
                     |  | Desktop GUI   |       | Axum Server      |   |
                     |  | (Iced / wgpu) |       | (Tokio Async)    |   |
                     |  +---------------+       +--------+---------+   |
                     +-----------------------------------|-------------+
                                                         | WebSocket / HTTP
                                                         v
                                                +------------------+
                                                | Browser / Wasm   |
                                                | (http://localhost|
                                                +------------------+
```

---

## 3. Recommended Project Structure

```text
grit/
├── Cargo.toml
├── build.rs                   # Pre-compiles Wasm frontend assets if needed
├── src/
│   ├── main.rs                # Entry point (CLI argument parsing, mode routing)
│   ├── git/
│   │   ├── mod.rs             # Git command wrappers (stage, commit, push, pull)
│   │   ├── watcher.rs         # Repository state & .git directory file watcher
│   │   └── types.rs           # Shared Git status data structures
│   ├── server/
│   │   ├── mod.rs             # Axum router & server setup
│   │   ├── websocket.rs       # Real-time WebSocket state streaming
│   │   └── static_files.rs    # rust-embed static file handlers
│   └── ui/
│       ├── mod.rs             # Iced desktop GUI application entry
│       ├── components/        # File list, commit box, branch header, history
│       └── state.rs           # UI state and event handlers
└── web/                       # Web interface assets (embedded into binary)
```

---

## 4. Phase-by-Phase Roadmap

### Phase 1: Core Engine & Data Types
1. Define shared data structures (`FileChange`, `GitStatus`, `CommitInfo`, `Action`).
2. Implement backend Git wrappers using `std::process::Command` (calling `git` CLI) or `git2-rs`.
3. Set up the `notify` crate to watch the working directory and `.git` folder for changes, triggering auto-refreshes.

### Phase 2: Native Desktop UI (Iced)
1. Build four core Iced layout panels:
   * **Staging View:** Unstaged vs Staged file lists with click-to-stage/unstage.
   * **Commit Box:** Message input field + Commit button.
   * **Action Header:** Current branch badge, Push, Pull, Fetch buttons.
   * **History Log:** Scrollable commit list.
2. Wire async actions (`git push`, `git pull`) using Iced async tasks (`Task` / `Command`).

### Phase 3: Axum Local Server & Embedded Assets
1. Create Axum HTTP handlers for resting state and repository manipulation.
2. Set up WebSocket channel to push live Git status updates to connected clients.
3. Use `rust-embed` to bake the static web directory into the binary.

### Phase 4: Web Interface & CLI Dual-Mode
1. Build the lightweight browser UI (or compile Iced Wasm frontend).
2. Implement CLI flag parsing (`clap` crate):
   * `./grit` -> Launches Desktop GUI + background server.
   * `./grit --headless` -> Starts local server only.
3. Test dual access (modifying repo via native GUI updates browser UI in real time).

---

## 5. CLI Usage Quick Reference

```bash
# Build unified release executable
cargo build --release

# Run as Native Desktop App (also serves Web UI at http://localhost:8080)  # historical port; default is now 5000
./target/release/grit

# Run as Headless Daemon (Web UI only)
./target/release/grit --headless --port 8080  # historical example; default port is 5000
```
