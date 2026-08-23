# Codebase Audit Summary

**Audit Target:** `/home/bronson/Projects/grit`
**Date:** 2026-08-23

---

## Executive Summary

Grit is a healthy, well-structured Rust workspace with zero TODO debt, zero debug leftovers, no orphan files, and no `#[allow(dead_code)]` crutches. The primary risks are one latent functional bug (remote-mode tab close is silently dropped), significant documentation drift in `AGENTS.md`/`ARCHITECTURE.md`/`README.md` describing an older startup flow and port default, and moderate copy-paste duplication concentrated in test helpers and a few handler pairs. One compiler warning exists (unused test variable).

## Key Metrics

- **Unused/Orphan Files:** 0
- **Dead Functions/Exports:** 2 (inert `ENABLED` kill-switch const; unreachable duplicate onclick assignment in app.js)
- **Dead CSS Selectors:** 1 group (`.browser-actions`)
- **Commented-Out Code / Debug Logs:** 0
- **Open TODOs/FIXMEs:** 0
- **Compiler Warnings:** 1 (unused variable `initial`, src/server/websocket.rs:850, test code)

---

## Findings & Recommendations

### 1. Latent Bug (found during audit)

| File Path | Type | Details | Recommended Action |
| :--- | :--- | :--- | :--- |
| `src/ui/state.rs:198-203` | Functional Bug | Remote/connect-mode `CloseTab` extracts `let id = tab.id;` then calls `Self::remote_op(port, None, GitAction::CloseTab)`. Daemon requires the id (`websocket.rs:157-164`: "CloseTab ignored: no tab id provided"), so clicking a tab × in remote mode silently does nothing. Test `remote_mode_close_tab_waits_for_server_echo` only asserts local state. | Pass `Some(id)`; add server-side assertion test |

### 2. Unused Files & Dead Code

| File Path | Type | Details | Recommended Action |
| :--- | :--- | :--- | :--- |
| `web/dist/style.css:411,421` | Dead CSS | `.browser-actions button` (+ `:hover`) — class referenced nowhere in index.html/app.js | Delete |
| `web/dist/app.js:963-969` | Dead JS | `commit-push-btn.onclick` assigned twice; first handler (Commit + Push) overwritten by `{CommitAllPush}` at 975-979; ragged indent 957-974 suggests merge leftover | Remove first assignment, fix indentation |
| `web/dist/app.js:1045` | Formatting | Fused `});//#endregion` on one line | Split into two lines |
| `src/actions.rs:18` | Inert API | `pub const ENABLED: bool = true` unreferenced kill-switch; branches at :31,:106 unreachable-in-practice | Remove or wire up |
| `src/server/mod.rs:35` | Inert Field | `HealthResponse.status` always `"ok"` | Drop field or report real status |
| `registry.rs:28` (`WebState.active`) | Dual Source of Truth | Maintained Rust-side (open_repo_tab/remove_tab/health_handler) but desktop `apply_sync` ignores it — two competing "selected tab" notions | Document or unify |

### 3. Code Structure & Complexity Smells

| File Path | Issue | Context / Severity | Suggested Refactor |
| :--- | :--- | :--- | :--- |
| `src/ui/state.rs:176-434` | High Complexity | `GritApp::update` = 259 lines, ~30 match arms, deepest chains 4 levels (OpenNewRepo 244-286, CloseTab 194-221, WebTabsSync 415-431) | Extract per-arm handlers |
| `src/git/mod.rs:393-492` | High Complexity | `get_commit_summary` = 100 lines, 5-level nesting at 417-436 | Extract stat-parsing helpers |
| `src/actions.rs:352-443` | Long Function | `spawn_terminal` non-macOS branch = 92 lines | Extract probe-list builder |
| `src/git/mod.rs:494-581` + `168-191` | Manual Sync Hazard | `execute_action` (19 arms) mirrored 1:1 by `placeholder_command` (19 trivial arms) | Generate placeholder from action enum or merge |
| `src/server/websocket.rs:123-153`, `mod.rs:320-342` | Nesting Depth 4 | `handle_websocket`, `sync_loop` | Early-return guards |

### 4. Duplication

| Location | Details | Recommended Action |
| :--- | :--- | :--- |
| `git/mod.rs:64-69` vs `registry.rs:264-269` | `epoch_millis()` identical bodies | Share one helper |
| Test helpers ×3 modules | `init_repo` triplicated (git/mod.rs:634, server/mod.rs:526, websocket.rs:249); `connect_with_retry`/`recv_state` duplicated server/mod.rs:552-594 vs websocket.rs:275-319 **with diverged constants** (40×50ms+5s vs 100×50ms+20s) | Move to shared test-support module |
| `server/mod.rs:61-95` vs `103-152` | `files_handler` vs `commit_handler` same skeleton; zeroed `CommitSummary{}` fallback literal ×3 inside commit_handler (111-149) | Factor common handler shape |
| `ui/state.rs:248-254` vs `websocket.rs:37-43` | Input validation duplicated verbatim incl. error strings | Single validator |
| `git/mod.rs:230-264` | staged/unstaged parse loops near-duplicates | Parameterize |
| `ui/state.rs:506-511` vs `568-573` | Error-bar widget dup incl. color literal `from_rgb(0.9, 0.25, 0.25)` | Extract widget fn |
| `ui/state.rs` ×4 sites | remote-op dispatch pattern repeated (199-203, 259-268, 419-429, 441-444) | Helper method |
| Active-index clamp ×3 | registry.rs:175-177, ui/state.rs:171-173, 215-217 | Shared fn on registry |

