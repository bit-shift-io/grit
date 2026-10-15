# Notes: Remote Sync Indicator (Out-of-Date Badge)

> Replaces the earlier "File Browser Feature" and "View Dock + Embedded
> Terminals" sections, which are now superseded by `src/git/files.rs`,
> `src/folio.rs`, `src/krust.rs`, and `ARCHITECTURE.md`.

## Goal

Show the user a basic "this repo is out of sync with origin" indicator and
nothing more. No auto-pull, no auto-merge — the user pulls on their own
terms. Detected once per tab per session.

## Decisions

- **`git ls-remote`, not `git fetch`.** The cheapest possible query of remote
  state: no objects, no packfile. Measured against this repo's real origin,
  a single-ref query is 57 bytes of payload.

- **`ls-remote` writes nothing.** Not `FETCH_HEAD`, not `refs/remotes/*`. This
  matters specifically for Grit: `fetch` writes into `.git/`, which the single
  recursive `notify` watch covers, so every fetch would trip the 200ms
  debouncer and fan a `refresh_all` out to *every* tab. `ls-remote` never
  touches the filesystem, so it is inert.

- **Compare two SHAs.** `ls-remote origin refs/heads/<upstream-branch>` (the
  live SHA) vs `git rev-parse refs/remotes/<upstream-branch>` (the cached
  SHA). Equal means provably in sync. Unequal means stale — but not by how
  much; only a real fetch yields a commit count, and we do not need one.

- **Local-only knowledge is worthless for this.** Checking the most recent
  local commit tells us nothing about the remote; local refs are only a cache
  of the last fetch. Some network contact is unavoidable. `ls-remote` is the
  minimum viable contact.

- **Trigger on first client WebSocket connect, not daemon boot.** The daemon
  can boot scheduled or headless with no network; the user opening the web UI
  is a far stronger signal that connectivity exists. It also covers *all* tabs
  in one parallel fan-out rather than one tab at a time like a select handler
  would, and maps exactly onto "once per session". Confirmed both UIs hit
  this path: the browser via `/ws`, and the desktop GUI because
  `src/ui/remote.rs:17,49` connects to `ws://127.0.0.1:{port}/ws`.

- **Retry every 30s until an answer arrives; give up after 10 minutes.**
  A uniform cadence, not a backoff ladder — the trigger is good enough that
  front-loaded retries are not needed, and "retry until answered" is simpler
  to reason about than a schedule.

- **"Answer" means conclusive, and `NoRemote` counts as one.** Only
  `InSync` / `OutOfDate` / `NoRemote` settle a tab. Only `Unreachable`
  (exit 128) retries — otherwise local-only repos burn all 20 attempts for
  nothing, and a failed check would consume the one-shot budget.

- **Failures never alarm.** `Unreachable` maps to `out_of_date: false`. Never
  false-alarm: an offline laptop must not be told to update.

- **A plain bool, not a four-state enum.** Unknown and in-sync both render as
  "no badge", which is exactly the conservative behaviour we want. The enum is
  a one-line upgrade later if the UI ever wants to show "checked N ago".

- **Manual Fetch/Pull re-arms the check.** This is load-bearing under a
  10-minute ceiling: someone opens Grit on a train, the retries expire, they
  tunnel out — the badge stays stale until restart. A successful manual fetch
  proves connectivity, so reset the tab to unsettled and let the next tick
  pick it up. Don't special-case the check inline; just clear the flag.

- **No new `GitAction` variant.** Deliberate: it keeps the exhaustive
  28-variant test at `src/git/actions.rs:562` and the round-trip list at
  `src/git/types.rs:301` untouched, and requires no protocol or `app.js`
  change. Selection stays client-local.

- **Retry is only defensible because `ls-remote` is idempotent and
  non-mutating.** A 30s loop around `fetch --prune` would be much harder to
  justify. This assumption is load-bearing.

## Known Gaps / Trade-offs

- **`run()` in `src/git/mod.rs` has no timeout.** The sync check needs its own
  watchdog: a hung SSH connect would otherwise leak a `spawn_blocking` task
  forever. `tokio::time::timeout` around the join is *not* sufficient — it
  stops waiting but leaves the git child alive.

- **`GitError` carries no exit code.** It is `{message, stderr, stdout}`, so
  exit 128 is currently indistinguishable from any other failure. Add `code`
  so "unreachable" can be told apart from other errors during debugging.

