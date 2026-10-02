# Tasks

Two independent task lists are recorded in this file, matching the two audits
in `AUDIT.md`:

- **Part 1 — Build Optimisation & Codebase Tasks** (2026-09-27): **completed**,
  every item checked off, with verification results.
- **Part 2 — Web UI Refactoring Plan**: **not started**, all items unchecked.

---

## Part 1 — Build Optimisation & Codebase Tasks (completed)

> **Goal:** Apply the changes from AUDIT.md to reduce crate counts and fix minor issues.
> Each task must keep `cargo check` / `cargo check --features desktop` / `cargo test` green at every step.
> 
> Run `cargo test` after each Rust change. After JS edits, run the python3 syntax tokenizer.

---

### 1. Fix Desktop Test Build Break (High Priority)

`src/ui/state.rs:802` is missing fields `remote_branches` and `stashes` in `RepoState` initializer.

- [x] Add `remote_branches: vec![]` to the `repo_state()` helper in `src/ui/state.rs` around line 802
- [x] Add `stashes: vec![]` to the `repo_state()` helper in `src/ui/state.rs` around line 802
- [x] Verify with `cargo check --features desktop --all-targets`

### 2. Optimise Manifest Dependencies (High Priority)

#### 2a. Trim tower-http features
- [x] Change `tower-http = { version = "0.7", features = ["fs", "cors", "trace"] }` to `tower-http = { version = "0.7", default-features = false, features = ["cors"] }` in `Cargo.toml`

#### 2b. Trim futures-util features  
- [x] Change `futures-util = "0.3"` to `futures-util = { version = "0.3", default-features = false, features = ["std", "sink"] }` in `Cargo.toml`

#### 2c. Replace tokio full features with explicit list
- [x] Replace `tokio = { version = "1", features = ["full"] }` with `tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "sync", "time", "io-util"] }` in `Cargo.toml`
- [x] Verify with `cargo check && cargo test`

#### 2d. Trim tracing-subscriber features
- [x] Change `tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }` to `tracing-subscriber = { version = "0.3", default-features = false, features = ["fmt", "ansi"] }` in `Cargo.toml`

#### 2e. Align tokio-tungstenite to 0.29 to dedupe
- [x] Change `tokio-tungstenite = { version = "0.30", optional = true }` to `tokio-tungstenite = { version = "0.29", optional = true }` in `Cargo.toml`
- [x] Change `tokio-tungstenite = "0.30"` in `[dev-dependencies]` to `tokio-tungstenite = "0.29"` in `Cargo.toml`
- [x] Verify `cargo check && cargo test` still pass

### 3. Optimise Iced Features for Desktop (High Priority)

- [x] Update iced dependency in `Cargo.toml` to use explicit features: remove defaults, keep `tiny-skia`, `crisp`, `web-colors`, `thread-pool`, `x11`, `wayland`, `tokio` (drop `linux-theme-detection` and `wgpu`)
- [x] Verify `cargo check --features desktop` still passes

**Result:** web-only 1123 → 780 crates; desktop 19524 → 12146 crates. `cargo test` 141 pass, `cargo test --features desktop` 169 pass. Note: `ashpd`/`zbus`/`sctk-adwaita` still resolve via `rfd`'s `xdg-portal` backend, not via iced.

### 4. Replace freedesktop-desktop-entry with hand-rolled parser (Medium Priority)

The crate pulls gettext-sys and costs 50 crates. We already have a hand-rolled parser in `actions.rs:707-708`.

- [x] Implement INI-style parser for `.desktop` TerminalEmulator entries in `src/actions.rs` to replace `desktop_file_terminals()` that uses `freedesktop_desktop_entry` crate
- [x] Update `primary_binary()` to work with our parsed data
- [x] Remove `freedesktop-desktop-entry = "0.8"` dependency from `Cargo.toml`
- [x] Verify with `cargo check && cargo test` — 145 pass. `gettext-sys` no longer resolves.

**Note:** the freedesktop crate's `default_paths()` returns each XDG data dir's `applications/` **subdirectory** — scanning the data dirs themselves silently finds nothing. `desktop_dirs()` now appends `applications` and dedupes. `desktop_file_scan_picks_up_installed_terminals` is the test that catches this.

### 5. Replace clap with hand-rolled CLI parser (Medium Priority)

Only 3 flags needed. Must preserve existing behavior and test expectations.

