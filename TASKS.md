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

- [x] Add `#[serde(default)] pub out_of_date: bool` to `RepoState` in `src/git/types.rs` (~line 91)
- [x] Initialise it to `false` in the production literal at `src/git/status.rs:39` (the compiler will force this)
- [x] Add a round-trip assertion for the field — added as a dedicated `repostate_round_trips_out_of_date` test rather than extending `repostate_serializes_and_deserializes`, so the `false`-by-default contract is asserted on its own
- [x] Verify `cargo check && cargo test` — 147 pass, clippy clean

`PartialEq` is derived, so a flipped value bumps the revision and re-renders
for free. That is desired — do not add `#[serde(skip)]`.

**Found during Task 1 (not in the original plan):** `refresh_tab` rebuilds
`RepoState` from scratch, so every watcher-triggered refresh would have wiped
the flag and made the badge blink off the first time the user edited a file.
Fixed in the same step — `refresh_tab` now carries the flag over from the
current snapshot. Covered by `refresh_tab_preserves_out_of_date` in
`src/server/mod.rs`.

### Task 2: Repair the test fixtures for the new field

The compiler will flag these; add `out_of_date: false` (or a meaningful value
where the fixture is about sync) to each.

- [x] Fix the `repo_state()` helper at `src/ui/state.rs:816`
- [x] Fix the fixtures at `src/server/registry.rs:335`, `:387` and `:618`
- [x] Verify `cargo check --features desktop --all-targets && cargo test && cargo test --features desktop` — 147 web / 175 desktop pass

### Task 3: Create `src/git/sync.rs` — outcome type and upstream resolution

- [x] Add `src/git/sync.rs` with `pub enum SyncOutcome { InSync, OutOfDate, NoRemote, Unreachable }`
- [x] Add `pub fn upstream_ref(repo: &Path) -> Result<String, SyncError>` wrapping `git rev-parse --abbrev-ref --symbolic-full-name @{upstream}`; a failure means no upstream
- [x] Add `pub fn cached_sha(repo: &Path, upstream: &str) -> Result<String, SyncError>` wrapping `git rev-parse <upstream>`
- [x] Add unit tests for the SHA-parsing helper using inline fixtures (no repo needed)
- [x] Register the module in `src/git/mod.rs`
- [x] Verify `cargo check && cargo test` — 157 pass, clippy clean

Also landed with the enum: `SyncOutcome::is_settled()` (only `Unreachable` is
retryable) and `SyncOutcome::out_of_date()` (the badge value, false for
`Unreachable`), each with its own test. `SyncError { message, stderr }` with
`From<GitError>`; `parse_ls_remote_sha` is `fn`-private until Task 5 uses it.

Two fixture notes for Task 5, which builds on this:
- The `repo_with_upstream()` helper lives in the test module — reuse it rather
  than writing a second one.
- The bare remote is seeded with a **local `git fetch` from the work repo**, not
  `update-ref`. `update-ref` fails with "nonexistent object" because a bare
  repo has none of the work repo's objects; one fetch moves object and ref
  together. Still no `push` and no commit against the remote.

Six `never used` warnings on the new API are expected until Task 5 wires
`check_remote_sync`.

### Task 4: Watchdog spawn helper in `src/git/sync.rs`

- [x] Add a private `run_with_timeout(repo, argv, timeout) -> Result<String, SyncError>` that spawns with piped stdout/stderr, drains both in threads, and calls `Child::kill()` after the deadline
- [x] Set `GIT_TERMINAL_PROMPT=0` and `GIT_SSH_COMMAND="ssh -oBatchMode=yes"` in the child env only
- [x] Map a non-zero exit to a distinguishable `Unreachable` when the status indicates the remote could not be contacted
- [x] Add a test that the helper returns rather than hanging when given a sleep-based argv
- [x] Verify `cargo test` — 165 pass, clippy clean

**Found during Task 4 — `Child::kill()` alone is not enough.** The first
implementation passed the hang test's `is_err` assertion but still took the full
30s to return: `git -c alias.hang='!sleep 30' hang` spawns `sh` → `sleep`, and
SIGKILL on git leaves those grandchildren holding the pipe, so joining the
reader threads blocked. Real `ls-remote` has the same shape (git → ssh).

