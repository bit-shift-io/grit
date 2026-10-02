# Codebase Audit

Two independent audits are recorded in this file. Their scopes do not overlap,
so both are kept:

- **Part 1 — Build & Dependency Audit** (2026-09-27): Rust crate-graph
  reduction. **Completed** — see `TASKS.md` Part 1 for the applied changes and
  the measured before/after crate counts.
- **Part 2 — Web UI Audit: Grit vs Tally**: front-end modularisation of the
  monolithic `web/dist/app.js`. **Open** — see `TASKS.md` Part 2.

---

## Part 1 — Build & Dependency Audit (completed)

**Audit Target:** `grit` — fast, native, single-binary Git client (Rust / Iced / Axum)
**Date:** 2026-09-27
**Focus:** Build-time optimisation via dependency graph reduction (primary), plus general health sweep

---

### Executive Summary

The Rust code itself is in very good shape: **zero compiler warnings** on the web-only and desktop
builds, **zero unreferenced public items**, **zero TODO/FIXME/HACK/XXX markers**, and no
commented-out code or stray debug statements. The previous audit's findings (innerHTML injection,
dead CSS ids, orphan `CONTEXT.md`, silent `get_file_pair` failures) have all been resolved.

The real cost lives entirely in `Cargo.toml`. The web-only build resolves **1123 crates**; the
desktop build resolves **19524 crates**. Roughly **38% of each graph is removable with no change in
observable behaviour** — almost all of it from features and optional machinery that the code never
touches. `target/` has reached **12 GB**.

The three worst offenders are all "declared but unused":

1. **`iced`'s `linux-theme-detection`** — pulls `ashpd` → `zbus` (a full D-Bus stack), `x11rb`, and
   `sctk-adwaita`, while `src/ui/state.rs:770` hardcodes `.theme(iced::Theme::Dark)`. **6995 crates.**
2. **`iced`'s `wgpu`** — the entire GPU backend (wgpu-core/naga/wgpu-hal) for a UI built only from
   `button`/`column`/`row`/`rule`/`text`/`text_input`/`pick_list`. No canvas, no image, no SVG.
   **3844 crates.**
3. **`tower-http`'s `fs` + `trace` features** — the code imports exactly one symbol from
   `tower-http`: `CorsLayer` at `src/server/mod.rs:14`. Assets are served from the embedded
   `rust-embed` bundle instead. **133 crates.**

All numbers below are *measured*, not estimated: each configuration was applied to a scratch copy of
the manifest and resolved with `cargo tree --no-dedupe`, then diffed against baseline.

---

### Key Metrics

| Metric | Value |
| :--- | :--- |
| Rust LOC (`src/`) | 9099 across 28 files |
| Crates, web-only build | **1123** → **701** achievable (**-38%**) |
| Crates, desktop build | **19524** → **12529** achievable (**-37%**) |
| `target/` size | **12 GB** |
| Unused / orphan files | 0 |
| Dead functions or exports | 0 |
| Commented-out code / debug logs | 0 (`eprintln!` ×2 in `main.rs`, `mod.rs` — both intentional user-facing errors) |
| Open TODOs / FIXMEs | 0 |
| Compiler warnings | 0 (web-only **and** desktop) |
| **Blocking build error** | **1** — `cargo check --features desktop --all-targets` fails (see §4) |

---

### Findings & Recommendations

#### 1. Dependency Removal — Web-Only Build (default `cargo build`)

Ranked by crates eliminated. Every row is independently applicable.

| # | Change | Crates saved | Code refs | Effort |
| :--- | :--- | :--- | :--- | :--- |
| 1 | `tower-http` → `default-features = false, features = ["cors"]` | **-133** | 1 (`CorsLayer`, `server/mod.rs:14`) | Trivial |
| 2 | `futures-util` → `default-features = false, features = ["std", "sink"]` | **-91** | 11 | Trivial |
| 3 | `tokio` → replace `features = ["full"]` with the explicit list | **-77** | 139 | Trivial |
| 4 | Drop `freedesktop-desktop-entry` entirely | **-50** | 4 | Medium |
| 5 | `tracing-subscriber` → drop `env-filter` | **-36** | 1 (`fmt::init`) | Trivial |
| 6 | Drop `clap` | **-27** | 1 (`Parser`) | Medium |
| 7 | Align `tokio-tungstenite` to `0.29` (dedupe) | -6 distinct | dev + `ui/remote.rs` | Trivial |
| 8 | Drop `dirs` | -4 | 2 | Trivial |
| | **Combined** | **-422** | | |

