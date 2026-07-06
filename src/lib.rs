pub mod core;

use std::{
    env,
    ops::Range,
    path::{Path, PathBuf},
};

use anyhow::Result;
use core::{
    ByteDocument, Endianness, PreviewEncoding, Selection, find_bytes, inspector_values,
    parse_hex_bytes,
};
use gpui::{
    AnyElement, App, Application, Bounds, ClickEvent, ClipboardItem, Context, ElementId,
    FocusHandle, Focusable, KeyBinding, KeyDownEvent, Menu, MenuItem, SharedString, SystemMenuType,
    Window, WindowBounds, WindowOptions, actions, div, prelude::*, px, rgb, rgba, size,
    uniform_list,
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

struct ByteForge {
    docs: Vec<ByteDocument>,
    active: usize,
    compare_with: Option<usize>,
    cursor: u64,
    selection: Option<Selection>,
    bytes_per_row_ix: usize,
    endian: Endianness,
    encoding: PreviewEncoding,
    edit_mode: EditMode,
    pending_hex: Option<u8>,
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
            cursor: 0,
            selection: None,
            bytes_per_row_ix: 1,
            endian: Endianness::Little,
            encoding: PreviewEncoding::Utf8,
            edit_mode: EditMode::Overwrite,
            pending_hex: None,
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
            cursor: 0,
            selection: None,
            bytes_per_row_ix: 1,
            endian: Endianness::Little,
            encoding: PreviewEncoding::Utf8,
            edit_mode: EditMode::Overwrite,
            pending_hex: None,
            status: "Ready.".into(),
            focus_handle: cx.focus_handle(),
        }
    }

    fn bytes_per_row(&self) -> usize {
        BYTES_PER_ROW_OPTIONS[self.bytes_per_row_ix]
    }

    fn active_doc(&self) -> Option<&ByteDocument> {
        self.docs.get(self.active)
    }

    fn active_doc_mut(&mut self) -> Option<&mut ByteDocument> {
        self.docs.get_mut(self.active)
    }

    fn clamp_cursor(&mut self) {
        let len = self.active_doc().map(ByteDocument::len).unwrap_or(0);
        self.cursor = self.cursor.min(len.saturating_sub(1));
    }

    fn set_status(&mut self, text: impl Into<SharedString>) {
        self.status = text.into();
    }

    fn set_cursor(&mut self, offset: u64, extend: bool, cx: &mut Context<Self>) {
        let len = self.active_doc().map(ByteDocument::len).unwrap_or(0);
        if len == 0 {
            self.cursor = 0;
            self.selection = None;
            cx.notify();
            return;
        }

        let offset = offset.min(len - 1);
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
        cx.notify();
    }

    fn selection_range(&self) -> Option<Range<u64>> {
        self.selection.as_ref().map(Selection::normalized)
    }

    fn selected_or_cursor_range(&self) -> Option<Range<u64>> {
        if let Some(range) = self.selection_range() {
            Some(range)
        } else if self.active_doc().is_some_and(|doc| !doc.is_empty()) {
            Some(self.cursor..self.cursor + 1)
        } else {
            None
        }
    }

    fn open_files(&mut self, _: &OpenFiles, _: &mut Window, cx: &mut Context<Self>) {
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
            self.active = self.docs.len() - opened;
            self.cursor = 0;
            self.selection = None;
            self.compare_with = None;
            self.set_status(format!("Opened {opened} file(s)."));
        } else if let Some(err) = last_error {
            self.set_status(err);
        }
    }

    fn save_as(&mut self, _: &SaveAs, _: &mut Window, cx: &mut Context<Self>) {
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
        let Some(range) = self.selection_range() else {
            self.set_status("No selection to delete.");
            cx.notify();
            return;
        };
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
        self.compare_with = Some(if next == self.active {
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
            return;
        }
        let next = if delta.is_negative() {
            self.cursor.saturating_sub(delta.unsigned_abs())
        } else {
            self.cursor.saturating_add(delta as u64).min(doc.len() - 1)
        };
        self.set_cursor(next, extend, cx);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
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
            self.active = ix;
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
                cx.listener(|this, _, window, cx| this.open_files(&OpenFiles, window, cx)),
            ))
            .child(self.toolbar_button(
                "save-as",
                "Save As",
                cx.listener(|this, _, window, cx| this.save_as(&SaveAs, window, cx)),
            ))
            .child(self.toolbar_button(
                "copy-hex",
                "Copy Hex",
                cx.listener(|this, _, window, cx| this.copy_hex(&CopyHex, window, cx)),
            ))
            .child(self.toolbar_button(
                "copy-text",
                "Copy Text",
                cx.listener(|this, _, window, cx| this.copy_text(&CopyText, window, cx)),
            ))
            .child(self.toolbar_button(
                "undo",
                "Undo",
                cx.listener(|this, _, window, cx| this.undo(&Undo, window, cx)),
            ))
            .child(self.toolbar_button(
                "redo",
                "Redo",
                cx.listener(|this, _, window, cx| this.redo(&Redo, window, cx)),
            ))
            .child(self.toolbar_button(
                "paste-hex",
                "Paste Hex",
                cx.listener(|this, _, window, cx| this.paste_hex(&PasteHex, window, cx)),
            ))
            .child(self.toolbar_button(
                "paste-text",
                "Paste Text",
                cx.listener(|this, _, window, cx| this.paste_text(&PasteText, window, cx)),
            ))
            .child(self.toolbar_button(
                "delete",
                "Delete",
                cx.listener(|this, _, window, cx| {
                    this.delete_selection(&DeleteSelection, window, cx)
                }),
            ))
            .child(self.toolbar_button(
                "find-clip",
                "Find Clip",
                cx.listener(|this, _, window, cx| this.find_next(&FindNext, window, cx)),
            ))
            .child(self.toolbar_button(
                "compare",
                "Compare",
                cx.listener(|this, _, window, cx| this.compare_next(&CompareNext, window, cx)),
            ))
            .child(self.toolbar_button(
                "row-width",
                format!("{} B/row", self.bytes_per_row()),
                cx.listener(|this, _, window, cx| this.next_row_width(&NextRowWidth, window, cx)),
            ))
            .child(self.toolbar_button(
                "edit-mode",
                self.edit_mode.label(),
                cx.listener(|this, _, window, cx| {
                    this.toggle_insert_mode(&ToggleInsertMode, window, cx)
                }),
            ))
            .child(self.toolbar_button(
                "endian",
                match self.endian {
                    Endianness::Little => "Little",
                    Endianness::Big => "Big",
                },
                cx.listener(|this, _, window, cx| this.toggle_endian(&ToggleEndian, window, cx)),
            ))
            .child(self.toolbar_button(
                "encoding",
                self.encoding.label(),
                cx.listener(|this, _, window, cx| this.next_encoding(&NextEncoding, window, cx)),
            ))
    }

    fn toolbar_button(
        &self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        div()
            .id(id)
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(rgb(0x303741))
            .text_color(rgb(0xe9eef5))
            .text_sm()
            .cursor_pointer()
            .hover(|style| style.bg(rgb(0x3e4855)))
            .child(label.into())
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
        let Some(doc) = self.active_doc() else {
            return div()
                .flex_1()
                .items_center()
                .justify_center()
                .bg(rgb(0x101318))
                .text_color(rgb(0x8792a2))
                .child("Open files with the toolbar, menu, or Ctrl+O.");
        };

        let bytes_per_row = self.bytes_per_row();
        let row_count = doc
            .len()
            .div_ceil(bytes_per_row as u64)
            .min(usize::MAX as u64) as usize;
        div().flex_1().bg(rgb(0x101318)).child(
            uniform_list(
                "hex-rows",
                row_count,
                cx.processor(move |this, range: Range<usize>, _window, cx| {
                    let mut rows = Vec::with_capacity(range.end - range.start);
                    for row in range {
                        rows.push(this.render_hex_row(row, cx));
                    }
                    rows
                }),
            )
            .h_full(),
        )
    }

    fn render_hex_row(&self, row: usize, cx: &mut Context<Self>) -> AnyElement {
        let bytes_per_row = self.bytes_per_row();
        let offset = row as u64 * bytes_per_row as u64;
        let bytes = self
            .active_doc()
            .map(|doc| doc.read_range(offset, bytes_per_row))
            .unwrap_or_default();

        let mut byte_cells = Vec::with_capacity(bytes_per_row);
        for ix in 0..bytes_per_row {
            let byte_offset = offset + ix as u64;
            let label = bytes
                .get(ix)
                .map(|byte| format!("{byte:02X}"))
                .unwrap_or_else(|| "  ".to_string());
            byte_cells.push(self.render_byte_cell(byte_offset, label, cx));
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
                .unwrap_or(' ');
            ascii_cells.push(self.render_ascii_cell(byte_offset, ch, cx));
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

    fn render_byte_cell(&self, offset: u64, label: String, cx: &mut Context<Self>) -> AnyElement {
        let exists = self.active_doc().is_some_and(|doc| offset < doc.len());
        let selected = self
            .selection_range()
            .is_some_and(|range| range.start <= offset && offset < range.end);
        let cursor = offset == self.cursor && exists;
        let different = self.is_different(offset);

        let mut cell = div()
            .id(byte_cell_id(offset, 0))
            .w(px(28.0))
            .h(px(22.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_sm()
            .cursor_pointer()
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| this.set_cursor(offset, false, cx)));

        cell = if selected {
            cell.bg(rgb(0x2e6fb8)).text_color(rgb(0xffffff))
        } else if cursor {
            cell.bg(rgb(0xd7a94a)).text_color(rgb(0x101318))
        } else if different {
            cell.bg(rgba(0xff5c5c40))
        } else if exists {
            cell.bg(rgb(0x171c22))
        } else {
            cell.text_color(rgb(0x3c4654))
        };
        cell.into_any_element()
    }

    fn render_ascii_cell(&self, offset: u64, ch: char, cx: &mut Context<Self>) -> AnyElement {
        let exists = self.active_doc().is_some_and(|doc| offset < doc.len());
        let selected = self
            .selection_range()
            .is_some_and(|range| range.start <= offset && offset < range.end);
        let mut cell = div()
            .id(byte_cell_id(offset, 1))
            .w(px(14.0))
            .h(px(22.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .child(ch.to_string())
            .on_click(cx.listener(move |this, _, _, cx| this.set_cursor(offset, false, cx)));
        cell = if selected {
            cell.bg(rgb(0x2e6fb8)).text_color(rgb(0xffffff))
        } else if exists {
            cell.text_color(rgb(0xb8c1cf))
        } else {
            cell.text_color(rgb(0x3c4654))
        };
        cell.into_any_element()
    }

    fn is_different(&self, offset: u64) -> bool {
        let Some(compare_ix) = self.compare_with else {
            return false;
        };
        let Some(left) = self.active_doc().and_then(|doc| doc.byte_at(offset)) else {
            return false;
        };
        self.docs
            .get(compare_ix)
            .and_then(|doc| doc.byte_at(offset))
            .is_none_or(|right| right != left)
    }

    fn render_inspector(&self) -> impl IntoElement {
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
            .w(px(300.0))
            .h_full()
            .bg(rgb(0x15181d))
            .border_l_1()
            .border_color(rgb(0x303741))
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
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

    fn render_status(&self) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .bg(rgb(0x20242a))
            .border_t_1()
            .border_color(rgb(0x303741))
            .text_color(rgb(0xb8c1cf))
            .text_sm()
            .child(self.status.clone())
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
            .child(self.render_status())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Keystroke, TestAppContext, WindowHandle};

    fn bind_test_keys(cx: &mut App) {
        cx.bind_keys([
            KeyBinding::new("secondary-c", CopyHex, Some("ByteForge")),
            KeyBinding::new("secondary-shift-c", CopyText, Some("ByteForge")),
            KeyBinding::new("secondary-x", Cut, Some("ByteForge")),
            KeyBinding::new("secondary-z", Undo, Some("ByteForge")),
            KeyBinding::new("secondary-shift-z", Redo, Some("ByteForge")),
            KeyBinding::new("secondary-v", PasteHex, Some("ByteForge")),
            KeyBinding::new("secondary-shift-v", PasteText, Some("ByteForge")),
            KeyBinding::new("delete", DeleteSelection, Some("ByteForge")),
            KeyBinding::new("secondary-a", SelectAll, Some("ByteForge")),
            KeyBinding::new("secondary-f", FindNext, Some("ByteForge")),
            KeyBinding::new("secondary-d", CompareNext, Some("ByteForge")),
            KeyBinding::new("secondary-b", NextRowWidth, Some("ByteForge")),
            KeyBinding::new("insert", ToggleInsertMode, Some("ByteForge")),
            KeyBinding::new("left", MoveLeft, Some("ByteForge")),
            KeyBinding::new("right", MoveRight, Some("ByteForge")),
            KeyBinding::new("up", MoveUp, Some("ByteForge")),
            KeyBinding::new("down", MoveDown, Some("ByteForge")),
            KeyBinding::new("shift-left", SelectLeft, Some("ByteForge")),
            KeyBinding::new("shift-right", SelectRight, Some("ByteForge")),
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
}

fn byte_cell_id(offset: u64, lane: u64) -> usize {
    usize::try_from(offset.saturating_mul(2).saturating_add(lane)).unwrap_or(usize::MAX)
}

pub fn run_app() {
    Application::new().run(|cx: &mut App| {
        gpui_component::init(cx);
        cx.activate(true);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([
            KeyBinding::new("secondary-o", OpenFiles, Some("ByteForge")),
            KeyBinding::new("secondary-s", SaveAs, Some("ByteForge")),
            KeyBinding::new("secondary-c", CopyHex, Some("ByteForge")),
            KeyBinding::new("secondary-shift-c", CopyText, Some("ByteForge")),
            KeyBinding::new("secondary-x", Cut, Some("ByteForge")),
            KeyBinding::new("secondary-z", Undo, Some("ByteForge")),
            KeyBinding::new("secondary-shift-z", Redo, Some("ByteForge")),
            KeyBinding::new("secondary-v", PasteHex, Some("ByteForge")),
            KeyBinding::new("secondary-shift-v", PasteText, Some("ByteForge")),
            KeyBinding::new("delete", DeleteSelection, Some("ByteForge")),
            KeyBinding::new("backspace", DeleteSelection, Some("ByteForge")),
            KeyBinding::new("secondary-a", SelectAll, Some("ByteForge")),
            KeyBinding::new("secondary-f", FindNext, Some("ByteForge")),
            KeyBinding::new("secondary-d", CompareNext, Some("ByteForge")),
            KeyBinding::new("secondary-b", NextRowWidth, Some("ByteForge")),
            KeyBinding::new("insert", ToggleInsertMode, Some("ByteForge")),
            KeyBinding::new("left", MoveLeft, Some("ByteForge")),
            KeyBinding::new("right", MoveRight, Some("ByteForge")),
            KeyBinding::new("up", MoveUp, Some("ByteForge")),
            KeyBinding::new("down", MoveDown, Some("ByteForge")),
            KeyBinding::new("shift-left", SelectLeft, Some("ByteForge")),
            KeyBinding::new("shift-right", SelectRight, Some("ByteForge")),
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
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Find Clipboard", FindNext),
                    MenuItem::action("Compare Next File", CompareNext),
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