- [x] Replace `clap`-based CLI parsing in `src/main.rs` with manual parsing of `--headless`, `--port`, `--path`, `--version`, `--help`
- [x] Update the two tests `parses_defaults()` and `parses_headless_port_and_path()` to work with the new parser (they call `Cli::parse_from`)
- [x] Remove `clap = { version = "4", features = ["derive"] }` from `Cargo.toml`
- [x] Verify with `cargo check && cargo test` — 144 passed (added 3 new tests; all still green)

### 6. Replace dirs with std::env (Medium Priority)

Two simple lookups.

- [x] Replace `dirs::home_dir()` in `src/folio.rs:81` with `std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))`
- [x] Replace `dirs::config_dir()` in `src/shared_config.rs:26` with an `XDG_CONFIG_HOME`-then-`$HOME/.config` lookup (keeps the `is_absolute` filter `dirs` applied)
- [x] Remove `dirs = "6"` from `Cargo.toml`
- [x] Verify with `cargo check && cargo test` — 141 pass, including the 4 `shared_config` tests that set `XDG_CONFIG_HOME`

### 7. Refactor Large Functions (Medium Priority)

#### 7a. Split `GritApp::update` in `src/ui/state.rs`
- [x] Refactor the 173-line `update()` method into smaller helper methods (e.g., `handle_tab_message`, `handle_git_message`, `handle_dialog_message`) to improve readability
- [x] Maintain identical behavior - all existing tests must still pass — 173 desktop tests pass

**Done as:** `update()` now handles only tab/form messages and delegates the
rest to `handle_git_message()` (git actions + watcher/sync messages) and
`handle_open_repo_result()` (the one non-trivial form outcome). The `_` arm in
`handle_git_message` is unreachable via `update` and returns `Task::none()`.

#### 7b. Split `run_streamed` in `src/git/mod.rs`
- [x] Extract fallback strategies into helper functions in `run_streamed()` (currently ~95 lines)
- [x] Maintain identical behavior - all existing tests must still pass — 145 web tests pass

**Note:** the audit described `run_streamed` as holding "4 sequential fallback
strategies" — that is not what the function contains. The actual duplication was
the two near-identical stdout/stderr reader threads. Extracted
`spawn_stream_reader()` (generic over `R: Read`) and hoisted `flush_chunk()` to
module scope, with `StreamBuffers`/`Buffers` type aliases. `run_streamed` is now
~20 lines.

### 8. Update Documentation (Low Priority)

- [x] Update `ARCHITECTURE.md:13` to reflect rendering approach after removing wgpu if needed
- [x] Update `ARCHITECTURE.md:17` to reflect CLI parsing changes if clap removed
- [x] Update `ARCHITECTURE.md:19` regarding tracing-subscriber feature set
- [x] Update `ARCHITECTURE.md:265` startup-flow diagram (`clap` → hand-rolled)
- [x] Update `AGENTS.md` §2 — `main.rs` CLI note, `actions.rs` `.desktop` note, add missing `src/folio.rs` entry
- [x] Mark `NOTES.md` as historical (banner at top) — describes the removed in-process file browser
- [x] Verify no other doc drift

### 9. Switch rfd to async API (Low Priority)

- [x] Change `rfd::FileDialog::new()` to use `rfd::AsyncFileDialog` API in `src/ui/state.rs:213` to avoid blocking the iced runtime worker
- [x] Update the surrounding code to work with async dialog — `AsyncFileDialog` yields a `FileHandle`, so the task body maps it via `handle.path().to_path_buf()` to keep `Message::FolderPicked(Option<PathBuf>)` unchanged
- [x] Verify with `cargo check --features desktop` — 173 tests pass

### 10. Clear the Pre-existing Clippy Backlog

The 26 lints left over from HEAD were outside the audit's scope, but they are
real and the fix is mechanical.

- [x] `cargo clippy --fix --all-targets` — 24 machine-applicable fixes applied
      (`to_path_buf` in test helpers, `match`+`_ => {}` → `if let`,
      `map_or` → `is_none_or`, redundant closure, derivable `impl`)
- [x] Hand-wrap the two over-long lines the autofix introduced in
      `server/websocket.rs` tests (593, 631)
- [x] Verify: `cargo clippy --all-targets` → **0 lints**, `cargo build --all-targets`
      → **0 warnings**, 145 web + 173 desktop tests pass

**Do NOT run `cargo fmt` on this repo.** The tree is *not* rustfmt-clean at HEAD
(verified: `cargo fmt --check` reports diffs in untouched files such as
`src/actions.rs:163`), so a blanket format would produce large unrelated churn.
Long lines that predate this work were left as-is.

