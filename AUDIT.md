# Codebase Audit — Grit

**Audit Target:** `grit` — Fast, native, single-binary Git client (Rust / Iced / Axum)  
**Date:** 2026-10-16  
**Auditor:** AI Agent Pair-Programmer (Google Antigravity)  
**Scope:** Full-stack comprehensive audit: Compilation & Clippy health, Dependency & Binary size, Security & Sandboxing, Architecture & Concurrency invariants, Web UI (`web/dist/`), and Documentation integrity.

---

## 1. Executive Summary & Health Scorecard

The Grit codebase is in exceptionally strong engineering health. Recent refactoring efforts resolved previously identified build breakages (such as the desktop test compilation issue at HEAD `3544d39`) and achieved dramatic crate-graph reductions across both web-only and desktop targets. The Rust implementation is 100% safe Rust (zero `unsafe` blocks), contains zero `panic!` invocations in production code, has zero TODO/FIXME markers, and maintains strict non-blocking UI and single-writer concurrency invariants.

However, this audit identified **two significant security vulnerabilities** in the web daemon layer (an unconstrained path traversal in file deletion and missing Origin validation on the WebSocket endpoint), **two frontend integration bugs** with external companion tools (`krust` and `folio`), **one compiler warning** in the desktop clippy pass, and notable **documentation drift** in `ARCHITECTURE.md` and `AGENTS.md`.

### Health Scorecard

| Area | Status | Notes |
| :--- | :---: | :--- |
| **Compiler (`cargo check`)** | **PASS** | 0 warnings on default web-only and `--features desktop --all-targets` |
| **Clippy (`cargo clippy`)** | **WARN** | 0 warnings on default build; **1 warning** on desktop (`field_reassign_with_default`) |
| **Test Suite (`cargo test`)** | **PASS** | **191 web / 219 desktop tests passing**, 0 failed |
| **Memory & Type Safety** | **EXCELLENT** | **0 `unsafe` blocks**, 0 production `panic!`, only 11 justified unwraps |
| **Dependencies & Build Graph** | **OPTIMIZED** | **705 crates** (web-only, -37%) / **12,071 crates** (desktop, -38%) |
| **Security & Sandboxing** | **NEEDS WORK** | Path traversal in `DiscardUntracked`; missing WebSocket Origin validation |
| **Web Frontend (`web/dist/`)** | **FUNCTIONAL** | Valid JS (`node --check` clean), safe DOM (0 `innerHTML`), but 2 dock/probe bugs |
| **Documentation Fidelity** | **DRIFT** | Omissions in `ARCHITECTURE.md` tree; stale references in `AGENTS.md` |

---

## 2. Key Codebase Metrics

| Metric | Measured Value | Comparison / Target |
| :--- | :--- | :--- |
| **Rust Source LOC (`src/`)** | **10,958 lines** (28 files) | Cleanly divided into git, server, and ui modules |
| **Frontend LOC (`web/dist/`)** | **2,739 lines** (4 files) | `app.js` (1,885), `style.css` (732), `index.html` (119) |
| **Total Codebase LOC** | **13,879 lines** (32 files) | Single self-contained repository |
| **Crates, Web-only (`cargo tree`)** | **705 crates** | Down from 1,123 crates baseline (-37.2%) |
| **Crates, Desktop (`--features desktop`)** | **12,071 crates** | Down from 19,524 crates baseline (-38.2%) |
| **`target/` Directory Size** | **16 GB** | Accumulated debug and release build artifacts |
| **Production `unsafe` blocks** | **0** | 100% Safe Rust |
| **Production `panic!` calls** | **0** | Verified via AST grep across all non-test modules |
| **Open TODO / FIXME / HACK markers** | **0** | Clean repository hygiene |
| **Non-test `unwrap()` / `expect()`** | **11** | Strictly limited to piped stdio handles and thread scopes |

---

## 3. Detailed Findings

### 3.1 Security & Sandboxing (High Priority)

