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

* **`src/main.rs`**: Entry point parsing CLI parameters via a hand-rolled parser in the same file (`--headless`, `--port`, `--path`, `--help`, `--version`). Boots background Tokio tasks and conditionally launches the `Iced` main thread GUI.
* **`src/actions.rs`**: Script discovery (`discover()`) and terminal-window launching (`launch()`) for one-click script runs; probes `$TERMINAL`/`TERM_PROGRAM`/`/proc` ancestors/`xdg-terminal-exec`/hand-rolled `.desktop` entries with a `GRIT_NO_TERMINAL` test hook.
* **`src/shared_config.rs`**: Cross-subsystem configuration shared between the native UI and the web daemon. `GritConfig` holds `tabs` and `host` (the daemon's bind host; `serde(default)` is load-bearing — without it a config written before the field existed parses as no tabs at all). All writes to `config.json` go through `config_write_lock()`, since tab persistence and the settings page both read-modify-write it.
* **`src/settings.rs`**: The LAN settings model — where each app's `config.json` lives, reading/writing the `host` field while preserving fields grit does not own, the restart baseline, and the `/proc` port→pid lookup used to restart the sibling daemons.
* **`src/folio.rs`**: Best-effort auto-launcher for the `folio` file-explorer daemon (`FOLIO_BIN`, `FOLIO_PORT` = 4000, sibling of `krust.rs`). Powers the web UI's files dock. Folio binds the host from its own `config.json` (`host`, default `127.0.0.1`); the settings page writes it.
* **`src/git/`**: Git engine subsystem.
  * **`types.rs`**: Shared data models (`RepoState`, `FileChange`, `GitStatus`, `GitAction`) — single source of truth for both UI events and WebSocket JSON payloads.
  * **`mod.rs`**: Invokes local `git` CLI subcommands using `std::process::Command`.
  * **`sync.rs`**: Remote-sync probe for the out-of-date badge. `check_remote_sync(repo)` compares a live `git ls-remote <remote> refs/heads/<branch>` against the cached `refs/remotes/<upstream>` and returns `SyncOutcome` (`InSync` / `OutOfDate` / `NoRemote` / `Unreachable`). Read-only by construction — it never fetches, never merges, and bypasses `execute_action_logged` so `ls-remote` never shows up in a tab's transcript. `run_with_timeout` is a 20s watchdog that sets `GIT_TERMINAL_PROMPT=0` + `GIT_SSH_COMMAND=ssh -oBatchMode=yes`, kills the whole process group, and bounds the pipe drain. Failures are typed (`SyncErrorKind::Unreachable` / `TimedOut` / `Local`): transport errors stay retryable, local errors settle at once. Offline must never produce a badge.
  * **`watcher.rs`**: Watches the repository root recursively (a single recursive watch also covers `.git/`) using `notify` with a 200ms debouncer.
* **`src/krust.rs`**: Best-effort auto-launcher for the `krust` web terminal daemon on startup. `krust_is_up()` TCP-probes `127.0.0.1:3000`; `find_krust_binary()` resolves `$KRUST_BIN` first, then scans `$PATH` for `krust`/`krust.exe`; `ensure_krust()` spawns it detached if it is down and available. Never fatal — missing binary just leaves the terminal dock buttons hidden. Krust binds the host from its own `config.json` (`host`, default `127.0.0.1`); its `HOST`/`PORT` env overrides were removed so the file is authoritative, and a non-loopback host makes it accept any WebSocket origin.
* **`src/server/`**: Embedded Axum web server subsystem.
  * **`mod.rs`**: Sets up HTTP endpoints, background refresh loops, and WebSocket routing. Also owns `sync_ticker` (see below).
  * **`websocket.rs`**: Processes inbound WebSocket actions and broadcasts state updates to connected clients. Calls `AppState::request_sync_check()` right after the first snapshot is sent — that `AtomicBool::swap` plus `Arc<Notify>` wake-up is what starts the sync ticker, which stays dormant (zero probes) until a client actually connects.
  * **`registry.rs`**: `TabRegistry` / `WebState` shared workspace: tab allocation, revision counters for stale-frame suppression, broadcast fan-out, log sequencing.
  * **`static_files.rs`**: Serves embedded static web assets using `rust-embed`.
* **`src/ui/`**: Native desktop GUI built on `Iced`.
  * **`state.rs`**: Primary application state and message dispatch system.
  * **`remote.rs`**: Remote-mode client (HTTP/WS) used by the desktop GUI to talk to an external daemon and receive sync updates.
  * **`components/`**: View panels (`header.rs`, `staging.rs`, `commit.rs`, `history.rs`, `actions.rs`, `diff.rs`).
* **`web/dist/`**: THE ONLY web UI source — five hand-maintained native ES modules (`common.js` core, `diff.js`, `terminal.js` dock/view routing, `settings.js` the LAN settings page, `app.js` entry) plus `index.html` / `style.css`, with no package.json, bundler, or build step; embedded at compile time via `rust-embed`. Contains the per-tab view dock (tab name = Dashboard, `F` = files/folio, `T` = terminal/`term-1`; `F`/`T` hide while their daemon is down) and all client-side view routing (`activeView`, `showView()`, `?view=` deep-link). Log is **not** a dock button — it is a collapsible rollup section in `index.html`. JS edits are reviewed manually; `node` is available (see §4).

### Remote-sync ticker — key facts
* **What it is**: a 30s `tokio::select!` over an interval + `Arc<Notify>` in `src/server/mod.rs`. Each pass probes only *unsettled* tabs (per-tab settled set in `TabRegistry`, outside `WebState`, pruned on tab close), staggers the git calls by `tab.id % 8` × 250ms, and publishes `RepoState::out_of_date`.
* **Trigger**: first WebSocket client connect per daemon process. `run_server` spawns the ticker already dormant — an idle daemon with no clients never touches the network.
* **Settling**: `InSync` / `OutOfDate` / `NoRemote` settle a tab; `Unreachable` retries every 30s until a 10-minute ceiling, then settles with the badge **off**. A successful `Fetch`/`Pull` calls `reset_settled` to re-arm. Never auto-pulls.
* **Badge surfaces**: web `↑` on the tab chip (`renderTabBar` in `web/dist/common.js`, styled by `#tabs .tab-badge-sync`). The iced desktop deliberately has no badge — the field is available at `src/ui/state.rs:505` if that changes.

### LAN settings page — key facts
* **What it is**: the gear at the far right of the tab bar opens a settings view (`activeView === "settings"`, deep-linkable as `?view=settings`) with one LAN toggle per app. `web/dist/settings.js` talks to `GET /settings`, `POST /settings` and `POST /settings/restart` — a plain HTTP surface, deliberately separate from the WebSocket `GitAction` protocol.
* **Config is the source of truth**: each app keeps a `host` in `~/.config/bitshift/<app>/config.json` (`127.0.0.1` off, `0.0.0.0` on). Grit writes all three, read-modify-writing the sibling files as opaque JSON so fields it does not own (krust's `port`/`allowed_origins`, folio's `last_app`) survive.
* **Applying it**: grit rebinds its own listener live (`ServerControl` plus the supervisor loop in `server::run`); a failed switch falls back to the last-good host so the daemon never goes dark, and `GET /settings` reports the error. Krust and folio only read their config at startup, so the page offers a restart button that finds each pid by the port it listens on (`/proc`), SIGTERMs then SIGKILLs, and respawns via `ensure_*`. Restarting krust kills every live terminal shell.
* **LAN on relaxes origins**: grit and krust accept any WebSocket origin once bound non-loopback; folio is already `CorsLayer::permissive()`.
* **Dock URLs follow the page host**: `KRUST_BASE`/`FOLIO_BASE` are built from `location.hostname`, so a LAN browser reaches the services on the server rather than on itself.

### krust (web terminal) integration — key facts
* **What it is**: Grit embeds the external `krust` web-terminal daemon as a single dock view (`term-1`), an `<iframe>` pointing at `http://localhost:3000/?s=<session>&dir=<repo>` with the xterm terminal library. Requires krust (`~/Projects/krust`, port 3000, `KRUST_PORT` env override) to be installed and running; Grit auto-starts it if missing (see `src/krust.rs`), otherwise the `T` buttons stay hidden.
* **Sessions are per-repo**: a session id is derived from the active repo path at first activation — `grit-{repoScope(repoPath)}-term-{n}` where `repoScope` is `<basename>-<hash36>` (or `root` when no repo). Switching active repos rebinds the frame to that repo's session and fires `GET /reset?session_id=<old>` to recycle the abandoned one.
* **Reset**: there is no on-screen reset button; switching active repos rebinds the `term-1` iframe and fire-and-forget calls krust's `GET /reset?session_id=<old>` → kills the abandoned PTY child and drops the session. krust must be built from `~/Projects/krust` HEAD (release binary at `~/Projects/krust/target/release/krust`) — the `/reset` endpoint and `?dir=` cwd support are recent additions, NOT in older installed copies.
* **krust CORS**: krust serves `.layer(CorsLayer::permissive())`, so all fetch-based probes (`/`, `/reset`) and WebSocket upgrades work cross-origin from `localhost:5000`.
* **Environment test hooks**: `KRUST_BIN` (binary override — wins over PATH), `KRUST_PORT` (krust bind port, default 3000). Grit constants: `KRUST_BASE`, `KRUST_PROBE_MS=5000` (probe cadence), `KRUST_SESSIONS` map in `common.js`.

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
* **JS Syntax Checking**: `web/dist/` is five native ES modules (`common.js` / `diff.js` / `terminal.js` / `settings.js` / `app.js`) with no bundler, linter, or build step. `node` v22 **is** present on the dev box — syntax-check each edited module with `cp web/dist/<f>.js /tmp/x.mjs && node --check /tmp/x.mjs` (the `.mjs` copy makes node treat it as ESM). Optionally still run the python3 tokenizer pass (script in `/tmp`) that balances `(){}[]`, checks quote/template-literal nesting (`'`/`"`/`` ` `` incl. `${}`), flags duplicate function definitions, and reports per-line numbers. Review large edits manually.
* **Rollup ARIA Conventions**: All `.section-title` elements must initialize with `role="button"`, `tabindex="0"`, `aria-expanded` (true for expanded sections, false for `.collapsed` ones), and `aria-controls` pointing to the corresponding `.section-body` id. When adding new rollup sections to `index.html`, follow this pattern. Arrow direction is handled via CSS `::before` pseudo-element (`▼` open, `▶` closed) — do not embed arrow characters directly in the HTML or JS.
* **Web Changes Need a Rebuild**: `web/dist/*` is embedded at compile time — the running `cargo run`/release binary serves the OLD assets until Grit is rebuilt and restarted. The same applies to krust client changes via `~/Projects/krust`.
* **Don't Touch Running Daemons**: The user restarts krust/grit himself (usually from `~/.local/bin`). Never kill/replace/restart running instances unless explicitly asked; instead state what must be restarted to pick up new builds.
