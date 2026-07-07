# ByteForge

ByteForge is a modern desktop binary editor prototype written in Rust with [GPUI](https://gpui.rs/). It is designed around fast viewport rendering, large-file-friendly I/O, and byte-level editing operations that classic hex editors often make awkward.

> Status: early prototype. The current release is usable for evaluation, but not yet recommended as the only editor for irreplaceable files.

## Features

- Multi-file workspace with tab switching.
- Open one or more files from the toolbar/menu or by dragging files onto the window.
- Left/right split view with independent file tabs, active tab state, file assignment, and pane focus.
- Virtualized hex view using GPUI `uniform_list`; only visible rows are rendered.
- GPUI Component-backed hex viewport scrollbar.
- Monospaced, non-wrapping hex rows and address columns.
- Memory-mapped original file content through `memmap2`.
- Piece-table editing model for insert, overwrite, replacement, and arbitrary range deletion.
- Insert and overwrite modes.
- Undo/redo for user-level edits. Selection replacement and overwrite are one undo step.
- Cursor movement, Shift+click, drag selection, Shift+Left/Right selection, Select All, Delete, Cut, Copy, and Paste.
- Cursor can move to EOF + 1 for appending; the normal cursor position acts as a one-byte selection for Delete/Copy/Find when applicable.
- Direct hex nibble input in the hex view.
- Hex and text clipboard formats.
- Save and Save As operations that stream the current piece table to disk.
- Inspector panel with offset, integer widths, floats, endian mode, text preview, and clickable read-only file-format fields for PNG, BMP, WAV, GIF, JPEG, and ZIP.
- Preview encoding toggle: UTF-8, UTF-16LE, UTF-16BE, Shift-JIS, ASCII.
- Same-offset multi-file comparison highlighting.
- Clipboard search: parses hex first, then falls back to literal text bytes, and scrolls to hits.
- Replace panel for step-by-step replacement and replace-all using hex bytes or text.
- Goto offset text box with decimal and `0x` hexadecimal forms.
- Headless GPUI test suite covering the implemented UI actions.

## Download

Download the latest Windows build from [Releases](https://github.com/gack951/byteforge/releases).

The release asset contains:

- `byteforge.exe`
- `README.md`
- `LICENSE`

## Build From Source

Requirements:

- Rust 1.96 or newer
- Windows is the primary tested platform

```powershell
git clone https://github.com/gack951/byteforge.git
cd byteforge
cargo build --release
target\release\byteforge.exe
```

Open files from the toolbar/menu, drag files onto the window, or pass a file path:

```powershell
target\release\byteforge.exe path\to\file.bin
```

## Keyboard Shortcuts

| Shortcut | Action |
| --- | --- |
| `Ctrl+O` | Open files |
| `Ctrl+S` | Save |
| `Ctrl+Shift+S` | Save As |
| `Ctrl+C` | Copy selected bytes as hex |
| `Ctrl+Shift+C` | Copy selected bytes as text preview |
| `Ctrl+X` | Cut selected bytes as hex |
| `Ctrl+V` | Paste hex from clipboard |
| `Ctrl+Shift+V` | Paste text bytes |
| `Ctrl+Z` | Undo |
| `Ctrl+Y` | Redo |
| `Ctrl+A` | Select all |
| `Delete` / `Backspace` | Delete selection |
| `Ctrl+F` | Find clipboard bytes |
| `Ctrl+H` | Open replacement panel |
| `Ctrl+G` | Goto offset |
| `Ctrl+D` | Compare with next open file |
| `Ctrl+\` | Toggle split view |
| `Ctrl+M` | Move active file to the other split pane |
| `Ctrl+1` / `Ctrl+2` | Focus left/right split pane |
| `Ctrl+B` | Cycle bytes per row |
| `Ctrl+Alt+E` | Toggle endian mode |
| `Ctrl+Alt+N` | Cycle preview encoding |
| `Insert` | Toggle insert/overwrite |
| Arrow keys | Move cursor |
| `Shift+Left/Right` | Extend selection |

## Selection And Insertion

- Click a byte to move the cursor. With no explicit range, the cursor byte is treated as a one-byte selection for operations such as Delete, Copy, and Find.
- Shift+click extends selection from the current anchor to the clicked byte.
- Drag across the hex or ASCII cells to select a range.
- Move right past the last byte, or use Goto with the file length, to place the cursor at EOF + 1. Insert or paste there to append.
- Insert mode shows the cursor as an outlined insertion point; overwrite mode shows the current byte as a filled cursor.

## Validation

```powershell
cargo fmt --check
cargo check --all-targets
cargo test
```

The test suite includes:

- Piece-table insert/delete/overwrite/replace behavior.
- Undo/redo and dirty-state behavior.
- Hex parser and virtual-document search.
- File-format detection for PNG, BMP, WAV, GIF, JPEG, and ZIP, including edited document bytes.
- 32 MiB large-document viewport/edit/search performance guard.
- GPUI headless tests for keyboard navigation, selection, Shift+click/drag selection state, direct hex input, copy/cut/paste/delete, undo/redo, find auto-scroll, Goto, split panes, visible controls/tabs/scrollbars, format-field range selection, EOF append cursor behavior, compare, mode toggles, tab activation, open-path, save-path, and 4 MiB document rendering.

## Roadmap

- Crash-safe save and in-place save options.
- Dirty-range indexing and visual change markers.
- Worker-backed search, diff, checksums, string extraction, and entropy analysis.
- Bookmarks, search result panel, minimap, and diff navigation.
- Binary templates and richer data inspector formats.
- Installer, file associations, app icon, recent files, and persisted settings.

## License

MIT