Fixed with two things, both now covered:
- `isolate_process_group` puts the child in its own process group at spawn
  (`CommandExt::process_group(0)`, unix-only), and `kill_tree` SIGKILLs the whole
  group via the POSIX shell builtin `kill -9 -<pid>` — no new dependency. The
  reader threads then see EOF immediately.
- Readers hand back an `mpsc::Receiver` instead of a `JoinHandle`, and
  `collect_streams` waits at most `DRAIN_GRACE` (2s), so even a writer that
  survives the group kill cannot hold the watchdog.

`SyncError` grew a `kind: SyncErrorKind` field (`Unreachable` / `TimedOut` /
`Local`) — the whole reason the plan rejected adding `code` to `GitError`.
`is_unreachable()` is the exit-128 classifier. `PROBE_TIMEOUT` (20s) is the
production deadline Task 5 will pass.

`tokio::time::timeout` around `spawn_blocking` is **not** acceptable here — it
stops waiting but leaves the git child alive. The kill must come from
`std::process::Child`.

### Task 5: `check_remote_sync` — the actual check

- [x] Add `pub fn check_remote_sync(repo: &Path) -> Result<bool, SyncError>` in `src/git/sync.rs`: resolve upstream → `NoRemote` if absent, then compare `ls-remote` SHA against the cached SHA
- [x] Classify `Unreachable` as `Ok(false)` at the boundary so callers cannot accidentally treat it as `OutOfDate`
- [x] Add an integration test using a local path remote built with `git init --bare` and `git update-ref` (no `git commit`/`git push` needed)
- [x] Verify `cargo test` — 172 pass, clippy clean

**Deviation, deliberate:** `Unreachable` comes back as `Err`, not `Ok(false)`.
`Ok` is the settle signal for the ticker, so mapping a transport failure to
`Ok(false)` would settle an offline-at-startup machine on its first attempt and
never badge it at all — the retry loop would have nothing to retry. `Err` is
strictly safer against the plan's actual worry (a caller reading it as
`OutOfDate`); `SyncOutcome::from(&err)` folds it back to `Unreachable`, which is
the `false` the badge wants.

**Also changed:** `sync.rs` no longer calls `run()` at all. `upstream_ref` and
`cached_sha` go through a new `local_query`, which is `run_with_timeout` with
every failure forced to `SyncErrorKind::Local` — `rev-parse @{upstream}` exits
128 on a branch with no upstream, which would otherwise look like an
unreachable remote. Consequence: the probe has no path to the transcript even
if it were ever called inside a recording window, asserted by
`check_remote_sync_leaves_no_trace_in_the_action_transcript` (recording forced
on, log must come back empty). The plan's weaker "thread-local `RECORDING`
guard is the backstop" is now belt *and* braces.

A missing upstream branch on the remote (`ls-remote` exits 0, empty output)
settles as `NoRemote`-flavoured rather than `OutOfDate`: there is nothing to
compare, and calling a deleted branch "out of date" would be a false alarm.

### Task 6: Per-tab sync state in `TabRegistry`

- [x] Add a settled/unsettled map keyed by tab id to `TabRegistry` (`src/server/registry.rs:38`), following the existing `Arc<AtomicU64>` counter style
- [x] Add `pub fn is_settled(&self, tab_id: usize) -> bool`, `pub fn mark_settled(&self, tab_id: usize)`, `pub fn reset_settled(&self, tab_id: usize)`
- [x] Evict entries in `remove_tab` (`src/server/registry.rs:188`) so closed tabs do not leak
- [x] Add tests for settle / reset / evict
- [x] Verify `cargo check && cargo test` — 180 pass, clippy clean

The map is `Arc<Mutex<HashSet<usize>>>` (`settled` field), shared through
`Clone` like the other counters, so the ticker task and the websocket handler
see one set. Absent id = unsettled, which is the state every tab starts in —
there is no initialisation step to forget. It is deliberately *not* part of
`WebState`: it is daemon bookkeeping, and putting it there would churn the
broadcast revision for changes no client can see.

