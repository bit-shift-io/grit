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


