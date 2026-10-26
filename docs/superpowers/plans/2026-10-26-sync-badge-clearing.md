# Sync Badge Clearing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a tab's "remote is ahead" badge disappear when the user pulls or fetches.

**Architecture:** The badge is `RepoState.out_of_date`, owned by the sync ticker in `src/server/mod.rs`. Three fixes: the per-tab give-up clock is re-armed when a tab settles (today it is stale, so a re-armed probe is skipped and the badge sticks); the ticker is woken when a tab is re-armed instead of waiting up to 30s; and a successful Pull/Fetch publishes `out_of_date = false` immediately. A fourth change closes a read-modify-write race on the flag.

**Tech Stack:** Rust, tokio, axum. Tests use the existing `src/test_support.rs` helpers.

**Spec:** `docs/superpowers/specs/2026-10-26-lan-settings-and-sync-badge-design.md` (Part 2)

## Global Constraints

- Never block a runtime worker: git runs under `tokio::task::spawn_blocking` (AGENTS.md §3.2).
- A settled tab must never be re-probed within a session; the ticker's `is_settled` filter is load-bearing and must keep holding.
- Nothing here may cause a probe (a network call) for a tab that is already settled.
- `cargo check` and `cargo test` must pass after every task.

## Review Focus

1. A tab open longer than the 10-minute ceiling shows a stuck badge after a pull (Task 1).
2. A successful pull/fetch must clear the badge without a visible delay (Task 2).
3. A **failed** pull must leave the badge exactly as it was (Task 2).
4. A tab with no upstream or no remote must settle quietly, not panic (Task 2).
5. `refresh_tab` must not wipe a badge the ticker just set, nor resurrect one it just cleared (Task 3).

---

### Task 1: re-arm the give-up clock when a tab settles

**Files:**
- Modify: `src/server/mod.rs` (`SyncPass` handling in `sync_pass` ~line 378, `SyncBudget` ~line 436)
- Test: `src/server/mod.rs` (`mod tests`)

**Interfaces:**
- Consumes: `SyncBudget` (existing).
- Produces: `SyncBudget::forget(&mut self, tab_id: usize)`.

**Why:** `SyncBudget.started[tab]` is set the first time a tab is seen unsettled and only pruned when the tab is *closed*. Once a tab has been open past `SYNC_GIVE_UP_AFTER`, that entry is stale, so the next pass — the one a Pull just re-armed — classifies the tab as expired, returns `GaveUp`, and settles it **without probing**, leaving `out_of_date` untouched. Hence a badge that survives until a daemon restart.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn settling_a_tab_forgets_its_budget_entry() {
    let mut budget = SyncBudget::new(std::time::Duration::from_secs(600));
    budget.started_at(7, std::time::Instant::now());
    budget.forget(7);
    assert!(!budget.started.contains_key(&7));
}

