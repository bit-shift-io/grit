# AGENTS.md — AI Agent Context & Execution Guidelines (Grit)

> **Purpose:** This document provides task context, architectural conventions, execution principles, and control mechanisms for AI agents operating on the Grit codebase. Read this alongside `ARCHITECTURE.md` before making structural or code changes.

---

## 1. Project Context & Environment

Grit is a fast, native, single-binary Git client written in Rust. It runs as both a native desktop UI (`Iced`) and an embedded web daemon (`Axum` over WebSockets).

### Essential Execution Commands

```bash
cargo check                     # Quick compiler check (web-only build, default)
cargo test                      # Run unit and integration tests
cargo run -- --headless         # Run headless server daemon mode (localhost:5000)
cargo build --release           # Build web-only single-binary distribution executable

cargo check --features desktop  # Check with the desktop GUI included
cargo run --features desktop -- --path .   # Run full desktop GUI + background server
cargo build --release --features desktop   # Build the desktop + web UI binary
```

> **Build profiles:** The **default** build is web-only — it compiles just the
> embedded web daemon and excludes `iced`/`rfd` and all GUI-only code paths.
> Pass `--features desktop` to compile the native `Iced` desktop UI on top.

---

## 2. Directory Map & Component Roles

* **`src/main.rs`**: Entry point parsing CLI parameters via `clap`. Boots background Tokio tasks and conditionally launches the `Iced` main thread GUI.
* **`src/actions.rs`**: Script discovery (`discover()`) and terminal-window launching (`launch()`) for one-click script runs; probes `$TERMINAL`/`TERM_PROGRAM`/`/proc` ancestors/`xdg-terminal-exec`/desktop entries with a `GRIT_NO_TERMINAL` test hook.
* **`src/shared_config.rs`**: Cross-subsystem configuration shared between the native UI and the web daemon.
* **`src/git/`**: Git engine subsystem.
  * **`types.rs`**: Shared data models (`RepoState`, `FileChange`, `GitStatus`, `GitAction`) — single source of truth for both UI events and WebSocket JSON payloads.
  * **`mod.rs`**: Invokes local `git` CLI subcommands using `std::process::Command`.
  * **`watcher.rs`**: Watches the repository root recursively (a single recursive watch also covers `.git/`) using `notify` with a 200ms debouncer.
* **`src/krust.rs`**: Best-effort auto-launcher for the `krust` web terminal daemon on startup. `krust_is_up()` TCP-probes `127.0.0.1:3000`; `find_krust_binary()` resolves `$KRUST_BIN` first, then scans `$PATH` for `krust`/`krust.exe`; `ensure_krust()` spawns it detached if it is down and available. Never fatal — missing binary just leaves the terminal dock buttons hidden.
* **`src/server/`**: Embedded Axum web server subsystem.
  * **`mod.rs`**: Sets up HTTP endpoints, background refresh loops, and WebSocket routing.
  * **`websocket.rs`**: Processes inbound WebSocket actions and broadcasts state updates to connected clients.
  * **`registry.rs`**: `TabRegistry` / `WebState` shared workspace: tab allocation, revision counters for stale-frame suppression, broadcast fan-out, log sequencing.
  * **`static_files.rs`**: Serves embedded static web assets using `rust-embed`.
* **`src/ui/`**: Native desktop GUI built on `Iced`.
  * **`state.rs`**: Primary application state and message dispatch system.
  * **`remote.rs`**: Remote-mode client (HTTP/WS) used by the desktop GUI to talk to an external daemon and receive sync updates.
  * **`components/`**: View panels (`header.rs`, `staging.rs`, `commit.rs`, `history.rs`, `actions.rs`, `diff.rs`).
* **`web/dist/`**: THE ONLY web UI source — hand-maintained `index.html` / `style.css` / `app.js` with no package.json or build step, embedded at compile time via `rust-embed`. Contains the left view dock (Dashboard/`F`/`L` + krust terminal views) and all client-side view routing (`activeView`, `showView()`, `?view=` deep-link). JS edits are reviewed manually — there is **no node/deno/bun** on the dev box for syntax checking; use the python3 brace/quote tokenizer (see §4).

