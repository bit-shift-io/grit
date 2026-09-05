# Grit

Fast, native, single-binary Git client written in Rust.

Grit gives you both a native desktop GUI (built with [Iced](https://iced.rs/))
and an embedded web daemon (Axum over WebSockets) served from `localhost:5000`,
all contained in one self-contained executable.

## Features

- Native desktop GUI and local web UI from a single binary
- Multi-repo tabs: open several repositories and switch between them
- Tabs are shared between the desktop app and every connected browser;
  opening or closing a tab on one side updates all the others instantly
- Per-viewer tab selection on the web UI, plus `?t=N` deep links
- Add repositories through a form with name auto-fill and a server-side
  folder browser
- Stage / unstage files
- See the current git status and branch
- View file diffs (side-by-side in the web UI)
- Commit, push, pull, and checkout branches
- Browse commit history
- Project Actions: scripts/executables in the repo root, `scripts/`, or
  `tools/` (any casing) are auto-discovered (live, via the file watcher) into
  an Actions dropdown; picking one launches it in a new terminal window
  (interactive menu scripts work; window stays open showing exit status),
  fire-and-forget (spawn + drop, no process tracking)
- Reclone button: delete the repo directory and `git clone` it back from
  `origin` (two-press confirm) — recovers from upstream-side changes like a
  renamed default branch, but discards local branches, stashes, and unpushed
  work; requires an `origin` remote
- Open tabs persist across restarts via a shared config file
  (`$XDG_CONFIG_HOME/bitshift/grit/config.json`)
- Connect-mode: if a Grit daemon is already running on `--port` (e.g. a
  systemd service), the desktop app attaches to it as a client instead of
  starting its own server — one source of truth, no config races
- Web view dock: switch between Dashboard, Files, and Log views with a left
  dock (or the `D` / `F` / `L` keys); deep-link with `?view=files` etc.
- Embedded terminals: two dock views (`1` / `2`) render per-repo shells via
  the optional `krust` web-terminal daemon, which Grit auto-starts if it's
  installed; each view has a hover `Reset` button for a fresh shell

## Web UI

The web UI at `http://localhost:5000` is a self-contained dashboard (no Node
toolchain — plain HTML/CSS/JS embedded in the binary).

- **Views**: `D`ashboard (status + actions + branches + stashes + history),
  `F`iles browser, `L`og, and the two embedded terminals (`1`, `2`). Click a
  dock button or press the key. The active view is remembered in the URL
  (`?view=…`).
- **Dash badge**: count of uncommitted changes.
- **Log badge**: count of failed operations.
- **Terminals**: each terminal opens a shell in the active repository. Sessions
  are scoped per repository, so switching repos rebinds the terminal to that
  repo's shell (abandoned shells are recycled automatically). Click `Reset`
  over a terminal to kill its shell and start fresh. Terminals require a krust
  build with the `/reset` endpoint and `?dir=` cwd support — recent additions,
  **not present in older installed copies** — so rebuild from `~/Projects/krust`
  HEAD. If krust isn't running, Grit tries to start it on port 3000 (`KRUST_BIN`
  / `KRUST_PORT` override); if it can't be found the terminal dock buttons stay
  hidden.

## Build

```bash
cargo build --release
```

## Run

```bash
# Native desktop GUI (also serves the web UI at http://localhost:5000;
# attaches to an already-running daemon on that port if one exists)
./target/release/grit --path .

# Headless web daemon only
./target/release/grit --headless --port 5000
```

## Run as a systemd service (boot start)

A **user unit** is the recommended way to run Grit at boot: it inherits your
graphical session environment, so Project Actions can open terminal windows on
your desktop, and it survives logout/login.

Create `~/.config/systemd/user/grit.service`:

```ini
[Unit]
Description=Grit Git client daemon
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart=%h/Projects/grit/target/release/grit --headless --port 5000
Restart=on-failure

[Install]
WantedBy=graphical-session.target
```

Then enable and start it:

```bash
systemctl --user daemon-reload
systemctl --user enable --now grit
journalctl --user -u grit -f   # follow logs
```

Notes:

- `After=`/`PartOf=`/`WantedBy=graphical-session.target` tie the service to
  your login session; GNOME publishes the Wayland/X environment to the user
  manager, so scripts launched from Grit open in Ptyxis or your configured
  terminal. Pin one explicitly with `Environment=TERMINAL=ptyxis` in the
  `[Service]` section if you prefer.
- A **system-level** unit (`/etc/systemd/system/grit.service`, runs before
  login) works too, but it cannot reach your graphical session: script
  launches fall back to detached execution with output in
  `journalctl -u grit`. You can bridge the session manually with
  `Environment=XDG_RUNTIME_DIR=/run/user/<uid>` and
  `Environment=WAYLAND_DISPLAY=wayland-0`, but the user unit is simpler.
- The desktop GUI automatically attaches to a running daemon on its port
  instead of starting a second server, so `grit --path .` after boot connects
  to the systemd instance.

## Development

```bash
cargo check        # Quick compiler check
cargo test         # Run unit and integration tests
cargo run -- --path .   # Run the GUI on the current directory
```

See `ARCHITECTURE.md` for the system design and module map.