**Row 1 detail.** `fs` and `trace` are dead. `src/server/static_files.rs` serves everything from
`rust_embed`; there is no `ServeDir` anywhere. `TraceLayer` is never imported. `fs` drags in
`tokio-util` + `http-range-header`; `trace` drags in `http-body-util` + `tracing` plumbing.

**Row 3 detail.** Only these tokio modules are referenced: `net`, `sync`, `time`, `io`, `runtime`,
`macros`. `full` additionally compiles `process`, `signal`, `fs`, `sync` extras, and the
multi-threaded scheduler surface grit never touches. Suggested:
`features = ["rt-multi-thread", "macros", "net", "sync", "time", "io-util"]` — **but verify**:
`src/git/watcher.rs` uses `notify`, not `tokio::fs`, and git itself runs through `std::process::Command`
(per `AGENTS.md` §3.3), so `process`/`fs`/`signal` should be droppable. Needs a `cargo check` to confirm.

**Row 4 detail — the highest-value code change.** `freedesktop-desktop-entry` is used for exactly
one job: scanning installed `.desktop` files for `Categories=…TerminalEmulator` to discover terminal
emulators (`src/actions.rs:290-342`, `707-708`). That is a plain INI scan over
`~/.local/share/applications` and `/usr/share/applications`, and this repo *already* hand-rolls the
same `.desktop` parsing in `decode_entry` at `src/actions.rs:707`. The crate costs 50 crates and,
worse, pulls **`gettext-sys` — a C library that is compiled and linked during every build**. Removing
it means writing one ~40-line INI scanner. Note that the priority order in `desktop_file_terminals()`
already comes from the iteration order, so no behavioural loss is expected.

**Row 5 detail.** `tracing_subscriber::fmt::init()` is the only call. `env-filter` pulls
`regex` + `matchers` + `aho-corasick`. Combined with row 4 (which removes the *other* `regex` source),
the whole regex stack disappears from the graph — that is why rows 4+5 together are worth -80, not
-86. Use `default-features = false, features = ["fmt", "ansi"]`.

**Row 6 detail.** `clap` exists to parse three flags (`--headless`, `--port`, `--path`) plus
`--version`/`--help`, declared at `src/main.rs:18-32`. 27 crates for that. Hand-rolling costs ~50
lines and loses nothing the UI uses. Keep in mind two `#[test]`s at `main.rs:144-157` assert on
`Cli::parse_from`, so they need rewriting alongside.

**Row 7 detail.** `axum 0.8` resolves `tokio-tungstenite 0.29`, while the manifest pins `0.30` for
both the `desktop` optional dep and the dev-dep. Result: **two complete copies** of the websocket
stack compile. Dropping to `0.29` removes `tungstenite 0.30`, `tokio-tungstenite 0.30`, `sha1 0.11`,
`chacha20`, `rand 0.10`, and `rand_core 0.10`, collapsing the duplicated `sha1`/`digest`/`block-buffer`
stack from two generations to one. `src/ui/remote.rs:9` imports `tokio_tungstenite::tungstenite::Message`,
which becomes the *same* type `axum::extract::ws` uses — so it gets simpler, not harder.

**Row 8 detail.** Two calls: `dirs::home_dir()` (`folio.rs:81`) and `dirs::config_dir()`
(`shared_config.rs:26`). Both are `$HOME` and `$XDG_CONFIG_HOME`/`.config` respectively — ~6 lines
of `std::env` code.

