# LAN settings page + sync-badge clearing — design

Date: 2026-10-26
Status: draft — awaiting approval
Repos touched: `grit` (this repo), `~/Projects/krust`, `~/Projects/folio`

## Scope

Three requests arrived together. This document covers the first two; the third
is deliberately deferred.

1. **Settings page.** A gear icon at the far right opens a settings page with
   LAN on/off toggles for grit, folio and krust. Toggling writes each app's
   config file. *(this document)*
2. **Sync badge.** When a tab shows the "remote is ahead" badge and the user
   pulls, the badge must disappear. Today it does not. *(this document)*
3. **Terminals dropping.** Switching between terminals sometimes drops them all
   until a page refresh. *(deferred — brainstorm separately, last)*

Parts 1 and 2 are independent of each other. Part 1 is a project; part 2 is a
small change.

---

# Part 1 — LAN settings

## Intent and why

Grit, folio and krust all bind loopback-only today. The user wants to reach the
whole trio from other machines on their own LAN and wants a single place to turn
that on and off. They trust their network and explicitly do **not** want
authentication or per-session gating.

Success looks like: open grit, click the gear, flip "Krust — LAN access" on, and
after a restart of krust (or the restart button) the krust terminal is reachable
from another machine at `http://<this-machine>:3000`.

## Goals

- One settings page reachable from a gear button at the far right of the tab bar.
- Independent LAN toggles for grit, folio and krust.
- The toggle is the config file as source of truth: what's saved is what the app
  reads on its next start.
- Grit's own toggle applies immediately, with no restart and no dropped page.
- A "restart" affordance for the two services grit cannot rebind in place.

## Non-goals / anti-goals

- No authentication, tokens, or per-session gating. LAN on means LAN reachable,
  full stop.
- No settings UI in the iced desktop app. The gear is a web-UI feature.
- No IPv6. All three bind IPv4 (`0.0.0.0` / `127.0.0.1`).
- Not making krust or folio rebind in place. They take the restart path.
- No auto-restart of grit itself. Grit rebinds its listener live instead.
- Anti-goal: silently losing a user's other config. Every write must preserve
  fields grit does not own (krust's `port`/`allowed_origins`, folio's `last_app`).

## Current state (what this touches)

| App | Bind today | Config today | LAN knob today |
|---|---|---|---|
| grit | `[127,0,0,1]` hard-coded (`src/server/mod.rs:620`) | `~/.config/bitshift/grit/config.json`, `{tabs:[…]}` | none |
| krust | `host:port` from config (`server/src/config.rs`), default `127.0.0.1` | `~/.config/bitshift/krust/config.json`, `{host,port,allowed_origins}` | `host` field / `HOST` env |
| folio | `("127.0.0.1", port)` hard-coded (`src/main.rs:48`) | `~/.config/bitshift/folio/config.json`, `{last_app:{…}}` | none |

Also in scope:

- Grit rejects non-loopback browser `Origin`s (`websocket::origin_is_allowed`,
  `src/server/websocket.rs:179`) — a LAN browser would be 403'd.
- Krust has an `allowed_origins` allow-list (`is_origin_allowed`).
- Folio is already `CorsLayer::permissive()` — no origin work needed.
- The web UI hard-codes `KRUST_BASE = http://localhost:3000` and
  `FOLIO_BASE = http://localhost:4000` (`web/dist/common.js:12,17`). From a LAN
  browser `localhost` is the *client*, so both docks break. This must become the
  current host.

## Approaches considered

**A. Global per-app toggle (chosen).** One boolean per app; LAN on = bind
`0.0.0.0`. Simple, matches what was asked.

**B. Per-session token gating.** Bind publicly but only serve the terminals/files
sessions whose URL carries a secret. Rejected: it cannot work as intuition
suggests — binding is a property of the one listening socket per process (grit
spawns exactly one krust and one folio), so "LAN for this terminal but not that
one" is an access-control problem, not a bind problem, and needs real auth code
in both daemons. The user does not want auth.

**C. All-restart.** Every toggle writes config; one button restarts all three
(including grit re-exec). Rejected: grit serves the settings page itself, so it
cannot cleanly restart mid-request, and re-exec races on the port.