#[tokio::test]
async fn a_rearmed_tab_does_not_inherit_a_stale_give_up_clock() {
    // The regression: a tab that earned its badge long ago, then had the
    // remote fetched, must re-probe and clear — not give up on a stale clock.
    let (dir, _bare, advance) = repo_with_remote();
    let app = app_for_repo(dir.path());
    advance(dir.path());

    let mut budget = SyncBudget::new(std::time::Duration::from_secs(600));
    let first = std::time::Instant::now();
    sync_pass(&app, &mut budget, first).await;
    assert!(out_of_date(&app), "the first pass should earn the badge");
    assert!(app.registry.is_settled(0));

    // The tab sits settled well past the ceiling.
    let later = first + std::time::Duration::from_secs(700);

    // A pull re-arms it, and the remote it complained about is now fetched.
    app.registry.reset_settled(0);
    std::process::Command::new("git")
        .args(["fetch", "origin"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    sync_pass(&app, &mut budget, later).await;

    assert!(
        !out_of_date(&app),
        "a re-armed probe gave up on a clock left over from before the pull"
    );
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib a_rearmed_tab_does_not_inherit`
Expected: FAIL — `forget` does not exist; once added, the assertion still fails until the call sites are wired.

- [ ] **Step 3: Implement**

Add `forget` next to `forget_missing` in `SyncBudget`. In `sync_pass`, call `budget.forget(tab_id)` alongside **every** `mark_settled` — both the `SyncPass::GaveUp` arm and the `SyncPass::Answered` arm. A settled tab keeps no clock; the next time it becomes unsettled it starts a fresh one.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib server::`
Expected: PASS, including the pre-existing `sync_ticker_gives_up_after_the_ceiling`.

- [ ] **Step 5: Commit**

```bash
git add src/server/mod.rs
git commit -m "fix(sync): re-arm the give-up clock when a tab settles"
```

---

### Task 2: clear the badge on a successful pull or fetch

**Files:**
- Modify: `src/server/mod.rs` (`AppState`), `src/server/websocket.rs` (~line 430-467)
- Test: `src/server/mod.rs`, `src/server/websocket.rs`

**Interfaces:**
- Consumes: `reset_settled` (existing), `SyncOutcome` (existing).
- Produces: `AppState::wake_sync(&self)`; `AppState::clear_sync_badge(&self, tab_id: usize)`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn clear_sync_badge_publishes_false_only_when_needed() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let app = app_for(dir.path());
    app.registry.update_state(0, crate::git::types::RepoState {
        out_of_date: true, ..Default::default()
    });
    app.clear_sync_badge(0);
    assert!(!out_of_date(&app));

    // Idempotent: a second call must not publish a new revision.
    let revision = app.registry.revision();
    app.clear_sync_badge(0);
    assert_eq!(app.registry.revision(), revision);
}
```

And in `src/server/websocket.rs`, an integration test following the existing action-driving harness (see the tests using `run_server` + `encode_client_message`):

```rust
#[tokio::test]
async fn a_successful_pull_clears_the_badge_immediately() {
    // No sync ticker is spawned: the clear must come from the action itself,
    // which is what makes the badge vanish on the click rather than a tick later.
    let (dir, _bare, advance) = repo_with_remote();
    advance(dir.path());
    // ... start the server, connect, assert the badge is up, send
    //     encode_client_message(Some(0), &GitAction::Pull), then assert
    //     the next state frames report out_of_date == false.
}

#[tokio::test]
async fn a_failed_pull_leaves_the_badge_alone() {
    // Point origin at a path that does not exist, then Pull: the action fails,
    // so out_of_date must still be true.
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib clear_sync_badge`
Expected: FAIL — method does not exist.

- [ ] **Step 3: Implement**

`wake_sync` is `self.sync_signal.notify_one()`. `clear_sync_badge` reuses the "publish only when the value changes" shape of `sync_badge_state` so it never churns the revision.

In `websocket.rs`, inside the existing `Ok((Ok(()), _))` arm, after `app.registry.reset_settled(tab_id)`:

```rust
app.registry.reset_settled(tab_id);
app.clear_sync_badge(tab_id);
app.wake_sync();
```

Justification to keep in the comment: a successful `git pull` leaves the branch at the upstream tip and a `git fetch` refreshes the remote-tracking ref, so "remote is ahead of what we know" is already false. The woken ticker re-probes and can re-raise the badge if the remote moved again meanwhile.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/server/mod.rs src/server/websocket.rs
git commit -m "fix(sync): clear the badge on a successful pull/fetch and wake the ticker"
```

---

### Task 3: stop `refresh_tab` racing the badge

**Files:**
- Modify: `src/server/registry.rs` (`TabRegistry`), `src/server/mod.rs` (`refresh_tab` ~line 157)
- Test: `src/server/registry.rs`, `src/server/mod.rs`

**Interfaces:**
- Consumes: `TabRegistry::modify` (private, existing).
- Produces: `TabRegistry::update_state_keeping_sync(&self, id: usize, state: RepoState)`.

**Why:** `refresh_tab` reads `out_of_date` from a snapshot and writes it back outside the registry's write lock. The ticker can write `false` in between and have it clobbered. Copying the flag under the same lock removes the window without changing the observable carry-over behaviour that `refresh_tab_preserves_out_of_date` pins.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn update_state_keeping_sync_keeps_the_current_badge_value() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let app = app_for(dir.path());
    app.registry.update_state(0, crate::git::types::RepoState {
        out_of_date: true, ..Default::default()
    });

    // A fresh status collection must never be allowed to own the flag.
    app.registry.update_state_keeping_sync(0, crate::git::types::RepoState {
        out_of_date: false,
        current_branch: "topic".to_string(),
        ..Default::default()
    });

    let state = app.registry.snapshot();
    assert!(state.tabs[0].state.out_of_date, "the ticker owns this flag");
    assert_eq!(state.tabs[0].state.current_branch, "topic", "the rest of the state must land");
}

#[test]
fn update_state_keeping_sync_ignores_an_unknown_tab() {
    let app = app_for(/* any repo */);
    let before = app.registry.revision();
    app.registry.update_state_keeping_sync(999, Default::default());
    assert_eq!(app.registry.revision(), before);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib update_state_keeping_sync`
Expected: FAIL — method does not exist.

- [ ] **Step 3: Implement**

Implement it with `modify`: find the tab, set `state.out_of_date = tab.state.out_of_date`, return `false` when the result equals the current state (so no pointless revision bump). Switch `refresh_tab` to use it and delete its snapshot read.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test`
Expected: PASS, including the existing `refresh_tab_preserves_out_of_date`.

- [ ] **Step 5: Commit**

```bash
git add src/server/registry.rs src/server/mod.rs
git commit -m "fix(sync): carry out_of_date under the registry lock"
```