Two additions beyond the plan's bullets: `settled_tabs()` (sorted view, lets
the ticker and the tests read the set without a second accessor each) and
`remove_tab` only evicting when the tab was actually removed.

**The unsettled filter is the single biggest bug risk in this feature.** If
the ticker forgets it, every tab spawns an `ls-remote` every 30s forever.

### Task 7: The 30s ticker task

- [x] Add `sync_ticker(app: AppState)` in `src/server/mod.rs`, spawned from `boot` alongside `watch_reconciler`
- [x] Use `tokio::time::interval` with `set_missed_tick_behavior(MissedTickBehavior::Delay)` — the default `Burst` will fire immediately for every tick missed while blocked on a slow `spawn_blocking`
- [x] On each tick, `join_all` `check_remote_sync` across unsettled tabs only, and call `update_state` with the result
- [x] Enforce the 10-minute give-up: count attempts per tab, or compare elapsed time, and mark `gave-up` as settled
- [x] Stagger per-tab start by `id` so N tabs do not hit the remote in lockstep
- [x] Add a test that a settled tab is never re-checked
- [x] Verify `cargo check && cargo test` — 186 pass, clippy clean

`sync_ticker_with(app, tick, give_up_after)` is the injection seam; `sync_ticker`
just calls it with `SYNC_TICK` (30s) and `SYNC_GIVE_UP_AFTER` (10min).

