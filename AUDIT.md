# Web UI Audit: Grit vs Tally

## Summary

Grit's web UI is functional and correct, but its `app.js` (1688 lines) is a single monolithic file that mixes concerns. Tally demonstrates a cleaner approach with modular JS files, better architectural documentation, and clearer separation of concerns. The refactor is worthwhile as an incremental improvement to maintainability, especially if the web UI will evolve further.

## Current State (Grit)

**Files:**
- `web/dist/app.js`: 1688 lines - everything in one file (WebSocket, state, rendering, diff/LCS, history, branches/stashes, terminals, etc.)
- `web/dist/style.css`: 663 lines - single stylesheet
- `web/dist/index.html`: Single SPA with all sections, uses `?view=` for routing (dashboard/files/term-1)
- Embedded via `rust-embed` at compile time, hand-maintained, no build step

**Strengths:**
- Good sectioning with clear headers (`// ======================================`)
- Solid state management: single-expansion invariant (`expanded`), revision tracking to suppress redundant renders, caching (`pairCache`, `commitCache`)
- Clean view switching with deep links and proper focus handling for iframes (krust/folio)
- Robust reconnection logic with exponential backoff, handles page visibility changes
- Well-structured folder browser for repo selection

**Weaknesses:**
- **Monolithic file**: All concerns mixed together (connection, DOM, business logic, algorithms). Hard to navigate and modify safely.
- **Size**: 1688 lines is large enough that changes in one area risk side effects in others.
- **Limited architectural documentation**: Fewer "why" comments explaining invariants compared to Tally. Design constraints aren't explicitly documented at file/module level.
- **Mixed responsibilities**: Diff/LCS algorithm (~150-200 lines), WebSocket handling, state management, and DOM rendering all co-located.
- **No module boundaries**: Everything shares global scope, making future testing or extraction harder.

## Comparison with Tally

Tally (`~/Projects/tally/web/dist/`):
- Modular split: `common.js` (126 lines) for shared utilities (`getJson`, `showNotice`, rollup wiring) + page-specific modules (`dashboard.js`, `forms.js`, `reports.js`, editor domain in `app.js`).
- Excellent architectural comments explaining *why* (e.g. "server is the only source of truth", "edits are staged", invariants documented upfront).
- CSS split (`base.css`, `grid.css`) by concern.
- Per-page JS modules loaded as needed.

Grit is a more complex SPA (live WebSocket state, tabs, terminals, file browser) so single HTML page is appropriate. But modularization is still beneficial.

## What to Adopt from Tally

1. **Shared utilities module** - Extract common helpers (like Tally's `common.js`) for reusability and single source of truth.
2. **Architectural documentation style** - Add file-level comments explaining key invariants and design decisions ("why", not just "what").
3. **Separation of concerns** - Split large self-contained areas (especially diff rendering, which is pure-ish logic) into focused modules.
4. **Clear module boundaries** - Even without bundlers, ES modules or at least logical separation improves maintainability.

## Recommended Refactoring Plan

**Priority: Incremental, low-risk changes that preserve behavior.**

### 1. Extract `common.js` (utils + shared helpers) - Low risk, high value
Create `web/dist/common.js` with shared utilities:
- DOM helpers and utilities (isBlank, sameValue equivalents if useful, or keep as-is)
- Section/rollup helpers (`toggleSection`, ARIA state management)
- URL update helpers (`updateUrlView`, `updateUrlTab`)
- Message sending helpers (`sendAction`, `sendRaw`)
- General utilities used across modules

### 2. Extract `diff.js` - Medium risk isolation, very high value
Largest self-contained chunk (~150-200 lines). Contains:
- `showDiff()`, `renderFilePair()`, `splitLines()`, `alignLines()`, `renderSideBySide()`, `renderDiffRow()`, `renderChunk()`

This is mostly pure logic + DOM construction. Extracting it makes the main file smaller and the complex LCS algorithm easier to reason about and modify.

### 3. Extract feature modules - Medium effort, high value
Split remaining concerns into focused modules. Suggested split (load in order or as ES modules):
- `core.js` - Constants, WebSocket (openSocket, reconnect, sendRaw), global state, `handleStateMessage`, initial setup
- `tabs.js` - Tab bar rendering (`renderTabBar`), add-repo form (`setupAddRepoForm`, folder browser), tab management
- `views.js` - View switching (`showView`, `setView`, `updateUrlView`, `updateUrlTab`), dock logic, external service probes (krust/folio)
- `changes.js` - Staging/commit UI (`appendChangeRow`, `renderScriptRunner`, commit actions), discard operations
- `diff.js` - As above
- `history.js` - History rendering (`renderHistory`), search, recent commits
- `branches-stashes.js` - Branches/stashes rendering and event handlers
- `log.js` - Log rendering (`renderLog`), clear log
- `terminals.js` - krust/folio frame management (`ensureKrustFrame`, `ensureFolioFrame`, session handling)

### 4. Update HTML to load modules - Required if splitting
Options:
- **ES modules** (cleanest): Change to `<script type="module" src="/main.js"></script>` and use `import` statements. Works in modern browsers, no build step needed.
- **Ordered script tags**: Load in dependency order (common.js first, then core, then others, then app/main). Simpler if avoiding modules, but globals-based.

ES modules are preferred and align better with maintainability.

### 5. Add architectural comments - Low effort, high value
Add file-level header comments to key modules following Tally's style, documenting:
- Purpose and responsibilities
- Key invariants (e.g. "exactly one active view at all times", "single expanded section", "revision-based render suppression", "server is source of truth via WebSocket pushes")
- Design decisions that constrain future changes

### 6. Minor CSS split (optional) - Nice to have
Could split `style.css` into `base.css`, `layout.css`, `components.css`, `terminals.css` for easier navigation, but not critical.

## Verdict

**Yes, worth refactoring.** Recommend incremental approach: start with diff.js extraction + adding architectural comments (low risk), then extract common utilities, then gradually split other modules. This improves maintainability without rewriting working code.

The biggest wins are reducing cognitive load, making the complex diff logic easier to work with in isolation, and matching Tally's clearer documentation practices.
