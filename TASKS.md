# Implementation Tasks: Grit Connect-Mode Desktop GUI

> **Goal:** When a Grit headless daemon is already running (e.g. systemd on boot), the desktop GUI must attach to it as a WebSocket client instead of spawning its own parallel server — eliminating dual-writer races on `config.json`. With no daemon running, behavior stays exactly as today (embedded server + local registry).

## Phase 1 — Daemon Probe

- [x] Add `pub async fn is_daemon_running(port: u16) -> bool` to `src/server/mod.rs`: GET `http://127.0.0.1:{port}/health` with ~500ms timeout; true only on HTTP 200 (task 1 file: `src/server/mod.rs`)
- [x] Unit-test the probe in `src/server/mod.rs` tests: true against a spawned test router, false against a closed port (task 1 file: `src/server/mod.rs`)

## Phase 2 — Mode Selection

- [x] Move `tokio-tungstenite` from `[dev-dependencies]` to `[dependencies]` in `Cargo.toml` (needed by runtime WS client)
- [x] In `src/main.rs`, GUI branch: call `is_daemon_running(cli.port)`; introduce `enum GuiMode { Embedded(crate::server::registry::TabRegistry), Remote { port: u16 } }`; pass it to `ui::state::run(...)`; headless branch unchanged (1–2 files: `src/main.rs`, `src/ui/state.rs` signature)

## Phase 3 — Shared Wire Format

- [x] In `src/server/websocket.rs`, extract `pub fn encode_client_message(tab: Option<usize>, action: &ServerAction) -> String` (or equivalent) so desktop reuses the exact `{"tab":…,"action":…}` JSON the browser sends — no format drift; unit-test round-trip against `ClientMessage` deserialization (1 file: `src/server/websocket.rs`)

## Phase 4 — Remote Client Module

- [x] Create `src/ui/remote.rs`: `pub async fn run_client(port: u16, tx: tokio::sync::mpsc::UnboundedSender<Vec<crate::server::registry::WebTab>>)` — connect `ws://127.0.0.1:{port}/ws`, parse each text frame's `tabs` array, forward via `tx`, reconnect with backoff on drop/close (new file; add `mod remote;` to `src/ui/mod.rs`)
- [x] Create `pub async fn send_op(port: u16, msg_json: String)` in `src/ui/remote.rs`: one-shot connect → send → close, for fire-and-forget ops (same file)

## Phase 5 — Desktop Rewire

- [x] `src/ui/state.rs`: store `GuiMode` on `GritApp`; subscription becomes mode-aware — Embedded keeps existing watch-channel bridge, Remote spawns `run_client` feeding the same `Message::WebTabsSync` (1–2 files: `src/ui/state.rs`, `src/ui/remote.rs`)
- [x] `src/ui/state.rs` update(): in Remote mode route OpenNewRepo / CloseTab / git actions through `send_op(encode_client_message(...))` instead of local registry ops / local git execution; Embedded path untouched (1 file: `src/ui/state.rs`)
- [x] Startup explicit-`--path` open: Embedded fires local op as today; Remote waits for first sync then sends `NewTab` once (guard flag) (1 file: `src/ui/state.rs`)

## Phase 6 — Tests & Docs

- [x] Integration test: spawn real daemon (existing `boot` + `run_server` test helpers), drive `run_client`, assert `WebTabsSync` payloads arrive; send a `CloseTab` via `send_op` and observe empty tab list broadcasts (1 file: `src/ui/remote.rs` tests or `src/server/mod.rs` tests)
- [x] Full `cargo check --all-targets` zero warnings + `cargo test` green
- [x] Document connect-mode in `ARCHITECTURE.md` (§ modes) and one-line note in `README.md`
