# Implementation Tasks: Project Actions — Script Discovery & Launch

> **Goal:** Scan `root/`, `scripts/`, and `tools/` of every open repository for executables and surface them in a new **Actions** section (dropdown picker + Run). Launch is fire-and-forget (`spawn` + drop, no process tracking). Discovery rides the existing watcher → refresh → `RepoState` plumbing so new/deleted executables appear automatically. All logic lives in one isolated module (`src/actions.rs`) guarded by a runtime kill-switch, so the feature can be disabled or excised cleanly for security.

## Phase 1 — Wire Format & Types

- [x] Add `ScriptEntry { name: String, rel_path: String }` to `src/git/types.rs` (serde derives, `Display` showing name); add `scripts: Vec<ScriptEntry>` to `RepoState` (defaulting empty); extend the serialization round-trip test (1 file: `src/git/types.rs`)
- [x] Add `GitAction::RunScript(String)` variant (rel path payload); include it in the JSON round-trip test (1 file: `src/git/types.rs`)

## Phase 2 — Actions Module: Discovery

- [x] Create `src/actions.rs`: `pub const ENABLED: bool`, scan dirs constant (`"", "scripts", "tools"`), `pub fn discover(repo_path: &Path) -> Vec<ScriptEntry>` — non-recursive scan of the three dirs only, unix exec-bit detection, hidden files skipped, deterministic sort, cap at 32; unit tests: finds exec script in `scripts/`, skips non-executable + hidden + files in other dirs, empty when nothing found (2 files: new `src/actions.rs`, `src/main.rs` `mod actions;`)
- [x] Windows portability inside `discover`: treat `.bat/.cmd/.ps1/.exe` extensions as executable under `#[cfg(windows)]`, exec bit otherwise (1 file: `src/actions.rs`, cfg-gated compile check only)

## Phase 3 — Actions Module: Launcher

- [x] Add `pub fn launch(repo_path: &Path, rel_path: &str) -> Result<(), String>` to `src/actions.rs`: refuse when `!ENABLED`; reject paths that escape the repo root (`components()` containment check) or are not files; spawn detached (`stdin/stdout/stderr` → null, child handle dropped = orphaned to init, cwd = repo dir); test spawns a shell script that writes a marker file and asserts the marker appears (1 file: `src/actions.rs`)

## Phase 4 — Plumbing Into State & Execution

- [x] `src/git/mod.rs`: `get_repository_status` fills `RepoState.scripts` via `actions::discover` (empty vec when disabled); `execute_action` gains a `RunScript` arm delegating to `actions::launch`; extend an existing status test to assert script pickup (1 file: `src/git/mod.rs`)
- [x] WebSocket integration test: send `{"tab":0,"action":{"RunScript":"scripts/say.sh"}}` over WS against a repo containing that script and assert its side effect (marker file) appears — proves the daemon/web path end-to-end (1 file: `src/server/websocket.rs` tests)

## Phase 5 — Desktop UI (Actions Section)

- [x] Create `src/ui/components/actions.rs`: section view with a `pick_list` dropdown of discovered scripts plus a confirm row ("Run" / "Cancel") mirroring the nuke two-press pattern; hidden entirely when `scripts` is empty; register module in `src/ui/components/mod.rs` (2 files)
- [x] `src/ui/state.rs`: `Message::RunScriptSelected(ScriptEntry)` stores pending script on the active tab, `Message::CancelScript` clears it, confirmed selection dispatches `GitAction::RunScript` through the existing `run_action` (works unchanged in Embedded and Remote modes since it rides the `GitAction` wire format); render the panel in `repo_view`; unit tests: select sets pending, cancel clears, refresh/state updates don't resurrect stale pendings (1 file: `src/ui/state.rs`)

## Phase 6 — Docs & Verification

- [x] Document the Actions subsystem in `ARCHITECTURE.md` (module boundary, kill-switch, security posture: containment check, two-press confirm, no tracking) and add a one-line feature note to `README.md` (2 files)
- [x] Full verification: `cargo check --all-targets` zero warnings, `cargo test` green, removal audit — deleting `src/actions.rs` + the thin wiring lines compiles clean without orphaned references (no command)

## Phase 7 — Web Actions Section & Watcher Load Fix

