# Web UI Refactoring Plan

## Goal
Refactor Grit's web UI for better maintainability by extracting modules and adding architectural documentation, following Tally's patterns while preserving all existing functionality. The web UI is hand-maintained (embedded via rust-embed), no build step - keep it simple.

## Prerequisites
- Understand current behavior: `web/dist/app.js` (1688 lines), `index.html`, `style.css`
- No existing tests for web UI (it's hand-tested). Be careful not to change behavior.
- All changes to `web/dist/*` require rebuild + restart to take effect (per AGENTS.md)

## Tasks

### Task 1: Create common utilities module (common.js)
Extract shared helpers used across the codebase.

**Files to create/modify:**
- Create `web/dist/common.js`

**What to extract:**
- Section toggle helpers: `toggleSection` (lines ~132-179 in app.js) - manages single expanded section invariant
- URL helpers: `updateUrlView`, `updateUrlTab` (around line ~1596, ~1612, also updateUrlTab is used elsewhere)
- DOM utilities if any shared patterns emerge
- General helpers that don't depend on module-specific state

**Implementation notes:**
- Keep functions simple, export via global scope for now (or use ES modules). Prefer ES modules for cleanliness.
- Document the single-expansion invariant in comments.

### Task 2: Extract diff rendering module (diff.js)
Largest self-contained chunk - extract diff/LCS logic.

**Files to create/modify:**
- Create `web/dist/diff.js`

**What to extract (from app.js):**
- `showDiff(detailEl, tab, path)` (line ~1175)
- `renderFilePair(detailEl, pair)` (line ~1198)
- `splitLines(text)` (line ~1219)
- `alignLines(a, b)` (line ~1227)
- `renderSideBySide(leftText, rightText)` (around ~1340+)
- `renderDiffRow(row)` (around ~1365+)
- `renderChunk(leftLines, rightLines, start, end, leftType, rightType)` (around ~1420+)
- Any related helpers (DIFF_MARKER_RATIO, LCS_DIFF_CELL_CAP constants used)

**Dependencies/coupling:**
- Uses `pairCache` (Map) - could keep as global or pass reference. Better to keep minimal coupling; pairCache lives in app.js state.
- Uses constants from core.

**Implementation notes:**
- This is mostly pure logic + DOM construction - good candidate for extraction.
- Add file-level comment explaining diff approach (side-by-side, LCS for alignment, chunking with DIFF_MARKER_RATIO).

### Task 3: Create core module (core.js)
Extract WebSocket, connection, core state, message handling.

**Files to create/modify:**
- Create `web/dist/core.js`

**What to extract:**
- Constants: RECONNECT_*, SCROLL_HIT_SLACK_PX, DIFF_MARKER_RATIO, LCS_DIFF_CELL_CAP, HISTORY_SEARCH_*, BRANCH_FILTER_DEBOUNCE_MS, KRUST_BASE, KRUST_PROBE_MS, KRUST_SESSIONS, FOLIO_BASE (lines 1-79)
- WebSocket management: `ws`, `reconnectTimer`, `reconnectDelayMs`, `scheduleReconnect()`, `openSocket()`, `setConnStatus()`, `sendRaw()` (lines ~26-79)
- Global state: `activeTabId`, `lastState`, `lastRevision`, `expanded`, `awaitingNewTab`, `browser`, `clearedUpToSeq`, `showAddForm`, `activeView`, `historyQuery`, `historySearchTimer`, `knownTabIds`, `commitCache`, `pairCache` (lines ~83-137)
- Message handling: `handleStateMessage()` (lines ~254-320)
- Core helpers: `sendAction()`, `activeTab()` (lines ~324-338)

**Implementation notes:**
- Keep state in module scope (or attach to window if mixing with non-module code). With ES modules, use module scope and export what's needed.
- Document key invariants: server is source of truth via WS pushes, revision-based render suppression, single expanded section, exactly one active view.

### Task 4: Create views module (views.js)
Extract view switching and dock logic.

**Files to create/modify:**
- Create `web/dist/views.js`

**What to extract:**
- `showView(view)` (line ~1553)
- `updateUrlView(view)` (line ~1596)
- `setView(view)` (line ~1612)
- `probeExternal()` (line ~1630+)
- krust/folio frame management: `ensureKrustFrame()`, `ensureFolioFrame()`, related helpers (around ~1650+)
- View-related event handlers

**Implementation notes:**
- Depends on core state (activeView) and DOM elements.

### Task 5: Create tabs module (tabs.js)
Extract tab bar and add-repo form.

**Files to create/modify:**
- Create `web/dist/tabs.js`

**What to extract:**
- `getTabNameFromPath()` (line ~185)
- `setupAddRepoForm()` (line ~195+)
- `renderTabBar()` and related tab rendering (earlier in file - look around where tabs rendered)
- Tab-related helpers

**Note:** Need to locate full `renderTabBar` function in original file.

### Task 6: Create changes module (changes.js)
Extract staging, commit UI, change rows.

**Files to create/modify:**
- Create `web/dist/changes.js`

**What to extract:**
- `appendChangeRow()` (line ~900+)
- `toggleCommitActions()`, `buildCommitActions()` (around ~960+)
- `renderScriptRunner()`, script execution (earlier sections)
- Staging/commit/discard action handlers

### Task 7: Create history module (history.js)
Extract history rendering and search.

**Files to create/modify:**
- Create `web/dist/history.js`

**What to extract:**
- `renderHistory()` and related helpers (around ~400-550 range based on earlier grep)
- History search logic, RECENT_COMMIT_COUNT constant usage
- Commit details expansion

### Task 8: Create branches-stashes module (branches-stashes.js)
Extract branches and stashes.

**Files to create/modify:**
- Create `web/dist/branches-stashes.js`

**What to extract:**
- `renderBranches()`, branch filtering, create branch (around ~670+)
- `renderStashes()`, create stash, stash actions (around ~1480+)
- Branch filter debounce logic

### Task 9: Create log module (log.js)
Extract log rendering.

**Files to create/modify:**
- Create `web/dist/log.js`

**What to extract:**
- `renderLog()` (around ~770+)
- Clear log handler (`document.getElementById("clear-log-btn").onclick` at ~850+)

### Task 10: Create terminals module (terminals.js)
Extract krust/folio specific logic not in views.

**Files to create/modify:**
- Create `web/dist/terminals.js`

**What to extract:**
- Krust session management details
- Frame URL construction, reset logic
- Terminal-specific helpers

### Task 11: Create main entry point (main.js) and update index.html
Tie everything together.

**Files to modify/create:**
- Create `web/dist/main.js` - imports modules, initializes app, wires up global event listeners, calls `openSocket()`, sets up global handlers that span modules
- Update `web/dist/index.html` to load via ES modules: `<script type="module" src="/main.js"></script>`

**What moves to main:**
- Global event listeners not specific to a module (visibilitychange, pageshow - lines ~66-79)
- Keydown handler (line ~1645+)
- Initial `openSocket()` call (line ~79)
- App initialization and coordination

**HTML changes:**
```html
<script type="module" src="/main.js"></script>
```
Instead of `<script src="/app.js"></script>`

### Task 12: Preserve original app.js as reference (optional) or remove after verification
Don't delete immediately. Keep `app.js.bak` or just verify behavior first.

### Task 13: Add architectural comments
Add file-level comments to each new module following Tally's style:
- Purpose, responsibilities
- Key invariants
- Design decisions
- "Why" explanations for non-obvious choices

### Task 14: Verify and rebuild
- Ensure no behavior changes (functionality identical)
- After changes, rebuild with cargo (web assets embedded at compile time)
- Test key flows: connect, add repo, view changes, diff, history, branches, terminals
- Verify `cargo check` passes

## Implementation strategy

1. **Incremental approach**: Extract modules one by one. After each extraction, we could in theory load both, but easier to build up main.js that imports from modules and also temporarily keep functions in app.js? Or better: create modules, move code, update main. But since it's one SPA and we're restructuring, better to do it systematically.

2. **Use ES modules**: Cleanest, no global namespace pollution. All modules export what main needs, main imports. This matches modern practices and is fine for hand-maintained files served by Axum.

3. **Handle shared state**: State lives in core.js (module scope). Other modules import from core or receive state as parameters. Many rendering functions take `(state, tab, ...)` as params already - that’s good design, makes extraction easier.

4. **Preserve exact behavior**: Don't change logic, just move code. Copy-paste carefully, keep same variable names, same DOM operations.

5. **Order of extraction**: Extract diff.js first (isolated), then core.js (foundation), then others, then wire up main.js and update index.html.

## Notes
- AGENTS.md says web/dist/* is embedded at compile time - rebuild required after changes.
- No build tools/linter available for JS - rely on careful manual review and Python tokenizer if needed for syntax checks (as mentioned in AGENTS.md).
- Don't restart running daemons unless asked - user restarts himself.