**D. Make krust and folio rebind live too.** Rejected: each would need a
cross-process trigger (admin endpoint, SIGHUP + PID discovery, or config
watching) plus in-place rebind logic touching their serve loops. For a rare
action, restarting two processes is the better trade. Keeping terminals alive
across a toggle belongs to part 3, not here.

**E. Settings transport: separate HTTP endpoints (chosen) vs extending the
websocket `GitAction` protocol.** Rejected the websocket route: `GitAction` is
the single source of truth for *git* actions, and a settings toggle is not one;
adding it would muddy a deliberate invariant and touch the desktop remote
protocol.

## Design

### Applying changes

- **Grit's own toggle: live rebind.** Grit owns its listener and can drop the
  accept loop and start a new one on the new address without losing established
  connections (axum spawns per-connection tasks; dropping the `axum::serve`
  future stops accepting but leaves those tasks running). Must be verified by
  test — the settings page itself is a live connection that must survive.
- **Krust and folio: restart.** Writes take effect on their next process start.
  A "Restart krust & folio" button performs the restart on request.

### Config schema

Each app gets the **same shape**: a `host` string, `"127.0.0.1"` (LAN off) or
`"0.0.0.0"` (LAN on). This matches the field krust already has and keeps the
three uniform (a power user can also set a specific IP by hand).

- **grit** — add to `GritConfig` (`src/shared_config.rs:20`):
  ```rust
  #[serde(default = "default_host")]
  pub host: String,
  ```
  `default_host()` returns `"127.0.0.1"`. Add `impl Default for GritConfig`.
  Existing configs (no `host`) keep their tabs — but note `load_config` and
  `load_tabs_from` currently return empty tabs *on parse failure*, so the field
  must be `serde(default)` or every existing config silently loses its tabs.
  This is the single highest-risk detail in part 1; cover it with a test that
  loads a config written before the field existed.
- **krust** — `host` already exists. No schema change. **Remove the
  `HOST`/`PORT` environment overrides** in `KrustConfig::load()`
  (`server/src/config.rs:56-63`) so the config file is the only source of truth.
  Without this the toggle lies: `load()` reads the file and *then* overwrites
  `host`/`port` from the environment, and `HOST` is set by default in zsh, so a
  hand-started krust would ignore grit's write. This is the only place in krust
  that reads those variables — nothing else depends on them.
- **folio** — add a `host` field to its config struct (`src/config.rs`), serde
  default `"127.0.0.1"`. The struct is currently named `OpenWithConfig` and is
  only about `last_app`; renaming it to `FolioConfig` is the honest move and its
  call sites are few (`src/server/mod.rs`). Rename if cheap, otherwise leave the
  name and add a comment.

### Config writes (grit is the writer)

Grit writes all three files. For krust and folio it must **read-modify-write**
the file as `serde_json::Value` and set only `host`, so unowned fields survive.
For its own file it uses the typed `GritConfig`.

**Write coordination.** Grit already read-modify-writes its own config on every
registry change (`persist_web_state` → `load_config`/`save_config`,
`src/shared_config.rs:101`). The settings endpoint writing the same file can race
that and lose one side's change. Guard **all** grit config writes with one
process-wide mutex (and do the read and the write inside it). The cross-process
case is narrower: folio rewrites its own config when the user picks an editor, so
it must **re-read the file and merge** (set only `last_app`) rather than writing
its whole in-memory struct — otherwise it can revert grit's `host`. Krust never
writes its own config, so grit is its only writer.

Config paths must mirror each app's own rule exactly, or grit writes a file the
app never reads:

| App | Path rule (mirror, do not "improve") |
|---|---|
| grit | `$XDG_CONFIG_HOME` (absolute paths only) else `$HOME/.config`, then `bitshift/grit/config.json` |
| krust | `$XDG_CONFIG_HOME` else `$HOME/.config`, then `bitshift/krust/config.json` |
| folio | `$XDG_CONFIG_HOME` else `$HOME/.config`, then `bitshift/folio/config.json` |

Note grit's own code filters `XDG_CONFIG_HOME` to absolute paths while krust and
folio do not. Mirror each side as-is rather than unifying; the goal is "the file
the app reads".

