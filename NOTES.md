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