- [x] Web UI script runner: `#script-runner` row (`<select>` + Run Script button) in the existing Actions section of `web/dist/index.html`; hidden when no scripts discovered (1 file)
- [x] `web/dist/app.js`: `renderScriptRunner(tab)` populates the dropdown from `RepoState.scripts` on every broadcast (preserving selection), Run button confirms exact rel_path then sends `{ RunScript: relPath }` for the active tab (1 file)
- [x] Watcher dedupe: drop the separate `.git/` recursive watch in `src/git/watcher.rs` — the root watch already covers it, halving event volume per repo (1 file)
- [x] Watcher churn filter: ignore `Access` events and events living only under `target`/`node_modules`/`__pycache__`/`.venv`/`venv` before debouncing; unit tests for the filter + a build-churn quiet test (1 file: `src/git/watcher.rs`)
- [x] Remove redundant per-tab GUI filesystem watchers in `src/ui/state.rs` — the daemon owns watching and broadcasts updates via `WebTabsSync` in both Embedded and Remote modes (1 file)

## Phase 8 — Launch Reliability & One-Click Run

- [x] `src/actions.rs` launch(): inherit stdout/stderr so script output is visible where Grit runs; `/bin/sh` fallback for shebang-less scripts on ENOEXEC; detached reaper thread replaces handle forgetting; test for the shebang-less path (1 file)
- [x] Desktop one-click run: remove two-press confirm (`pending_script`, `RunScriptConfirmed`, `CancelScript`) — dropdown selection dispatches `RunScript` immediately with a stale-entry guard; tests updated (2 files: `src/ui/state.rs`, `src/ui/components/actions.rs`)
- [x] Web one-click run: drop `confirm()` gate on the Run Script button (1 file: `web/dist/app.js`)
- [x] Case-insensitive scan dirs: `discover()` matches any root-level `scripts`/`tools` directory regardless of casing (`Scripts/`, `TOOLS/`, ...), keeping real names in rel paths for case-sensitive filesystems; test added (1 file: `src/actions.rs`)
- [x] Terminal-window launching: `launch()` runs scripts in the system terminal — probe order: `$TERMINAL` → `TERM_PROGRAM` → `/proc` ancestor detection → `xdg-terminal-exec`/`xdg-terminal` → installed `.desktop` TerminalEmulator entries (via `freedesktop-desktop-entry`) → DE preferences → known-emulator table → unknown desktop entries (positional) → direct-spawn fallback + `GRIT_NO_TERMINAL` test hook; keep-open payload showing exit status; tests for payload/PATH lookup/desktop parsing (1 file: `src/actions.rs`)

## Phase 9 — Audit Fixes: Latent Bug & Robustness

- [x] Fix remote-mode `CloseTab` silently dropped: pass `Some(id)` (the extracted `tab.id`) instead of `None` in the remote branch of `Message::CloseTab` in `src/ui/state.rs`; strengthen `remote_mode_close_tab_waits_for_server_echo` with a server-side assertion in `src/server/websocket.rs` tests that the daemon receives the id and closes the correct tab (2 files: `src/ui/state.rs`, `src/server/websocket.rs`)
- [x] Poison-tolerant `TabRegistry.write_lock`: replace `lock().unwrap()` in `src/server/registry.rs` with `unwrap_or_else(PoisonError::into_inner)` so a panicking writer cannot permanently wedge the daemon; unit test simulating a poisoned guard (1 file: `src/server/registry.rs`)
- [x] `get_file_pair` in `src/git/mod.rs`: stop masking read failures with `unwrap_or_default()` — return `Err` when blob/worktree reads fail; adjust callers + tests (1 file)
- [x] Timestamp hardening in `src/git/mod.rs`: replace silent epoch-0 fallbacks for unparseable dates with propagated errors where signatures allow; regression test for malformed timestamp input (1 file)

## Phase 10 — Documentation Reconciliation [HIGH]

- [x] `AGENTS.md`: fix default headless port 8080→**5000**; extend §2 directory map with `src/actions.rs`, `src/shared_config.rs`, `src/server/registry.rs`, `src/ui/remote.rs`, `src/ui/components/{actions,diff}.rs`; describe `watcher.rs` as recursive repo-root watching, not `.git`-only (1 file)
- [x] `ARCHITECTURE.md` startup flow: correct that `boot()` returns `(AppState, refresh_rx)` and does NOT run the sync loop — sync loop spawns inside `run_server`, wired by `run()`; document background initial refresh so clients may connect before git scans finish (1 file)
- [x] `ARCHITECTURE.md` subsystem accuracy: watcher = single persistent `watch_reconciler` with one recursive root watcher per unique path; §3.3 registry gains `write_lock`, `revision: AtomicU64` (stale-frame suppression), `next_log_seq`, and `WebTab.log` wire field; §3.1/§4 document `display_available()` auto-headless fallback + `create_listener` SO_REUSEADDR; note `WebState.active` is daemon-side truth ignored by desktop `apply_sync` (1 file)
- [x] `README.md`: correct Nuke wording — in-place `fetch origin` + `reset --hard origin/<branch>` + `clean -fdx`; it does NOT delete or re-clone (1 file)
- [x] `NOTES.md`: annotate remaining port-8080 mentions (~lines 29/50/146/150) as historical (1 file)

