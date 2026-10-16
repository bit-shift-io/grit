# Audit Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve every finding in `AUDIT.md` — two high-severity web-daemon vulnerabilities, one medium arbitrary-file-read, two external-tool frontend bugs, one dead listener, one clippy warning, doc drift, and the `web/dist/app.js` modularization request.

**Architecture:** Rust fixes land in the existing `git/` and `server/` subsystems behind small, unit-tested helpers (`contained_repo_path`, `origin_is_allowed`) so both GUI and web paths share one guard. The web rewrite is a no-bundler ES-module split (`common.js` / `diff.js` / `terminal.js` + entry `app.js`) preserving all behavior and the tooling-free invariant.

**Tech Stack:** Rust 2021, Axum 0.8, tower-http 0.7 (`CorsLayer`), tokio-tungstenite 0.29 (tests), plain ES modules served by rust-embed.

**Spec:** `/home/bronson/Projects/grit/AUDIT.md`

## Global Constraints

- No `git2-rs`/libgit2; all git work shells out to the CLI (`ARCHITECTURE.md` invariant 5).
- Single self-contained binary: web assets embed via `rust-embed`; no bundler, no runtime sidecars (`ARCHITECTURE.md` invariant 1). ES modules are allowed; a build step is not.
- Async locking only (`tokio::sync::Mutex`), never `std::sync::Mutex` across runtime tasks.
- Web assets are compile-time embedded: **any `web/dist/*` change requires a Grit rebuild + restart to be seen** — never restart the user's running daemons yourself.
- `cargo test` must stay green on the default (web-only) build; `cargo clippy --features desktop --all-targets` must be warning-free.
- JS has no test harness; every JS task ends with `node --check` (parsed as ESM), a manual browser smoke pass, and a `cargo test` run to confirm the embedded bundle still serves.
- Preserve every doc comment and the public `pub(crate)` surface unless a task says otherwise.

## Review Focus

Inputs/conditions the spec implies but no task's automated tests exercise; each is pinned by a test in the owning task, and by the manual smoke pass.

- A `DiscardUntracked` path that is a **symlink inside the repo pointing outside it** — must be refused, not followed and deleted (Task 1).
- A legitimately-nested repo-relative path such as `src/main.rs` or `a/b/c.txt` — must still resolve and delete as before, i.e. the guard must not over-reject (Task 1, Task 2).
- A **missing (new/untracked) file** requested via `/files` — the containment guard must not fail on a non-existent path; `current` stays empty (Task 2).
- WebSocket handshakes **without** an `Origin` header (desktop remote client, test harness) — must still be accepted, or every existing WS test and remote mode breaks (Task 3).
- Browser `Origin` values that merely *contain* a loopback string (`http://localhost.evil.com`, `https://127.0.0.1.evil.com`) — must be rejected (Task 3).
- The `?view=` deep link and terminal/folio fallback when the external daemon is down — must behave identically after the module split (Task 12, Task 13).

---

## Phase A — Security & Rust Hygiene

### Task 1: Containment helper + SEC-01 (`DiscardUntracked`)

**Files:**
- Modify: `src/git/mod.rs` (add `contained_repo_path`)
- Modify: `src/git/actions.rs:108-115` (use helper; handle directories)
- Test: `src/git/actions.rs` (tests module, near `discard_untracked_removes_file` at :260)

**Interfaces:**
- Produces: `pub(crate) fn contained_repo_path(repo_path: &Path, rel: &str) -> Result<std::path::PathBuf, GitError>` in `src/git/mod.rs`. Rejects an absolute `rel`, rejects any `..` / root / prefix component, joins the remainder lexically, and — when the joined target exists — canonicalizes and re-checks `starts_with(canonical_repo)` so a symlink cannot escape. Missing targets are still returned (callers decide whether absence is fatal).
- Consumes: existing `GitError`, `Path` re-export via `use super::*`.

- [ ] **Step 1: Write the failing tests** in `src/git/actions.rs` tests module

```rust
#[test]
fn discard_untracked_rejects_absolute_path() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("secret.txt");
    fs::write(&victim, "keep me\n").unwrap();

    let err = execute_action(
        dir.path(),
        GitAction::DiscardUntracked(victim.display().to_string()),
    )
    .unwrap_err();
    assert!(err.message.contains("outside") || err.message.contains("suspicious"), "got: {}", err.message);
    assert!(victim.exists(), "file outside the repo must survive");
}

#[test]
fn discard_untracked_rejects_traversal() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    // Both tempdirs share a parent (the system temp dir), so `../<name>` from
    // the repo reaches the sibling's file.
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("escape.txt");
    fs::write(&victim, "keep me\n").unwrap();
    let rel = format!(
        "../{}/escape.txt",
        outside.path().file_name().unwrap().to_string_lossy()
    );

    assert!(execute_action(dir.path(), GitAction::DiscardUntracked(rel)).is_err());
    assert!(victim.exists(), "traversal target must survive");
}

#[test]
fn discard_untracked_removes_directory() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    fs::write(dir.path().join("seed.txt"), "v\n").unwrap();
    commit_all(dir.path(), "initial");
    fs::create_dir(dir.path().join("newdir")).unwrap();
    fs::write(dir.path().join("newdir/inner.txt"), "x\n").unwrap();

    execute_action(dir.path(), GitAction::DiscardUntracked("newdir".into())).unwrap();
    assert!(!dir.path().join("newdir").exists(), "untracked dir removed recursively");
}
```

