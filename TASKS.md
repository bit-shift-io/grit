# Implementation Tasks: File Browser Section (Web UI)

> **Goal:** Add a **Files** section to the web UI with a tree-style file browser on the left, a preview pane on the right for text and image files, and a button to open files in an external editor. The file tree loads all files upfront via a new server endpoint; file previews load lazily on click.

## Design Requirements

- Collapsible "Files" section under Changes section
- Left pane: tree-style file browser showing repository structure
- Right pane: preview for text files (syntax-highlighted or monospace) and image files
- Button to open/edit file in external editor
- Loads entire file tree at once (via server endpoint)
- File previews lazy-loaded on click

---

## Phase 1 — Server Endpoint: File Tree

- [x] Add `GET /filetree?tab=<id>` handler in `src/server/mod.rs`:
  - Query struct: `FileTreeQuery { tab: usize }`
  - Use `tab_scoped_git_call` pattern to get repo path
  - Spawn blocking task to walk the repository directory recursively
  - Skip `.git/` directory and common ignored patterns (node_modules, target, .venv, etc.)
  - Return `FileTreeResponse { root: FileTreeNode }` where each node has:
    - `name: String`
    - `path: String` (relative to repo root)
    - `is_dir: bool`
    - `children: Option<Vec<FileTreeNode>>` (None for files, Some for dirs)
  - Sort entries: directories first, then alphabetically
- [x] Add `FileTreeQuery`, `FileTreeResponse`, `FileTreeNode` structs in `src/server/mod.rs` (or `src/git/types.rs` if reusable)
- [x] Write test `filetree_endpoint_returns_repository_tree` in `src/server/mod.rs::tests`

## Phase 2 — Server Endpoint: File Content

- [x] Add `GET /filecontent?tab=<id>&path=<path>` handler in `src/server/mod.rs`:
  - Query struct: `FileContentQuery { tab: usize, path: String }`
  - Use `tab_scoped_git_call` to resolve repo path
  - Spawn blocking task to read the file at `{repo_path}/{path}`
  - Detect content type:
    - Text files: return `FileContent { content: String, content_type: "text", mime: "text/plain" }`
    - Image files (png, jpg, gif, svg, webp): return `FileContent { content: base64_data, content_type: "image", mime: "<detected>" }`
    - Binary/unknown: return `FileContent { content: "", content_type: "binary", mime: "application/octet-stream" }`
  - Limit text file reads to reasonable size (e.g., 1MB) to prevent memory issues
  - Return 404 if file not found
- [x] Add `FileContentQuery`, `FileContent` structs
- [x] Write test `filecontent_endpoint_returns_text_content` in `src/server/mod.rs::tests`

## Phase 3 — HTML Structure

- [x] Add `<section id="files-section" class="section collapsed" style="display: none;">` to `web/dist/index.html` after `#history-section`:
  ```html
  <section id="files-section" class="section collapsed" style="display: none;">
    <h2 class="section-title">Files <span class="arrow">&#9652;</span></h2>
    <div class="section-body">
      <div class="file-browser">
        <div class="file-tree" id="file-tree"></div>
        <div class="file-preview" id="file-preview">
          <div class="file-preview-header">
            <span id="file-preview-name" class="file-preview-name"></span>
            <button id="open-external-btn" class="file-open-external" title="Open in external editor">Edit</button>
          </div>
          <div id="file-preview-content" class="file-preview-content">
            <div class="file-preview-empty muted">Select a file to preview</div>
          </div>
        </div>
      </div>
    </div>
  </section>
  ```
  (1 file: `web/dist/index.html`)

## Phase 4 — CSS Styling

- [x] Add file browser styles to `web/dist/style.css`:
  - `.file-browser`: flex row, full width, min-height ~300px, max-height ~600px
  - `.file-tree`: left pane, width ~40%, overflow-y scroll, border-right, monospace font
  - `.file-preview`: right pane, flex: 1, overflow auto
  - `.file-preview-header`: flex row, sticky top, background color, border-bottom, contains file name and edit button
  - `.file-preview-name`: font-weight bold, monospace, flex: 1
  - `.file-open-external`: match existing button styles
  - `.file-preview-content`: overflow auto, padding
  - `.file-preview-empty`: centered muted text
  - `.file-tree-item`: flex row, padding, cursor pointer, hover highlight (match `.row-hover`)
  - `.file-tree-item.selected`: background highlight to show active file
  - `.file-tree-item .tree-icon`: small icon/spacer for indentation
  - `.file-tree-item .tree-name`: flex: 1, overflow hidden, text-overflow ellipsis
  - `.file-tree-dir > .tree-children`: nested container (initially hidden, expanded on click)
  - `.file-tree-dir.expanded > .tree-children`: display block
  - `.file-preview-content pre`: monospace, white-space pre-wrap, overflow-wrap break-word
  - `.file-preview-content img`: max-width 100%, height auto
  - `.file-preview-binary`: centered muted text for binary files
  - (1 file: `web/dist/style.css`)