## Phase 11 — Duplication Removal

- [x] Shared `epoch_millis()`: keep one definition in `src/git/mod.rs` (pub(crate)) and reuse from `src/server/registry.rs`, deleting the duplicate body (2 files)
- [x] Create `#[cfg(test)]` support module `src/test_support.rs` (registered in `src/main.rs`) hosting shared `init_repo`, `connect_with_retry`, `recv_state`; reconcile diverged constants to one policy (100 retries × 50ms poll, 20s receive timeout) (2 files)
- [x] Migrate `src/git/mod.rs` tests to shared `test_support::init_repo`, removing the local copy (1-2 files)
- [x] Migrate `src/server/mod.rs` tests to shared helpers, removing local duplicates (1 file)
- [x] Migrate `src/server/websocket.rs` tests to shared helpers, removing local duplicates (1 file)
- [x] Input validation dedupe: single validator fn in `src/git/types.rs` (blank-name/tab rules incl. exact error strings) consumed by `src/ui/state.rs` (2 files)
- [x] Consume the same validator from `src/server/websocket.rs`, deleting its verbatim copy (1-2 files)
- [x] Extract shared error-bar widget fn in `src/ui/state.rs` covering both duplicated sites incl. the `from_rgb(0.9, 0.25, 0.25)` literal (1 file)
- [x] Parameterize the staged/unstaged porcelain parse loops in `src/git/mod.rs` into one helper (1 file) — bonus: fixed latent rename-parsing bug (`R100\told\tnew` previously yielded tab-mangled path + fallback status); covered by unit test + end-to-end `git mv` test
- [x] Collapse the four repeated remote-dispatch patterns in `src/ui/state.rs` into one helper method (1 file)
- [x] Add `TabRegistry::clamp_active_index` helper in `src/server/registry.rs`; use it at both desktop clamp sites in `src/ui/state.rs` (2 files)
- [x] Factor the `files_handler`/`commit_handler` common skeleton and hoist the zeroed `CommitSummary{}` fallback literal (×3) into one constructor in `src/server/mod.rs` (1 file)

## Phase 12 — Structure & Complexity Refactors

- [x] Thin out `GritApp::update` in `src/ui/state.rs`: extract the deepest arms (`OpenNewRepo`, `CloseTab`, `WebTabsSync`) into private handler methods (1 file)
- [x] `get_commit_summary` in `src/git/mod.rs`: extract stat-parsing helpers to cut the 5-level nesting (1 file)
- [x] `spawn_terminal` in `src/actions.rs`: extract the terminal-probe list builder from the non-macOS branch (1 file)
- [x] `src/git/mod.rs`: merge/derive `placeholder_command` from the `execute_action` match so the 19-arm mirror cannot drift (1 file)
- [x] Flatten `handle_websocket` (`src/server/websocket.rs`) and `sync_loop` (`src/server/mod.rs`) with early-return guards to nesting ≤3 (2 files)

## Phase 13 — Dead Code, Warnings & Magic Numbers

- [x] Remove inert `pub const ENABLED` kill-switch + unreachable branches in `src/actions.rs` and its reference in `src/git/mod.rs`; align ARCHITECTURE.md Actions wording that still claims a kill-switch (2-3 files)
- [x] Drop `HealthResponse.status` (always `"ok"`) in `src/server/mod.rs` after confirming `web/dist/app.js` never reads it (≤2 files)
- [x] Fix compiler warning: unused `initial` variable in test code `src/server/websocket.rs` (~line 850) (1 file)
- [x] Delete dead `.browser-actions button` + `:hover` rules from `web/dist/style.css` (1 file)
- [x] `web/dist/app.js`: remove overwritten first `commit-push-btn.onclick` assignment, fix ragged indentation (~957-974), split fused `});//#endregion` (~1045) (1 file)
- [x] Named consts in `src/git/mod.rs`: inline history/log depth `"n", "50"` (1 file)
- [x] Named consts in `src/server/mod.rs`: broadcast capacity 128, daemon probe 500ms (both sites), listen backlog 1024 (1 file)
- [x] Named consts in `src/ui/state.rs`: mpsc `channel(100)` ×2, window size 960×680 (1 file)
- [x] Named consts in `src/actions.rs`: `/proc` ancestor-walk cap `0..16`, ENOEXEC `raw_os_error() == Some(8)` (1 file)

