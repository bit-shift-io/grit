# Notes: File Browser Feature (Web UI)

## Confirmed Requirements

- **Type**: Repository file tree browser (not full filesystem)
- **Click behavior**: Show file content (read-only viewer, like GitHub's file view)
- **Layout**: Collapsible "Files" section under Changes, tree-style left pane + right preview pane
- **Preview**: Text files and image formats
- **External editor**: Button to open/edit in external editor
- **Tree loading**: Load entire file tree at once (via server endpoint)
- **Preview loading**: Lazy-loaded on click

## Architecture Notes

- New `GET /filetree?tab=<id>` endpoint walks repo directory, skips `.git/` and ignored patterns
- New `GET /filecontent?tab=<id>&path=<path>` endpoint returns file content with type detection
- File tree cached in frontend Map keyed by tab ID
- File browser follows existing section patterns (collapsible, visibility tied to tab state)

## Open Questions

- What ignored patterns to skip? (node_modules, target, .venv, __pycache__, etc.)
- Should we respect `.gitignore` for the file tree?
- External editor: use `$EDITOR` env var? Fallback to `xdg-open`/`open`?

---

# Notes: View Dock + Embedded Terminals (Web UI)

## Decisions

- **Dock, not menu**: a compact left rail (Ubuntu-dock style) holding
  Dashboard / Files / Log plus two terminal buttons. Single-key shortcuts
  `D` `F` `L` `1` `2`; deep-link via `?view=…` so each view is bookmarkable.
- **No per-view HTML rewiring**: view routing is plain display toggling in
  `showView()`; all sub-renders still run every render to keep per-view client
  caches warm. Body class `view-*` selects layout (files gets full remaining
  height, log gets a capped max-height).
- **Terminals are external**: Grit never embeds a PTY. It auto-launches the
  standalone `krust` daemon (best-effort, `src/krust.rs`, `KRUST_BIN`/PATH
  resolution) and renders `localhost:3000` in per-view `<iframe>`s. If krust
  is absent or down the `T` buttons hide and an active terminal view is
  force-switched back to Dashboard (5s probe cadence).
- **Sessions scoped per repo, not per tab**: `grit-{repoScope}-term-{n}` where
  `repoScope` hashes the absolute repo path. Repo switching rebinds the frame
  and recycles the abandoned session. Rationale: keeps each repo's shell state
  even if the tab id allocation shifts; shells start in the repo via `?dir=`.
- **`/reset` on krust**: added a `GET /reset?session_id=` endpoint (kills the
  PTY child + drops the session) and `CorsLayer::permissive()`; the Reset
  overlay button calls it then reloads with a `&r=` cache-buster.

## Open Questions

- Content Security Policy for krust frames / mixed-content if the daemon ever
  moves off-loopback.
- Whether the desktop (Iced) UI should gain an equivalent view rail.