### krust (web terminal) integration — key facts
* **What it is**: Grit embeds the external `krust` web-terminal daemon as two dock views (`term-1` / `term-2`), each an `<iframe>` pointing at `http://localhost:3000/?s=<session>&dir=<repo>` with the xterm terminal library. Requires krust (`~/Projects/krust`, port 3000, `KRUST_PORT` env override) to be installed and running; Grit auto-starts it if missing (see `src/krust.rs`), otherwise the `T` buttons stay hidden.
* **Sessions are per-repo**: a session id is derived from the active repo path at first activation — `grit-{repoScope(repoPath)}-term-{n}` where `repoScope` is `<basename>-<hash36>` (or `root` when no repo). Switching active repos rebinds the frame to that repo's session and fires `GET /reset?session_id=<old>` to recycle the abandoned one.
* **Reset**: the `Reset` overlay button on each terminal view calls krust's `GET /reset?session_id=<sid>` → kills the PTY child and drops the session, then reloads the iframe with a `&r=<ts>` cache-buster. krust must be built from `~/Projects/krust` HEAD (release binary at `~/Projects/krust/target/release/krust`) — the `/reset` endpoint and `?dir=` cwd support are recent additions, NOT in older installed copies.
* **krust CORS**: krust serves `.layer(CorsLayer::permissive())`, so all fetch-based probes (`/`, `/reset`) and WebSocket upgrades work cross-origin from `localhost:5000`.
* **Environment test hooks**: `KRUST_BIN` (binary override — wins over PATH), `KRUST_PORT` (krust bind port, default 3000). Grit constants: `KRUST_BASE`, `KRUST_PROBE_MS=5000` (probe cadence), `KRUST_SESSIONS` map in `app.js`.

---

## 3. Core Development Conventions & Invariants

1. **Strict Type Safety across Boundaries**: Data structures in `src/git/types.rs` serve as the single source of truth. Both native `Iced` UI events and WebSocket JSON payloads must map cleanly to `GitAction` and `RepoState`.
2. **Never Block the Main UI Thread**: `Iced` event handlers must remain snappy. Offload all filesystem and `git` command executions to background Tokio tasks or `iced::Command` / `iced::Task` handlers.
3. **Delegation to Local Git CLI**: Do NOT introduce `git2-rs` or `libgit2` C-bindings. Executing local `git` processes preserves user SSH keys, GPG signing configurations, and local `.gitconfig` custom setups.
4. **Single-Binary Release Integrity**: The project must always compile into a self-contained executable. Do not introduce runtime dependencies on loose external files or un-embedded assets.

---

## 4. Agent Operational Rules & Gotchas

* **Async Mutex Usage**: When locking state across `Axum` routes or Tokio tasks, use `tokio::sync::Mutex` rather than `std::sync::Mutex` to prevent blocking runtime worker threads.
* **Debounced FS Events**: File system notifications can fire rapidly during operations like `git checkout`. Always route file changes through the debouncer in `src/git/watcher.rs` to avoid event storms and UI re-render flashing.
* **Error Propagation**: Return custom structured errors (`GitError`) from Git commands rather than panicking or unwrap calls, ensuring errors can be displayed gracefully in both GUI and Web interfaces.
* **Incremental Edits**: Keep PRs or code updates focused. Ensure `cargo check` and `cargo test` pass cleanly after every modification.
* **JS Syntax Checking**: `web/dist/app.js` has no bundler or linter and no node/deno/bun on the dev box. After meaningful JS edits, run a python3 tokenizer pass (script in `/tmp`) that balances `(){}[]`, checks quote/template-literal nesting (`'`/`"`/`` ` `` incl. `${}`), flags duplicate function definitions, and reports per-line numbers. Review large edits manually.
* **Web Changes Need a Rebuild**: `web/dist/*` is embedded at compile time — the running `cargo run`/release binary serves the OLD assets until Grit is rebuilt and restarted. The same applies to krust client changes via `~/Projects/krust`.
* **Don't Touch Running Daemons**: The user restarts krust/grit himself (usually from `~/.local/bin`). Never kill/replace/restart running instances unless explicitly asked; instead state what must be restarted to pick up new builds.