#### [SEC-01] Path Traversal & Arbitrary File Deletion in `DiscardUntracked`
* **Severity:** **HIGH**
* **Location:** [`src/git/actions.rs:108-115`](file:///home/bronson/Projects/grit/src/git/actions.rs#L108-L115)
* **Description:**  
  When handling `GitAction::DiscardUntracked(p)`, the code constructs a target path via `repo_path.join(&p)` and calls `std::fs::remove_file(&target)`.
  1. In Rust standard library, `Path::join` replaces the entire path if the argument `p` is an absolute path (e.g., `Path::new("/home/user/repo").join("/etc/hosts")` yields `PathBuf::from("/etc/hosts")`).
  2. If `p` contains relative directory traversals (`../../`), `join` traverses out of the repository directory.
  3. Unlike `src/actions.rs::launch()`, which enforces strict containment checks (`starts_with('/')`, `contains("..")`, `canonical.starts_with(&canonical_repo)`), `DiscardUntracked` performs **no containment verification**.
  4. Furthermore, if `p` points to an untracked directory rather than a file, `std::fs::remove_file` fails with `EISDIR`.
* **Recommendation:**  
  Validate that `p` is neither absolute nor contains traversal components, resolve its canonical path, verify `canonical.starts_with(&canonical_repo)`, and handle directories with `std::fs::remove_dir_all`.

#### [SEC-02] Permissive CORS & Missing WebSocket Origin Validation (CSWSH)
* **Severity:** **HIGH**
* **Location:** [`src/server/mod.rs:128`](file:///home/bronson/Projects/grit/src/server/mod.rs#L128) and [`src/server/websocket.rs:88`](file:///home/bronson/Projects/grit/src/server/websocket.rs#L88)
* **Description:**  
  The HTTP server sets `CorsLayer::permissive()`, allowing any web origin to read REST responses. More critically, the WebSocket upgrade route `/ws` performs **no check on the HTTP `Origin` header**.
  Because WebSockets are exempt from standard browser Same-Origin Policy (SOP), any malicious website visited in the user's browser (e.g. `https://malicious-site.example`) can open a WebSocket connection to `ws://localhost:5000/ws`. Upon connection:
  - The attacker receives real-time JSON dumps of all open repositories, local paths, commits, and file changes.
  - The attacker can send arbitrary `GitAction` commands, including `DiscardUntracked` (deleting files), `DiscardAll` (resetting uncommitted work), `RunScript` (executing repo scripts), or `CommitAllPush`.
* **Recommendation:**  
  Reject WebSocket upgrades whose `Origin` header does not match `localhost`, `127.0.0.1`, or the configured daemon address.

#### [SEC-03] Arbitrary File Read in REST `/files` Handler
* **Severity:** **MEDIUM**
* **Location:** [`src/server/handlers.rs:61-64`](file:///home/bronson/Projects/grit/src/server/handlers.rs#L61-L64) and [`src/git/files.rs:81`](file:///home/bronson/Projects/grit/src/git/files.rs#L81)
* **Description:**  
  The `/files?tab=X&path=Y` endpoint delegates to `get_file_pair`, which executes `std::fs::read_to_string(repo_path.join(path))`. If `path` is an absolute path or contains traversal sequences, any arbitrary text file readable by the daemon process on the host can be read and returned via HTTP JSON.
* **Recommendation:**  
  Sanitize `path` against path traversal and verify that the target path remains inside `repo_path`.

---

### 3.2 Frontend & UI Integration (`web/dist/`)

#### [FE-01] Broken Krust Terminal Button Hiding (`.krust-btn` Class Missing)
* **Severity:** **MEDIUM**
* **Location:** [`web/dist/app.js:1258-1268`](file:///home/bronson/Projects/grit/web/dist/app.js#L1258-L1268) & [`web/dist/app.js:1865`](file:///home/bronson/Projects/grit/web/dist/app.js#L1865)
* **Description:**  
  In `app.js`, `probeKrust()` periodically probes `http://localhost:3000/` and attempts to hide terminal buttons when Krust is unavailable:
  ```javascript
  document.querySelectorAll(".krust-btn").forEach((btn) => {
    btn.style.display = ok ? "" : "none";
  });
  ```
  However, in `renderTabBar()`, the terminal button is created with:
  ```javascript
  const termBtn = document.createElement("button");
  termBtn.className = "tab-view-btn"; // Does NOT include krust-btn!
  ```
  Because the class `.krust-btn` is never attached to `termBtn`, the query matches zero elements. As a result, the `T` button is **always displayed**, even when Krust is not installed or not running, violating the specification in `AGENTS.md`.
* **Recommendation:**  
  Add `termBtn.classList.add("krust-btn")` in `renderTabBar()`.

#### [FE-02] Missing Folio File Explorer Button Toggle
* **Severity:** **LOW**
* **Location:** [`web/dist/app.js:804-811`](file:///home/bronson/Projects/grit/web/dist/app.js#L804-L811)
* **Description:**  
  While `probeFolio()` detects if Folio is running on port 4000 and falls back to `dashboard` if inactive, it never toggles the visibility or active state of the "F" (Files) tab button in the tab bar.
* **Recommendation:**  
  Add a `.folio-btn` class to `filesBtn` and toggle its display or disabled styling based on `folioAvailable`.

#### [FE-03] Dead `keydown` Event Listener
* **Severity:** **LOW**
* **Location:** [`web/dist/app.js:1811-1815`](file:///home/bronson/Projects/grit/web/dist/app.js#L1811-L1815)
* **Description:**  
  An empty global `keydown` listener exists at the bottom of `app.js`:
  ```javascript
  document.addEventListener("keydown", (event) => {
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    const tag = event.target && event.target.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
  });
  ```
  This is orphan boilerplate from previous keyboard shortcut experiments and performs no actions.
* **Recommendation:**  
  Remove the dead event listener or implement the intended shortcuts.

#### [FE-04] Monolithic `app.js` Architecture
* **Severity:** **INFORMATIONAL / MAINTAINABILITY**
* **Location:** [`web/dist/app.js`](file:///home/bronson/Projects/grit/web/dist/app.js) (1,885 lines)
* **Description:**  
  All frontend concerns (WebSocket transport, state management, diff alignment algorithms, folder picker, commit/branch/stash actions, and iframe docks) are combined in one global script. While functional, modularizing into ES modules (`common.js`, `diff.js`, `terminal.js`, `app.js`) without a bundler will significantly improve maintainability and testability.

---

### 3.3 Rust Compiler & Clippy Analysis

#### [RUST-01] Clippy Warning: `field_reassign_with_default`
* **Severity:** **LOW / CODE HYGIENE**
* **Location:** [`src/ui/state.rs:577-588`](file:///home/bronson/Projects/grit/src/ui/state.rs#L577-L588)
* **Description:**  
  `cargo clippy --features desktop --all-targets` emits:
  ```text
  warning: field assignment outside of initializer for an instance created with Default::default()
     --> src/ui/state.rs:578:9
  ```
  `let mut style = iced::widget::button::Style::default();` followed by field overwrites should be initialized directly within the struct literal.
* **Recommendation:**  
  Construct `iced::widget::button::Style` in a single expression using struct update syntax.

---

### 3.4 Concurrency, Threading & Subsystem Architecture

* **UI Non-blocking Invariant:** **VERIFIED**. All git commands are strictly dispatched through `tokio::task::spawn_blocking` in both the server and desktop UI layers. The Iced UI thread remains completely non-blocking.
* **Single Writer Invariant:** **VERIFIED**. The `TabRegistry` in `src/server/registry.rs` remains the single authority for tab state and ID allocation. Both desktop and web clients receive state snapshots and submit action requests.
* **Process Watchdog & Leaks:** **VERIFIED**. `src/git/sync.rs::run_with_timeout` cleanly manages subprocesses by isolating them in POSIX process groups (`process_group(0)`) and terminating process trees (`kill -9 -<pid>`), preventing orphaned grandchildren from holding pipes.
* **Watch Churn Protection:** **VERIFIED**. `src/git/watcher.rs` debounces filesystem events with a 200ms delay, filtering out transient `.git` lock churn.

---

### 3.5 Documentation Drift

| Document | Stated Content | Actual Codebase Reality | Severity |
| :--- | :--- | :--- | :---: |
| [`ARCHITECTURE.md:42-66`](file:///home/bronson/Projects/grit/ARCHITECTURE.md#L42-L66) | Directory map omits key files | Omits `src/actions.rs`, `src/folio.rs`, `src/test_support.rs`, `src/server/handlers.rs`, and git submodules (`actions.rs`, `files.rs`, `history.rs`, `status.rs`) | **Medium** |
| [`AGENTS.md:§2`](file:///home/bronson/Projects/grit/AGENTS.md) | "two dock views (`term-1` / `term-2`)" | Only one terminal view (`term-1`) exists in `index.html` and `app.js` | **Low** |
| [`AGENTS.md:§2`](file:///home/bronson/Projects/grit/AGENTS.md) | "Reset overlay button on each terminal view" | No Reset overlay button exists in `index.html` or `app.js` | **Low** |
| [`AGENTS.md:§2`](file:///home/bronson/Projects/grit/AGENTS.md) | "left view dock (Dashboard/`F`/`L` + krust)" | No `L` dock button exists; Log is an expandable rollup section | **Low** |
| [`AGENTS.md:§4`](file:///home/bronson/Projects/grit/AGENTS.md) | "no node/deno/bun on the dev box" | Node.js is installed at `/usr/bin/node` (`node --check` works) | **Informational** |

---

## 4. Prioritized Action Plan

1. **[Security - High] Sanitize `GitAction::DiscardUntracked`**:
   Add path traversal checks to `src/git/actions.rs`, ensuring the canonical path stays within the repository root before deletion.
2. **[Security - High] Enforce WebSocket Origin Checking**:
   In `src/server/websocket.rs`, inspect the `Origin` header during handshake and reject non-local origins to eliminate Cross-Site WebSocket Hijacking.
3. **[Security - Medium] Contain `/files` Path Resolution**:
   Validate query paths in `src/server/handlers.rs` before reading disk files.
4. **[Bugfix - Medium] Fix Krust Button Toggle**:
   Add class `krust-btn` to the terminal button in `web/dist/app.js:1259` so `probeKrust()` can properly hide/show it.
5. **[Code Hygiene - Low] Resolve Desktop Clippy Warning**:
   Update `tab_button_style` in `src/ui/state.rs:577` to initialize `Style` directly.
6. **[Code Hygiene - Low] Drop Dead `keydown` Listener**:
   Remove lines 1811-1815 in `web/dist/app.js`.
7. **[Documentation - Low] Synchronize `ARCHITECTURE.md` and `AGENTS.md`**:
   Update file trees and correct terminal/dock descriptions.
8. **[Maintainability - Future] Frontend Modularization**:
   Split `web/dist/app.js` into focused ES modules (utils, diff, git-actions, terminals).