**Not a win — recorded so it is not re-attempted.** Switching `rfd` from its default
`["xdg-portal", "async-std"]` to `["xdg-portal", "tokio"]` removes `async-fs` and `async-net` but
*adds* 7 crates back through `ashpd`'s tokio integration. Net **+7**. It does eliminate a redundant
second async runtime from the graph, which is worth something for binary size and clarity, but it
will not speed up the build. Also worth noting: `rfd::FileDialog::pick_folder()` at
`src/ui/state.rs:213` is a **blocking** call wrapped in `Task::perform`, i.e. it blocks an iced
runtime worker. `rfd::AsyncFileDialog` is the correct API there — a correctness issue, not a build one.

#### 2. Dependency Removal — Desktop Build (`--features desktop`)

| # | Change | Crates saved | Justification |
| :--- | :--- | :--- | :--- |
| 1 | Drop iced's `linux-theme-detection` | **-6995** | `ashpd`→`zbus` (D-Bus), `x11rb`, `sctk-adwaita` — all to auto-detect a theme the app hardcodes as Dark (`ui/state.rs:770`) |
| 2 | Drop iced's `wgpu`, keep `tiny-skia` | **-3844** | Text/file-list UI; no `canvas`, `image`, or `svg` widget is used anywhere |
| 3 | Optionally drop `wayland` | **-1884** | Only if the dev box and users are X11-only |
| | **Combined (1+2)** | **-6995** → 12529 crates (**-36%**) | |

This is a one-line change to `Cargo.toml`:

```toml
iced = { version = "0.14", optional = true, default-features = false,
         features = ["tiny-skia", "crisp", "web-colors", "thread-pool",
                     "x11", "wayland", "tokio"] }
```

`crisp` and `wayland` are neutral in crate count (measured identical either way) but are cheap to
keep and avoid gratuitous breakage. Add `linux-theme-detection` back later only if the app ever
respects the system theme — and then remove the hardcoded `Theme::Dark` at the same time.

#### 3. Documentation Drift

| File | Issue | Recommendation |
| :--- | :--- | :--- |
| `ARCHITECTURE.md:13` | Claims desktop UI uses "native rendering (`wgpu` / `winit`)" | Update if `wgpu` is dropped in favour of `tiny-skia` |
| `ARCHITECTURE.md:17` | Lists `clap` as the "CLI Engine" | Update if `clap` is removed |
| `ARCHITECTURE.md:19` | Lists `tracing-subscriber` alongside `tracing` | Note the reduced feature set once trimmed |
| `AGENTS.md:25` | Says the default build "excludes `iced`/`rfd`" — still accurate, but the directory map omits `src/folio.rs` entirely | Add `folio.rs` to the §2 directory map (it is the `folio` file-explorer auto-launcher, the sibling of `krust.rs`) |
| `NOTES.md`, `TODO.md` | Historical planning docs for the now-removed in-process file browser (`src/git/files.rs:1-3` documents the removal) | Mark as historical or archive, so they stop describing a system that no longer exists |

#### 4. Build Breakage — Requires a Fix

| File | Issue | Severity | Recommended Action |
| :--- | :--- | :--- | :--- |
| `src/ui/state.rs:802` | `cargo check --features desktop --all-targets` **fails**: `error[E0063]: missing fields 'remote_branches' and 'stashes' in initializer of types::RepoState`. The `repo_state()` test helper was not updated when those fields were added to `RepoState` (`src/git/types.rs`). Working tree is clean, so this is committed at HEAD (`3544d39`). | **High** — the entire desktop test target does not compile, so no desktop test has run since those fields landed | Add `remote_branches: vec![]` and `stashes: vec![]` to the literal at `ui/state.rs:802` |

Note that plain `cargo check --features desktop` (no `--all-targets`) passes, which is why this can
hide. `cargo test` (web-only) also passes, which is why CI-style default runs miss it.

#### 5. Code Structure & Complexity Smells

