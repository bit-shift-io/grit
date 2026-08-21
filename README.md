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
  fire-and-forget, disableable via `actions::ENABLED`
- Nuke button: wipe all local changes and re-clone the repository from scratch
- Open tabs persist across restarts via a shared config file
  (`$XDG_CONFIG_HOME/bitshift/grit/config.json`)
- Connect-mode: if a Grit daemon is already running on `--port` (e.g. a
  systemd service), the desktop app attaches to it as a client instead of
  starting its own server — one source of truth, no config races

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
