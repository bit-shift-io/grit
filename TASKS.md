# Tasks

Current task list. One section per in-flight feature.

- **Remote Sync Indicator** — open, all items unchecked.

Rationale, measurements and rejected alternatives for the active plan live in
`NOTES.md`. Read it before starting.

---

## Remote Sync Indicator (open)

### Goal
Show a basic "this repo is out of sync with origin" badge on each tab and
nothing more. No auto-pull, no auto-merge — the user pulls themselves.
Detected via `git ls-remote`, once per tab per session.

### Decisions locked (do not re-litigate mid-implementation)
- `git ls-remote origin refs/heads/<upstream-branch>`, compared against the
  cached `refs/remotes/<upstream>` SHA. Never `git fetch`.
- Trigger: first WebSocket client connect after daemon start. Not boot, not
  tab-select. Covers both UIs because the desktop GUI is itself a `/ws` client.
- Retry every 30s until a **conclusive** answer; give up after 10 minutes.
  `Unreachable` retries; `InSync` / `OutOfDate` / `NoRemote` all settle.
- Failures are never alarming — `Unreachable` yields `out_of_date: false`.
- **No new `GitAction` variant.** Do not touch `src/git/actions.rs:562` or
  the round-trip list at `src/git/types.rs:301`.
- **Do not add `code` to `GitError`.** Considered and rejected: 11 literal
  construction sites, and `src/git/sync.rs` cannot use `run()` anyway because
  it needs a watchdog. `sync.rs` gets its own error type.

### Prerequisites
- Read `NOTES.md` first. The "Known Gaps" section explains why there is a
  dedicated spawn helper instead of reusing `src/git/mod.rs::run`.
- No client-side protocol or `app.js` changes are required for the trigger.

---

### Task 1: Add the `out_of_date` field to `RepoState`

- [ ] Add `#[serde(default)] pub out_of_date: bool` to `RepoState` in `src/git/types.rs` (~line 91)
- [ ] Initialise it to `false` in the production literal at `src/git/status.rs:39` (the compiler will force this)
- [ ] Add a round-trip assertion for the field in the `repostate_serializes_and_deserializes` test at `src/git/types.rs:230`
- [ ] Verify `cargo check && cargo test`

`PartialEq` is derived, so a flipped value bumps the revision and re-renders
for free. That is desired — do not add `#[serde(skip)]`.

### Task 2: Repair the test fixtures for the new field

The compiler will flag these; add `out_of_date: false` (or a meaningful value
where the fixture is about sync) to each.

- [ ] Fix the `repo_state()` helper at `src/ui/state.rs:816`
- [ ] Fix the fixtures at `src/server/registry.rs:335`, `:387` and `:618`
- [ ] Verify `cargo check --features desktop --all-targets && cargo test && cargo test --features desktop`

### Task 3: Create `src/git/sync.rs` — outcome type and upstream resolution

- [ ] Add `src/git/sync.rs` with `pub enum SyncOutcome { InSync, OutOfDate, NoRemote, Unreachable }`
- [ ] Add `pub fn upstream_ref(repo: &Path) -> Result<String, SyncError>` wrapping `git rev-parse --abbrev-ref --symbolic-full-name @{upstream}`; a failure means no upstream
- [ ] Add `pub fn cached_sha(repo: &Path, upstream: &str) -> Result<String, SyncError>` wrapping `git rev-parse <upstream>`
- [ ] Add unit tests for the SHA-parsing helper using inline fixtures (no repo needed)
- [ ] Register the module in `src/git/mod.rs`
- [ ] Verify `cargo check && cargo test`

### Task 4: Watchdog spawn helper in `src/git/sync.rs`

- [ ] Add a private `run_with_timeout(repo, argv, timeout) -> Result<String, SyncError>` that spawns with piped stdout/stderr, drains both in threads, and calls `Child::kill()` after the deadline
- [ ] Set `GIT_TERMINAL_PROMPT=0` and `GIT_SSH_COMMAND="ssh -oBatchMode=yes"` in the child env only
- [ ] Map a non-zero exit to a distinguishable `Unreachable` when the status indicates the remote could not be contacted
- [ ] Add a test that the helper returns rather than hanging when given a sleep-based argv
- [ ] Verify `cargo test`

`tokio::time::timeout` around `spawn_blocking` is **not** acceptable here — it
stops waiting but leaves the git child alive. The kill must come from
`std::process::Child`.

### Task 5: `check_remote_sync` — the actual check

- [ ] Add `pub fn check_remote_sync(repo: &Path) -> Result<bool, SyncError>` in `src/git/sync.rs`: resolve upstream → `NoRemote` if absent, then compare `ls-remote` SHA against the cached SHA
- [ ] Classify `Unreachable` as `Ok(false)` at the boundary so callers cannot accidentally treat it as `OutOfDate`
- [ ] Add an integration test using a local path remote built with `git init --bare` and `git update-ref` (no `git commit`/`git push` needed)
- [ ] Verify `cargo test`

### Task 6: Per-tab sync state in `TabRegistry`

