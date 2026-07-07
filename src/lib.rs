pub mod core;

use std::{
    env,
    ops::Range,
    path::{Path, PathBuf},
};

use anyhow::Result;
use core::{
    ByteDocument, Endianness, FormatField, FormatSummary, PreviewEncoding, Selection,
    detect_format_fields, find_bytes, inspector_values, parse_hex_bytes,
};
use gpui::{
    AnyElement, App, Application, Bounds, ClickEvent, ClipboardItem, Context, ExternalPaths,
    FocusHandle, Focusable, KeyBinding, KeyDownEvent, Menu, MenuItem, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ScrollStrategy, SharedString, SystemMenuType,
    UniformListScrollHandle, Window, WindowBounds, WindowOptions, actions, div, prelude::*, px,
    rgb, rgba, size, uniform_list,
};

const BYTES_PER_ROW_OPTIONS: [usize; 4] = [8, 16, 24, 32];

actions!(
    byteforge,
    [
        OpenFiles,
        SaveAs,
        CopyHex,
        CopyText,
        Cut,
        Undo,
        Redo,
        PasteHex,
        PasteText,
        DeleteSelection,
        SelectAll,
        FindNext,
        CompareNext,
        Goto,
        ToggleSplit,
        MoveToOtherSplit,
        FocusLeftPane,
        FocusRightPane,
        ToggleEndian,
        NextEncoding,
        NextRowWidth,
        ToggleInsertMode,
        MoveLeft,
        MoveRight,
        MoveUp,
        MoveDown,
        SelectLeft,
        SelectRight,
        Quit,
    ]
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EditMode {
    Insert,
    Overwrite,
}

impl EditMode {
    fn label(self) -> &'static str {
        match self {
            Self::Insert => "Insert",
            Self::Overwrite => "Overwrite",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PaneSide {
    Left,
    Right,
}

impl PaneSide {
    fn label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Right => "Right",
        }
    }
}

struct ByteForge {
    docs: Vec<ByteDocument>,
    active: usize,
    compare_with: Option<usize>,
    split: bool,
    focused_pane: PaneSide,
    right_active: Option<usize>,
    cursor: u64,
    selection: Option<Selection>,
    bytes_per_row_ix: usize,
    endian: Endianness,
    encoding: PreviewEncoding,
    edit_mode: EditMode,
    pending_hex: Option<u8>,
    drag_anchor: Option<u64>,
    goto_open: bool,
    goto_input: String,
    left_scroll_handle: UniformListScrollHandle,
    right_scroll_handle: UniformListScrollHandle,
    #[cfg(test)]
    test_open_paths: Option<Vec<PathBuf>>,
    #[cfg(test)]
    test_save_path: Option<PathBuf>,
    status: SharedString,
    focus_handle: FocusHandle,
}

impl ByteForge {
    fn new(cx: &mut Context<Self>) -> Self {
        let mut docs = Vec::new();
        for arg in env::args().skip(1) {
            match ByteDocument::open(&arg) {
                Ok(doc) => docs.push(doc),
                Err(err) => eprintln!("{err:#}"),
            }
        }

        Self {
            docs,
            active: 0,
            compare_with: None,
            split: false,
            focused_pane: PaneSide::Left,
            right_active: None,
            cursor: 0,
            selection: None,
            bytes_per_row_ix: 1,
            endian: Endianness::Little,
            encoding: PreviewEncoding::Utf8,
            edit_mode: EditMode::Overwrite,
            pending_hex: None,
            drag_anchor: None,
            goto_open: false,
            goto_input: String::new(),
            left_scroll_handle: UniformListScrollHandle::new(),
            right_scroll_handle: UniformListScrollHandle::new(),
            #[cfg(test)]
            test_open_paths: None,
            #[cfg(test)]
            test_save_path: None,
            status: "Open one or more files to begin.".into(),
            focus_handle: cx.focus_handle(),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn with_documents(docs: Vec<ByteDocument>, cx: &mut Context<Self>) -> Self {
        Self {
            docs,
            active: 0,
            compare_with: None,
            split: false,
            focused_pane: PaneSide::Left,
            right_active: None,
            cursor: 0,
            selection: None,
            bytes_per_row_ix: 1,
            endian: Endianness::Little,
            encoding: PreviewEncoding::Utf8,
            edit_mode: EditMode::Overwrite,
            pending_hex: None,
            drag_anchor: None,
            goto_open: false,
            goto_input: String::new(),
            left_scroll_handle: UniformListScrollHandle::new(),
            right_scroll_handle: UniformListScrollHandle::new(),
            #[cfg(test)]
            test_open_paths: None,
            #[cfg(test)]
            test_save_path: None,
            status: "Ready.".into(),
            focus_handle: cx.focus_handle(),
        }
    }

    fn bytes_per_row(&self) -> usize {
        BYTES_PER_ROW_OPTIONS[self.bytes_per_row_ix]
    }

    fn active_doc(&self) -> Option<&ByteDocument> {
        self.docs.get(self.focused_active_ix())
    }

    fn active_doc_mut(&mut self) -> Option<&mut ByteDocument> {
        let ix = self.focused_active_ix();
        self.docs.get_mut(ix)
    }

    fn focused_active_ix(&self) -> usize {
        if self.split && self.focused_pane == PaneSide::Right {
            self.right_active.unwrap_or(self.active)
        } else {
            self.active
        }
    }

    fn active_ix_for(&self, side: PaneSide) -> Option<usize> {
        match side {
            PaneSide::Left => (!self.docs.is_empty()).then_some(self.active),
            PaneSide::Right => {
                if self.split {
                    self.right_active
                        .or((!self.docs.is_empty()).then_some(self.active))
                } else {
                    None
                }
            }
        }
    }

    fn focus_pane(&mut self, side: PaneSide) {
        if side == PaneSide::Right && !self.split {
            return;
        }
        self.focused_pane = side;
        self.pending_hex = None;
    }

    fn set_active_for_focused_pane(&mut self, ix: usize) {
        if self.split && self.focused_pane == PaneSide::Right {
            self.right_active = Some(ix);
        } else {
            self.active = ix;
        }
    }

    fn scroll_handle_for(&self, side: PaneSide) -> UniformListScrollHandle {
        match side {
            PaneSide::Left => self.left_scroll_handle.clone(),
            PaneSide::Right => self.right_scroll_handle.clone(),
        }
    }

    fn scroll_to_cursor(&self) {
        let row = (self.cursor / self.bytes_per_row() as u64).min(usize::MAX as u64) as usize;
        self.scroll_handle_for(self.focused_pane)
            .scroll_to_item(row, ScrollStrategy::Center);
    }

    fn clamp_cursor(&mut self) {
        let len = self.active_doc().map(ByteDocument::len).unwrap_or(0);
        self.cursor = self.cursor.min(len);
    }

    fn set_status(&mut self, text: impl Into<SharedString>) {
        self.status = text.into();
    }

    fn set_cursor(&mut self, offset: u64, extend: bool, cx: &mut Context<Self>) {
        let len = self.active_doc().map(ByteDocument::len).unwrap_or(0);
        let offset = offset.min(len);
        if extend {
            let anchor = self
                .selection
                .as_ref()
                .map(|selection| selection.anchor)
                .unwrap_or(self.cursor);
            self.selection = Some(Selection::new(anchor, offset));
        } else {
            self.selection = None;
        }
        self.cursor = offset;
        self.pending_hex = None;
        self.scroll_to_cursor();
        cx.notify();
    }

    fn begin_cell_selection(
        &mut self,
        doc_ix: usize,
        side: PaneSide,
        offset: u64,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        self.focus_pane(side);
        self.set_active_for_focused_pane(doc_ix);
        self.drag_anchor = Some(offset);
        self.set_cursor(offset, extend, cx);
    }

    fn drag_cell_selection(
        &mut self,
        doc_ix: usize,
        side: PaneSide,
        offset: u64,
        cx: &mut Context<Self>,
    ) {
        if self.drag_anchor.is_none() {
            return;
        }
        self.focus_pane(side);
        self.set_active_for_focused_pane(doc_ix);
        self.set_cursor(offset, true, cx);
    }

    fn finish_cell_selection(&mut self, cx: &mut Context<Self>) {
        self.drag_anchor = None;
        cx.notify();
    }

    fn selection_range(&self) -> Option<Range<u64>> {
        let len = self.active_doc().map(ByteDocument::len).unwrap_or(0);
        self.selection.as_ref().and_then(|selection| {
            let mut range = selection.normalized();
            range.start = range.start.min(len);
            range.end = range.end.min(len);
            (range.start < range.end).then_some(range)
        })
    }

    fn selected_or_cursor_range(&self) -> Option<Range<u64>> {
        if let Some(range) = self.selection_range() {
            Some(range)
        } else if let Some(doc) = self.active_doc() {
            if self.cursor < doc.len() {
                Some(self.cursor..self.cursor + 1)
            } else {
                Some(doc.len()..doc.len())
            }
        } else {
            None
        }
    }

    fn open_files(&mut self, _: &OpenFiles, _: &mut Window, cx: &mut Context<Self>) {
        #[cfg(test)]
        if let Some(paths) = self.test_open_paths.take() {
            self.open_paths(paths);
            cx.notify();
            return;
        }

        let Some(paths) = rfd::FileDialog::new().pick_files() else {
            return;
        };

        self.open_paths(paths);
        cx.notify();
    }

    fn open_paths(&mut self, paths: Vec<PathBuf>) {
        let mut opened = 0;
        let mut last_error = None;
        for path in paths {
            match ByteDocument::open(&path) {
                Ok(doc) => {
                    self.docs.push(doc);
                    opened += 1;
                }
                Err(err) => last_error = Some(format!("{err:#}")),
            }
        }

        if opened > 0 {
            let first_opened = self.docs.len() - opened;
            self.set_active_for_focused_pane(first_opened);
            self.cursor = 0;
            self.selection = None;
            self.compare_with = None;
            self.set_status(format!("Opened {opened} file(s)."));
        } else if let Some(err) = last_error {
            self.set_status(err);
        }
    }

    fn open_dropped_paths(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        self.open_paths(paths.to_vec());
        cx.notify();
    }

    fn save_as(&mut self, _: &SaveAs, _: &mut Window, cx: &mut Context<Self>) {
        #[cfg(test)]
        if let Some(path) = self.test_save_path.take() {
            self.save_active_as_path(&path);
            cx.notify();
            return;
        }

        let Some(path) = rfd::FileDialog::new().save_file() else {
            return;
        };
        self.save_active_as_path(&path);
        cx.notify();
    }

    fn save_active_as_path(&mut self, path: &Path) {
        match self.active_doc_mut().map(|doc| doc.save_as(path)) {
            Some(Ok(())) => self.set_status("Saved."),
            Some(Err(err)) => self.set_status(format!("Save failed: {err:#}")),
            None => self.set_status("No active file."),
        }
    }

    fn copy_hex(&mut self, _: &CopyHex, _: &mut Window, cx: &mut Context<Self>) {
        let Some(range) = self.selected_or_cursor_range() else {
            return;
        };
        let Some(doc) = self.active_doc() else {
            return;
        };
        let bytes = doc.read_range(range.start, (range.end - range.start) as usize);
        let text = bytes
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" ");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.set_status("Copied hex.");
        cx.notify();
    }

    fn copy_text(&mut self, _: &CopyText, _: &mut Window, cx: &mut Context<Self>) {
        let Some(range) = self.selected_or_cursor_range() else {
            return;
        };
        let Some(doc) = self.active_doc() else {
            return;
        };
        let bytes = doc.read_range(range.start, (range.end - range.start) as usize);
        let text: String = bytes
            .iter()
            .map(|byte| {
                if (0x20..=0x7e).contains(byte) {
                    *byte as char
                } else {
                    '.'
                }
            })
            .collect();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.set_status("Copied text preview.");
        cx.notify();
    }

    fn cut(&mut self, action: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        self.copy_hex(&CopyHex, window, cx);
        self.delete_selection(&DeleteSelection, window, cx);
        self.set_status("Cut selection as hex.");
        let _ = action;
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        match self.active_doc_mut().map(ByteDocument::undo) {
            Some(true) => {
                self.clamp_cursor();
                self.selection = None;
                self.pending_hex = None;
                self.set_status("Undo.");
            }
            Some(false) => self.set_status("Nothing to undo."),
            None => self.set_status("No active file."),
        }
        cx.notify();
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        match self.active_doc_mut().map(ByteDocument::redo) {
            Some(true) => {
                self.clamp_cursor();
                self.selection = None;
                self.pending_hex = None;
                self.set_status("Redo.");
            }
            Some(false) => self.set_status("Nothing to redo."),
            None => self.set_status("No active file."),
        }
        cx.notify();
    }

    fn paste_hex(&mut self, _: &PasteHex, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            self.set_status("Clipboard does not contain text.");
            cx.notify();
            return;
        };
        match parse_hex_bytes(&text).and_then(|bytes| self.apply_bytes(bytes)) {
            Ok(count) => self.set_status(format!("Pasted {count} byte(s) from hex.")),
            Err(err) => self.set_status(format!("Paste hex failed: {err:#}")),
        }
        cx.notify();
    }

    fn paste_text(&mut self, _: &PasteText, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            self.set_status("Clipboard does not contain text.");
            cx.notify();
            return;
        };
        let len = text.len();
        match self.apply_bytes(text.into_bytes()) {
            Ok(_) => self.set_status(format!("Pasted {len} text byte(s).")),
            Err(err) => self.set_status(format!("Paste text failed: {err:#}")),
        }
        cx.notify();
    }

    fn delete_selection(&mut self, _: &DeleteSelection, _: &mut Window, cx: &mut Context<Self>) {
        let Some(range) = self.selected_or_cursor_range() else {
            self.set_status("No byte to delete.");
            cx.notify();
            return;
        };
        if range.is_empty() {
            self.set_status("No byte at cursor.");
            cx.notify();
            return;
        }
        match self.active_doc_mut().map(|doc| doc.delete(range.clone())) {
            Some(Ok(())) => {
                self.cursor = range.start;
                self.selection = None;
                self.clamp_cursor();
                self.set_status("Deleted selection.");
            }
            Some(Err(err)) => self.set_status(format!("Delete failed: {err:#}")),
            None => self.set_status("No active file."),
        }
        cx.notify();
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        let Some(len) = self.active_doc().map(ByteDocument::len) else {
            return;
        };
        if len > 0 {
            self.selection = Some(Selection::new(0, len - 1));
            self.cursor = len - 1;
            self.set_status(format!("Selected {len} byte(s)."));
        }
        cx.notify();
    }

    fn find_next(&mut self, _: &FindNext, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            self.set_status("Copy search bytes or text to the clipboard first.");
            cx.notify();
            return;
        };

        let needle = parse_hex_bytes(&text).unwrap_or_else(|_| text.into_bytes());
        let Some(doc) = self.active_doc() else {
            return;
        };
        let start = self.cursor.saturating_add(1).min(doc.len());
        match find_bytes(doc, &needle, start).or_else(|| find_bytes(doc, &needle, 0)) {
            Some(offset) => {
                self.cursor = offset;
                self.selection = Some(Selection::new(offset, offset + needle.len() as u64 - 1));
                self.scroll_to_cursor();
                self.set_status(format!("Found {} byte(s) at 0x{offset:X}.", needle.len()));
            }
            None => self.set_status("Pattern not found."),
        }
        cx.notify();
    }

    fn compare_next(&mut self, _: &CompareNext, _: &mut Window, cx: &mut Context<Self>) {
        if self.docs.len() < 2 {
            self.compare_with = None;
            self.set_status("Open at least two files to compare.");
            cx.notify();
            return;
        }

        let next = match self.compare_with {
            Some(ix) => (ix + 1) % self.docs.len(),
            None => (self.active + 1) % self.docs.len(),
        };
        let active = self.focused_active_ix();
        self.compare_with = Some(if next == active {
            (next + 1) % self.docs.len()
        } else {
            next
        });
        let name = self.docs[self.compare_with.unwrap()].name();
        self.set_status(format!("Comparing against {name}."));
        cx.notify();
    }

    fn toggle_endian(&mut self, _: &ToggleEndian, _: &mut Window, cx: &mut Context<Self>) {
        self.endian = match self.endian {
            Endianness::Little => Endianness::Big,
            Endianness::Big => Endianness::Little,
        };
        cx.notify();
    }

    fn next_encoding(&mut self, _: &NextEncoding, _: &mut Window, cx: &mut Context<Self>) {
        self.encoding = self.encoding.next();
        cx.notify();
    }

    fn next_row_width(&mut self, _: &NextRowWidth, _: &mut Window, cx: &mut Context<Self>) {
        self.bytes_per_row_ix = (self.bytes_per_row_ix + 1) % BYTES_PER_ROW_OPTIONS.len();
        cx.notify();
    }

    fn toggle_insert_mode(&mut self, _: &ToggleInsertMode, _: &mut Window, cx: &mut Context<Self>) {
        self.edit_mode = match self.edit_mode {
            EditMode::Insert => EditMode::Overwrite,
            EditMode::Overwrite => EditMode::Insert,
        };
        self.pending_hex = None;
        cx.notify();
    }

    fn goto(&mut self, _: &Goto, _: &mut Window, cx: &mut Context<Self>) {
        self.goto_open = true;
        self.goto_input = format!("0x{:X}", self.cursor);
        self.pending_hex = None;
        self.set_status("Goto: type an offset, Enter to jump, Esc to cancel.");
        cx.notify();
    }

    fn toggle_split(&mut self, _: &ToggleSplit, _: &mut Window, cx: &mut Context<Self>) {
        self.split = !self.split;
        if self.split {
            self.right_active = self.right_active.or_else(|| {
                if self.docs.len() > 1 {
                    Some((self.active + 1) % self.docs.len())
                } else {
                    (!self.docs.is_empty()).then_some(self.active)
                }
            });
            self.set_status("Split view enabled.");
        } else {
            self.focused_pane = PaneSide::Left;
            self.set_status("Split view disabled.");
        }
        cx.notify();
    }

    fn move_to_other_split(
        &mut self,
        _: &MoveToOtherSplit,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.docs.is_empty() {
            return;
        }
        if !self.split {
            self.split = true;
        }
        match self.focused_pane {
            PaneSide::Left => {
                self.right_active = Some(self.active);
                self.focused_pane = PaneSide::Right;
            }
            PaneSide::Right => {
                if let Some(ix) = self.right_active {
                    self.active = ix;
                }
                self.focused_pane = PaneSide::Left;
            }
        }
        self.set_status(format!(
            "Moved active file to {} pane.",
            self.focused_pane.label()
        ));
        cx.notify();
    }

    fn focus_left_pane(&mut self, _: &FocusLeftPane, _: &mut Window, cx: &mut Context<Self>) {
        self.focus_pane(PaneSide::Left);
        cx.notify();
    }

    fn focus_right_pane(&mut self, _: &FocusRightPane, _: &mut Window, cx: &mut Context<Self>) {
        if self.split {
            self.focus_pane(PaneSide::Right);
            cx.notify();
        }
    }

    fn move_left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(-1, false, cx);
    }

    fn move_right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(1, false, cx);
    }

    fn move_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(-(self.bytes_per_row() as i64), false, cx);
    }

    fn move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(self.bytes_per_row() as i64, false, cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(-1, true, cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(1, true, cx);
    }

    fn move_cursor(&mut self, delta: i64, extend: bool, cx: &mut Context<Self>) {
        let Some(doc) = self.active_doc() else {
            return;
        };
        if doc.is_empty() {
            self.set_cursor(0, extend, cx);
            return;
        }
        let next = if delta.is_negative() {
            self.cursor.saturating_sub(delta.unsigned_abs())
        } else {
            self.cursor.saturating_add(delta as u64).min(doc.len())
        };
        self.set_cursor(next, extend, cx);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.goto_open {
            self.handle_goto_key(event, cx);
            return;
        }

        if event.keystroke.modifiers.control || event.keystroke.modifiers.platform {
            return;
        }

        let Some(key_char) = event.keystroke.key_char.as_deref() else {
            return;
        };
        let Some(ch) = key_char.chars().next() else {
            return;
        };
        if let Some(nibble) = ch.to_digit(16) {
            self.input_hex_nibble(nibble as u8, cx);
            cx.stop_propagation();
        }
    }

    fn handle_goto_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => {
                self.confirm_goto(cx);
                cx.stop_propagation();
            }
            "escape" => {
                self.goto_open = false;
                self.set_status("Goto cancelled.");
                cx.notify();
                cx.stop_propagation();
            }
            "backspace" => {
                self.goto_input.pop();
                cx.notify();
                cx.stop_propagation();
            }
            _ => {
                let Some(key_char) = event.keystroke.key_char.as_deref() else {
                    return;
                };
                for ch in key_char.chars() {
                    if ch.is_ascii_hexdigit() || ch == 'x' || ch == 'X' {
                        self.goto_input.push(ch);
                    }
                }
                cx.notify();
                cx.stop_propagation();
            }
        }
    }

    fn confirm_goto(&mut self, cx: &mut Context<Self>) {
        let Some(target) = parse_offset(&self.goto_input) else {
            self.set_status("Goto failed: enter decimal or 0x-prefixed hex.");
            cx.notify();
            return;
        };
        let len = self.active_doc().map(ByteDocument::len).unwrap_or(0);
        self.cursor = target.min(len);
        self.selection = None;
        self.goto_open = false;
        self.pending_hex = None;
        self.scroll_to_cursor();
        self.set_status(format!("Moved to 0x{:X}.", self.cursor));
        cx.notify();
    }

    fn input_hex_nibble(&mut self, nibble: u8, cx: &mut Context<Self>) {
        if self.active_doc().is_none() {
            return;
        }

        if let Some(high) = self.pending_hex.take() {
            let byte = (high << 4) | nibble;
            match self.apply_bytes(vec![byte]) {
                Ok(_) => self.set_status(format!("Wrote 0x{byte:02X}.")),
                Err(err) => self.set_status(format!("Write failed: {err:#}")),
            }
        } else {
            self.pending_hex = Some(nibble);
            self.set_status(format!("Pending hex nibble {:X}_", nibble));
        }
        cx.notify();
    }

    fn apply_bytes(&mut self, bytes: Vec<u8>) -> Result<usize> {
        let count = bytes.len();
        if let Some(range) = self.selection_range() {
            let start = range.start;
            let doc = self
                .active_doc_mut()
                .expect("selection requires active doc");
            doc.replace_range(range, bytes)?;
            self.cursor = start + count as u64;
            self.selection = None;
        } else {
            let offset = self.cursor;
            match self.edit_mode {
                EditMode::Insert => self
                    .active_doc_mut()
                    .expect("active doc")
                    .insert(offset, bytes)?,
                EditMode::Overwrite => self
                    .active_doc_mut()
                    .expect("active doc")
                    .overwrite(offset, bytes)?,
            }
            self.cursor = offset + count as u64;
        }
        self.clamp_cursor();
        Ok(count)
    }

    fn activate_doc(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.docs.len() {
            self.set_active_for_focused_pane(ix);
            self.cursor = 0;
            self.selection = None;
            self.pending_hex = None;
            if self.compare_with == Some(ix) {
                self.compare_with = None;
            }
            cx.notify();
        }
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .gap_1()
            .items_center()
            .px_2()
            .py_1()
            .bg(rgb(0x20242a))
            .border_b_1()
            .border_color(rgb(0x303741))
            .child(self.toolbar_button(
                "open",
                "Open",
                Some("Ctrl+O"),
                cx.listener(|this, _, window, cx| this.open_files(&OpenFiles, window, cx)),
            ))
            .child(self.toolbar_button(
                "save-as",
                "Save As",
                Some("Ctrl+S"),
                cx.listener(|this, _, window, cx| this.save_as(&SaveAs, window, cx)),
            ))
            .child(self.toolbar_button(
                "copy-hex",
                "Copy Hex",
                Some("Ctrl+C"),
                cx.listener(|this, _, window, cx| this.copy_hex(&CopyHex, window, cx)),
            ))
            .child(self.toolbar_button(
                "copy-text",
                "Copy Text",
                Some("Ctrl+Shift+C"),
                cx.listener(|this, _, window, cx| this.copy_text(&CopyText, window, cx)),
            ))
            .child(self.toolbar_button(
                "undo",
                "Undo",
                Some("Ctrl+Z"),
                cx.listener(|this, _, window, cx| this.undo(&Undo, window, cx)),
            ))
            .child(self.toolbar_button(
                "redo",
                "Redo",
                Some("Ctrl+Y"),
                cx.listener(|this, _, window, cx| this.redo(&Redo, window, cx)),
            ))
            .child(self.toolbar_button(
                "paste-hex",
                "Paste Hex",
                Some("Ctrl+V"),
                cx.listener(|this, _, window, cx| this.paste_hex(&PasteHex, window, cx)),
            ))
            .child(self.toolbar_button(
                "paste-text",
                "Paste Text",
                Some("Ctrl+Shift+V"),
                cx.listener(|this, _, window, cx| this.paste_text(&PasteText, window, cx)),
            ))
            .child(self.toolbar_button(
                "delete",
                "Delete",
                Some("Del"),
                cx.listener(|this, _, window, cx| {
                    this.delete_selection(&DeleteSelection, window, cx)
                }),
            ))
            .child(self.toolbar_button(
                "find-clip",
                "Find",
                Some("Ctrl+F"),
                cx.listener(|this, _, window, cx| this.find_next(&FindNext, window, cx)),
            ))
            .child(self.toolbar_button(
                "goto",
                "Goto",
                Some("Ctrl+G"),
                cx.listener(|this, _, window, cx| this.goto(&Goto, window, cx)),
            ))
            .child(self.toolbar_button(
                "compare",
                "Compare",
                Some("Ctrl+D"),
                cx.listener(|this, _, window, cx| this.compare_next(&CompareNext, window, cx)),
            ))
            .child(self.toolbar_button(
                "split",
                "Split",
                Some("Ctrl+\\"),
                cx.listener(|this, _, window, cx| this.toggle_split(&ToggleSplit, window, cx)),
            ))
            .child(self.toolbar_button(
                "move-pane",
                "Move Pane",
                Some("Ctrl+M"),
                cx.listener(|this, _, window, cx| {
                    this.move_to_other_split(&MoveToOtherSplit, window, cx)
                }),
            ))
    }

    fn toolbar_button(
        &self,
        id: &'static str,
        label: impl Into<SharedString>,
        shortcut: Option<&'static str>,
        listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        let shortcut = shortcut.map(SharedString::from);
        div()
            .id(id)
            .debug_selector(|| format!("button-{id}"))
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(rgb(0x303741))
            .text_color(rgb(0xe9eef5))
            .text_sm()
            .cursor_pointer()
            .hover(|style| style.bg(rgb(0x3e4855)))
            .child(div().child(label.into()))
            .children(shortcut.into_iter().map(|shortcut| {
                div()
                    .h(px(17.0))
                    .px_1()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .bg(rgb(0x46505e))
                    .text_color(rgb(0xe6edf7))
                    .text_xs()
                    .child(shortcut)
            }))
            .on_click(listener)
            .into_any_element()
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.docs.is_empty() {
            return div()
                .px_3()
                .py_2()
                .bg(rgb(0x15181d))
                .text_color(rgb(0x8792a2))
                .child("No file open");
        }

        div()
            .flex()
            .gap_1()
            .px_2()
            .py_1()
            .bg(rgb(0x15181d))
            .children(self.docs.iter().enumerate().map(|(ix, doc)| {
                let mut tab = div()
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .text_sm()
                    .cursor_pointer()
                    .child(format!(
                        "{}{}",
                        doc.name(),
                        if doc.is_dirty() { "*" } else { "" }
                    ))
                    .id(("tab", ix))
                    .on_click(cx.listener(move |this, _, _, cx| this.activate_doc(ix, cx)));
                tab = if ix == self.active {
                    tab.bg(rgb(0x2c3643)).text_color(rgb(0xffffff))
                } else if Some(ix) == self.compare_with {
                    tab.bg(rgb(0x443827)).text_color(rgb(0xffd89c))
                } else {
                    tab.bg(rgb(0x20242a)).text_color(rgb(0xb8c1cf))
                };
                tab
            }))
    }

    fn render_hex_view(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.split {
            div()
                .flex()
                .flex_1()
                .child(self.render_hex_pane(PaneSide::Left, cx))
                .child(div().w(px(1.0)).h_full().bg(rgb(0x303741)))
                .child(self.render_hex_pane(PaneSide::Right, cx))
        } else {
            div()
                .flex()
                .flex_1()
                .child(self.render_hex_pane(PaneSide::Left, cx))
        }
    }

    fn render_hex_pane(&mut self, side: PaneSide, cx: &mut Context<Self>) -> AnyElement {
        let Some(doc_ix) = self.active_ix_for(side) else {
            return div()
                .flex_1()
                .items_center()
                .justify_center()
                .bg(rgb(0x101318))
                .text_color(rgb(0x8792a2))
                .child("Open files with the toolbar, menu, or Ctrl+O.")
                .into_any_element();
        };
        let Some(doc) = self.docs.get(doc_ix) else {
            return div().flex_1().into_any_element();
        };

        let bytes_per_row = self.bytes_per_row();
        let row_count = (doc.len() / bytes_per_row as u64 + 1).min(usize::MAX as u64) as usize;
        let focus_color = if self.focused_pane == side {
            rgb(0x3f7ab7)
        } else {
            rgb(0x303741)
        };
        div()
            .id(("hex-pane", pane_index(side)))
            .flex_1()
            .flex()
            .flex_col()
            .bg(rgb(0x101318))
            .border_1()
            .border_color(focus_color)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.focus_pane(side);
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .px_2()
                    .py_1()
                    .bg(rgb(0x15181d))
                    .text_xs()
                    .text_color(rgb(0xb8c1cf))
                    .child(format!(
                        "{} pane: {}{}",
                        side.label(),
                        doc.name(),
                        if doc.is_dirty() { "*" } else { "" }
                    ))
                    .child(if self.focused_pane == side {
                        "focused"
                    } else {
                        ""
                    }),
            )
            .child(
                div().flex_1().child(
                    uniform_list(
                        ("hex-rows", pane_index(side)),
                        row_count,
                        cx.processor(move |this, range: Range<usize>, _window, cx| {
                            let mut rows = Vec::with_capacity(range.end - range.start);
                            for row in range {
                                rows.push(this.render_hex_row(doc_ix, side, row, cx));
                            }
                            rows
                        }),
                    )
                    .track_scroll(self.scroll_handle_for(side))
                    .h_full(),
                ),
            )
            .into_any_element()
    }

    fn render_hex_row(
        &self,
        doc_ix: usize,
        side: PaneSide,
        row: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let bytes_per_row = self.bytes_per_row();
        let offset = row as u64 * bytes_per_row as u64;
        let bytes = self
            .docs
            .get(doc_ix)
            .map(|doc| doc.read_range(offset, bytes_per_row))
            .unwrap_or_default();

        let mut byte_cells = Vec::with_capacity(bytes_per_row);
        for ix in 0..bytes_per_row {
            let byte_offset = offset + ix as u64;
            let label = bytes
                .get(ix)
                .map(|byte| format!("{byte:02X}"))
                .or_else(|| {
                    self.docs
                        .get(doc_ix)
                        .and_then(|doc| (byte_offset == doc.len()).then_some("++".to_string()))
                })
                .unwrap_or_else(|| "  ".to_string());
            byte_cells.push(self.render_byte_cell(doc_ix, side, byte_offset, label, cx));
        }

        let mut ascii_cells = Vec::with_capacity(bytes_per_row);
        for ix in 0..bytes_per_row {
            let byte_offset = offset + ix as u64;
            let ch = bytes
                .get(ix)
                .map(|byte| {
                    if (0x20..=0x7e).contains(byte) {
                        *byte as char
                    } else {
                        '.'
                    }
                })
                .or_else(|| {
                    self.docs
                        .get(doc_ix)
                        .and_then(|doc| (byte_offset == doc.len()).then_some('+'))
                })
                .unwrap_or(' ');
            ascii_cells.push(self.render_ascii_cell(doc_ix, side, byte_offset, ch, cx));
        }

        div()
            .flex()
            .items_center()
            .h(px(28.0))
            .px_2()
            .text_sm()
            .font_family("Consolas, ui-monospace, SFMono-Regular, monospace")
            .text_color(rgb(0xdbe4ef))
            .child(
                div()
                    .w(px(132.0))
                    .text_color(rgb(0x768497))
                    .child(format!("{offset:016X}")),
            )
            .child(div().flex().gap_1().children(byte_cells))
            .child(div().ml_4().flex().children(ascii_cells))
            .into_any_element()
    }

    fn render_byte_cell(
        &self,
        doc_ix: usize,
        side: PaneSide,
        offset: u64,
        label: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let doc_len = self.docs.get(doc_ix).map(ByteDocument::len).unwrap_or(0);
        let exists = offset < doc_len;
        let insertion_point = offset == doc_len;
        let clickable = offset <= doc_len;
        let selected = self
            .selection_range()
            .is_some_and(|range| range.start <= offset && offset < range.end);
        let cursor = offset == self.cursor && self.focused_pane == side && clickable;
        let different = self.is_different_for(doc_ix, offset);

        let mut cell = div()
            .id(("byte", byte_cell_id(side, offset, 0)))
            .w(px(28.0))
            .h(px(22.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_sm()
            .border_1()
            .border_color(rgba(0x00000000))
            .cursor_pointer()
            .child(label);

        if clickable {
            cell = cell
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.begin_cell_selection(doc_ix, side, offset, event.modifiers.shift, cx);
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                    if this.drag_anchor.is_some() && event.dragging() {
                        this.drag_cell_selection(doc_ix, side, offset, cx);
                        cx.stop_propagation();
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseUpEvent, _, cx| {
                        this.finish_cell_selection(cx);
                        cx.stop_propagation();
                    }),
                );
        }

        cell = if selected {
            cell.bg(rgb(0x2e6fb8)).text_color(rgb(0xffffff))
        } else if cursor && self.edit_mode == EditMode::Insert {
            cell.bg(rgb(0x171c22))
                .border_color(rgb(0xd7a94a))
                .text_color(rgb(0xf3d68b))
        } else if cursor {
            cell.bg(rgb(0xd7a94a)).text_color(rgb(0x101318))
        } else if different {
            cell.bg(rgba(0xff5c5c40))
        } else if exists {
            cell.bg(rgb(0x171c22))
        } else if insertion_point {
            cell.bg(rgb(0x121920)).text_color(rgb(0x7ea4d8))
        } else {
            cell.text_color(rgb(0x3c4654))
        };
        cell.into_any_element()
    }

    fn render_ascii_cell(
        &self,
        doc_ix: usize,
        side: PaneSide,
        offset: u64,
        ch: char,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let doc_len = self.docs.get(doc_ix).map(ByteDocument::len).unwrap_or(0);
        let exists = offset < doc_len;
        let clickable = offset <= doc_len;
        let cursor = offset == self.cursor && self.focused_pane == side && clickable;
        let selected = self
            .selection_range()
            .is_some_and(|range| range.start <= offset && offset < range.end);
        let mut cell = div()
            .id(("byte", byte_cell_id(side, offset, 1)))
            .w(px(14.0))
            .h(px(22.0))
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(rgba(0x00000000))
            .cursor_pointer()
            .child(ch.to_string());
        if clickable {
            cell = cell
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.begin_cell_selection(doc_ix, side, offset, event.modifiers.shift, cx);
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                    if this.drag_anchor.is_some() && event.dragging() {
                        this.drag_cell_selection(doc_ix, side, offset, cx);
                        cx.stop_propagation();
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseUpEvent, _, cx| {
                        this.finish_cell_selection(cx);
                        cx.stop_propagation();
                    }),
                );
        }
        cell = if selected {
            cell.bg(rgb(0x2e6fb8)).text_color(rgb(0xffffff))
        } else if cursor && self.edit_mode == EditMode::Insert {
            cell.border_color(rgb(0xd7a94a)).text_color(rgb(0xf3d68b))
        } else if cursor {
            cell.bg(rgb(0xd7a94a)).text_color(rgb(0x101318))
        } else if exists {
            cell.text_color(rgb(0xb8c1cf))
        } else {
            cell.text_color(rgb(0x3c4654))
        };
        cell.into_any_element()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn is_different(&self, offset: u64) -> bool {
        self.is_different_for(self.focused_active_ix(), offset)
    }

    fn is_different_for(&self, doc_ix: usize, offset: u64) -> bool {
        let Some(compare_ix) = self.compare_with else {
            return false;
        };
        let Some(left) = self.docs.get(doc_ix).and_then(|doc| doc.byte_at(offset)) else {
            return false;
        };
        self.docs
            .get(compare_ix)
            .and_then(|doc| doc.byte_at(offset))
            .is_none_or(|right| right != left)
    }

    fn render_inspector(&self) -> impl IntoElement {
        let format_summary = self
            .active_doc()
            .and_then(|doc| detect_format_fields(doc, 96));
        let bytes = self
            .active_doc()
            .map(|doc| doc.read_range(self.cursor, 64))
            .unwrap_or_default();
        let values = inspector_values(&bytes, self.cursor, self.endian, self.encoding);
        let selection = self
            .selection_range()
            .map(|range| format!("{} byte(s)", range.end - range.start))
            .unwrap_or_else(|| "none".to_string());
        let compare = self
            .compare_with
            .and_then(|ix| self.docs.get(ix))
            .map(ByteDocument::name)
            .unwrap_or_else(|| "off".to_string());

        div()
            .id("inspector")
            .w(px(300.0))
            .h_full()
            .bg(rgb(0x15181d))
            .border_l_1()
            .border_color(rgb(0x303741))
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .overflow_y_scroll()
            .text_sm()
            .text_color(rgb(0xdbe4ef))
            .child(div().text_lg().child("Inspector"))
            .child(
                self.meta_line(
                    "File",
                    self.active_doc()
                        .map(ByteDocument::name)
                        .unwrap_or_default(),
                ),
            )
            .child(
                self.meta_line(
                    "Path",
                    self.active_doc()
                        .and_then(ByteDocument::path)
                        .map(|path| path.display().to_string())
                        .unwrap_or_default(),
                ),
            )
            .child(
                self.meta_line(
                    "Length",
                    self.active_doc()
                        .map(|d| d.len().to_string())
                        .unwrap_or_default(),
                ),
            )
            .child(self.meta_line("Selection", selection))
            .child(self.meta_line("Compare", compare))
            .child(
                self.meta_line(
                    "Hex input",
                    self.pending_hex
                        .map(|n| format!("{n:X}_"))
                        .unwrap_or_else(|| "--".to_string()),
                ),
            )
            .child(div().h(px(1.0)).bg(rgb(0x303741)).my_2())
            .child(self.render_format_section(format_summary))
            .child(div().h(px(1.0)).bg(rgb(0x303741)).my_2())
            .children(
                values
                    .into_iter()
                    .map(|value| self.meta_line(value.label, value.value)),
            )
    }

    fn meta_line(
        &self,
        label: impl Into<SharedString>,
        value: impl Into<SharedString>,
    ) -> impl IntoElement {
        div()
            .flex()
            .justify_between()
            .gap_2()
            .child(div().text_color(rgb(0x8792a2)).child(label.into()))
            .child(div().text_align(gpui::TextAlign::Right).child(value.into()))
    }

    fn render_format_section(&self, summary: Option<FormatSummary>) -> AnyElement {
        let Some(summary) = summary else {
            return self.meta_line("Format", "unknown").into_any_element();
        };

        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(self.meta_line("Format", summary.format))
            .children(
                summary
                    .fields
                    .into_iter()
                    .map(|field| self.render_format_field(field)),
            )
            .into_any_element()
    }

    fn render_format_field(&self, field: FormatField) -> AnyElement {
        let active = field.contains(self.cursor);
        let background = if active { 0x26313d } else { 0x1b1f26 };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(rgb(background))
            .border_1()
            .border_color(if active { rgb(0x5a87c7) } else { rgb(0x303741) })
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x8d99aa))
                            .child(format!("0x{:08X} +{}", field.offset, field.len)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x9fb1c8))
                            .text_align(gpui::TextAlign::Right)
                            .child(field.value),
                    ),
            )
            .child(div().text_color(rgb(0xe3ebf6)).child(field.name))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0xaeb8c6))
                    .child(field.meaning),
            )
            .into_any_element()
    }

    fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let message = if self.goto_open {
            format!("Goto > {}", self.goto_input)
        } else {
            self.status.to_string()
        };
        let endian_label = match self.endian {
            Endianness::Little => "Little",
            Endianness::Big => "Big",
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .px_2()
            .py_1()
            .bg(rgb(0x20242a))
            .border_t_1()
            .border_color(rgb(0x303741))
            .text_color(rgb(0xb8c1cf))
            .text_sm()
            .child(div().child(message))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(self.toolbar_button(
                        "focus-left",
                        "Left",
                        Some("Ctrl+1"),
                        cx.listener(|this, _, window, cx| {
                            this.focus_left_pane(&FocusLeftPane, window, cx)
                        }),
                    ))
                    .child(self.toolbar_button(
                        "focus-right",
                        "Right",
                        Some("Ctrl+2"),
                        cx.listener(|this, _, window, cx| {
                            this.focus_right_pane(&FocusRightPane, window, cx)
                        }),
                    ))
                    .child(self.toolbar_button(
                        "row-width",
                        format!("{} B/row", self.bytes_per_row()),
                        Some("Ctrl+B"),
                        cx.listener(|this, _, window, cx| {
                            this.next_row_width(&NextRowWidth, window, cx)
                        }),
                    ))
                    .child(self.toolbar_button(
                        "edit-mode",
                        self.edit_mode.label(),
                        Some("Ins"),
                        cx.listener(|this, _, window, cx| {
                            this.toggle_insert_mode(&ToggleInsertMode, window, cx)
                        }),
                    ))
                    .child(self.toolbar_button(
                        "endian",
                        endian_label,
                        Some("Ctrl+Alt+E"),
                        cx.listener(|this, _, window, cx| {
                            this.toggle_endian(&ToggleEndian, window, cx)
                        }),
                    ))
                    .child(self.toolbar_button(
                        "encoding",
                        self.encoding.label(),
                        Some("Ctrl+Alt+N"),
                        cx.listener(|this, _, window, cx| {
                            this.next_encoding(&NextEncoding, window, cx)
                        }),
                    )),
            )
    }
}