- [ ] **Step 2: Run tests, confirm failure**

Run: `cargo test discard_untracked`
Expected: `rejects_absolute_path` and `removes_directory` FAIL (absolute path currently deletes the victim / `remove_file` errors with EISDIR); `rejects_traversal` FAIL (no guard).

- [ ] **Step 3: Add `contained_repo_path` to `src/git/mod.rs`**

```rust
use std::path::{Component, Path, PathBuf};

/// Joins a client-supplied relative path onto `repo_path` without allowing it
/// to escape the repository: absolute paths and `..` components are rejected,
/// and an existing target is canonicalized so symlinks cannot point outside.
/// Non-existent targets are returned as-is (callers decide if absence is fatal).
pub(crate) fn contained_repo_path(repo_path: &Path, rel: &str) -> Result<PathBuf, GitError> {
    let rel_path = Path::new(rel);
    let mut out = repo_path.to_path_buf();
    for comp in rel_path.components() {
        match comp {
            Component::Normal(c) => out.push(c),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(GitError {
                    message: format!("refusing path outside repository: {rel}"),
                    stderr: String::new(),
                    stdout: String::new(),
                });
            }
        }
    }
    if let (Ok(canon_out), Ok(canon_repo)) = (out.canonicalize(), repo_path.canonicalize()) {
        if !canon_out.starts_with(&canon_repo) {
            return Err(GitError {
                message: format!("refusing path outside repository: {rel}"),
                stderr: String::new(),
                stdout: String::new(),
            });
        }
    }
    Ok(out)
}
```

- [ ] **Step 4: Use it in `execute_action` (`src/git/actions.rs`)**

Replace the `DiscardUntracked` arm:

```rust
GitAction::DiscardUntracked(p) => {
    let target = contained_repo_path(repo_path, &p)?;
    let meta = std::fs::symlink_metadata(&target).map_err(|e| GitError {
        message: format!("failed to remove {}: {e}", p),
        stderr: String::new(),
        stdout: String::new(),
    })?;
    let result = if meta.is_dir() {
        std::fs::remove_dir_all(&target)
    } else {
        std::fs::remove_file(&target)
    };
    result.map_err(|e| GitError {
        message: format!("failed to remove {}: {e}", p),
        stderr: String::new(),
        stdout: String::new(),
    })?;
}
```

- [ ] **Step 5: Run tests, confirm pass**

Run: `cargo test discard_untracked`
Expected: PASS (all four, including the pre-existing `discard_untracked_removes_file`).

- [ ] **Step 6: Commit**

```bash
git add src/git/mod.rs src/git/actions.rs
git commit -m "fix(security): contain DiscardUntracked paths inside the repository"
```

---

### Task 2: SEC-03 — contain `/files` path resolution

**Files:**
- Modify: `src/git/files.rs:38` (`get_file_diff` untracked read) and `:81` (`get_file_pair` worktree read)
- Test: `src/git/files.rs` tests module (near `get_file_pair_returns_empty_original_for_untracked` at :153)

**Interfaces:**
- Consumes: `contained_repo_path` from Task 1.
- Produces: `get_file_pair` / `get_file_diff` now return `Err(GitError)` (message `refusing path outside repository: …`) for absolute or `..`-bearing paths; unchanged behavior otherwise.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn get_file_pair_rejects_traversal() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let sibling = dir.path().parent().unwrap().join("grit-secret.txt");
    fs::write(&sibling, "top secret\n").unwrap();

    let err = get_file_pair(dir.path(), "../grit-secret.txt").unwrap_err();
    assert!(err.message.contains("outside repository"), "got: {}", err.message);
    let _ = fs::remove_file(&sibling);
}

#[test]
fn get_file_pair_rejects_absolute_path() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let err = get_file_pair(dir.path(), "/etc/hostname").unwrap_err();
    assert!(err.message.contains("outside repository"), "got: {}", err.message);
}