- [ ] Add a settled/unsettled map keyed by tab id to `TabRegistry` (`src/server/registry.rs:38`), following the existing `Arc<AtomicU64>` counter style
- [ ] Add `pub fn is_settled(&self, tab_id: usize) -> bool`, `pub fn mark_settled(&self, tab_id: usize)`, `pub fn reset_settled(&self, tab_id: usize)`
- [ ] Evict entries in `remove_tab` (`src/server/registry.rs:188`) so closed tabs do not leak
- [ ] Add tests for settle / reset / evict
- [ ] Verify `cargo check && cargo test`

**The unsettled filter is the single biggest bug risk in this feature.** If
the ticker forgets it, every tab spawns an `ls-remote` every 30s forever.

### Task 7: The 30s ticker task

- [ ] Add `sync_ticker(app: AppState)` in `src/server/mod.rs`, spawned from `boot` alongside `watch_reconciler`
- [ ] Use `tokio::time::interval` with `set_missed_tick_behavior(MissedTickBehavior::Delay)` — the default `Burst` will fire immediately for every tick missed while blocked on a slow `spawn_blocking`
- [ ] On each tick, `join_all` `check_remote_sync` across unsettled tabs only, and call `update_state` with the result
- [ ] Enforce the 10-minute give-up: count attempts per tab, or compare elapsed time, and mark `gave-up` as settled
- [ ] Stagger per-tab start by `id` so N tabs do not hit the remote in lockstep
- [ ] Add a test that a settled tab is never re-checked
- [ ] Verify `cargo check && cargo test`

Model this on `watch_reconciler` (`src/server/mod.rs:167`) — one task managing
N tabs, not N tasks. One `Iced`/UI thread must never be blocked (AGENTS.md
rule 2); use `spawn_blocking` for every git call.

### Task 8: Fire the ticker on first client connect

- [ ] Add `client_seen: Arc<AtomicBool>` to `AppState` (`src/server/mod.rs:31`), plus an accessor and a test constructor
- [ ] In `handle_websocket` (`src/server/websocket.rs:103`), after the initial snapshot is sent (~line 110), call `swap(true, SeqCst)` and spawn the check fan-out on first connect only
- [ ] Use `swap`, not `load` + `store` — several browser tabs racing on a reload would otherwise all kick duplicate runs
- [ ] Place it *after* the snapshot send so the badge cannot arrive before the tab list renders
- [ ] Add a test that a second connection does not trigger another fan-out
- [ ] Verify `cargo check && cargo test`

### Task 9: Re-arm after a manual Fetch or Pull

- [ ] In `dispatch_and_refresh` (`src/server/websocket.rs:249`), after a successful `GitAction::Fetch` or `GitAction::Pull`, call `reset_settled` for that tab
- [ ] Do **not** run the check inline — just clear the flag and let the next 30s tick pick it up
- [ ] Verify `cargo check && cargo test`

Load-bearing under a 10-minute ceiling: offline at startup, online later, the
badge is stale until restart without this.

### Task 10: Web UI badge

- [ ] Render a badge on the tab chip in `renderTabs` (`web/dist/app.js`, tab-click handler around lines 1377-1402) when `tab.state.out_of_date` is true
- [ ] Add the badge style to `web/dist/style.css` (a dot or `↑`; non-interactive)
- [ ] Non-interactive — the existing Pull button is the action
- [ ] Run the python3 brace/quote tokenizer from AGENTS.md §4 over `app.js`
- [ ] Rebuild with `cargo build` — `web/dist/*` is embedded at compile time

### Task 11: Desktop badge

- [ ] Render the same indicator in the Iced tab bar (`Message::OpenTab` is handled at `src/ui/state.rs:196`; find the tab-bar view)
- [ ] No data plumbing needed — `RepoState` arrives over the existing payload
- [ ] Verify `cargo check --features desktop`

### Task 12: Documentation

- [ ] Add the sync-check ticker to the background-loop section of `ARCHITECTURE.md`
- [ ] Note `src/git/sync.rs` and the first-client trigger in the `AGENTS.md` directory map
- [ ] Update the stale route list in `ARCHITECTURE.md` while you are there (`/filetree`, `/filecontent`, `/filesearch`, `/apps` no longer exist)

### Verification checklist
- [ ] `cargo check` green
- [ ] `cargo check --features desktop --all-targets` green
- [ ] `cargo test` green
- [ ] `cargo test --features desktop` green
- [ ] Offline machine: no badge, no false alarm, no leaked processes
- [ ] Local-only repo with no remote: settles immediately, no network traffic
- [ ] Manual Fetch after going online: badge clears
- [ ] `git ls-remote` never appears in a tab's terminal log

### Notes
- **Why no log entry:** the sync check bypasses `execute_action_logged` entirely, so the transcript stays reserved for user-initiated commands. The thread-local `RECORDING` guard in `src/git/mod.rs:47` is the backstop.
- `ls-remote` writes nothing, so it never trips the notify watcher and never causes a `refresh_all` fan-out to other tabs.
- Measured: 57 bytes of payload, ~3.4s over SSH to github.com. The cost is connection setup, not bandwidth. Not worth optimising at 20 retries per session; if it ever is, SSH `ControlMaster` is the lever.
- `web/dist/*` is embedded at compile time — rebuild required after changes.
- No build tools or linter available for JS — rely on careful manual review plus the Python tokenizer.
- Don't restart running daemons — the user does that himself.