### Appendix: Audit §5 Items Missed by the Original Plan

Two items from AUDIT.md §5 were not in the numbered plan above; both handled
after the fact.

- [x] `src/git/history.rs` — the audit asked for one explanatory comment per
  git-output parse function so the lenient `unwrap_or_default()` calls are not
  later "fixed" into panics. Added to `parse_log_output`, the
  `get_commit_summary` header parse, and the name-status/numstat pair.

- [x] `src/server/websocket.rs:170` — audit said `push_state` takes a bare
  `SplitSink<WebSocket, Message>` rather than `&mut`. **Stale finding: the
  signature already reads `sender: &mut futures_util::stream::SplitSink<...>`.**
  No change needed.

### Appendix: Clippy Baseline (Not in the Audit)

Clippy was not part of the audit — its bar was "zero compiler warnings", which
holds. Verified against unmodified HEAD in a throwaway `git worktree`:

| | HEAD (3544d39) | After this change |
| :--- | ---: | ---: |
| Compiler warnings | 0 | **0** |
| Clippy lints | 27 | **26** |

**No new lints introduced.** Two lints appear at "new" locations
(`src/git/mod.rs:219`, `src/main.rs:130`) but are the *same* pre-existing lints,
shifted by added lines — `flush_chunk` was relocated out of `run_streamed`, and
`main.rs:130` is the untouched `#[cfg(not(feature = "desktop"))]` block that was
at line 44 before. One lint disappeared: `actions.rs:325`
(`contains()` vs `iter().any()`), removed with the `desktop_file_terminals` rewrite.

The remaining 26 clippy lints are pre-existing and untouched by this work
(16 × `to_path_buf` in `server/mod.rs`/`handlers.rs`, plus `git/watcher.rs`,
`server/websocket.rs`, `server/registry.rs`, `git/files.rs`). Fixing them is a
separate cleanup pass, out of scope here.

### Verification Checklist

- [x] `cargo check` passes (web-only)
- [x] `cargo check --features desktop --all-targets` passes
- [x] `cargo check --features desktop` passes
- [x] `cargo test` passes — 145 passed, 0 failed
- [x] `cargo test --features desktop` passes — 173 passed, 0 failed
- [x] No JS changed, so no python3 tokenizer run needed

### Results

| Metric | Before | After | Change |
| :--- | :---: | :---: | :---: |
| Crates, web-only build | 1123 | **705** | **-37%** |
| Crates, desktop build | 19524 | **12146** | **-38%** |
| `gettext-sys` C build | yes | **no** | removed |
| Compiler warnings | 0 | 0 | — |

**Crates removed from the manifest entirely:** `clap`, `dirs`,
`freedesktop-desktop-entry` (the only `gettext-sys` source). `iced` now resolves
without `wgpu` or `linux-theme-detection`.

**Not done (deliberately):** `ashpd`/`zbus`/`sctk-adwaita` still resolve on the
desktop build, but via `rfd`'s `xdg-portal` backend rather than iced. Dropping
that would mean giving up the native folder picker.

**Not done (deliberately): `wayland` (-1884 further desktop crates).** The audit
marked this optional and conditioned on an X11-only deployment. Checked the dev
box: `XDG_SESSION_TYPE=wayland`, `WAYLAND_DISPLAY=wayland-1`,
`XDG_CURRENT_DESKTOP=COSMIC` — Wayland is the *primary* session (XWayland at
`DISPLAY=:0` is secondary). Dropping the feature would break the desktop GUI on
this machine. Re-evaluate only if Wayland support is genuinely no longer needed.

**Runtime note:** per `AGENTS.md`, do not restart the user's running
`krust`/`grit` daemons — state what needs restarting instead. None of the changes
in sections 1–10 touch `web/dist/`, so those only need a `grit` rebuild+restart.

**However:** `web/dist/style.css` is *also* dirty in the working tree, from a
change made outside this plan (`.term-view { overflow: hidden }` and
`box-sizing: border-box` on the krust/folio frames — an iframe layout fix). It is
not part of sections 1–10. Because `web/dist/*` is embedded at compile time, that
CSS will not reach the running daemon until `grit` is rebuilt and restarted.
Review it separately before committing, since it is a behaviour change to the
web UI rather than a build optimisation.

---

## Part 2 — Web UI Refactoring Plan (open)

### Goal
Refactor Grit's web UI for better maintainability by extracting modules and adding architectural documentation, following Tally's patterns while preserving all existing functionality. The web UI is hand-maintained (embedded via rust-embed), no build step - keep it simple.

