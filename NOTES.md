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

## Open Questions

- Badge placement: tab chip only, or also the branch label in the header?
- Should the desktop `Iced` tab bar get the same indicator, or is the tab chip
  in the web UI enough to start?
- Does `out_of_date` belong on `RepoState` (participates in `PartialEq`, so it
  triggers a revision bump and re-render — desired) or in a side-channel map?
- Do we ever want to auto-fetch once the badge is set, to show a commit count?