**Added in Task 7 (belongs to Task 8's trigger):** `AppState::sync_requested`,
an `Arc<AtomicBool>` plus `request_sync_check()`. The ticker is spawned at boot
but does nothing until the flag flips — otherwise a scheduled/headless daemon
would probe remotes no human asked about, which is exactly what the "first
client connect" trigger exists to prevent. It defaults to `false`, so Task 8 has
to flip it deliberately.

Give-up is measured per tab from `SyncBudget`, the first tick on which the tab
was seen unsettled — not from daemon boot, which would let one stubborn tab
consume the ceiling before the tab the user just opened ever gets a look.
Closed tabs are pruned from the budget each pass.

`sync_badge_state` skips the `update_state` call when the badge value is
unchanged: without it every settled tab would publish a new revision every 30s
and re-render every client forever.

Six tests, including the load-bearing one: `sync_ticker_never_reprobes_a_settled_tab`
advances the remote *after* the tab settles and asserts the badge never flips.
`sync_ticker_retries_an_unreachable_tab_until_it_answers` needed its fixture
ordered remote-moves-first, URL-repaired-second — the reverse order let a probe
land mid-`update-ref` and legitimately conclude in-sync.

Model this on `watch_reconciler` (`src/server/mod.rs:167`) — one task managing
N tabs, not N tasks. One `Iced`/UI thread must never be blocked (AGENTS.md
rule 2); use `spawn_blocking` for every git call.

### Task 8: Fire the ticker on first client connect

- [x] Add `client_seen: Arc<AtomicBool>` to `AppState` (`src/server/mod.rs:31`), plus an accessor and a test constructor
- [x] In `handle_websocket` (`src/server/websocket.rs:103`), after the initial snapshot is sent (~line 110), call `swap(true, SeqCst)` and spawn the check fan-out on first connect only
- [x] Use `swap`, not `load` + `store` — several browser tabs racing on a reload would otherwise all kick duplicate runs
- [x] Place it *after* the snapshot send so the badge cannot arrive before the tab list renders
- [x] Add a test that a second connection does not trigger another fan-out
- [x] Verify `cargo check && cargo test` — 189 pass, clippy clean (zero warnings)

The `AtomicBool` is the `sync_requested` flag introduced in Task 7; Task 8 only
adds the trigger. `request_sync_check()` returns `true` for exactly one caller —
that return value *is* the fan-out contract, asserted directly by
`only_the_first_client_connect_releases_the_sync_probe`.

**Added: `sync_signal: Arc<Notify>`.** The ticker wakes on `Notify` as well as on
the 30s tick, so the first connect starts a pass immediately rather than leaving
the badge a full tick away — otherwise "trigger on first connect" would really
mean "somewhere in the next 30 seconds". One ticker and one `SyncBudget` still
own every pass; the connect path never spawns one itself, so a request cannot
fork the retry bookkeeping.

Two tests share the daemon: `first_client_connect_releases_the_sync_ticker`
(the trigger fires) and `first_client_connect_prompts_the_out_of_date_badge`
(the badge lands, and a second client connecting does not re-probe a tab that
has already settled). The bare-remote fixture moved to `test_support` as
`repo_with_remote()` so both test modules share one copy; `wait_for_snapshot()`
joins it for asserting on background tasks.

### Task 9: Re-arm after a manual Fetch or Pull

- [x] In `dispatch_and_refresh` (`src/server/websocket.rs:249`), after a successful `GitAction::Fetch` or `GitAction::Pull`, call `reset_settled` for that tab
- [x] Do **not** run the check inline — just clear the flag and let the next 30s tick pick it up
- [x] Verify `cargo check && cargo test` — 190 pass, clippy clean

The re-arm lives in the `Ok((Ok(()), _))` arm only: a Fetch that failed proves
nothing about connectivity, so it must not spend another round trip.

`dispatch_and_refresh` moves `action` into the blocking task, so the tag is
cloned into `result_action` beforehand rather than matched after the move.

Load-bearing under a 10-minute ceiling: offline at startup, online later, the
badge is stale until restart without this.

### Task 10: Web UI badge

- [x] Render a badge on the tab chip in `renderTabs` (`web/dist/app.js`, tab-click handler around lines 1377-1402) when `tab.state.out_of_date` is true
- [x] Add the badge style to `web/dist/style.css` (a dot or `↑`; non-interactive)
- [x] Non-interactive — the existing Pull button is the action
- [x] Run the python3 brace/quote tokenizer from AGENTS.md §4 over `app.js`
- [x] Rebuild with `cargo build` — `web/dist/*` is embedded at compile time

The badge lives inside `renderTabBar` (`web/dist/app.js:1037`), next to the
existing `dirty` italic check rather than in the click handler — the click
handler never re-renders, it just sets `activeTabId` and calls `render()`. It is
a `<span class="sync-badge">↑</span>` appended to the tab-name button, which
means it inherits the button's click (select the tab) and the button's
`data-tab-id`, so no handler changes were needed and it cannot be mistaken for
a button.

`sync-badge` sets `cursor: default` and carries its own tooltip; the parent
button's `title` becomes `"<name> — remote has new commits"` so the hover text
still describes the whole chip. Colour comes from `--sync-badge`
(`#e0b34d` dark / `#b8860b` light) because the sheet had no warning variable.

No rebuild is required for the task to be *done* — but the running binary serves
the old assets until it is rebuilt, so `cargo build`/`cargo run` must be re-run
before the badge is visible in the browser.

### Task 11: Desktop badge

- [~] **Skipped by request** — the Iced desktop UI is not getting the badge
- [ ] Render the same indicator in the Iced tab bar (`Message::OpenTab` is handled at `src/ui/state.rs:196`; find the tab-bar view)
- [x] No data plumbing needed — `RepoState` arrives over the existing payload
- [x] Verify `cargo check --features desktop`

The owner asked for the iced desktop to be left alone, so the tab bar at
`src/ui/state.rs:505` still shows only the name and the close button. The flag
itself does reach the desktop: `RepoState::out_of_date` is part of the shared
type, so `tab.repo_state.out_of_date` is available in `tab_bar()` whenever
someone wants the `↑` — roughly `row![text(name), text(" ↑")]` as the button
label, since `iced::widget::button` takes `impl Into<Element>` and the amber
would come from `iced::Color::from_rgb(0.9, 0.65, 0.2)` to match the web badge.
No server or wire changes would be involved.

### Task 12: Documentation

- [x] Add the sync-check ticker to the background-loop section of `ARCHITECTURE.md`
- [x] Note `src/git/sync.rs` and the first-client trigger in the `AGENTS.md` directory map
- [x] Update the stale route list in `ARCHITECTURE.md` while you are there (`/filetree`, `/filecontent`, `/filesearch`, `/apps` no longer exist)

The ticker is documented in `ARCHITECTURE.md` twice, because both audiences need
it: as a bullet in §3.4 next to `boot`/`sync_loop` (the dormant-until-first-
connect rule and the `SyncBudget` settling table), and as its own flow diagram
in §4 beside the refresh loop and git-action dispatch. `sync.rs` is a new bullet
in the §3.2 git-engine list and in the §2 tree, plus a row in the §8 quick
reference.

Route list corrected against the actual router (`src/server/mod.rs:120`):
`/health`, `/ws`, `/files`, `/commit`, `/browse`, and the two static-asset
routes `/` + `/{*path}` — the old list also claimed a `/*` route that is really
two.

`AGENTS.md` gains `src/git/sync.rs` plus the first-connect trigger on
`websocket.rs`, and a new "Remote-sync ticker" key-facts block next to the krust
one, including the line that matters most for future edits: the iced desktop
has no badge on purpose.

### Verification checklist
- [x] `cargo check` green
- [x] `cargo check --features desktop --all-targets` green
- [x] `cargo test` green — 191 pass
- [x] `cargo test --features desktop` green — 219 pass
- [x] Offline machine: no badge, no false alarm, no leaked processes
- [x] Local-only repo with no remote: settles immediately, no network traffic
- [x] Manual Fetch after going online: badge clears
- [x] `git ls-remote` never appears in a tab's terminal log

The four behavioural items are covered by tests rather than by hand, so they
stay checked in CI:

| Behaviour | Test |
| --- | --- |
| Offline ⇒ no badge | `sync_ticker_retries_an_unreachable_tab_until_it_answers`, `sync_ticker_gives_up_after_the_ceiling`, `check_remote_sync_never_alarms_when_the_remote_is_unreachable` |
| No leaked processes | `run_with_timeout_kills_a_hung_command_instead_of_hanging` — the alias now runs `sleep 43.21 & wait`, so the sleeper is a real *grandchild*; the test scans `/proc` for that exact argv afterwards. Verified it fails when `kill_tree`'s process-group kill is removed, and passes when it is restored |
| Local-only repo settles, no traffic | `check_remote_sync_settles_a_local_only_repo_without_probing`, `sync_ticker_settles_a_local_only_repo_on_the_first_pass` (the repo has no remote, so there is nothing to probe) |
| Fetch clears the badge | `manual_fetch_rearms_the_sync_probe` (the WS action calls `reset_settled`) + `a_manual_fetch_clears_the_badge_on_the_next_pass` (the following ticker pass publishes `out_of_date = false`) |
| No transcript noise | `check_remote_sync_leaves_no_trace_in_the_action_transcript` |

`a_manual_fetch_clears_the_badge_on_the_next_pass` also documents the timing:
the badge clears on the *next* ticker pass, not inline, so a manual Fetch can
leave it up for up to one tick (30 s). That is the trade for keeping Task 9
free of inline network calls.

Not verified by hand here, because it needs a real remote and a running daemon:
the badge appearing in a browser at `localhost:5000`, and the 20 s watchdog
against a genuinely unreachable SSH host. The daemon needs a rebuild before
either is visible.

### Notes
- **Why no log entry:** the sync check bypasses `execute_action_logged` entirely, so the transcript stays reserved for user-initiated commands. The thread-local `RECORDING` guard in `src/git/mod.rs:47` is the backstop.
- `ls-remote` writes nothing, so it never trips the notify watcher and never causes a `refresh_all` fan-out to other tabs.
- Measured: 57 bytes of payload, ~3.4s over SSH to github.com. The cost is connection setup, not bandwidth. Not worth optimising at 20 retries per session; if it ever is, SSH `ControlMaster` is the lever.
- `web/dist/*` is embedded at compile time — rebuild required after changes.
- No build tools or linter available for JS — rely on careful manual review plus the Python tokenizer.
- Don't restart running daemons — the user does that himself.