### 5. Robustness

| File Path | Issue | Recommendation |
| :--- | :--- | :--- |
| `src/server/registry.rs:98` | `write_lock.lock().unwrap()` panics on poisoned mutex (only bare unwrap in non-test code) | `unwrap_or_else(\|p\| p.into_inner())` |
| `src/git/mod.rs:386-391` | `get_file_pair` declares `Result` but `.unwrap_or_default()` twice — never errs; failed reads silently become empty file content | Return Err on failed reads |
| `src/git/mod.rs:324,401,472-473` | Timestamp parses silently fall back to epoch 0 | Surface parse failure |

### 6. Conventions Verified Clean

- **Mutex convention**: No violations — only std Mutex is intentional `TabRegistry.write_lock`; short sections, never across `.await`.
- Zero `console.*`/`dbg!`/commented-out code; all 31 app.js top-level functions used; no unused pub items besides those listed above.

### 7. Documentation Drift [HIGH]

| File Path | Stale Claim | Reality |
| :--- | :--- | :--- |
| `AGENTS.md:17` | Headless runs `localhost:8080` | Default port is **5000** (`main.rs:20`); README/ARCHITECTURE already say 5000 |
| `NOTES.md:29,50,146,150` | Port 8080 | Historical notes; clarify or annotate as historical |
| `ARCHITECTURE.md:124-125` | `boot()` runs sync loop | False — boot returns `(AppState, refresh_rx)`; sync_loop spawned in `run_server` (mod.rs:352-357), wired by `run()` (497-498) |
| `ARCHITECTURE.md:122-125,192-195` | Startup flow predates background initial refresh (mod.rs:444-455) — clients now connect before git scans finish | Update flow description |
| `ARCHITECTURE.md:123` | "spawns per-tab .git watchers" | Single persistent `watch_reconciler`, one recursive repo-root watcher per unique path (mod.rs:366-414, watcher.rs:28-30) |
| `ARCHITECTURE.md §3.3` | Registry described as watch + id allocator | Missing: `write_lock`, `revision: AtomicU64` (drives stale-frame suppression in sync_loop), `next_log_seq`; `WebTab` wire type includes `log: Vec<LogEntry>` |
| `ARCHITECTURE.md §3.1/§4` | GUI always launches without `--headless` | Undocumented `display_available()` fallback switches to headless daemon when DISPLAY/WAYLAND_DISPLAY unset (main.rs:36-43,82-86); `create_listener` SO_REUSEADDR also undocumented |
| `README.md:28` | "Nuke … re-clone the repository" | Inaccurate — runs fetch origin + reset --hard origin/<branch> + clean -fdx **in place** (git/mod.rs:187,558+) |
| `AGENTS.md §2` | Directory map incomplete | Missing shared_config.rs, server/registry.rs, actions.rs, ui/remote.rs, ui/components/{actions,diff}.rs; watcher described as `.git/`-only but watches repo root recursively |

### 8. Magic Numbers

| Location | Value | Suggestion |
| :--- | :--- | :--- |
| git/mod.rs:299-300 | History depth `"n", "50"` inline | Named const |
| server/mod.rs:28 | `broadcast::channel(128)` | Named const |
| server/mod.rs:470,479 | Daemon probe 500ms ×2 | Named const |
| server/mod.rs:509 | `listen(1024)` backlog | Named const |
| ui/state.rs:609,635 | mpsc `channel(100)` ×2 | Named const |
| ui/state.rs:760 | Window 960×680 | Named consts |
| actions.rs:239 | /proc walk cap `0..16` | Named const |
| actions.rs:176 | `raw_os_error() == Some(8)` (ENOEXEC) | Named constant |

---

## Top Priority Action Plan

1. **[High]** Fix remote-mode `CloseTab` bug: pass `Some(id)` in `src/ui/state.rs:203`; extend the existing remote-mode test to assert the daemon receives the id.
2. **[High]** Reconcile docs with reality: AGENTS.md port + directory map; ARCHITECTURE.md boot/sync_loop split, background refresh, watch_reconciler, registry fields (write_lock/revision/next_log_seq), WebTab.log, headless-display fallback, create_listener/SO_REUSEADDR; README.md Nuke wording.
3. **[Medium]** Poison-tolerant `write_lock` in registry.rs; dedupe test helpers into one module (reconciling diverged retry/timeout constants).
4. **[Medium]** Extract shared validation + error-bar widget; parameterize staged/unstaged parsing.
5. **[Low]** Delete dead CSS `.browser-actions`, duplicate commit-push-btn.onclick, fix fused endregion line, resolve unused `initial` warning, name the magic numbers.