| File | Issue | Context | Suggested Refactor |
| :--- | :--- | :--- | :--- |
| `src/ui/state.rs:184` | `update()` is **173 lines** — a single flat `match` over every `Message` variant | Largest function in the repo by 2×; every new message lengthens it further | Split into `handle_tab_message` / `handle_git_message` / `handle_dialog_message` sub-dispatchers |
| `src/git/mod.rs:163` | `run_streamed()` is **95 lines** with 4 sequential fallback strategies | Hard to reason about which git error is swallowed | Extract one `try_diff(repo, args) -> Option<String>` helper per strategy |
| `src/server/websocket.rs:170` | `SplitSink<WebSocket, Message>` passed as a bare generic parameter instead of `&mut` in one signature, while sibling code uses `&mut` | Inconsistent; invites aliasing mistakes | Normalise to `&mut` |
| `src/git/history.rs:13-16, 173-176` | 9 `unwrap_or_default()` calls in git-output parsing | Acceptable here — git log formats vary and a lenient parse is correct — but worth a comment stating that intent so it is not "fixed" later | Add one explanatory comment per parse function |

#### 6. Comment Quality — Clean

No stale comments, no commented-out blocks, no `dbg!`. The module docs in `src/git/files.rs:1-3`
and `src/folio.rs:1-13` are accurate and usefully explain *why* code was removed. This is a
well-maintained codebase on the documentation front.

---

### Top Priority Action Plan

1. **[High]** Fix the desktop test build break at `src/ui/state.rs:802` — add the two missing
   `RepoState` fields. Nothing else can be validated on `--features desktop` until this lands.
2. **[High]** Apply the four trivial manifest changes that need no code edits: `tower-http` →
   cors-only, `futures-util` → `default-features = false`, `tokio` off `full`, `tracing-subscriber`
   off `env-filter`, and `tokio-tungstenite` → `0.29`. **~330 crates gone from the default build**
   for a one-file diff.
3. **[High]** Drop `iced`'s `linux-theme-detection` and `wgpu` in favour of `tiny-skia`. **-6995
   crates (-36%)** on the desktop build, with no change to any UI behaviour.
4. **[Medium]** Hand-roll the `.desktop` `TerminalEmulator` scan in `src/actions.rs` and delete
   `freedesktop-desktop-entry`, eliminating the `gettext-sys` C build. ~40 lines of INI parsing,
   and the repo already does this once at `actions.rs:707`.
5. **[Medium]** Replace `clap` with a hand-rolled parser for the three flags in `src/main.rs`, and
   drop `dirs` for two `std::env` lookups. Rewrite the two `Cli::parse_from` tests at `main.rs:144`.
6. **[Medium]** Split `GritApp::update` (`src/ui/state.rs:184`, 173 lines) and
   `run_streamed` (`src/git/mod.rs:163`, 95 lines).
7. **[Low]** Update `ARCHITECTURE.md:13,17,19` and add `src/folio.rs` to the `AGENTS.md` §2 map;
   archive `NOTES.md` / `TODO.md`.
8. **[Low]** Switch `rfd::FileDialog` → `rfd::AsyncFileDialog` at `src/ui/state.rs:213` to stop
   blocking an iced runtime worker on a modal dialog.

---

### Verification Notes

- **Measured, not estimated.** Every crate-count figure was produced by copying `Cargo.toml`,
  `Cargo.lock`, `src/`, and `web/` to a scratch directory, applying the candidate change, and running
  `cargo tree --prefix none --no-dedupe`. The project's real `Cargo.toml` was never modified.
- Counts are `--no-dedupe` line totals, so a change that shifts a version *within* an existing crate
  (e.g. `rand 0.9` → `rand 0.10`) can move the total slightly while still removing distinct crates.
  Where the line count and the distinct-crate set disagreed, the distinct-crate set is reported —
  see the `tokio-tungstenite` and `rfd` rows.
- Baseline established at commit `3544d39` with a clean working tree.
- Suggested post-change gate: `cargo check`, `cargo check --features desktop --all-targets`
  (currently broken — see §4), `cargo test`, and `cargo test --features desktop`.
