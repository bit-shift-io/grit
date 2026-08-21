# Codebase Audit Summary

**Audit Target:** `grit`
**Date:** 2026-08-21

---

## Executive Summary

The codebase is healthy after the single-writer refactor: no TODO/FIXME debt, no debug leftovers, no dead exports, and all UI components are wired. The main debt is **documentation rot** — `ARCHITECTURE.md` predates the tab-registry/single-writer redesign and misdescribes several modules — plus one **pass-through wrapper module** (`src/server/persistence.rs`) whose tests literally duplicate `shared_config.rs` tests. Production file sizes are reasonable once test modules are excluded.

## Key Metrics

- **Unused/Orphan Files:** 1 (`src/server/persistence.rs` — pure delegation, deletable)
- **Dead Functions/Exports:** 0 (2 over-visible helpers noted below)
- **Commented-Out Code / Debug Logs:** 0
- **Open TODOs/FIXMEs:** 0
- **Compiler warnings:** 1 (unused `use super::*`, inside the deletable module's tests)

---

## Findings & Recommendations

### 1. Unused Files & Dead Code

| File Path | Type | Details | Recommended Action |
| :--- | :--- | :--- | :--- |
| `src/server/persistence.rs` | Pass-through module | Only forwards `persist_web_state`/`restore_web_state` to `shared_config`; its 3 tests (`save_then_load_round_trips`, `load_missing_file_returns_empty`, `load_invalid_json_returns_empty`) are verbatim duplicates of tests already in `shared_config.rs` | Delete module + `mod persistence;` line; re-point `src/server/mod.rs` imports to `crate::shared_config`; keep `shared_config.rs` tests as the single copy. Removes the sole compiler warning too |

### 2. Code Structure & Complexity Smells

| File Path | Issue | Context / Severity | Suggested Refactor |
| :--- | :--- | :--- | :--- |
| `src/shared_config.rs` | Over-visible helpers | Low | `web_state_from_saved` (1 use) and `prune_dead_tabs` (2 uses) are called only within the module — demote to `fn` unless external use is planned |
| `src/git/mod.rs` | Long dispatcher | `execute_action` ≈71 lines / 40 match arms — Medium-Low | Idiomatic enum dispatch; optionally split into `action_staging.rs`/`action_sync.rs` if it keeps growing. No change required now |
| `web/dist/app.js` | Flat 783-line script | Medium-Low | Splitting into modules needs a bundler, conflicting with the embed-at-compile-time, zero-tooling constraint. Leave as-is; consider `//#region` markers if it grows past ~1000 lines |
| `src/ui/state.rs` | 1024 lines | Low | ~354 lines are tests; production core (~670) is cohesive state+update+view. Fine |

### 3. Comments & Technical Debt

| File Path | Type | Snippet / Context | Recommendation |
| :--- | :--- | :--- | :--- |
| `ARCHITECTURE.md` | **Stale document (High)** | Directory tree omits `src/shared_config.rs`, `src/server/registry.rs`, `src/server/persistence.rs`; §3.3 describes `websocket.rs` as GitAction-only dispatch (no `open_repo_tab`/`close_tab_by_id` tab ops, no `WebTabsSync` broadcast protocol); §3.4 says desktop persists config (it no longer does — server-side only); GUI port stated as `localhost:8080` but clap default is **5000**; `ui/mod.rs` described as "entry and run loop" (it is 3 module declarations; `run()` lives in `state.rs`) | Rewrite §2 tree and §§3.3–3.4 around the current design: **registry is the single mutation point**, desktop & web are both clients fed by `WebTabsSync`; persistence flows through `$XDG_CONFIG_HOME/bitshift/grit/config.json` via `shared_config.rs`; boot restores/heals config |
| `TASKS.md` | Historical roadmap | All items checked; describes original build phases | Keep as history or archive; not misleading |
| `NOTES.md`, `CLAUDE.md`, `GEMINI.md` | OK | Design rationale / pointers | No action |

---

## Top Priority Action Plan

1. **[High]** Update `ARCHITECTURE.md` to describe the post-refactor architecture (single-writer registry, `WebTabsSync` client model, shared config ownership, real file tree, port 5000).
2. **[Medium]** Delete `src/server/persistence.rs`; move its callers' imports to `crate::shared_config`; drop duplicated tests (fixes last warning).
3. **[Low]** Tighten visibility of `shared_config::{web_state_from_saved, prune_dead_tabs}`.
4. **[Low]** Optional: region markers in `app.js`; revisit `execute_action` split only on next growth.
