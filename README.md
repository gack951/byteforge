# ByteForge

ByteForge is a modern desktop binary editor prototype written in Rust with [GPUI](https://gpui.rs/). It is designed around fast viewport rendering, large-file-friendly I/O, and byte-level editing operations that classic hex editors often make awkward.

> Status: early prototype. The current release is usable for evaluation, but not yet recommended as the only editor for irreplaceable files.

## Features

- Multi-file workspace with tab switching.
- Virtualized hex view using GPUI `uniform_list`; only visible rows are rendered.
- Memory-mapped original file content through `memmap2`.
- Piece-table editing model for insert, overwrite, replacement, and arbitrary range deletion.
- Insert and overwrite modes.
- Undo/redo for user-level edits. Selection replacement and overwrite are one undo step.
- Cursor movement, Shift+Left/Right selection, Select All, Delete, Cut, Copy, and Paste.
- Direct hex nibble input in the hex view.
- Hex and text clipboard formats.
- Save As that streams the current piece table to disk.
- Inspector panel with offset, integer widths, floats, endian mode, and text preview.
- Preview encoding toggle: UTF-8, UTF-16LE, UTF-16BE, Shift-JIS, ASCII.
- Same-offset multi-file comparison highlighting.
- Clipboard search: parses hex first, then falls back to literal text bytes.
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

Open files from the toolbar/menu, or pass a file path:

```powershell
target\release\byteforge.exe path\to\file.bin
```

## Keyboard Shortcuts

| Shortcut | Action |
| --- | --- |
| `Ctrl+O` | Open files |
| `Ctrl+S` | Save As |
| `Ctrl+C` | Copy selected bytes as hex |
| `Ctrl+Shift+C` | Copy selected bytes as text preview |
| `Ctrl+X` | Cut selected bytes as hex |
| `Ctrl+V` | Paste hex from clipboard |
| `Ctrl+Shift+V` | Paste text bytes |
| `Ctrl+Z` | Undo |
| `Ctrl+Shift+Z` | Redo |
| `Ctrl+A` | Select all |
| `Delete` / `Backspace` | Delete selection |
| `Ctrl+F` | Find clipboard bytes |
| `Ctrl+D` | Compare with next open file |
| `Ctrl+B` | Cycle bytes per row |
| `Insert` | Toggle insert/overwrite |
| Arrow keys | Move cursor |
| `Shift+Left/Right` | Extend selection |

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
- 32 MiB large-document viewport/edit/search performance guard.
- GPUI headless tests for keyboard navigation, selection, direct hex input, copy/cut/paste/delete, undo/redo, find, compare, mode toggles, tab activation, open-path, save-path, and 4 MiB document rendering.

## Roadmap

- Crash-safe save and in-place save options.
- Dirty-range indexing and visual change markers.
- Worker-backed search, diff, checksums, string extraction, and entropy analysis.
- Goto offset, bookmarks, search result panel, minimap, and diff navigation.
- Binary templates and richer data inspector formats.
- Installer, file associations, app icon, recent files, and persisted settings.

## License

MIT