### Prerequisites
- Understand current behavior: `web/dist/app.js` (1688 lines), `index.html`, `style.css`
- No existing tests for web UI (it's hand-tested). Be careful not to change behavior.
- All changes to `web/dist/*` require rebuild + restart to take effect (per AGENTS.md)

### Tasks

#### Task 1: Create common utilities module (common.js)
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

#### Task 2: Extract diff rendering module (diff.js)
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

#### Task 3: Create core module (core.js)
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

#### Task 4: Create views module (views.js)
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

#### Task 5: Create tabs module (tabs.js)
Extract tab bar and add-repo form.

**Files to create/modify:**
- Create `web/dist/tabs.js`

**What to extract:**
- `getTabNameFromPath()` (line ~185)
- `setupAddRepoForm()` (line ~195+)
- `renderTabBar()` and related tab rendering (earlier in file - look around where tabs rendered)
- Tab-related helpers

**Note:** Need to locate full `renderTabBar` function in original file.

#### Task 6: Create changes module (changes.js)
Extract staging, commit UI, change rows.

**Files to create/modify:**
- Create `web/dist/changes.js`

**What to extract:**
- `appendChangeRow()` (line ~900+)
- `toggleCommitActions()`, `buildCommitActions()` (around ~960+)
- `renderScriptRunner()`, script execution (earlier sections)
- Staging/commit/discard action handlers

#### Task 7: Create history module (history.js)
Extract history rendering and search.

**Files to create/modify:**
- Create `web/dist/history.js`

**What to extract:**
- `renderHistory()` and related helpers (around ~400-550 range based on earlier grep)
- History search logic, RECENT_COMMIT_COUNT constant usage
- Commit details expansion

#### Task 8: Create branches-stashes module (branches-stashes.js)
Extract branches and stashes.

**Files to create/modify:**
- Create `web/dist/branches-stashes.js`

**What to extract:**
- `renderBranches()`, branch filtering, create branch (around ~670+)
- `renderStashes()`, create stash, stash actions (around ~1480+)
- Branch filter debounce logic

#### Task 9: Create log module (log.js)
Extract log rendering.

**Files to create/modify:**
- Create `web/dist/log.js`

**What to extract:**
- `renderLog()` (around ~770+)
- Clear log handler (`document.getElementById("clear-log-btn").onclick` at ~850+)

#### Task 10: Create terminals module (terminals.js)
Extract krust/folio specific logic not in views.

**Files to create/modify:**
- Create `web/dist/terminals.js`

**What to extract:**
- Krust session management details
- Frame URL construction, reset logic
- Terminal-specific helpers

#### Task 11: Create main entry point (main.js) and update index.html
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

#### Task 12: Preserve original app.js as reference (optional) or remove after verification
Don't delete immediately. Keep `app.js.bak` or just verify behavior first.

#### Task 13: Add architectural comments
Add file-level comments to each new module following Tally's style:
- Purpose, responsibilities
- Key invariants
- Design decisions
- "Why" explanations for non-obvious choices

#### Task 14: Verify and rebuild
- Ensure no behavior changes (functionality identical)
- After changes, rebuild with cargo (web assets embedded at compile time)
- Test key flows: connect, add repo, view changes, diff, history, branches, terminals
- Verify `cargo check` passes

### Implementation strategy

1. **Incremental approach**: Extract modules one by one. After each extraction, we could in theory load both, but easier to build up main.js that imports from modules and also temporarily keep functions in app.js? Or better: create modules, move code, update main. But since it's one SPA and we're restructuring, better to do it systematically.

2. **Use ES modules**: Cleanest, no global namespace pollution. All modules export what main needs, main imports. This matches modern practices and is fine for hand-maintained files served by Axum.

3. **Handle shared state**: State lives in core.js (module scope). Other modules import from core or receive state as parameters. Many rendering functions take `(state, tab, ...)` as params already - that’s good design, makes extraction easier.

4. **Preserve exact behavior**: Don't change logic, just move code. Copy-paste carefully, keep same variable names, same DOM operations.

5. **Order of extraction**: Extract diff.js first (isolated), then core.js (foundation), then others, then wire up main.js and update index.html.

### Notes
- AGENTS.md says web/dist/* is embedded at compile time - rebuild required after changes.
- No build tools/linter available for JS - rely on careful manual review and Python tokenizer if needed for syntax checks (as mentioned in AGENTS.md).
- Don't restart running daemons unless asked - user restarts himself.
