# Codebase Audit Summary

**Audit Target:** `grit` (`/home/bronson/Projects/grit`)
**Date:** 2026-10-01

---

## Executive Summary

Grit is in healthy shape: both the web-only default build and the `desktop` feature build pass `cargo check` with **zero warnings**, all 2026-08-23 findings have been resolved, and tests are rich and well-organized. The one functional risk is in the web UI — three commit buttons collapse into effectively two behaviors, and neither of the two "staged" buttons actually commits only staged changes (both run `git add -A`), with no staged-only wire action existing in the `GitAction` enum. Secondary risks are moderate duplication (SKIP/ext lists, file-op handlers, frontend dropdown/preview helpers), a handful of robustness nits (silent `unwrap_or_default` paths, an unguarded raw file read), and one orphaned document (`CONTEXT.md`) that is a glossary for an unrelated game project.

A comparative audit of Tally's web UI (which recently underwent its own web refactor) surfaced additional hardening opportunities: ARIA attribute gaps on rollup elements, arrow-direction inconsistency, CSS duplication risk, and missing tests for initial rollup state. Grit's single-page architecture avoids Tally's fragmentation problem, but the rollup component still needs the same accessibility and consistency hardening to prevent drift as Grit grows.

---

## Key Metrics

- **Unused/Orphan Files:** 1 (`CONTEXT.md` — unrelated game glossary)
- **Dead Functions/Exports:** 0
- **Commented-Out Code / Debug Logs:** 0
- **Open TODOs/FIXMEs:** 0
- **Compiler Warnings:** 0 (both `cargo check` and `cargo check --features desktop`)
- **Rollup ARIA Coverage:** Partial — `aria-expanded` missing on initial load
- **Rollup Arrow Consistency:** Mixed — ▲ vs ▶ depending on context

---

## Findings & Recommendations

### 1. Unused Files & Dead Code

| File Path | Type | Details | Recommended Action |
| :--- | :--- | :--- | :--- |
| `CONTEXT.md` | Orphan File | 33KB glossary of terms for an unrelated 2D platformer (kill zone, drawbridge, GameAPI, NPC cage objectives…) — zero relation to Grit | Remove or move out of the repo |
| `src/shared_config.rs:227` | Dead Code | `let active = if tabs.is_empty() { 0 } else { 0 };` — both branches yield `0`, variable value never varies | Replace with plain `let active = 0;` |
| `web/dist/index.html:79` | Dead DOM id | `id="file-preview"` never referenced by id in `app.js` (only descendants `preview-header`/`preview-content` are used) | Remove the id or query it deliberately |
| `web/dist/app.js:107` | Dead Local | `browserEl` declared but never used | Remove |
| `src/ui/state.rs:567-570` | Dead Branch | `tab_button_style` `hovered` branch sets `palette.background` identical to the non-hovered else branch | Collapse to a single branch |
| `web/dist/app.js:771` | Dead Class | `tree-arrow` class added to DOM but no CSS rule exists; JS also renders `.log-entry.success` with no matching rule | Remove dead class or add the intended styles |

### 2. Code Structure & Complexity Smells