- Per `AGENTS.md`, do not restart the user's running `krust`/`grit` daemons; state what needs
  restarting instead. Note also that `web/dist/*` is embedded at compile time, so a rebuild is
  required for any frontend change to take effect.

---

## Part 2 — Web UI Audit: Grit vs Tally (open)

### Summary

Grit's web UI is functional and correct, but its `app.js` (1688 lines) is a single monolithic file that mixes concerns. Tally demonstrates a cleaner approach with modular JS files, better architectural documentation, and clearer separation of concerns. The refactor is worthwhile as an incremental improvement to maintainability, especially if the web UI will evolve further.

### Current State (Grit)

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

### Comparison with Tally

Tally (`~/Projects/tally/web/dist/`):
- Modular split: `common.js` (126 lines) for shared utilities (`getJson`, `showNotice`, rollup wiring) + page-specific modules (`dashboard.js`, `forms.js`, `reports.js`, editor domain in `app.js`).
- Excellent architectural comments explaining *why* (e.g. "server is the only source of truth", "edits are staged", invariants documented upfront).
- CSS split (`base.css`, `grid.css`) by concern.
- Per-page JS modules loaded as needed.

Grit is a more complex SPA (live WebSocket state, tabs, terminals, file browser) so single HTML page is appropriate. But modularization is still beneficial.

### What to Adopt from Tally

1. **Shared utilities module** - Extract common helpers (like Tally's `common.js`) for reusability and single source of truth.
2. **Architectural documentation style** - Add file-level comments explaining key invariants and design decisions ("why", not just "what").
3. **Separation of concerns** - Split large self-contained areas (especially diff rendering, which is pure-ish logic) into focused modules.
4. **Clear module boundaries** - Even without bundlers, ES modules or at least logical separation improves maintainability.

### Recommended Refactoring Plan

**Priority: Incremental, low-risk changes that preserve behavior.**

#### 1. Extract `common.js` (utils + shared helpers) - Low risk, high value
Create `web/dist/common.js` with shared utilities:
- DOM helpers and utilities (isBlank, sameValue equivalents if useful, or keep as-is)
- Section/rollup helpers (`toggleSection`, ARIA state management)
- URL update helpers (`updateUrlView`, `updateUrlTab`)
- Message sending helpers (`sendAction`, `sendRaw`)
- General utilities used across modules

#### 2. Extract `diff.js` - Medium risk isolation, very high value
Largest self-contained chunk (~150-200 lines). Contains:
- `showDiff()`, `renderFilePair()`, `splitLines()`, `alignLines()`, `renderSideBySide()`, `renderDiffRow()`, `renderChunk()`

This is mostly pure logic + DOM construction. Extracting it makes the main file smaller and the complex LCS algorithm easier to reason about and modify.

#### 3. Extract feature modules - Medium effort, high value
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

#### 4. Update HTML to load modules - Required if splitting
Options:
- **ES modules** (cleanest): Change to `<script type="module" src="/main.js"></script>` and use `import` statements. Works in modern browsers, no build step needed.
- **Ordered script tags**: Load in dependency order (common.js first, then core, then others, then app/main). Simpler if avoiding modules, but globals-based.

ES modules are preferred and align better with maintainability.

#### 5. Add architectural comments - Low effort, high value
Add file-level header comments to key modules following Tally's style, documenting:
- Purpose and responsibilities
- Key invariants (e.g. "exactly one active view at all times", "single expanded section", "revision-based render suppression", "server is source of truth via WebSocket pushes")
- Design decisions that constrain future changes

#### 6. Minor CSS split (optional) - Nice to have
Could split `style.css` into `base.css`, `layout.css`, `components.css`, `terminals.css` for easier navigation, but not critical.

### Verdict

**Yes, worth refactoring.** Recommend incremental approach: start with diff.js extraction + adding architectural comments (low risk), then extract common utilities, then gradually split other modules. This improves maintainability without rewriting working code.

The biggest wins are reducing cognitive load, making the complex diff logic easier to work with in isolation, and matching Tally's clearer documentation practices.