#[test]
fn get_file_pair_allows_nested_path_and_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    fs::create_dir_all(dir.path().join("a/b")).unwrap();
    fs::write(dir.path().join("a/b/c.txt"), "hi\n").unwrap();

    let pair = get_file_pair(dir.path(), "a/b/c.txt").unwrap();
    assert_eq!(pair.current, "hi\n");

    let missing = get_file_pair(dir.path(), "a/b/nope.txt").unwrap();
    assert_eq!(missing.current, "");
}
```

- [ ] **Step 2: Run tests, confirm failure**

Run: `cargo test get_file_pair`
Expected: `rejects_traversal` FAIL (currently returns sibling contents), `rejects_absolute_path` FAIL.

- [ ] **Step 3: Apply the guard**

In `get_file_pair`, before use, compute `let full = contained_repo_path(repo_path, path)?;` once, then use `std::fs::read_to_string(&full)` for the worktree read. In `get_file_diff`, replace `let full_path = repo_path.join(path);` with `let full_path = contained_repo_path(repo_path, path)?;`.

- [ ] **Step 4: Run tests, confirm pass**

Run: `cargo test get_file_pair`
Expected: PASS. Also run `cargo test files` to confirm the existing suite is green.

- [ ] **Step 5: Commit**

```bash
git add src/git/files.rs
git commit -m "fix(security): reject traversal and absolute paths in file pair/diff reads"
```

---

### Task 3: SEC-02 — WebSocket `Origin` validation

**Files:**
- Modify: `src/server/websocket.rs:126-128` (`ws_handler`)
- Test: `src/server/websocket.rs` tests module (after `resolve_tab_repo_finds_only_known_tabs` at :393)

**Interfaces:**
- Produces: `pub(crate) fn origin_is_allowed(origin: Option<&str>) -> bool` — `None` (native client/tests) is allowed; a present Origin is allowed only when its host is `localhost`, `127.0.0.1`, or `[::1]`.
- `ws_handler` signature becomes `pub async fn ws_handler(ws: WebSocketUpgrade, State(app): State<AppState>, headers: axum::http::HeaderMap) -> Response` and returns `403 Forbidden` for a disallowed Origin (consumed by Task 4 too).

- [ ] **Step 1: Write the failing unit tests**

```rust
#[test]
fn origin_is_allowed_accepts_loopback_and_missing() {
    assert!(super::origin_is_allowed(None), "native client sends no Origin");
    assert!(super::origin_is_allowed(Some("http://localhost:5000")));
    assert!(super::origin_is_allowed(Some("http://127.0.0.1:5000")));
    assert!(super::origin_is_allowed(Some("http://[::1]:5000")));
}

