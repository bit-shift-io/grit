# Implementation Tasks: Branches Section (Web UI)

> **Goal:** Add a dedicated **Branches** section to the web UI with a scrollable list of local branches (current branch highlighted), branch search/filter, create new branch (from current HEAD), delete branch (with confirmation), and checkout/switch branch by clicking. The backend plumbing already exists (`GitAction::CheckoutBranch`, `CreateBranch`, `DeleteBranch`; `RepoState.branches`, `current_branch`). All work is web-only — no Rust changes needed.

## Phase 1 — HTML Structure

- [x] Add `<section id="branches-section" class="section" style="display: none;">` to `web/dist/index.html` between `#actions` and `<main>`, containing:
  - `<h2 class="section-title">Branches <span class="arrow">&#9662;</span></h2>`
  - `.section-body` with: a filter/search `<input id="branch-filter">`, a `<div id="branch-list">` container, and a `<button id="create-branch-btn">New Branch</button>` (1 file: `web/dist/index.html`)

## Phase 2 — CSS Styling

- [x] Add branch list styles to `web/dist/style.css`: `.branch-row` (flex row, hover highlight matching `.commit-head`), `.branch-row.current` (bold + accent indicator), `.branch-row .branch-actions` (right-aligned delete button), `#branch-filter` (match `#history-search` style), `#branch-list` (max-height + overflow-y scroll matching `#history`), `#create-branch-btn` (match action button style from `#actions button`) (1 file: `web/dist/style.css`)

## Phase 3 — JS: Render Branch List

- [x] Add `renderBranches(tab)` function in `web/dist/app.js`: reads `tab.state.branches` and `tab.state.current_branch`, filters by `#branch-filter` value (case-insensitive substring), renders each branch as a `.branch-row` with: branch name (clickable = checkout), current branch indicator (e.g. "current" label or checkmark), and a delete "×" button per non-current branch; empty state message when list is empty; call `renderBranches(tab)` from `render()` (1 file: `web/dist/app.js`)

## Phase 4 — JS: Event Handlers

- [x] Wire branch row click delegation on `#branch-list`: clicking the branch name (or row) sends `{ CheckoutBranch: branchName }` via `sendAction`; clicking the delete "×" button triggers `confirm()` then sends `{ DeleteBranch: branchName }`; wire `#create-branch-btn` click to prompt for new branch name, then send `{ CreateBranch: [name, currentHash] }` where `currentHash` is derived from `tab.state.history[0].hash` if available; wire `#branch-filter` input event to re-render the branch list (1 file: `web/dist/app.js`)

## Phase 5 — Section Visibility & Toggle

- [x] Add `branchesSection` to `render()` in `web/dist/app.js`: show when a repo tab is active (`style.display = "block"`), hide when in add-repo form; ensure the collapsible `.section-title` click handler from the existing `document.querySelectorAll(".section-title")` loop picks up the new section automatically (no extra wiring needed if the `<h2>` has class `section-title`) (1 file: `web/dist/app.js`)

## Phase 6 — Verification

- [x] Run `cargo check` and `cargo test` to confirm no Rust regressions (web-only change, but sanity check); manually verify: branch list renders, current branch highlighted, filter narrows list, create branch prompts and dispatches, delete branch confirms and dispatches, clicking a branch switches (1 file: no code changes)