## Phase 14 — Reclone Action (Nuke Repurposed)

- [x] Rename `GitAction::Nuke` → `GitAction::Reclone` in `src/git/types.rs`; update the JSON round-trip test (1 file)
- [x] Replace `nuke_repo` with `reclone_repo` in `src/git/mod.rs`: capture `git remote get-url origin` first (refuse without a remote), require `.git` to exist, `remove_dir_all`, then `git clone <url> <path>`; log a synthetic `rm -rf` entry; update `placeholder_command`; tests: reclone adopts remote branch layout (stray branch/dirty/untracked all vanish) + refuses repos without origin, leaving them untouched (1 file)
- [x] Watcher reset plumbing: `AppState.watcher_resets` channel; `boot()` hands the receiver to `watch_reconciler`, which drops the stale watch on reset so it respawns over the fresh clone; `dispatch_and_refresh` sends the reset after a successful Reclone (2 files: `src/server/mod.rs`, `src/server/websocket.rs`)
- [x] Integration test `reclone_respawns_the_filesystem_watcher`: full daemon boot, watcher proven live pre-reclone, Reclone over WS, post-reclone filesystem change must still broadcast (1 file: `src/server/mod.rs`)
- [x] Desktop UI renames: `Message::ReclonePressed`, `RepoTab.reclone_armed`, header button "Reclone"/"Confirm Reclone?" with `reclone_style` (2 files: `src/ui/state.rs`, `src/ui/components/header.rs`)
- [x] Web UI: `#reclone-btn` in `web/dist/index.html` with destructive tooltip; `web/dist/app.js` confirm dialog spelling out data loss, sends `{ "Reclone": null }` over the wire (2 files)
- [x] Docs: README Reclone wording (delete + fresh clone, data-loss caveat); ARCHITECTURE.md GitAction list, `reclone_armed` field, header description, and watch_reconciler reset-channel note (3 files)
- [x] Full verification: `cargo check` and `cargo check --features desktop` zero warnings, `cargo test` green including 3 new reclone tests (no command)

## Phase 15 — Live Streaming Log Output

- [x] Changes-heading font fix: `#overview` moved into the section `<h2>` keeps its muted grey but must not inherit the heading's bold weight (`web/dist/style.css`: `.section-title .muted { font-weight: normal; }`) (1 file)
- [x] Streaming git runner in `src/git/mod.rs`: `ProgressSink` type + thread-local installed by `execute_action_logged(..., progress)`; `run_streamed()` pipes stdout/stderr through per-stream reader threads appending to shared buffers, pushing combined-output snapshots on first content then at most every 150 ms (`STREAM_FLUSH_INTERVAL`); final transcript byte-shape unchanged vs the blocking path; plain `Command::output()` used when no sink is installed (1 file)
- [x] Registry live-update API: `TabRegistry::update_log_output(tab_id, seq, output)` revises only `Running` entries, skips no-op snapshots, ignores unknown tab/seq, rides existing `modify()` broadcast path; unit test covers revise/no-op/sealed/unknown cases (1 file)
- [x] Dispatcher wiring: `dispatch_and_refresh` builds the sink from the placeholder's seq and passes it into `execute_action_logged`; streaming snapshots now reach every client mid-command (1 file)
- [x] Tests: `streaming_progress_receives_live_output` (push-to-bare emits ≥1 non-empty snapshot while transcript stays intact); existing callers pass `None` (1-2 files)
- [x] Docs: ARCHITECTURE.md registry section notes the streaming channel; TASKS.md Phase 15 recorded (2 files)
- [x] Full verification: `cargo check`, `cargo check --features desktop`, `cargo test` all green (125 tests)
- [x] Realtime fixes after manual testing: `--progress` forced on push/pull/fetch argv (git suppresses progress meters on pipes); stream readers switched from line reads to fixed-size chunks with `\r`→`\n` normalization so `\r`-redrawn progress meters stream live instead of arriving as one blob at exit; preview test expectations updated (1 file)
- [x] Live-delivery fix: `handle_websocket` awaited dispatch inline in the per-connection select loop, so a client's own slow command blocked draining `broadcast_rx` and every streaming frame arrived at exit. Inbound actions now run on a spawned sequential worker (per-client ordering preserved) while the loop forwards broadcasts; worker drains its queue on disconnect so transcripts still land (1 file: `src/server/websocket.rs`)
- [x] Regression test `running_entry_reaches_issuing_client_mid_action`: sleeping `pre-commit` hook + CommitAll over WS; the `Running` placeholder must reach the issuing connection well inside the hook sleep, then be sealed by the final transcript (126 tests total) (1 file)