## Phase 5 — JS: Fetch & Render File Tree

- [x] Add `fetchFileTree(tab)` function in `web/dist/app.js`:
  - Fetch `/filetree?tab=${tab.id}`
  - Cache result in a module-level `Map` keyed by tab ID
  - Return parsed JSON
- [x] Add `renderFileTree(tab)` function:
  - Call `fetchFileTree(tab)` (use cache if available)
  - Render tree nodes recursively into `#file-tree`
  - Each directory node: click toggles expanded/collapsed (toggle `.expanded` class)
  - Each file node: click calls `selectFile(tab, filePath)`
  - Track selected file with module-level `selectedFilePath` variable
  - Highlight selected file with `.selected` class
- [x] Add `renderFilePreview(tab, filePath)` function:
  - Fetch `/filecontent?tab=${tab.id}&path=${encodeURIComponent(filePath)}`
  - Update `#file-preview-name` with file name
  - If `content_type === "text"`: render in `<pre>` with monospace font
  - If `content_type === "image"`: render as `<img src="data:${mime};base64,${content}">`
  - If `content_type === "binary"`: show "Binary file — cannot preview" message
- [x] Add `selectFile(tab, filePath)` function:
  - Set `selectedFilePath = filePath`
  - Re-render tree to update selection highlight
  - Call `renderFilePreview(tab, filePath)`
- [x] Wire "Edit" button: `sendAction({ OpenExternal: selectedFilePath })` — this action may not exist yet, so add a `TODO` or use `window.open` as placeholder
- [x] Call `renderFileTree(tab)` from `render()` when files section is visible
- [x] Add files section visibility logic in `render()`: show when repo tab active, hide when add-repo form (same pattern as other sections)
  (1 file: `web/dist/app.js`)

## Phase 6 — Git Action: Open External Editor

- [x] Add `GitAction::OpenExternal(String)` variant to `src/git/types.rs`
- [x] Handle `OpenExternal` in `src/server/websocket.rs`:
  - Resolve file path relative to repo root
  - Use `std::process::Command` to open with `$EDITOR` (fallback to `xdg-open` on Linux, `open` on macOS)
  - Log the command in the tab's command log
- [x] Add `"OpenExternal"` case to the frontend action sender (already noted as TODO in Phase 5)
  (3 files: `src/git/types.rs`, `src/server/websocket.rs`, `web/dist/app.js`)

## Phase 7 — Tree Indentation & Icons

- [x] Implement tree indentation: each nesting level adds `padding-left: 1.2rem` via CSS
- [x] Add expand/collapse icon for directories:
  - Use `▶` (collapsed) / `▼` (expanded) unicode arrows or CSS-only triangles
  - Show folder icon `📁` or `📂` for directories
  - Show file icon `📄` for files (optional, can skip for v1)
- [x] Ensure long file paths truncate with ellipsis
  (2 files: `web/dist/style.css`, `web/dist/app.js`)

## Phase 8 — Verification

- [x] Run `cargo check` and `cargo test` to confirm no Rust regressions
- [x] Manually verify:
  - Files section appears and collapses/expands correctly
  - File tree shows repository structure with proper nesting
  - Clicking a directory expands/collapses it
  - Clicking a file loads its preview in the right pane
  - Text files display with monospace formatting
  - Image files display inline
  - Binary files show appropriate message
  - Edit button opens file in external editor (or shows TODO feedback)
  - Selection highlighting follows the clicked file
  - Section works across tab switches
  (0 code changes)

---

## Files Modified Summary

| File | Changes |
|------|---------|
| `src/git/types.rs` | Add `OpenExternal(String)` variant to `GitAction` |
| `src/server/mod.rs` | Add `/filetree` and `/filecontent` handlers + tests |
| `src/server/websocket.rs` | Handle `OpenExternal` action |
| `web/dist/index.html` | Add `#files-section` with tree + preview layout |
| `web/dist/style.css` | Add `.file-browser`, `.file-tree`, `.file-preview` styles |
| `web/dist/app.js` | Add `fetchFileTree`, `renderFileTree`, `renderFilePreview`, `selectFile` functions |