| File Path | Issue | Context / Severity | Suggested Refactor |
| :--- | :--- | :--- | :--- |
| `src/git/mod.rs:779` + `826` | Duplication | `SKIP` const duplicated verbatim across `list_dir` and `search_files` | Hoist to one shared const |
| `src/git/mod.rs` (`mime_for_path`, `is_image_path`) vs `src/shared_config.rs:55-68` | Duplication | Extension/MIME classification lists maintained in two modules | Unify in a single home (e.g. `shared_config`) |
| `src/server/websocket.rs:227-314` | Duplication | Four near-identical file-op handlers (`OpenExternal`, `OpenWith`, `DeleteFile`, `RenameFile`) share the same tab-lookup + response pattern | Extract a shared file-op helper |
| `src/server/websocket.rs` (`OpenWith`) | Robustness | `exec.split_whitespace().next()` destroys `%f`-style field codes and splits quoted paths incorrectly | Use a shell-aware word splitter that preserves placeholders |
| `src/server/mod.rs` (`filecontent` raw) | Security Nit | `raw=true` does `std::fs::read` on the joined path with no containment/traversal guard (only the tab's `repo_path_for` check) | Reuse `git/mod.rs::list_dir`-style component guard before reading |
| `src/git/mod.rs` (`get_file_pair`) | Robustness | `original` built with `unwrap_or_default()` — a failed `git show HEAD:path` silently becomes empty content; function never returns `Err` in practice | Propagate real `Err` so the UI can distinguish "untracked" from "read failure" |
| `src/git/mod.rs` (`get_commit_summary`, `stash_files`) | Robustness | Multiple `git show` calls each `unwrap_or_default()`; failures are silently flattened | Aggregate and surface failures |
| `web/dist/app.js:864-905` vs `909-965` | Duplication | Two dropdown builders share a ~44-line identical tail (positioning, scroll listeners, outside-click closer) | Extract a `showDropdown` helper |
| `web/dist/app.js:636-637` / `938-939` / `988-989` | Duplication | Preview-reset boilerplate (`innerHTML=""` + muted placeholder) repeated three times | Extract `resetPreview()` |
| `web/dist/app.js:808` | Dead Code | `toggleDir` re-caches `dirChildren` already cached by `fetchFileTree` at 653 | Drop the redundant re-assignment |
| `web/dist/app.js:1711-1715` | Consistency | Branch filter isn't debounced while file/history filters are | Debounce it |
| `web/dist/app.js` (various) | Magic Numbers | Reconnect 500/5000ms, scroll 48px, diff marker 0.35, LCS DP cap 4000000, debounces 200/300/150ms, dropdown offset 2 | Name as constants at top of file |

### 3. Comments & Technical Debt

| File Path | Type | Snippet / Context | Recommendation |
| :--- | :--- | :--- | :--- |
| `web/dist/app.js:975` | Stale Comment | `// Clear filter when switching tabs` — actually runs on every `renderFileBrowser` call | Fix comment or guard the reset |
| `web/dist/app.js:833,847,851,967` | XSS/Self-XSS | Paths, `data.error`, and alt text inserted via unescaped `innerHTML` | Build DOM nodes / escape HTML entities; never inject user-controlled strings raw |
| `ARCHITECTURE.md` §3.4 + Startup diagram | Doc Drift | Route list documents only `/health /ws /files /commit /browse /*` — missing newer `/filetree /filecontent /filesearch /apps` handlers | Add the four handlers to the routes section |
| `ARCHITECTURE.md:122-125` | Doc Nit | References `Notes.md` (actual file is `NOTES.md`) | Fix casing |
| `AGENTS.md` §2 | Doc Drift | Directory map omits `shared_config.rs`, `git/watcher.rs`, `server/registry.rs`, `server/websocket.rs`, `server/static_files.rs`, `ui/remote.rs`, `ui/components/*` | Extend the map |
| `NOTES.md`, `TASKS.md` | Historical | File-browser feature notes; all TASKS.md phases `[x]`, all requirements shipped (incl. selectedFilePath + Edit button) | Mark clearly as historical, or archive |

---

### 4. Web UI Audit — Tally Comparison

A comparative audit of Tally's recent web refactor surfaced hardening opportunities for Grit's rollup component. Tally's refactor was driven by real defects; Grit can adopt the fixes proactively.

#### 4.1 ARIA Attribute Gaps (Critical)

Tally's rollup elements had no `aria-expanded` on initial load — screen readers reported undefined state for the actual loaded state. Grit's `toggleSection` / `wireRollup` logic initializes state reactively (on click), not at wire time.

**Fix:** Initialize ARIA on every rollup header at wire time:
```javascript
header.setAttribute('role', 'button');
header.setAttribute('tabindex', '0');
header.setAttribute('aria-expanded', 'true');   // initial state = expanded
header.setAttribute('aria-controls', body.id);
```

#### 4.2 Arrow Direction Inconsistency (High)

Tally used ▲ (up) for closed and ▶ (right) for open on some pages, and the opposite on others — a genuine split-brain, not a load-order bug. Grit's rollup arrow logic lives in one place, but the CSS class toggles (`collapsed` / not) must map to a single semantic convention.

**Fix:** Standardize on CSS-controlled arrows:
```css
.section-title .arrow::before { content: "▼"; }          /* open */
.section.collapsed .section-title .arrow::before { content: "▶"; }  /* closed */
```

#### 4.3 CSS Duplication Risk (Medium)

Tally duplicated `.hint` and `code` rules across `style.css`, and had unused `--grid-header-hover` in both colour schemes. Grit currently keeps all CSS in one file — this is fine for now, but splitting into logical modules (`base.css`, `components.css`, `diff.css`) prevents the duplication that creeps in as Grit grows.

**Fix:** No action needed yet; add a `CONTRIBUTING.md` note that new CSS rules go in the relevant module, not appended to `style.css`.

#### 4.4 Test Coverage for Rollup State (Medium)

Tally had zero tests pinning the initial collapsed state or the arrow glyph — all regressed silently. Grit's integration tests cover server state but not the initial DOM/ARIA of rollup elements.

**Fix:** Add a test (or manual checklist) that verifies:
- Rollups render expanded by default with `aria-expanded="true"`
- Arrow glyph points down (`▼`) when expanded, right (`▶`) when collapsed
- Clicking toggles both attributes in sync

#### 4.5 Card Component Semantics (N/A for Grit)

Tally's `.card` class served dual purposes (page link vs. expander) — a semantic ambiguity that caused cascade bugs. Grit does not use a `.card` component; its rollup `.section` pattern is clean and unambiguous. **No action needed** — Grit's single-page architecture avoids this entirely.

#### 4.6 Rollups as Sibling of `main` (N/A for Grit)

Tally's `#toolbar` was a rollup sibling of `main`, requiring special-case CSS to reach it. Grit's structure keeps all rollups inside `main` — no special casing needed. **No action needed.**

---

## Previously Reported — Now Resolved

All findings from the 2026-08-23 audit are verified fixed:

- **Web commit buttons** (`app.js:1576-1590`): were mislabeled — `commit-btn` staged everything (`CommitAll`) and `commit-push-btn` duplicated `stage-commit-push-btn`. Fixed 2026-09-05: new `CommitPush` action (commit staged + push) added end-to-end, `commit-btn` now sends staged-only `Commit`, `commit-push-btn` sends `CommitPush`.
- **CloseTab remote bug**: `ui/state.rs` now sends `close_tab_payload(Some(id))`; daemon requires the id; tests cover both directions.
- `ENABLED` kill-switch const removed from `actions.rs`; `.browser-actions` dead CSS removed; duplicate `commit-push-btn.onclick` assignment removed.
- Test helpers consolidated into `src/test_support.rs` with unified retry constants (100×50ms + 20s receive); `init_repo`/`connect_with_retry`/`recv_state` no longer triplicated.
- Single `epoch_millis`; poison-tolerant `TabRegistry.write_lock`; `HealthResponse` dropped the static `status` field in favor of real `tab_count`/`current_branch`/`change_count`.
- Docs reconciled: README Reclone description, ARCHITECTURE startup/boot/sync_loop/watch_reconciler descriptions, port 5000.

---

## Top Priority Action Plan

### Immediate (Critical)
1. **[High]** Add `aria-expanded`, `role="button"`, `tabindex="0"`, `aria-controls` to rollup headers at wire time — prevents screen-reader state mismatch.
2. **[High]** Standardize rollup arrow glyphs — `▼` open / `▶` closed via CSS only; remove any inline arrow characters from JS.

### Short-Term (High)
3. **[High]** Guard the raw `filecontent` read against path traversal; de-duplicate the SKIP const and the MIME/extension lists.
4. **[Medium]** Make silent failure paths loud: `get_file_pair` original, `get_commit_summary`, `stash_files` — return structured `GitError` instead of `unwrap_or_default()`.
5. **[Medium]** Add rollup initial-state test coverage (ARIA attributes, arrow direction, click toggle sync).

### Medium-Term (Medium)
6. **[Medium]** Surface-escape or DOM-build the four `innerHTML` injection points in `app.js`; extract the duplicated dropdown-builders and preview-reset helpers.
7. **[Low]** Remove dead code: `shared_config.rs:227` dead conditional, `file-preview` id, `browserEl`, `tree-arrow`/`.log-entry.success` styling, `tab_button_style` dead branch, and fix the v-like stale comment / magic-number naming in the frontend.

### Long-Term (Low)
8. **[Low]** Delete or relocate `CONTEXT.md`; add the four missing routes to `ARCHITECTURE.md` and extend the `AGENTS.md` directory map.
9. **[Low]** Consider CSS module split (`base.css`, `components.css`, `diff.css`) once the codebase grows past ~2000 lines.
