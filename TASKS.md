# Maintainability Tasks: Refactors & Test Hardening

> **Goal:** Make the codebase easier to work with — smaller, well-named modules,
> exhaustive wiring guarantees, and real (not placeholder) tests. Each task touches
> 1-2 files (plus whatever test modules move with the code) and keeps
> `cargo check` / `cargo check --features desktop` / `cargo test` green at every step.
>
> The previous plan (audit-fix checklist) is fully shipped; see "Previously Completed" at the bottom.

---

## Test Hardening

- [x] `src/git/watcher.rs` `watcher_emits_debounced_events` (~142-163): replace the no-op body (`let _ = first; assert!(true);`) with a real assertion that a debounced refresh event is received (and ideally that two rapid writes coalesce into ≤2 events)
- [x] `src/git/watcher.rs` `debounce_windows_are_160ms_apart` (~165-168): delete this fake test (only asserts `DEBOUNCE_MS >= 100`); fold a genuine timing assertion (e.g. rapid burst yields one event after ~`DEBOUNCE_MS`) into the strengthened test above
- [x] `src/git/mod.rs`: add test `every_action_variant_is_previewable` — an exhaustive `match action` over **all 31** `GitAction` variants (mirror the full list from `git_action_round_trips_through_json` in `types.rs:318`) asserting each either yields a non-empty `placeholder_command` (table-backed) or is explicitly listed as bespoke; no wildcard arm, so adding a variant to the enum forces an update here (compile-time wiring check). Found `SearchHistory`/`OpenExternal`/`OpenWith`/`DeleteFile`/`RenameFile` currently emit no placeholder — test enumerates them as explicitly bespoke.

## Module Split: `src/git/mod.rs` (2157 lines → core + 4 domain modules)

Move each group below into a new file with its adjacent tests; in `mod.rs` replace the moved bodies with `pub use <file>::*;` so all `crate::git::…` call sites (server/mod.rs, websocket.rs, ui/state.rs, integration tests) keep compiling unchanged. Re-run cargo check/test after each move.

- [x] Create `src/git/status.rs`: `get_repository_status`, `get_current_branch`, `list_branches`, `list_remote_branches`, `list_stashes`, `split_stash_subject`, `stash_files`, `list_changes`, `parse_status_line`, `parse_name_status`, `parse_epoch` + their tests (2 files)
- [x] Create `src/git/history.rs`: `get_history`, `search_history`, `get_commit_summary`, `parse_shortstat`, `file_status_label`, `parse_commit_files`, `HISTORY_LIMIT`, `SEARCH_HISTORY_LIMIT` + their tests (2 files)
- [x] Create `src/git/files.rs`: `list_dir`, `search_files`, `get_file_content`, `get_file_pair`, `get_file_diff` (keep its `cfg(any(test, feature = "desktop"))`), `is_image_path`, `mime_for_path`, `safe_join`, and **both** cfg-gated `list_apps_for_mime` variants + their tests (2 files)
- [x] Create `src/git/actions.rs`: `action_argv`, `placeholder_command`, `execute_action`, `execute_action_logged`, `reclone_repo` + the action tests (`*round_trip`, `commit_all_stages…`, `placeholder_command_previews…`, `multi_command_actions…`, `table_backed_actions…`, `every_action_variant_is_previewable`, stash/reclone/checkout/tag/branch tests) (2 files)
- [x] After all four moves: confirm `mod.rs` keeps only the shared core (`run`, `run_streamed`, `git_command`, `combine_streams`, `truncate_output`, `record_entry`/`record_synthetic`, `describe_command`, `notify_progress`, `epoch_millis`, `ProgressSink`, `MAX_LOG_OUTPUT_BYTES`, `STREAM_FLUSH_INTERVAL`) + the re-export lines (0 new files)

## Module Split: `src/server/mod.rs` (1556 lines → core + handlers)

- [x] Create `src/server/handlers.rs`: move `health_handler`, `browse_handler`, `files_handler`, `commit_handler`, `filetree_handler`, `filecontent_handler`, `filesearch_handler`, `apps_handler`, `shorten_path`, `tab_scoped_git_call`, the query structs (`FilesQuery`, `CommitQuery`, `BrowseQuery`, `FileTreeQuery`, `FileContentQuery`, `FilesearchQuery`), and the route-handler integration tests with them; re-export via `pub(crate) use handlers::*;` in `mod.rs` (2 files: `handlers.rs`, `mod.rs`)
- [x] `src/server/mod.rs`: keep `boot`, `run_server`, `sync_loop`, `watch_reconciler`, `build_router`, `create_listener`, server-core tests (health sync loop, daemon probe, ws upgrade, close-last-tab, streaming, reclone-watcher) (0 new files)

## Frontend Organization

- [x] `web/dist/app.js`: add section-banner comments at each logical region (constants, WebSocket/connection, generic UTIL helpers incl. `escapeHtml`/`showDropdown`/`resetPreview`, tab bar, staging, commit, history, branches, file browser, log, notifications) so the single embedded file stays navigable without a bundler (1 file)

## Verification

- [x] Final full pass: `cargo check`, `cargo check --features desktop`, `cargo test` clean; confirm `git_action_round_trips_through_json` and `every_action_variant_is_previewable` cover all 31 variants

---

## Previously Completed

> Audit-fix checklist (TASKS.md, 2026-09-05) — all shipped, `cargo test` 148 passed.

- [x] Delete `CONTEXT.md` orphan doc
- [x] `shared_config.rs:227` dead conditional → `let active = 0;`
- [x] `web/dist/index.html` dead `file-preview` id removed
- [x] `web/dist/app.js` dead `browserEl` removed
- [x] `web/dist/app.js` dead `tree-arrow` class removed
- [x] `src/ui/state.rs` `tab_button_style` hovered branch collapsed
- [x] `src/git/mod.rs` `SKIP` const hoisted to one `SKIP_DIRS`
- [x] MIME/ext lists unified (`TEXT_EXTS`/`IMAGE_EXTS` in `shared_config.rs`; `mime_for_path`/`is_image_path` reuse them)
- [x] `websocket.rs` file-op + search handlers extracted behind `resolve_tab_repo`
- [x] `websocket.rs` `OpenWith` uses shell-aware `split_shell_words`/`expand_command` (preserves `%f`)
- [x] `git::safe_join` traversal guard; raw `filecontent` rejects escapes (400)
- [x] `get_file_pair` propagates `Err` on HEAD read failure
- [x] `get_commit_summary`/`stash_files` best-effort behavior documented
- [x] `app.js` `showDropdown`, `resetPreview`, debounced branch filter, named constant block, `escapeHtml` on 4 innerHTML sites, stale comment fixed
- [x] `ARCHITECTURE.md` routes + Startup diagram list new handlers; `Notes.md` → `NOTES.md`
- [x] `AGENTS.md` directory map already complete (no change needed)