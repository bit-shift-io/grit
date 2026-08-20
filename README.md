# Grit

Fast, native, single-binary Git client written in Rust.

Grit gives you both a native desktop GUI (built with [Iced](https://iced.rs/))
and an embedded web daemon (Axum over WebSockets) served from `localhost:8080`,
all contained in one self-contained executable.

## Features

- Native desktop GUI and local web UI from a single binary
- Stage / unstage files
- See the current git status and branch
- View file diffs
- Commit, push, pull, and checkout branches
- Browse commit history
- Nuke button: wipe all local changes and re-clone the repository from scratch

## Build

```bash
cargo build --release
```

## Run

```bash
# Native desktop GUI (also serves the web UI at http://localhost:8080)
./target/release/grit --path .

# Headless web daemon only
./target/release/grit --headless --port 8080
```

## Development

```bash
cargo check        # Quick compiler check
cargo test         # Run unit and integration tests
cargo run -- --path .   # Run the GUI on the current directory
```