- **Cost is latency, not bandwidth.** 57 bytes still cost 3.4s over SSH to
  github.com on this machine (`user`+`sys` ≈ 0.02s — essentially all
  connection setup). Irrelevant at 20 retries per session, but it is the
  reason a small-repo `fetch` can beat `ls-remote` + later `fetch`. SSH
  multiplexing (`ControlMaster auto` / `ControlPersist`) would fix it and is
  not needed at this cadence.

- **Suspend/resume goes stale the other way.** The badge may claim in-sync
  while the machine is offline. Conservative rather than wrong; Fetch clears it.

- **Network probes rejected.** `nm-online` / NetworkManager was considered and
  rejected: interface-up → route → DNS → SSH agent → host reachable are five
  separate layers, and a connectivity probe can only see the first one or two.
  Retry-until-answered is strictly more reliable because only a real
  `ls-remote` attempt proves all five.

# Notes: Tab Badge Overlays (Sync + Terminal Activity)

> Follow-on to the sync badge above: replace the inline `↑` text hint with an
> overlay badge, and reuse the badge system to mark a terminal with new output.

## Goal

The out-of-date hint becomes a graphic overlay (up arrow in a round circle)
instead of the `↑` span appended to the tab name text, and the same overlay
system marks the terminal button when the terminal has produced output since
the user last looked at it.

## Decisions (confirmed 2026-10-15)

- **Terminal change detection = push from krust over `postMessage`.** No
  polling endpoint, no second observer WebSocket. The krust client page
  (`client/res/server.html`, embedded into the krust binary at build time)
  already owns the terminal WebSocket and an output byte counter (`bytesIn`,
  server.html:723), so a throttled activity ping to `parent` is plain JS —
  no wasm rebuild, no new HTTP route. Grit listens with
  `window.addEventListener("message")` and checks `event.origin` against the
  `KRUST_BASE` origin (grit:5000 vs krust:3000 are cross-origin).

- **Scope = live session only.** Grit has one krust iframe whose session id is
  derived from the active repo (`sessionIdFor`, app.js:1868) and recycles the
  old session via `GET /reset` when the active repo changes
  (`ensureKrustFrame`, app.js:1904). So "new output" is tracked for the
  session currently bound to the iframe; the badge shows on that tab's `T`
  button while the terminal view is hidden. Pings are ignored while the term
  view is active (typing echoes output), and the badge clears when the view is
  shown or the iframe rebinds to another session.

- **Reload-safe by construction.** The iframe only loads when the terminal view
  is opened, and the history replay that floods `bytesIn` lands while the view
  is active — so those pings are ignored and produce no false badge.

- **Badge geometry = the old dock's.** Absolute, top-right, `translate(30%,
  -30%)`, round, `pointer-events: none` so a click still selects the tab.
  Sync: amber `--sync-badge` circle with a white `↑` on the tab name.
  Terminal: plain green dot (`rgba(46,125,50,.85)`, the dock's krust green),
  no glyph, on the `T` button.

## Implemented

- krust `client/res/server.html`: `pingParentActivity()` at the end of
  `handleWsData` — throttled to 250ms, targeted at
  `new URL(document.referrer).origin` (falls back to `*`), no-op when the page
  is not framed.
- grit `web/dist/app.js`: `termUnseenSid` state, `sessionIdForRepo()` (per-tab
  session id for badge matching), `liveSessionId()` (reads the iframe's `?s=`),
  `handleKrustActivity()` (origin-checked listener). Badge appended in
  `renderTabBar`; cleared in `showView` (term view shown) and
  `ensureKrustFrame` (session rebind recycles the old one).
- grit `web/dist/style.css`: `#tabs .tab-badge` / `-sync` / `-term` replace the
  old inline `.sync-badge`; `.tab-name` and `.tab-view-btn` gained
  `position: relative`.

## Deployment

- Both changes take effect only after a rebuild: grit embeds `web/dist/*` via
  rust-embed, krust embeds `server.html`.
- The pings need the new krust binary; grit's listener tolerates an old krust
  (no pings, no badge) — so the features degrade independently.

## Open Questions

- Badge placement: tab chip only, or also the branch label in the header?
- Should the desktop `Iced` tab bar get the same indicator, or is the tab chip
  in the web UI enough to start?
- Does `out_of_date` belong on `RepoState` (participates in `PartialEq`, so it
  triggers a revision bump and re-render — desired) or in a side-channel map?
- Do we ever want to auto-fetch once the badge is set, to show a commit count?