**Ports.** Grit's dock assumes krust on 3000 and folio on 4000 today
(`KRUST_PORT`/`FOLIO_PORT` constants, `web/dist/common.js`). This change keeps
those defaults: `/settings` reads krust's configured `port` where present
(krust's config already carries one, so grit already has to parse that file) and
uses grit's `FOLIO_PORT` constant for folio, which has no port in its config and
is always spawned by grit with `--port 4000`. A user who moved either daemon to a
custom port is out of scope — the restart's `/proc` scan looks on the configured
port, so it will not kill an unrelated process, and will simply spawn the daemon
on the default port.

### Grit's listener rebind

- `GritConfig.host` is loaded in `main.rs` and passed to `server::run`, which
  takes `(registry, port, initial_host, control)`.
- Add a small `ServerControl` (shared, cloneable) holding the **desired** host,
  the last **effective** host, the last bind error, and a `tokio::sync::Notify`.
  It lives in `AppState` so the settings endpoint can drive it.
- `run` becomes a supervisor loop over a `current_host`:
  1. bind `create_listener(current_host, port)` (host parsed as an IPv4 literal).
     A failure at **startup** keeps today's behaviour: log loudly and return, so
     the daemon never comes up rather than coming up unreachable;
  2. `select!` between the `axum::serve` future and a host-change notification;
  3. on a change, drop the serve future — which stops accepting and releases the
     listener — and try to bind the requested host;
  4. if that bind fails, **immediately re-bind `current_host`** so the daemon
     stays reachable, record the error, and leave `effective_host` alone. Only a
     successful bind updates `current_host`/`effective_host` and clears the error.
- There is a brief window with no listener while a switch is in flight (a few
  ms). Established connections are unaffected — axum spawns a task per
  connection and dropping the accept loop does not abort them — so the settings
  page itself never drops. Only brand-new connections in that window are
  refused, which is why a failed rebind must restore the previous bind.
- `create_listener` gains a host parameter; `TcpSocket::new_v4` stays.
- `GET /settings` reports both desired `host` and `effective_host` plus any bind
  `error`, so a failed switch is visible rather than merely logged.

### Origin relaxation

Trust-the-LAN means: **when an app's effective host is non-loopback, accept any
origin.**

- **grit**: keep `origin_is_allowed(origin)` as the pure loopback test (it is
  unit-tested); add `origin_is_allowed_for(origin, lan: bool)` returning `true`
  when `lan`. The CORS predicate and `ws_handler` consult the *effective* host
  from `ServerControl`.
- **krust**: `is_origin_allowed` returns `true` whenever the bound host is not a
  loopback address. Update `KrustConfig` tests accordingly.
- **folio**: nothing to do.

### Restarting krust and folio

`POST /settings/restart` restarts both (or only those the user picks). Grit
cannot rely on owning the child process: krust and folio are typically started by
the user, not by grit, so there is no `Child` handle to use.

- Find the process **listening on the app's port** via `/proc`: parse
  `/proc/net/tcp` for the LISTEN entry, take its socket inode, then scan
  `/proc/*/fd` for the `socket:[inode]` symlink. Fall back to matching
  `/proc/*/exe` basename against `krust`/`folio`.
- `SIGTERM`, poll the port until free (bounded, ~2s), then `SIGKILL`.
- Respawn through the existing `ensure_krust()` / `ensure_folio(root)`.
- Nothing listening → just spawn.
- **Linux-only** (uses `/proc`). This is consistent with existing code
  (`src/actions.rs` already probes `/proc` ancestors for terminal detection).
- Report per-service `{ok, error}` back to the page.
- **Expected side effect to state in the UI:** restarting krust kills every live
  terminal shell; the browser's terminal reconnects to a *fresh* session and
  loses its scrollback. Restarting folio only briefly blinks the files dock.

### Settings HTTP surface

Separate from the WebSocket/git protocol:

- `GET /settings` →
  ```json
  {
    "grit":  { "lan": false, "host": "127.0.0.1", "effective_host": "127.0.0.1", "error": null },
    "folio": { "lan": true,  "host": "0.0.0.0", "pending_restart": true },
    "krust": { "lan": true,  "host": "0.0.0.0", "pending_restart": true },
    "ports": { "grit": 5000, "folio": 4000, "krust": 3000 }
  }
  ```
- `POST /settings` with `{ "grit": bool?, "folio": bool?, "krust": bool? }` —
  writes the present services' configs, triggers grit's rebind for `grit`, and
  returns the same shape as `GET /settings`.
- `POST /settings/restart` — restarts krust and folio; returns per-service
  `{ok, error}`.

`pending_restart` for krust/folio is derived from comparison against a snapshot
grit takes at boot (and re-takes after a successful restart): saved host differs
from boot snapshot ⇒ "restart needed". Grit itself is always applied live, so it
never has `pending_restart`; a failed rebind surfaces as `error`.

### Web UI

- **Gear button** (`web/dist/common.js`, `renderTabBar`): `#tabs` is rebuilt on
  every state frame, so the gear is appended there, after the existing `+`
  button, with `margin-left:auto` to push it to the far right. It is global, not
  tab-scoped: no `data-tab-id`. Give it `class="tab settings-btn"`, a title, and
  an `aria-label`.
- **Settings view**: a third `activeView` value, `"settings"`, alongside
  `"dashboard" | "files" | "term-1"`.
  - `web/dist/index.html`: add
    `<section id="settings-view" class="settings-view" style="display:none;">`
    inside `<main>`, with a heading and an empty container that JS fills.
  - `showView` (`web/dist/terminal.js`): hide the dashboard sections and both
    docks when `settings` is active; show `#settings-view`.
  - `web/dist/app.js`: route `.settings-btn` clicks to `setView("settings")`; add
    `"settings"` to the `?view=` deep-link list (and the `initialUrlView` latch in
    `common.js`).
- **Rendering** (built by JS, like the other views): fetch `GET /settings` on
  open; render one row per service with a toggle, the saved address, and for
  grit the effective address / any bind error. Show a "Restart krust & folio"
  button when either is `pending_restart`. Toggling posts `POST /settings` and
  re-renders from the response. Include a one-line plain explanation that LAN on
  means anyone on the network can reach it (and that krust is a shell) — a
  statement, not a nag.
- **Iframe hosts**: `KRUST_BASE` and `FOLIO_BASE` become
  `http://${location.hostname}:3000` / `:4000` so the docks follow the page's own
  host. Everything that consumes them (probes, iframe `src`) comes along.

### Files and rough size

| File | Change |
|---|---|
| grit `src/shared_config.rs` | `host` field, `Default`, path accessors for sibling configs |
| grit `src/settings.rs` (new) | read-modify-write of the three configs; `/proc` port→pid; restart |
| grit `src/server/mod.rs` | `create_listener(host,…)`, supervisor rebind loop, `ServerControl`, `/settings` routes, `wake_sync` |
| grit `src/server/websocket.rs` | origin check consults LAN mode |
| grit `src/main.rs` | load host from config, thread `ServerControl` in |
| grit `web/dist/{index.html,style.css,common.js,terminal.js,app.js}` | gear, settings view, settings client, hostname-derived bases |
| krust `server/src/config.rs` | origin allow-any when non-loopback; drop `HOST`/`PORT` env overrides |
| folio `src/config.rs` | `host` field (+ optional rename) |
| folio `src/main.rs` | bind from config host |

**Docs that become wrong.** Folio's `ARCHITECTURE.md`/`README.md`/`CONTEXT.md`
state that loopback binding *is* its security boundary; krust's `ARCHITECTURE.md`
still shows a `0.0.0.0` bind diagram and its config is now the only source of
truth. Update each repo's docs alongside its code so the security story stays
honest.

## Approved decisions

- Global per-app toggle (not per-session gating).
- Grit applies its toggle live; krust and folio take a restart button.
- Krust and folio are *not* made to rebind in place.
- Settings over separate HTTP endpoints, not the WebSocket protocol.
- Web UI only; iced desktop untouched.
- Config file is the source of truth; `host` string uniform across the trio.
- Krust's `HOST`/`PORT` environment overrides are removed outright.

## My calls (so they can be checked)

- Config field is a `host` string, not a `lan` bool — uniform with krust and lets
  a power user pin a specific IP.
- `KRUST_BASE`/`FOLIO_BASE` derive from `location.hostname`.
- Restart uses a `/proc` port→pid scan (Linux-only), no new dependency.
- `pending_restart` is a boot-snapshot comparison.
- folio's `OpenWithConfig` rename to `FolioConfig` is optional cleanup.
- A failed runtime rebind restores the previous bind rather than leaving grit
  unreachable until an external restart.
- One process-wide mutex guards grit's config writes; folio merges into its file
  on save instead of replacing it.
- Ports stay at krust's configured `port` and folio's `4000`; non-default daemon
  ports are out of scope.

---

# Part 2 — sync badge clears on pull

## Intent and why

When a tab carries the "remote is ahead" badge and the user pulls, the badge
should go away. Today it can stick until the daemon is restarted.

## Root cause

The badge is `RepoState.out_of_date`, recomputed by the sync ticker
(`src/server/mod.rs:338`). Two defects combine:

1. **The give-up clock is never re-armed.** `SyncBudget.started[tab]` is set the
   first time a tab is seen unsettled (`started_at`, `src/server/mod.rs:449`) and
   is only pruned when the tab is closed. After a tab has been open longer than
   `SYNC_GIVE_UP_AFTER` (10 min), a pull calls `reset_settled` but the budget
   entry is stale, so the very next pass classifies the tab as expired, returns
   `GaveUp`, and marks it settled **without probing or touching the badge**
   (`src/server/mod.rs:380`). The badge stays until restart.
2. **`reset_settled` does not wake the ticker**, so even when the probe would
   succeed the badge can lag up to a full 30s tick.

The client is not at fault: it renders whatever `out_of_date` the server sends.

## Fix

Four changes, smallest first:

1. **Re-arm the budget on settle.** Add `SyncBudget::forget(tab_id)` and call it
   wherever a tab is marked settled (both the conclusive path and `GaveUp`).
   Once a tab settles it has no clock; the next time it becomes unsettled it
   starts a fresh one. This is the correctness fix.
2. **Wake the ticker on re-arm.** Add `AppState::wake_sync()` (`sync_signal
   .notify_one()`) and call it right after `reset_settled` for a successful
   Fetch/Pull (`src/server/websocket.rs:444`), so the re-probe is immediate
   rather than up to 30s out.
3. **Clear the badge immediately on a successful Pull/Fetch.** A successful
   `git pull` leaves the branch at the upstream tip; a `git fetch` refreshes the
   remote-tracking ref — in both cases "remote is ahead of what we know" is
   already false, so publishing `out_of_date = false` right away is accurate, not
   optimistic guesswork. The re-probe then re-verifies (and can re-raise the
   badge if a commit landed meanwhile). This is what makes the badge visibly
   disappear *on the pull*, which is the actual request.

4. **Close a latent read-modify-write race.** `refresh_tab`
   (`src/server/mod.rs:157`) reads `out_of_date` from a snapshot and writes it
   back outside the registry's write lock; the ticker could write `false` in
   between and have it clobbered. Add `TabRegistry::update_state_keeping_sync(
   tab_id, state)` that copies the current tab's `out_of_date` onto the new state
   *inside* the existing `modify` lock, and use it in `refresh_tab`.

## Tests

- Regression for defect 1: settle a tab with `out_of_date = true`, artificially
  age its budget entry past the ceiling, `reset_settled`, run one pass, assert
  the badge clears. Must fail before the fix and pass after.
- `budget.forget` leaves no entry after a tab settles.
- A successful Pull publishes `out_of_date = false` immediately (before the next
  ticker pass).
- `refresh_tab` no longer clobbers a concurrent badge clear (race test).

---

# Part 3 — deferred

Terminals dropping when switching. Not designed here. Known leads from
reconnaissance, to seed the later conversation, not to act on now:

- There is exactly one krust iframe (`#krust-1`), shared across all repo tabs;
  "all terminals" therefore share one document, which explains simultaneous loss.
- `ensureKrustFrame` early-returns when the iframe's `src` already matches the
  computed session (`web/dist/terminal.js:112`), so a blanked/frozen document is
  never reloaded — only a page refresh rebuilds it.
- Grit's `GET /reset?session_id=…` recycle call is a **no-op 404**; krust has no
  `/reset` route.