#[test]
fn origin_is_allowed_rejects_foreign_and_lookalike() {
    assert!(!super::origin_is_allowed(Some("http://evil.example")));
    assert!(!super::origin_is_allowed(Some("https://localhost.evil.com")));
    assert!(!super::origin_is_allowed(Some("https://127.0.0.1.evil.com")));
    assert!(!super::origin_is_allowed(Some("null")), "file:// / sandboxed origins are null");
}
```

- [ ] **Step 2: Run tests, confirm failure**

Run: `cargo test origin_is_allowed`
Expected: FAIL — `origin_is_allowed` not defined (compile error).

- [ ] **Step 3: Implement `origin_is_allowed` + wire `ws_handler`**

```rust
/// True when a WebSocket handshake's Origin is acceptable. A missing Origin is
/// allowed: the desktop remote client and the test harness are native and never
/// send one. A browser Origin is allowed only for a loopback host, which is the
/// whole point of the check — any other site's page in the user's browser is a
/// CSWSH attempt.
pub(crate) fn origin_is_allowed(origin: Option<&str>) -> bool {
    let Some(origin) = origin else { return true };
    let host_port = origin.split("://").nth(1).unwrap_or("");
    let host_port = host_port.split('/').next().unwrap_or("");
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}
```

`ws_handler`:

```rust
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    let origin = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    if !origin_is_allowed(origin) {
        return (axum::http::StatusCode::FORBIDDEN, "forbidden origin").into_response();
    }
    ws.on_upgrade(move |socket| handle_websocket(socket, app))
}
```

- [ ] **Step 4: Add the end-to-end handshake test**

```rust
#[tokio::test]
async fn websocket_rejects_foreign_origin_but_accepts_loopback() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let app = app_for(dir.path());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let _server = run_server(listener, app, rx);

    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::Request as HttpRequest;

    let build = |origin: &str| {
        let mut req: HttpRequest<()> = format!("ws://{addr}/ws").into_client_request().unwrap();
        req.headers_mut()
            .insert("origin", origin.parse().unwrap());
        req
    };

    assert!(
        tokio_tungstenite::connect_async(build("http://evil.example")).await.is_err(),
        "foreign origin must be refused"
    );
    assert!(
        tokio_tungstenite::connect_async(build("http://localhost:5000")).await.is_ok(),
        "loopback origin must be accepted"
    );
}
```

- [ ] **Step 5: Run tests, confirm pass**

Run: `cargo test origin_is_allowed && cargo test websocket_rejects_foreign_origin`
Expected: PASS. Then `cargo test --features desktop` to confirm `src/ui/remote.rs` (which sends no Origin) still connects in its tests.

- [ ] **Step 6: Commit**

```bash
git add src/server/websocket.rs
git commit -m "fix(security): reject non-loopback WebSocket Origin (CSWSH)"
```

---

### Task 4: SEC-02 — tighten CORS to loopback

**Files:**
- Modify: `src/server/mod.rs:118-129` (`build_router`)
- Test: `src/server/mod.rs` tests module (after `ws_route_requires_websocket_upgrade` at :963)

**Interfaces:**
- Consumes: `crate::server::websocket::origin_is_allowed` from Task 3.
- Produces: `build_router` uses `CorsLayer::new().allow_origin(AllowOrigin::predicate(...)).allow_methods(Any).allow_headers(Any)` instead of `CorsLayer::permissive()`.

- [ ] **Step 1: Write the failing tests**

```rust
async fn cors_allow_origin_for(origin: &str) -> Option<String> {
    let dir = tempfile::tempdir().unwrap();
    let router = build_router(app_for(dir.path()));
    let response = router
        .oneshot(
            Request::builder()
                .uri("/health")
                .header("origin", origin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    response
        .headers()
        .get("access-control-allow-origin")
        .map(|v| v.to_str().unwrap().to_string())
}

#[tokio::test]
async fn cors_reflects_loopback_but_not_foreign_origin() {
    assert_eq!(
        cors_allow_origin_for("http://localhost:5000").await.as_deref(),
        Some("http://localhost:5000")
    );
    assert_eq!(cors_allow_origin_for("http://evil.example").await, None);
}
```

- [ ] **Step 2: Run tests, confirm failure**

Run: `cargo test cors_reflects_loopback`
Expected: FAIL — permissive CORS reflects `http://evil.example` (Some), so the `None` assertion fails.

- [ ] **Step 3: Replace the layer**

```rust
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
// ...
.route("/{*path}", get(static_files::serve_static))
.with_state(state)
.layer(
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _parts| {
            websocket::origin_is_allowed(origin.to_str().ok())
        }))
        .allow_methods(Any)
        .allow_headers(Any),
)
```

- [ ] **Step 4: Run tests, confirm pass**

Run: `cargo test cors_reflects_loopback`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/server/mod.rs
git commit -m "fix(security): restrict CORS responses to loopback origins"
```

---

### Task 5: RUST-01 — desktop clippy `field_reassign_with_default`

**Files:**
- Modify: `src/ui/state.rs:572-590` (`tab_button_style`)

**Interfaces:** none (local closure body only).

- [ ] **Step 1: Reproduce the warning**

Run: `cargo clippy --features desktop --all-targets`
Expected: `warning: field assignment outside of initializer ... src/ui/state.rs:578`.

- [ ] **Step 2: Initialize the struct in one expression**

```rust
    move |theme, _status| {
        let palette = theme.palette();
        iced::widget::button::Style {
            background: Some(iced::Background::Color(if is_active {
                palette.primary
            } else {
                palette.background
            })),
            text_color: if is_active {
                palette.background
            } else {
                palette.text
            },
            ..iced::widget::button::Style::default()
        }
    }
```

- [ ] **Step 3: Verify no warnings**

Run: `cargo clippy --features desktop --all-targets`
Expected: no `field_reassign_with_default` warning; command exits 0.

- [ ] **Step 4: Commit**

```bash
git add src/ui/state.rs
git commit -m "style(ui): build tab button Style in one initializer"
```

---

## Phase B — Frontend Bugfixes (`web/dist/`)

> Every task here: edit `web/dist/app.js`, then run the JS check (`node --check` on a `.mjs` copy), then `cargo test serves_javascript_bundle` to confirm the embedded asset still serves. A browser rebuild/restart is what makes the change visible; state that, do not restart daemons.

### Task 6: FE-01 — krust terminal button hiding

**Files:**
- Modify: `web/dist/app.js` (`renderTabBar`, the `termBtn` block at :1262-1270)

**Interfaces:** `probeKrust()` (:1923) selects `.krust-btn`; `renderTabBar` produces the elements it selects.

- [ ] **Step 1: Add the class and apply probe state at creation**

In `renderTabBar`, after `termBtn.className = "tab-view-btn";` add:

```javascript
termBtn.classList.add("krust-btn");
termBtn.style.display = krustAvailable ? "" : "none";
```

- [ ] **Step 2: JS syntax check**

Run: `cp web/dist/app.js /tmp/opencode/app.mjs && node --check /tmp/opencode/app.mjs && rm /tmp/opencode/app.mjs`
Expected: exit 0, no output. (If creating the repo's tokenizer is preferred, see Task 13; `node --check` is sufficient here.)

- [ ] **Step 3: Manual smoke (rebuild first: `cargo build --release`, restart the user's instance)**

Open the web UI with krust **stopped** → the `T` buttons are absent; start krust within 5 s → `T` buttons appear. Confirm the button remains hidden after a state refresh re-renders the tab bar.

- [ ] **Step 4: Commit**

```bash
git add web/dist/app.js
git commit -m "fix(web): tag terminal buttons krust-btn so probe hides them"
```

---

### Task 7: FE-02 — Folio file-explorer button toggle

**Files:**
- Modify: `web/dist/app.js` (`renderTabBar` `filesBtn` block at :1251-1260; `probeFolio` at :808-815)

**Interfaces:** `probeFolio()` sets `folioAvailable`; `renderTabBar` must consume it.

- [ ] **Step 1: Apply folio availability in `renderTabBar`**

After `filesBtn.className = "tab-view-btn";` add:

```javascript
filesBtn.classList.add("folio-btn");
filesBtn.style.display = folioAvailable ? "" : "none";
```

In `probeFolio`'s callback, after `folioAvailable = ok;`, re-render so visibility updates without waiting for a state push:

```javascript
if (lastState) renderTabBar(lastState);
```

- [ ] **Step 2: JS syntax check**

Run: `cp web/dist/app.js /tmp/opencode/app.mjs && node --check /tmp/opencode/app.mjs && rm /tmp/opencode/app.mjs`
Expected: exit 0.

- [ ] **Step 3: Manual smoke** — folio down ⇒ no `F` buttons; folio up ⇒ `F` buttons appear; toggling folio up/down updates within one probe interval without a page reload.

- [ ] **Step 4: Commit**

```bash
git add web/dist/app.js
git commit -m "fix(web): hide the Files button when folio is unavailable"
```

---

### Task 8: FE-03 — remove dead `keydown` listener

**Files:**
- Modify: `web/dist/app.js:1835-1839`

- [ ] **Step 1: Delete the listener**

Remove the entire `document.addEventListener("keydown", (event) => { ... });` block (5 lines) that only returns early and performs no action.

- [ ] **Step 2: JS syntax check**

Run: `cp web/dist/app.js /tmp/opencode/app.mjs && node --check /tmp/opencode/app.mjs && rm /tmp/opencode/app.mjs`
Expected: exit 0.

- [ ] **Step 3: Manual smoke** — ordinary typing in the commit box, branch filter, and history search still works (no regression from removing the no-op).

- [ ] **Step 4: Commit**

```bash
git add web/dist/app.js
git commit -m "chore(web): drop the no-op keydown listener"
```

---

## Phase C — FE-04: ES-module split of `web/dist/app.js`

**Target module graph (no cycles; arrows point at imports):**

```
app.js  ──▶ common.js
        ──▶ diff.js    ──▶ common.js
        ──▶ terminal.js ──▶ common.js
```

- **`common.js`** — constants (`RECONNECT_*`, `KRUST_BASE`, `KRUST_PROBE_MS`, `KRUST_SESSIONS`, `FOLIO_BASE`, `*_MS`, `LCS_DIFF_CELL_CAP`, `DIFF_MARKER_RATIO`, `SCROLL_HIT_SLACK_PX`, `RECENT_COMMIT_COUNT`), the exported mutable `store` object, pure helpers (`getTabNameFromPath`, `getTabNameFromUrl`, `repoScope`, `sessionIdForRepo`, `shorten`? — move only what other modules need), connection layer (`openSocket`, `scheduleReconnect`, `setConnStatus`, `sendRaw`, `handleStateMessage`), `activeTab`, `sendAction`, `render`, `renderTabBar`, `sortedTabs`, `updateUrlTab`, `getInitialTabId`, `toggleSection`, all panel renderers, `renderActivityIndicators`, `revealFailedCommand`, `commitOutcome`, `settlePendingCommit`, `appendChangeRow`, `buildCommitActions`, `toggleCommitActions`, `showCommitSummary`, `renderCommitSummary`, `lineCounts`. Exports the names `app.js`/`diff.js`/`terminal.js` consume.
- **`diff.js`** — `splitLines`, `alignLines`, `renderFilePair`, `renderSideBySide`, `toggleDiff`, `showDiff`, `scrollToFirstDiffBlock`, `scrollDetailToBlock`, `stepDiffBlock`.
- **`terminal.js`** — `setDockVisible`, `showView`, `setView`, `updateUrlView`, `probeExternal`, folio (`folioAvailable` moved into `store`, `folioFrameSrc`, `ensureFolioFrame`, `renderFileBrowser`, `probeFolio`), krust (`krustAvailable`/`termUnseenSid` in `store`, `currentRepoPath`, `sessionIdFor`, `liveSessionId`, `handleKrustActivity`, `krustFrameSrc`, `ensureKrustFrame`, `probeKrust`). Must NOT call `probeKrust`/`probeFolio`/`showView` at module top level.
- **`app.js`** — the entry module: imports the three, keeps the DOM "Action wiring" region, and runs `main()` (`openSocket()`, `setupAddRepoForm` handler wiring, `probeFolio()`, `probeKrust()`, `showView("dashboard")`, initial `?t=`/`?view=` routing).

**`store` shape (exact keys):** `ws`, `reconnectTimer`, `reconnectDelayMs`, `activeTabId`, `lastState`, `lastRevision`, `expanded`, `awaitingNewTab`, `browser`, `clearedUpToSeq`, `surfacedFailures`, `pendingCommit`, `showAddForm`, `activeView`, `historyQuery`, `historySearchTimer`, `knownTabIds`, `commitCache`, `pairCache`, `branchFilterTimer`, `folioAvailable`, `krustAvailable`, `termUnseenSid`.

Cross-module mutable state is **only** accessed as `store.X`; module-local immutable values stay `const`/`let` in their module.

### Task 9: Create `common.js` and switch to module loading

**Files:**
- Create: `web/dist/common.js`
- Modify: `web/dist/index.html:117` (`<script type="module" src="/app.js"></script>`)
- Modify: `web/dist/app.js` (import constants/helpers from `common.js`; remove the now-moved definitions)

**Interfaces:**
- Produces: `common.js` exporting constants and the pure helpers + `probeExternal`; `app.js` starts with `import { ... } from "./common.js";`.
- Consumes: nothing new.

- [ ] **Step 1: Move constants + pure helpers into `common.js`**

Move lines 4-17 constants, plus `getTabNameFromPath` (:150), `getTabNameFromUrl` (:156), `repoScope` (:1853), `sessionIdForRepo` (:1864), `splitLines` (:1414), `alignLines` (:1422), `lineCounts` (:1169), and `probeExternal` (:1824). `export` each; add `import` lines in `app.js`. Keep every body byte-identical.

- [ ] **Step 2: Switch the entry point to a module**

`index.html`: `<script type="module" src="/app.js"></script>`. There are no inline handlers and no `window.*` global dependencies other than `window.location`/`history`/`addEventListener` (verified), so module scope is safe.

- [ ] **Step 3: JS syntax check + served-asset check**

```bash
for f in common app; do cp web/dist/$f.js /tmp/opencode/$f.mjs && node --check /tmp/opencode/$f.mjs && rm /tmp/opencode/$f.mjs; done
cargo test serves_javascript_bundle
```
Expected: both parse; `serves_javascript_bundle` PASS (asserts `/app.js` is `text/javascript` and contains `WebSocket` — connection code still lives in `app.js`).

- [ ] **Step 4: Manual smoke** — page loads, WebSocket connects (status indicator), a repo renders, history/branches/stashes/log tabs work.

- [ ] **Step 5: Commit**

```bash
git add web/dist/common.js web/dist/app.js web/dist/index.html
git commit -m "refactor(web): extract constants and pure helpers into common.js ES module"
```

---

### Task 10: Introduce `store` and migrate mutable state

**Files:**
- Modify: `web/dist/common.js` (add `export const store = { ... }`)
- Modify: `web/dist/app.js` (rewrite every cross-module state reference to `store.X`)

**Interfaces:**
- Produces: `export const store` with exactly the keys listed above.
- Consumes: Task 9's module wiring.

- [ ] **Step 1: Add the `store` object** in `common.js`

```javascript
export const store = {
  ws: null,
  reconnectTimer: null,
  reconnectDelayMs: RECONNECT_BASE_MS,
  activeTabId: null,
  lastState: null,
  lastRevision: 0,
  expanded: { type: null, key: null, el: null },
  awaitingNewTab: false,
  browser: { dir: null, parent: null, seeding: false },
  clearedUpToSeq: 0,
  surfacedFailures: new Map(),
  pendingCommit: null,
  showAddForm: false,
  activeView: "dashboard",
  historyQuery: "",
  historySearchTimer: null,
  knownTabIds: new Set(),
  commitCache: new Map(),
  pairCache: new Map(),
  branchFilterTimer: null,
  folioAvailable: false,
  krustAvailable: false,
  termUnseenSid: null,
};
```

- [ ] **Step 2: Rewrite references in `app.js`**

In `app.js`, import `store` and replace each former top-level `let`/`const` state reference with `store.<name>` (every read and write). Remove the original declarations. The `initialUrlView` bootstrap stays local to `app.js`.

- [ ] **Step 3: Guard against stragglers**

Run: `grep -nE "\b(activeTabId|lastState|lastRevision|showAddForm|activeView|pairCache|commitCache|knownTabIds|pendingCommit|termUnseenSid|krustAvailable|folioAvailable|reconnectDelayMs)\b" web/dist/app.js`
Expected: only `store.<name>` occurrences remain (no bare identifier). Review the output; any bare hit is a missed migration.

- [ ] **Step 4: JS syntax check + served-asset check**

Run the Task 9 Step 3 commands. Expected: PASS.

- [ ] **Step 5: Manual smoke** — full checklist: add-repo form + folder browser, open/close tabs, `?t=` and `?view=` deep links, stage/unstage/discard, commit, branch create/filter, stash create/apply/drop, log streaming, diff open/close, terminal dock switch + unseen dot.

- [ ] **Step 6: Commit**

```bash
git add web/dist/common.js web/dist/app.js
git commit -m "refactor(web): centralize shared mutable state in a store object"
```

---

### Task 11: Extract `diff.js`

**Files:**
- Create: `web/dist/diff.js`
- Modify: `web/dist/app.js` (remove moved functions; import from `./diff.js`)

**Interfaces:**
- Produces: `diff.js` exporting `toggleDiff`, `showDiff`, `renderFilePair`, `renderSideBySide`, `scrollToFirstDiffBlock`, `scrollDetailToBlock`, `stepDiffBlock` (and its private `splitLines`/`alignLines` if only diff uses them).
- Consumes: `store` (for `pairCache`) and `splitLines`/`alignLines`/`lineCounts` from `common.js`.

- [ ] **Step 1: Move the diff region (app.js :1397-1527) into `diff.js`**, exporting the functions the wiring region (:1614-1643) calls. Add `import { store, splitLines, alignLines, lineCounts } from "./common.js";` as needed and `import { toggleDiff, showDiff, stepDiffBlock } from "./diff.js";` in `app.js`.

- [ ] **Step 2: JS syntax check + served-asset check** (Task 9 Step 3). Expected: PASS.

- [ ] **Step 3: Manual smoke** — click a changed file to open the side-by-side diff; `prev`/`next` block buttons step through hunks; untracked-file diff shows as an all-plus diff.

- [ ] **Step 4: Commit**

```bash
git add web/dist/diff.js web/dist/app.js
git commit -m "refactor(web): extract side-by-side diff rendering into diff.js"
```

---

### Task 12: Extract `terminal.js` and finalize the entry point

**Files:**
- Create: `web/dist/terminal.js`
- Modify: `web/dist/app.js` (move dock/terminal/folio regions; add `main()`; import from `./terminal.js`)

**Interfaces:**
- Produces: `terminal.js` exporting `showView`, `setView`, folio functions, and krust functions (including an `initDocks({ onChange })` or equivalent that starts `probeFolio`/`probeKrust` and registers `window.addEventListener("message", handleKrustActivity)`); `app.js` exposes a `main()` that calls it after DOM wiring.
- Consumes: `store`, `render`, `renderTabBar` from `common.js`.

- [ ] **Step 1: Move the Folio, dock/view, and krust regions (app.js :787-820, :1761-1946) into `terminal.js`.** Export `showView`, `setView`, `updateUrlView`, `setDockVisible`, `probeExternal` (from common? keep single home: leave `probeExternal` in `common.js` and import it), `folioFrameSrc`, `ensureFolioFrame`, `renderFileBrowser`, `probeFolio`, `handleKrustActivity`, `ensureKrustFrame`, `probeKrust`, `sessionIdFor`, `liveSessionId`, `krustFrameSrc`, `currentRepoPath`.

- [ ] **Step 2: Move the top-level boot calls into `main()` in `app.js`**: the current top-level `probeFolio(); setInterval(probeFolio, …)`, `probeKrust(); setInterval(probeKrust, …)`, `showView("dashboard")`, socket open, and add-repo form wiring all run inside `main()`, invoked once at the end of `app.js`. This removes cross-module top-level evaluation order hazards.

- [ ] **Step 3: JS syntax check + served-asset check**

```bash
for f in common diff terminal app; do cp web/dist/$f.js /tmp/opencode/$f.mjs && node --check /tmp/opencode/$f.mjs && rm /tmp/opencode/$f.mjs; done
cargo test serves_javascript_bundle
```
Expected: all four parse; test PASS.

- [ ] **Step 4: Manual smoke** — dashboard/files/terminal dock switching; `?view=term-1` deep link; repo switch rebinds the krust session and fires `/reset`; krust-down fallback to dashboard; folio-down fallback; unseen-output dot still appears.

- [ ] **Step 5: Commit**

```bash
git add web/dist/terminal.js web/dist/app.js
git commit -m "refactor(web): extract terminal and dock integration into terminal.js"
```

---

### Task 13: Final cleanup + AGENTS JS-tooling note

**Files:**
- Modify: `web/dist/app.js` (final sweep for dead code, region banners, import order)
- Create (optional): `/tmp/opencode/jscheck.py` brace/quote tokenizer per `AGENTS.md` §4 (dev-only scratch, not committed)
- Modify: `AGENTS.md` §4 only if the tooling line still claims node is absent (also handled in Task 15)

- [ ] **Step 1: Sweep `app.js`** for `//#region` banners that moved, unused imports, and any leftover duplicated helpers. Run `grep -nE "\b(getTabNameFromPath|splitLines|alignLines|lineCounts|probeExternal)\b" web/dist/app.js` — only imports and call sites should remain.

- [ ] **Step 2: Confirm no `export`/`import` mismatch**

Run: `grep -nE "^export " web/dist/common.js web/dist/diff.js web/dist/terminal.js` and cross-check every `import {...} from "./x.js"` name exists on the left side of an `export` in the source module. Fix any drift.

- [ ] **Step 3: Full verification**

```bash
for f in common diff terminal app; do cp web/dist/$f.js /tmp/opencode/$f.mjs && node --check /tmp/opencode/$f.mjs && rm /tmp/opencode/$f.mjs; done
cargo test
cargo clippy --features desktop --all-targets
```
Expected: all parse; `cargo test` green; clippy warning-free.

- [ ] **Step 4: Manual smoke** — repeat the Task 10 Step 5 checklist end to end on a fresh build.

- [ ] **Step 5: Commit**

```bash
git add web/dist/app.js
git commit -m "chore(web): tidy module entry point after ES-module split"
```

---

## Phase D — Documentation Sync

### Task 14: `ARCHITECTURE.md`

**Files:**
- Modify: `ARCHITECTURE.md:42-66` (tree), `:161-166` (routes note), `:262-293` (§3.7 Krust dock), `:422` (quick reference)

**Interfaces:** none (docs).

- [ ] **Step 1: Complete the directory tree** — add under `src/`: `actions.rs`, `folio.rs`, `test_support.rs`; under `server/`: `handlers.rs`; under `git/`: `actions.rs`, `files.rs`, `history.rs`, `status.rs`. Describe each in one line matching its module doc comment.

- [ ] **Step 2: Fix §3.7 (Krust)** — replace "two dock views (`term-1` / `term-2`)" with the single real view `term-1`; delete the "**Reset button**: each terminal view has a hover `Reset` overlay …" bullet (no such DOM element exists — repo-switch recycling still calls krust's `GET /reset?session_id=…`, so reword to that). Add a sentence that `L` is not a dock button: Log is a collapsible rollup section in `index.html`.

- [ ] **Step 3: Update §2/§8 for the module split** — under `web/dist/`, list `index.html`, `style.css`, `common.js`, `diff.js`, `terminal.js`, `app.js`; update the `web/dist/app.js` quick-reference row from "single file" to "entry module; shared core in common.js, diff in diff.js, docks/terminal in terminal.js". Keep invariant 1 ("no bundler") and add "native ES modules, no build step".

- [ ] **Step 4: Sanity check** — `grep -nE "term-2|Reset overlay" ARCHITECTURE.md` returns nothing.

- [ ] **Step 5: Commit**

```bash
git add ARCHITECTURE.md
git commit -m "docs: sync ARCHITECTURE.md with current modules and single terminal dock"
```

---

### Task 15: `AGENTS.md`

**Files:**
- Modify: `AGENTS.md` §2 ("krust integration"), §4 ("JS Syntax Checking")

- [ ] **Step 1: Correct the krust facts** — "two dock views (`term-1` / `term-2`)" → one view (`term-1`); remove the "`Reset` overlay button on each terminal view" claim (keep the fact that repo switches call `GET /reset?session_id=<old>`); change "left view dock (Dashboard/`F`/`L` + krust terminal views)" to reflect reality: dock buttons are Dashboard/`F` (files, folio) / `T` (terminal), and Log is a rollup section, not a dock button.

- [ ] **Step 2: Correct the JS tooling note** — replace "no node/deno/bun on the dev box for syntax checking" with the verified reality (`node` v22 is present; use `cp <f> /tmp/x.mjs && node --check /tmp/x.mjs` for ESM files), and note `web/dist/` is now ESM modules (`common.js`/`diff.js`/`terminal.js`/`app.js`) with no bundler.

- [ ] **Step 3: Sanity check** — `grep -nE "term-2|no node/deno/bun" AGENTS.md` returns nothing.

- [ ] **Step 4: Commit**

```bash
git add AGENTS.md
git commit -m "docs: correct AGENTS.md krust dock facts and JS tooling note"
```

---

## Final Verification (whole branch)

- [ ] `cargo test` — all existing + new tests pass.
- [ ] `cargo test --features desktop` — desktop suite passes (remote WS still connects without an Origin).
- [ ] `cargo clippy --features desktop --all-targets` — warning-free.
- [ ] `cargo check` (default) and `cargo check --features desktop` — clean.
- [ ] All four `web/dist/*.js` parse via `node --check` (`.mjs` copies).
- [ ] Manual smoke of the full web UI on a fresh `cargo build --release` + user restart.
- [ ] `git diff --stat HEAD~15` review: only intended files touched; no `web/dist` rebuild artifacts missing.