impl Focusable for ByteForge {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ByteForge {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus_handle(cx))
            .key_context("ByteForge")
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.open_dropped_paths(paths.paths(), cx);
            }))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(Self::open_files))
            .on_action(cx.listener(Self::save_as))
            .on_action(cx.listener(Self::copy_hex))
            .on_action(cx.listener(Self::copy_text))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::paste_hex))
            .on_action(cx.listener(Self::paste_text))
            .on_action(cx.listener(Self::delete_selection))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::find_next))
            .on_action(cx.listener(Self::compare_next))
            .on_action(cx.listener(Self::goto))
            .on_action(cx.listener(Self::toggle_split))
            .on_action(cx.listener(Self::move_to_other_split))
            .on_action(cx.listener(Self::focus_left_pane))
            .on_action(cx.listener(Self::focus_right_pane))
            .on_action(cx.listener(Self::toggle_endian))
            .on_action(cx.listener(Self::next_encoding))
            .on_action(cx.listener(Self::next_row_width))
            .on_action(cx.listener(Self::toggle_insert_mode))
            .on_action(cx.listener(Self::move_left))
            .on_action(cx.listener(Self::move_right))
            .on_action(cx.listener(Self::move_up))
            .on_action(cx.listener(Self::move_down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .bg(rgb(0x101318))
            .flex()
            .flex_col()
            .child(self.render_toolbar(cx))
            .child(self.render_tabs(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .child(self.render_hex_view(cx))
                    .child(self.render_inspector()),
            )
            .child(self.render_status(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        AvailableSpace, Entity, Keystroke, Modifiers, TestAppContext, VisualTestContext,
        WindowHandle, point, size,
    };

    fn bind_test_keys(cx: &mut App) {
        cx.bind_keys([
            KeyBinding::new("secondary-o", OpenFiles, None),
            KeyBinding::new("secondary-s", SaveAs, None),
            KeyBinding::new("secondary-c", CopyHex, None),
            KeyBinding::new("secondary-shift-c", CopyText, None),
            KeyBinding::new("secondary-x", Cut, None),
            KeyBinding::new("secondary-z", Undo, None),
            KeyBinding::new("secondary-y", Redo, None),
            KeyBinding::new("secondary-v", PasteHex, None),
            KeyBinding::new("secondary-shift-v", PasteText, None),
            KeyBinding::new("delete", DeleteSelection, None),
            KeyBinding::new("secondary-a", SelectAll, None),
            KeyBinding::new("secondary-f", FindNext, None),
            KeyBinding::new("secondary-g", Goto, None),
            KeyBinding::new("secondary-d", CompareNext, None),
            KeyBinding::new("secondary-\\", ToggleSplit, None),
            KeyBinding::new("secondary-m", MoveToOtherSplit, None),
            KeyBinding::new("secondary-1", FocusLeftPane, None),
            KeyBinding::new("secondary-2", FocusRightPane, None),
            KeyBinding::new("secondary-b", NextRowWidth, None),
            KeyBinding::new("secondary-alt-e", ToggleEndian, None),
            KeyBinding::new("secondary-alt-n", NextEncoding, None),
            KeyBinding::new("insert", ToggleInsertMode, None),
            KeyBinding::new("left", MoveLeft, None),
            KeyBinding::new("right", MoveRight, None),
            KeyBinding::new("up", MoveUp, None),
            KeyBinding::new("down", MoveDown, None),
            KeyBinding::new("shift-left", SelectLeft, None),
            KeyBinding::new("shift-right", SelectRight, None),
        ]);
    }

    fn open_test_window(
        cx: &mut TestAppContext,
        docs: Vec<ByteDocument>,
    ) -> WindowHandle<ByteForge> {
        cx.update(bind_test_keys);
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| ByteForge::with_documents(docs, cx))
            })
            .unwrap()
        });
        window
            .update(cx, |view, window, cx| {
                window.focus(&view.focus_handle(cx));
            })
            .unwrap();
        window
    }

    fn doc_bytes(view: &ByteForge) -> Vec<u8> {
        let doc = view.active_doc().unwrap();
        doc.read_range(0, doc.len() as usize)
    }

    fn clipboard_text(cx: &mut Context<ByteForge>) -> String {
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default()
    }

    fn draw_visual_app(cx: &mut VisualTestContext, view: &Entity<ByteForge>) {
        cx.draw(
            point(px(0.0), px(0.0)),
            size(
                AvailableSpace::Definite(px(1600.0)),
                AvailableSpace::Definite(px(900.0)),
            ),
            |_, _| view.clone(),
        );
    }

    fn click_button(cx: &mut VisualTestContext, selector: &'static str) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing button bounds for {selector}"));
        cx.simulate_click(bounds.center(), Modifiers::default());
    }

    #[gpui::test]
    fn headless_window_renders_with_large_document(cx: &mut TestAppContext) {
        let bytes = (0..(4 * 1024 * 1024)).map(|ix| (ix % 251) as u8).collect();
        let window = open_test_window(cx, vec![ByteDocument::from_bytes("large.bin", bytes)]);

        window
            .update(cx, |view, _, _| {
                assert_eq!(view.active_doc().unwrap().len(), 4 * 1024 * 1024);
                assert_eq!(view.bytes_per_row(), 16);
                assert_eq!(view.cursor, 0);
            })
            .unwrap();
    }

    #[gpui::test]
    fn hex_view_draws_rows_and_tracks_scroll_size(cx: &mut TestAppContext) {
        let bytes = (0..4096).map(|ix| (ix % 251) as u8).collect();
        let (view, cx) = cx.add_window_view(|_, cx| {
            ByteForge::with_documents(vec![ByteDocument::from_bytes("visible.bin", bytes)], cx)
        });

        cx.draw(
            point(px(0.0), px(0.0)),
            size(
                AvailableSpace::Definite(px(900.0)),
                AvailableSpace::Definite(px(520.0)),
            ),
            |_, _| view.clone(),
        );

        view.update(cx, |view, _| {
            let scroll_state = view.left_scroll_handle.0.borrow();
            let item_size = scroll_state
                .last_item_size
                .expect("hex uniform list should be laid out during draw");
            assert!(
                item_size.item.height > gpui::Pixels::ZERO,
                "hex rows must have visible height"
            );
            assert!(
                item_size.contents.height > item_size.item.height,
                "large document should produce scrollable list content"
            );
        });
    }

    #[gpui::test]
    fn keyboard_navigation_selection_and_direct_hex_input(cx: &mut TestAppContext) {
        let window = open_test_window(
            cx,
            vec![ByteDocument::from_bytes("sample.bin", b"abcdef".to_vec())],
        );

        cx.dispatch_keystroke(*window, Keystroke::parse("right").unwrap());
        cx.dispatch_keystroke(*window, Keystroke::parse("shift-right").unwrap());
        window
            .update(cx, |view, _, _| {
                assert_eq!(view.cursor, 2);
                assert_eq!(view.selection_range(), Some(1..3));
            })
            .unwrap();

        cx.dispatch_keystroke(*window, Keystroke::parse("A").unwrap());
        cx.dispatch_keystroke(*window, Keystroke::parse("F").unwrap());
        window
            .update(cx, |view, _, _| {
                assert_eq!(doc_bytes(view), b"a\xAFdef");
                assert_eq!(view.cursor, 2);
                assert!(view.selection.is_none());
            })
            .unwrap();
    }

    #[gpui::test]
    fn copy_cut_paste_delete_undo_redo_actions(cx: &mut TestAppContext) {
        let window = open_test_window(
            cx,
            vec![ByteDocument::from_bytes("sample.bin", b"abcdef".to_vec())],
        );

        window
            .update(cx, |view, window, cx| {
                view.select_all(&SelectAll, window, cx);
                view.copy_hex(&CopyHex, window, cx);
                assert_eq!(clipboard_text(cx), "61 62 63 64 65 66");
                view.copy_text(&CopyText, window, cx);
                assert_eq!(clipboard_text(cx), "abcdef");

                view.set_cursor(1, false, cx);
                view.set_cursor(3, true, cx);
                view.cut(&Cut, window, cx);
                assert_eq!(clipboard_text(cx), "62 63 64");
                assert_eq!(doc_bytes(view), b"aef");

                view.undo(&Undo, window, cx);
                assert_eq!(doc_bytes(view), b"abcdef");
                view.redo(&Redo, window, cx);
                assert_eq!(doc_bytes(view), b"aef");

                view.toggle_insert_mode(&ToggleInsertMode, window, cx);
                cx.write_to_clipboard(ClipboardItem::new_string("CA FE".to_string()));
                view.paste_hex(&PasteHex, window, cx);
                assert_eq!(doc_bytes(view), b"a\xCA\xFEef");

                cx.write_to_clipboard(ClipboardItem::new_string("xy".to_string()));
                view.paste_text(&PasteText, window, cx);
                assert_eq!(doc_bytes(view), b"a\xCA\xFExyef");

                view.set_cursor(1, false, cx);
                view.set_cursor(2, true, cx);
                view.delete_selection(&DeleteSelection, window, cx);
                assert_eq!(doc_bytes(view), b"axyef");
            })
            .unwrap();
    }

    #[gpui::test]
    fn find_compare_toggles_tabs_and_modes(cx: &mut TestAppContext) {
        let window = open_test_window(
            cx,
            vec![
                ByteDocument::from_bytes("left.bin", b"abcXXdef".to_vec()),
                ByteDocument::from_bytes("right.bin", b"abcYYdef!".to_vec()),
            ],
        );

        window
            .update(cx, |view, window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string("58 58".to_string()));
                view.find_next(&FindNext, window, cx);
                assert_eq!(view.selection_range(), Some(3..5));

                view.compare_next(&CompareNext, window, cx);
                assert_eq!(view.compare_with, Some(1));
                assert!(view.is_different(3));
                assert!(!view.is_different(0));

                assert_eq!(view.endian, Endianness::Little);
                view.toggle_endian(&ToggleEndian, window, cx);
                assert_eq!(view.endian, Endianness::Big);

                assert_eq!(view.encoding, PreviewEncoding::Utf8);
                view.next_encoding(&NextEncoding, window, cx);
                assert_eq!(view.encoding, PreviewEncoding::Utf16Le);

                assert_eq!(view.bytes_per_row(), 16);
                view.next_row_width(&NextRowWidth, window, cx);
                assert_eq!(view.bytes_per_row(), 24);

                assert_eq!(view.edit_mode, EditMode::Overwrite);
                view.toggle_insert_mode(&ToggleInsertMode, window, cx);
                assert_eq!(view.edit_mode, EditMode::Insert);

                view.activate_doc(1, cx);
                assert_eq!(view.active, 1);
                assert_eq!(view.active_doc().unwrap().name(), "right.bin");
            })
            .unwrap();
    }

    #[gpui::test]
    fn keybindings_dispatch_to_actions(cx: &mut TestAppContext) {
        let window = open_test_window(
            cx,
            vec![ByteDocument::from_bytes("sample.bin", b"abcdef".to_vec())],
        );

        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-a").unwrap());
        cx.dispatch_keystroke(*window, Keystroke::parse("delete").unwrap());
        window
            .update(cx, |view, _, _| {
                assert!(view.active_doc().unwrap().is_empty());
            })
            .unwrap();

        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-z").unwrap());
        window
            .update(cx, |view, _, _| {
                assert_eq!(doc_bytes(view), b"abcdef");
            })
            .unwrap();

        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-y").unwrap());
        window
            .update(cx, |view, _, _| {
                assert!(view.active_doc().unwrap().is_empty());
            })
            .unwrap();
    }

    #[gpui::test]
    fn menu_buttons_click_dispatch_to_actions(cx: &mut TestAppContext) {
        cx.update(bind_test_keys);
        let mut open_path = std::env::temp_dir();
        open_path.push(format!(
            "byteforge-button-open-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&open_path, b"opened").unwrap();

        let mut save_path = std::env::temp_dir();
        save_path.push(format!(
            "byteforge-button-save-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let (view, cx) = cx.add_window_view(|_, cx| {
            ByteForge::with_documents(
                vec![
                    ByteDocument::from_bytes("sample.bin", b"abcdef".to_vec()),
                    ByteDocument::from_bytes("other.bin", b"abcxef".to_vec()),
                ],
                cx,
            )
        });
        draw_visual_app(cx, &view);

        view.update(cx, |view, _| {
            view.test_open_paths = Some(vec![open_path.clone()])
        });
        click_button(cx, "button-open");
        view.update(cx, |view, _| {
            assert_eq!(view.docs.len(), 3);
            assert_eq!(
                view.active_doc().unwrap().name(),
                open_path.file_name().unwrap().to_string_lossy()
            );
        });

        view.update(cx, |view, _| {
            view.active = 0;
            view.focused_pane = PaneSide::Left;
            view.test_save_path = Some(save_path.clone());
        });
        click_button(cx, "button-save-as");
        assert_eq!(std::fs::read(&save_path).unwrap(), b"abcdef");

        view.update(cx, |view, cx| view.set_cursor(1, false, cx));
        click_button(cx, "button-copy-hex");
        let copied = view.update(cx, |_, cx| {
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .unwrap_or_default()
        });
        assert_eq!(copied, "62");

        click_button(cx, "button-copy-text");
        let copied = view.update(cx, |_, cx| {
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .unwrap_or_default()
        });
        assert_eq!(copied, "b");

        view.update(cx, |view, cx| {
            view.edit_mode = EditMode::Insert;
            view.set_cursor(0, false, cx);
            view.apply_bytes(b"X".to_vec()).unwrap();
        });
        click_button(cx, "button-undo");
        view.update(cx, |view, _| assert_eq!(doc_bytes(view), b"abcdef"));
        click_button(cx, "button-redo");
        view.update(cx, |view, _| assert_eq!(doc_bytes(view), b"Xabcdef"));

        view.update(cx, |view, cx| {
            view.docs[0] = ByteDocument::from_bytes("sample.bin", b"abcdef".to_vec());
            view.active = 0;
            view.edit_mode = EditMode::Insert;
            view.set_cursor(1, false, cx);
            cx.write_to_clipboard(ClipboardItem::new_string("CA FE".to_string()));
        });
        click_button(cx, "button-paste-hex");
        view.update(cx, |view, _| assert_eq!(doc_bytes(view), b"a\xCA\xFEbcdef"));

        view.update(cx, |view, cx| {
            view.docs[0] = ByteDocument::from_bytes("sample.bin", b"abcdef".to_vec());
            view.active = 0;
            view.edit_mode = EditMode::Insert;
            view.set_cursor(2, false, cx);
            cx.write_to_clipboard(ClipboardItem::new_string("Z".to_string()));
        });
        click_button(cx, "button-paste-text");
        view.update(cx, |view, _| assert_eq!(doc_bytes(view), b"abZcdef"));

        view.update(cx, |view, cx| {
            view.docs[0] = ByteDocument::from_bytes("sample.bin", b"abcdef".to_vec());
            view.active = 0;
            view.set_cursor(0, false, cx);
        });
        click_button(cx, "button-delete");
        view.update(cx, |view, _| assert_eq!(doc_bytes(view), b"bcdef"));

        view.update(cx, |view, cx| {
            view.docs[0] = ByteDocument::from_bytes("sample.bin", b"abcdef".to_vec());
            view.active = 0;
            view.set_cursor(0, false, cx);
            cx.write_to_clipboard(ClipboardItem::new_string("64".to_string()));
        });
        click_button(cx, "button-find-clip");
        view.update(cx, |view, _| assert_eq!(view.selection_range(), Some(3..4)));

        click_button(cx, "button-goto");
        view.update(cx, |view, _| assert!(view.goto_open));
        view.update(cx, |view, _| view.goto_open = false);

        click_button(cx, "button-compare");
        view.update(cx, |view, _| assert_eq!(view.compare_with, Some(1)));

        click_button(cx, "button-split");
        view.update(cx, |view, _| assert!(view.split));

        click_button(cx, "button-move-pane");
        view.update(cx, |view, _| assert_eq!(view.focused_pane, PaneSide::Right));

        draw_visual_app(cx, &view);
        click_button(cx, "button-focus-left");
        view.update(cx, |view, _| assert_eq!(view.focused_pane, PaneSide::Left));
        click_button(cx, "button-focus-right");
        view.update(cx, |view, _| assert_eq!(view.focused_pane, PaneSide::Right));

        let original_row_width = view.update(cx, |view, _| view.bytes_per_row());
        click_button(cx, "button-row-width");
        view.update(cx, |view, _| {
            assert_ne!(view.bytes_per_row(), original_row_width)
        });

        let original_mode = view.update(cx, |view, _| view.edit_mode);
        click_button(cx, "button-edit-mode");
        view.update(cx, |view, _| assert_ne!(view.edit_mode, original_mode));

        let original_endian = view.update(cx, |view, _| view.endian);
        click_button(cx, "button-endian");
        view.update(cx, |view, _| assert_ne!(view.endian, original_endian));

        let original_encoding = view.update(cx, |view, _| view.encoding);
        click_button(cx, "button-encoding");
        view.update(cx, |view, _| assert_ne!(view.encoding, original_encoding));

        let _ = std::fs::remove_file(open_path);
        let _ = std::fs::remove_file(save_path);
    }

    #[gpui::test]
    fn menu_button_shortcuts_dispatch_to_actions(cx: &mut TestAppContext) {
        let mut open_path = std::env::temp_dir();
        open_path.push(format!(
            "byteforge-shortcut-open-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&open_path, b"opened").unwrap();

        let mut save_path = std::env::temp_dir();
        save_path.push(format!(
            "byteforge-shortcut-save-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let window = open_test_window(
            cx,
            vec![
                ByteDocument::from_bytes("sample.bin", b"abcdef".to_vec()),
                ByteDocument::from_bytes("other.bin", b"abcxef".to_vec()),
            ],
        );

        window
            .update(cx, |view, _, _| {
                view.test_open_paths = Some(vec![open_path.clone()]);
            })
            .unwrap();
        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-o").unwrap());
        window
            .update(cx, |view, _, _| {
                assert_eq!(view.docs.len(), 3);
                assert_eq!(
                    view.active_doc().unwrap().name(),
                    open_path.file_name().unwrap().to_string_lossy()
                );
                view.active = 0;
                view.focused_pane = PaneSide::Left;
                view.test_save_path = Some(save_path.clone());
            })
            .unwrap();

        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-s").unwrap());
        assert_eq!(std::fs::read(&save_path).unwrap(), b"abcdef");

        window
            .update(cx, |view, _, cx| {
                view.set_cursor(1, false, cx);
            })
            .unwrap();
        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-c").unwrap());
        window
            .update(cx, |_, _, cx| assert_eq!(clipboard_text(cx), "62"))
            .unwrap();

        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-shift-c").unwrap());
        window
            .update(cx, |_, _, cx| assert_eq!(clipboard_text(cx), "b"))
            .unwrap();

        window
            .update(cx, |view, _, cx| {
                view.docs[0] = ByteDocument::from_bytes("sample.bin", b"abcdef".to_vec());
                view.active = 0;
                view.edit_mode = EditMode::Insert;
                view.set_cursor(1, false, cx);
                cx.write_to_clipboard(ClipboardItem::new_string("CA FE".to_string()));
            })
            .unwrap();
        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-v").unwrap());
        window
            .update(cx, |view, _, _| {
                assert_eq!(doc_bytes(view), b"a\xCA\xFEbcdef")
            })
            .unwrap();

        window
            .update(cx, |view, _, cx| {
                view.docs[0] = ByteDocument::from_bytes("sample.bin", b"abcdef".to_vec());
                view.active = 0;
                view.edit_mode = EditMode::Insert;
                view.set_cursor(2, false, cx);
                cx.write_to_clipboard(ClipboardItem::new_string("Z".to_string()));
            })
            .unwrap();
        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-shift-v").unwrap());
        window
            .update(cx, |view, _, _| assert_eq!(doc_bytes(view), b"abZcdef"))
            .unwrap();

        window
            .update(cx, |view, _, cx| {
                view.docs[0] = ByteDocument::from_bytes("sample.bin", b"abcdef".to_vec());
                view.active = 0;
                view.set_cursor(0, false, cx);
            })
            .unwrap();
        cx.dispatch_keystroke(*window, Keystroke::parse("delete").unwrap());
        window
            .update(cx, |view, _, _| assert_eq!(doc_bytes(view), b"bcdef"))
            .unwrap();

        window
            .update(cx, |view, _, cx| {
                view.docs[0] = ByteDocument::from_bytes("sample.bin", b"abcdef".to_vec());
                view.active = 0;
                view.set_cursor(0, false, cx);
                cx.write_to_clipboard(ClipboardItem::new_string("64".to_string()));
            })
            .unwrap();
        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-f").unwrap());
        window
            .update(cx, |view, _, _| {
                assert_eq!(view.selection_range(), Some(3..4))
            })
            .unwrap();

        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-g").unwrap());
        window
            .update(cx, |view, _, _| {
                assert!(view.goto_open);
                view.goto_open = false;
            })
            .unwrap();

        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-d").unwrap());
        window
            .update(cx, |view, _, _| assert_eq!(view.compare_with, Some(1)))
            .unwrap();

        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-\\").unwrap());
        window.update(cx, |view, _, _| assert!(view.split)).unwrap();

        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-m").unwrap());
        window
            .update(cx, |view, _, _| {
                assert_eq!(view.focused_pane, PaneSide::Right)
            })
            .unwrap();

        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-1").unwrap());
        window
            .update(cx, |view, _, _| {
                assert_eq!(view.focused_pane, PaneSide::Left)
            })
            .unwrap();
        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-2").unwrap());
        window
            .update(cx, |view, _, _| {
                assert_eq!(view.focused_pane, PaneSide::Right)
            })
            .unwrap();

        let original_row_width = window
            .update(cx, |view, _, _| view.bytes_per_row())
            .unwrap();
        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-b").unwrap());
        window
            .update(cx, |view, _, _| {
                assert_ne!(view.bytes_per_row(), original_row_width)
            })
            .unwrap();

        let original_mode = window.update(cx, |view, _, _| view.edit_mode).unwrap();
        cx.dispatch_keystroke(*window, Keystroke::parse("insert").unwrap());
        window
            .update(cx, |view, _, _| assert_ne!(view.edit_mode, original_mode))
            .unwrap();

        let original_endian = window.update(cx, |view, _, _| view.endian).unwrap();
        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-alt-e").unwrap());
        window
            .update(cx, |view, _, _| assert_ne!(view.endian, original_endian))
            .unwrap();

        let original_encoding = window.update(cx, |view, _, _| view.encoding).unwrap();
        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-alt-n").unwrap());
        window
            .update(cx, |view, _, _| {
                assert_ne!(view.encoding, original_encoding)
            })
            .unwrap();

        window
            .update(cx, |view, _, cx| {
                view.docs[0] = ByteDocument::from_bytes("sample.bin", b"abcdef".to_vec());
                view.active = 0;
                view.edit_mode = EditMode::Insert;
                view.set_cursor(0, false, cx);
                view.apply_bytes(b"X".to_vec()).unwrap();
            })
            .unwrap();
        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-z").unwrap());
        window
            .update(cx, |view, _, _| assert_eq!(doc_bytes(view), b"abcdef"))
            .unwrap();
        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-y").unwrap());
        window
            .update(cx, |view, _, _| assert_eq!(doc_bytes(view), b"Xabcdef"))
            .unwrap();

        let _ = std::fs::remove_file(open_path);
        let _ = std::fs::remove_file(save_path);
    }

    #[gpui::test]
    fn eof_cursor_append_and_cursor_delete(cx: &mut TestAppContext) {
        let window = open_test_window(
            cx,
            vec![ByteDocument::from_bytes("sample.bin", b"abcdef".to_vec())],
        );

        window
            .update(cx, |view, window, cx| {
                view.set_cursor(6, false, cx);
                assert_eq!(view.cursor, 6);
                assert_eq!(view.selected_or_cursor_range(), Some(6..6));

                view.toggle_insert_mode(&ToggleInsertMode, window, cx);
                view.apply_bytes(b"XY".to_vec()).unwrap();
                assert_eq!(doc_bytes(view), b"abcdefXY");
                assert_eq!(view.cursor, 8);

                view.set_cursor(1, false, cx);
                view.delete_selection(&DeleteSelection, window, cx);
                assert_eq!(doc_bytes(view), b"acdefXY");
                assert_eq!(view.cursor, 1);
            })
            .unwrap();
    }

    #[gpui::test]
    fn goto_and_find_scroll_to_target(cx: &mut TestAppContext) {
        let bytes = (0..512).map(|ix| (ix % 251) as u8).collect();
        let window = open_test_window(cx, vec![ByteDocument::from_bytes("sample.bin", bytes)]);

        window
            .update(cx, |view, window, cx| {
                view.goto(&Goto, window, cx);
                view.goto_input = "0x80".to_string();
                view.confirm_goto(cx);
                assert_eq!(view.cursor, 0x80);
                assert_eq!(
                    view.left_scroll_handle
                        .0
                        .borrow()
                        .deferred_scroll_to_item
                        .unwrap()
                        .item_index,
                    8
                );

                cx.write_to_clipboard(ClipboardItem::new_string("FA 00 01".to_string()));
                view.find_next(&FindNext, window, cx);
                assert_eq!(view.selection_range(), Some(250..253));
                assert_eq!(
                    view.left_scroll_handle
                        .0
                        .borrow()
                        .deferred_scroll_to_item
                        .unwrap()
                        .item_index,
                    15
                );

                view.goto(&Goto, window, cx);
                view.goto_input = "9999".to_string();
                view.confirm_goto(cx);
                assert_eq!(view.cursor, 512);
            })
            .unwrap();
    }

    #[gpui::test]
    fn split_panes_can_focus_and_move_open_files(cx: &mut TestAppContext) {
        let window = open_test_window(
            cx,
            vec![
                ByteDocument::from_bytes("left.bin", b"left".to_vec()),
                ByteDocument::from_bytes("right.bin", b"right".to_vec()),
            ],
        );

        window
            .update(cx, |view, window, cx| {
                view.toggle_split(&ToggleSplit, window, cx);
                assert!(view.split);
                assert_eq!(view.active_ix_for(PaneSide::Left), Some(0));
                assert_eq!(view.active_ix_for(PaneSide::Right), Some(1));

                view.focus_right_pane(&FocusRightPane, window, cx);
                assert_eq!(view.focused_pane, PaneSide::Right);
                assert_eq!(view.active_doc().unwrap().name(), "right.bin");

                view.activate_doc(0, cx);
                assert_eq!(view.active_ix_for(PaneSide::Right), Some(0));

                view.move_to_other_split(&MoveToOtherSplit, window, cx);
                assert_eq!(view.focused_pane, PaneSide::Left);
                assert_eq!(view.active_ix_for(PaneSide::Left), Some(0));
            })
            .unwrap();

        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-2").unwrap());
        window
            .update(cx, |view, _, _| {
                assert_eq!(view.focused_pane, PaneSide::Right);
            })
            .unwrap();

        cx.dispatch_keystroke(*window, Keystroke::parse("secondary-1").unwrap());
        window
            .update(cx, |view, _, _| {
                assert_eq!(view.focused_pane, PaneSide::Left);
            })
            .unwrap();
    }

    #[gpui::test]
    fn shift_click_and_drag_selection_share_ui_state_path(cx: &mut TestAppContext) {
        let window = open_test_window(
            cx,
            vec![ByteDocument::from_bytes(
                "sample.bin",
                b"0123456789".to_vec(),
            )],
        );

        window
            .update(cx, |view, _, cx| {
                view.begin_cell_selection(0, PaneSide::Left, 2, false, cx);
                view.drag_cell_selection(0, PaneSide::Left, 5, cx);
                assert_eq!(view.selection_range(), Some(2..6));
                assert_eq!(view.cursor, 5);
                view.finish_cell_selection(cx);
                assert!(view.drag_anchor.is_none());

                view.set_cursor(1, false, cx);
                view.begin_cell_selection(0, PaneSide::Left, 4, true, cx);
                assert_eq!(view.selection_range(), Some(1..5));
            })
            .unwrap();
    }

    #[gpui::test]
    fn open_paths_and_save_as_stream_current_document(cx: &mut TestAppContext) {
        let mut input = std::env::temp_dir();
        input.push(format!(
            "byteforge-open-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&input, b"abcdef").unwrap();

        let window = open_test_window(cx, Vec::new());
        let mut output = std::env::temp_dir();
        output.push(format!(
            "byteforge-save-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        window
            .update(cx, |view, _, _| {
                view.open_paths(vec![input.clone()]);
                assert_eq!(doc_bytes(view), b"abcdef");
                view.active_doc_mut()
                    .unwrap()
                    .replace_range(1..5, b"123".to_vec())
                    .unwrap();
                view.save_active_as_path(&output);
                assert!(!view.active_doc().unwrap().is_dirty());
            })
            .unwrap();

        assert_eq!(std::fs::read(&output).unwrap(), b"a123f");
        let _ = std::fs::remove_file(input);
        let _ = std::fs::remove_file(output);
    }

    #[gpui::test]
    fn dropped_paths_open_multiple_files(cx: &mut TestAppContext) {
        let mut first = std::env::temp_dir();
        first.push(format!(
            "byteforge-drop-a-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut second = first.clone();
        second.set_file_name(format!(
            "byteforge-drop-b-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&first, b"first").unwrap();
        std::fs::write(&second, b"second").unwrap();

        let window = open_test_window(cx, Vec::new());
        window
            .update(cx, |view, _, cx| {
                view.open_dropped_paths(&[first.clone(), second.clone()], cx);
                assert_eq!(view.docs.len(), 2);
                assert_eq!(view.active, 0);
                assert_eq!(view.docs[0].read_range(0, 5), b"first");
                assert_eq!(view.docs[1].read_range(0, 6), b"second");
                assert_eq!(view.status.as_ref(), "Opened 2 file(s).");
            })
            .unwrap();

        let _ = std::fs::remove_file(first);
        let _ = std::fs::remove_file(second);
    }
}

fn pane_index(side: PaneSide) -> usize {
    match side {
        PaneSide::Left => 0,
        PaneSide::Right => 1,
    }
}

fn byte_cell_id(side: PaneSide, offset: u64, lane: u64) -> usize {
    let side_offset = pane_index(side) as u64;
    usize::try_from(
        offset
            .saturating_mul(4)
            .saturating_add(side_offset.saturating_mul(2))
            .saturating_add(lane),
    )
    .unwrap_or(usize::MAX)
}

fn parse_offset(input: &str) -> Option<u64> {
    let trimmed = input.trim().replace('_', "");
    if trimmed.is_empty() {
        return None;
    }
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).ok()
    } else {
        trimmed
            .parse::<u64>()
            .ok()
            .or_else(|| u64::from_str_radix(&trimmed, 16).ok())
    }
}

pub fn run_app() {
    Application::new().run(|cx: &mut App| {
        gpui_component::init(cx);
        cx.activate(true);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([
            KeyBinding::new("secondary-o", OpenFiles, None),
            KeyBinding::new("secondary-s", SaveAs, None),
            KeyBinding::new("secondary-c", CopyHex, None),
            KeyBinding::new("secondary-shift-c", CopyText, None),
            KeyBinding::new("secondary-x", Cut, None),
            KeyBinding::new("secondary-z", Undo, None),
            KeyBinding::new("secondary-y", Redo, None),
            KeyBinding::new("secondary-v", PasteHex, None),
            KeyBinding::new("secondary-shift-v", PasteText, None),
            KeyBinding::new("delete", DeleteSelection, None),
            KeyBinding::new("backspace", DeleteSelection, None),
            KeyBinding::new("secondary-a", SelectAll, None),
            KeyBinding::new("secondary-f", FindNext, None),
            KeyBinding::new("secondary-g", Goto, None),
            KeyBinding::new("secondary-d", CompareNext, None),
            KeyBinding::new("secondary-\\", ToggleSplit, None),
            KeyBinding::new("secondary-m", MoveToOtherSplit, None),
            KeyBinding::new("secondary-1", FocusLeftPane, None),
            KeyBinding::new("secondary-2", FocusRightPane, None),
            KeyBinding::new("secondary-b", NextRowWidth, None),
            KeyBinding::new("secondary-alt-e", ToggleEndian, None),
            KeyBinding::new("secondary-alt-n", NextEncoding, None),
            KeyBinding::new("insert", ToggleInsertMode, None),
            KeyBinding::new("left", MoveLeft, None),
            KeyBinding::new("right", MoveRight, None),
            KeyBinding::new("up", MoveUp, None),
            KeyBinding::new("down", MoveDown, None),
            KeyBinding::new("shift-left", SelectLeft, None),
            KeyBinding::new("shift-right", SelectRight, None),
            KeyBinding::new("secondary-q", Quit, None),
        ]);
        cx.set_menus(vec![
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("Open Files...", OpenFiles),
                    MenuItem::action("Save As...", SaveAs),
                    MenuItem::separator(),
                    MenuItem::os_submenu("Services", SystemMenuType::Services),
                    MenuItem::separator(),
                    MenuItem::action("Quit", Quit),
                ],
            },
            Menu {
                name: "Edit".into(),
                items: vec![
                    MenuItem::action("Copy Hex", CopyHex),
                    MenuItem::action("Copy Text", CopyText),
                    MenuItem::action("Cut", Cut),
                    MenuItem::action("Undo", Undo),
                    MenuItem::action("Redo", Redo),
                    MenuItem::action("Paste Hex", PasteHex),
                    MenuItem::action("Paste Text", PasteText),
                    MenuItem::action("Delete Selection", DeleteSelection),
                    MenuItem::action("Select All", SelectAll),
                    MenuItem::action("Goto Offset", Goto),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Find Clipboard", FindNext),
                    MenuItem::action("Compare Next File", CompareNext),
                    MenuItem::action("Toggle Split View", ToggleSplit),
                    MenuItem::action("Move Active File To Other Pane", MoveToOtherSplit),
                    MenuItem::action("Focus Left Pane", FocusLeftPane),
                    MenuItem::action("Focus Right Pane", FocusRightPane),
                    MenuItem::action("Next Row Width", NextRowWidth),
                    MenuItem::action("Toggle Insert Mode", ToggleInsertMode),
                    MenuItem::action("Next Encoding", NextEncoding),
                    MenuItem::action("Toggle Endian", ToggleEndian),
                ],
            },
        ]);

        let bounds = Bounds::centered(None, size(px(1280.0), px(820.0)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("ByteForge".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |_, cx| cx.new(ByteForge::new),
            )
            .expect("failed to open main window");

        window
            .update(cx, |view, window, cx| {
                window.focus(&view.focus_handle(cx));
            })
            .ok();
    });
}
