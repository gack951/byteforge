use std::{
    cmp::{max, min},
    fs::File,
    io::{BufWriter, Write},
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use encoding_rs::{SHIFT_JIS, UTF_8, UTF_16BE, UTF_16LE};
use memmap2::Mmap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Endianness {
    Little,
    Big,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    ShiftJis,
    Ascii,
}

impl PreviewEncoding {
    pub const ALL: [Self; 5] = [
        Self::Utf8,
        Self::Utf16Le,
        Self::Utf16Be,
        Self::ShiftJis,
        Self::Ascii,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16LE",
            Self::Utf16Be => "UTF-16BE",
            Self::ShiftJis => "Shift-JIS",
            Self::Ascii => "ASCII",
        }
    }

    pub fn next(self) -> Self {
        let ix = Self::ALL.iter().position(|v| *v == self).unwrap_or(0);
        Self::ALL[(ix + 1) % Self::ALL.len()]
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Selection {
    pub anchor: u64,
    pub cursor: u64,
}

impl Selection {
    pub fn new(anchor: u64, cursor: u64) -> Self {
        Self { anchor, cursor }
    }

    pub fn normalized(&self) -> Range<u64> {
        min(self.anchor, self.cursor)..max(self.anchor, self.cursor) + 1
    }
}

#[derive(Clone, Debug)]
enum Source {
    Original,
    Add(usize),
}

#[derive(Clone, Debug)]
struct Piece {
    source: Source,
    start: u64,
    len: u64,
}

impl Piece {
    fn end(&self) -> u64 {
        self.start + self.len
    }
}

enum OriginalBytes {
    Mmap(Arc<Mmap>),
    Memory(Arc<Vec<u8>>),
    Empty,
}

impl OriginalBytes {
    fn slice(&self, range: Range<u64>) -> &[u8] {
        let start = range.start as usize;
        let end = range.end as usize;
        match self {
            Self::Mmap(mmap) => &mmap[start..end],
            Self::Memory(bytes) => &bytes[start..end],
            Self::Empty => &[],
        }
    }
}

#[derive(Clone)]
struct DocumentSnapshot {
    pieces: Vec<Piece>,
    len: u64,
    dirty: bool,
}

pub struct ByteDocument {
    path: Option<PathBuf>,
    original: OriginalBytes,
    additions: Vec<Arc<Vec<u8>>>,
    pieces: Vec<Piece>,
    len: u64,
    dirty: bool,
    undo_stack: Vec<DocumentSnapshot>,
    redo_stack: Vec<DocumentSnapshot>,
}

impl ByteDocument {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file =
            File::open(&path).with_context(|| format!("failed to open {}", path.display()))?;
        let metadata = file.metadata()?;
        if metadata.len() == 0 {
            return Ok(Self {
                path: Some(path),
                original: OriginalBytes::Empty,
                additions: Vec::new(),
                pieces: Vec::new(),
                len: 0,
                dirty: false,
                undo_stack: Vec::new(),
                redo_stack: Vec::new(),
            });
        }

        let mmap = unsafe { Mmap::map(&file) }
            .with_context(|| format!("failed to memory-map {}", path.display()))?;
        let len = mmap.len() as u64;
        Ok(Self {
            path: Some(path),
            original: OriginalBytes::Mmap(Arc::new(mmap)),
            additions: Vec::new(),
            pieces: vec![Piece {
                source: Source::Original,
                start: 0,
                len,
            }],
            len,
            dirty: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        })
    }

    pub fn from_bytes(name: impl Into<String>, bytes: Vec<u8>) -> Self {
        let len = bytes.len() as u64;
        Self {
            path: Some(PathBuf::from(name.into())),
            original: OriginalBytes::Memory(Arc::new(bytes)),
            additions: Vec::new(),
            pieces: if len == 0 {
                Vec::new()
            } else {
                vec![Piece {
                    source: Source::Original,
                    start: 0,
                    len,
                }]
            },
            len,
            dirty: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    pub fn name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled".to_string())
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn byte_at(&self, offset: u64) -> Option<u8> {
        self.read_range(offset, 1).first().copied()
    }

    pub fn read_range(&self, offset: u64, len: usize) -> Vec<u8> {
        if offset >= self.len || len == 0 {
            return Vec::new();
        }

        let wanted = offset..min(self.len, offset + len as u64);
        let mut logical = 0;
        let mut out = Vec::with_capacity((wanted.end - wanted.start) as usize);

        for piece in &self.pieces {
            let piece_logical = logical..logical + piece.len;
            logical += piece.len;
            let overlap =
                max(wanted.start, piece_logical.start)..min(wanted.end, piece_logical.end);
            if overlap.start >= overlap.end {
                continue;
            }

            let source_start = piece.start + (overlap.start - piece_logical.start);
            let source_end = source_start + (overlap.end - overlap.start);
            out.extend_from_slice(self.source_slice(&piece.source, source_start..source_end));
        }

        out
    }

    pub fn insert(&mut self, offset: u64, bytes: Vec<u8>) -> Result<()> {
        self.replace_range(offset..offset, bytes)
    }

    pub fn delete(&mut self, range: Range<u64>) -> Result<()> {
        self.replace_range(range, Vec::new())
    }

    pub fn overwrite(&mut self, offset: u64, bytes: Vec<u8>) -> Result<()> {
        if offset > self.len {
            bail!(
                "overwrite offset {} is past document length {}",
                offset,
                self.len
            );
        }
        let end = min(self.len, offset + bytes.len() as u64);
        self.replace_range(offset..end, bytes)
    }

    pub fn replace_range(&mut self, range: Range<u64>, bytes: Vec<u8>) -> Result<()> {
        if range.start > range.end || range.end > self.len {
            bail!(
                "replace range {:?} is outside document length {}",
                range,
                self.len
            );
        }
        if range.is_empty() && bytes.is_empty() {
            return Ok(());
        }

        self.push_undo_snapshot();
        if !range.is_empty() {
            self.delete_inner(range.clone())?;
        }
        if !bytes.is_empty() {
            self.insert_inner(range.start, bytes)?;
        }
        self.redo_stack.clear();
        self.dirty = true;
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo_stack.pop() else {
            return false;
        };
        let current = self.snapshot();
        self.restore(snapshot);
        self.redo_stack.push(current);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(snapshot) = self.redo_stack.pop() else {
            return false;
        };
        let current = self.snapshot();
        self.restore(snapshot);
        self.undo_stack.push(current);
        true
    }

    fn push_undo_snapshot(&mut self) {
        self.undo_stack.push(self.snapshot());
    }

    fn snapshot(&self) -> DocumentSnapshot {
        DocumentSnapshot {
            pieces: self.pieces.clone(),
            len: self.len,
            dirty: self.dirty,
        }
    }

    fn restore(&mut self, snapshot: DocumentSnapshot) {
        self.pieces = snapshot.pieces;
        self.len = snapshot.len;
        self.dirty = snapshot.dirty;
    }

    fn insert_inner(&mut self, offset: u64, bytes: Vec<u8>) -> Result<()> {
        if offset > self.len {
            bail!(
                "insert offset {} is past document length {}",
                offset,
                self.len
            );
        }
        if bytes.is_empty() {
            return Ok(());
        }

        let len = bytes.len() as u64;
        let add_ix = self.additions.len();
        self.additions.push(Arc::new(bytes));
        let inserted = Piece {
            source: Source::Add(add_ix),
            start: 0,
            len,
        };

        let mut next = Vec::with_capacity(self.pieces.len() + 1);
        let mut logical = 0;
        let mut inserted_done = false;

        for piece in &self.pieces {
            if !inserted_done && offset <= logical {
                next.push(inserted.clone());
                inserted_done = true;
            }

            let piece_end = logical + piece.len;
            if !inserted_done && offset > logical && offset < piece_end {
                let left_len = offset - logical;
                next.push(Piece {
                    source: piece.source.clone(),
                    start: piece.start,
                    len: left_len,
                });
                next.push(inserted.clone());
                next.push(Piece {
                    source: piece.source.clone(),
                    start: piece.start + left_len,
                    len: piece.len - left_len,
                });
                inserted_done = true;
            } else {
                next.push(piece.clone());
            }
            logical = piece_end;
        }

        if !inserted_done {
            next.push(inserted);
        }

        self.pieces = compact_pieces(next);
        self.len += len;
        Ok(())
    }

    fn delete_inner(&mut self, range: Range<u64>) -> Result<()> {
        if range.start > range.end || range.end > self.len {
            bail!(
                "delete range {:?} is outside document length {}",
                range,
                self.len
            );
        }
        if range.is_empty() {
            return Ok(());
        }

        let mut next = Vec::with_capacity(self.pieces.len());
        let mut logical = 0;
        for piece in &self.pieces {
            let piece_logical = logical..logical + piece.len;
            logical = piece_logical.end;

            if piece_logical.end <= range.start || piece_logical.start >= range.end {
                next.push(piece.clone());
                continue;
            }

            if range.start > piece_logical.start {
                next.push(Piece {
                    source: piece.source.clone(),
                    start: piece.start,
                    len: range.start - piece_logical.start,
                });
            }

            if range.end < piece_logical.end {
                let keep_start = range.end - piece_logical.start;
                next.push(Piece {
                    source: piece.source.clone(),
                    start: piece.start + keep_start,
                    len: piece_logical.end - range.end,
                });
            }
        }

        self.len -= range.end - range.start;
        self.pieces = compact_pieces(next);
        Ok(())
    }

    pub fn save_as(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let mut writer = BufWriter::new(
            File::create(path).with_context(|| format!("failed to create {}", path.display()))?,
        );
        for piece in &self.pieces {
            writer.write_all(self.source_slice(&piece.source, piece.start..piece.end()))?;
        }
        writer.flush()?;
        self.path = Some(path.to_path_buf());
        self.dirty = false;
        self.undo_stack.clear();
        self.redo_stack.clear();
        Ok(())
    }

    fn source_slice(&self, source: &Source, range: Range<u64>) -> &[u8] {
        match source {
            Source::Original => self.original.slice(range),
            Source::Add(ix) => {
                let bytes = &self.additions[*ix];
                &bytes[range.start as usize..range.end as usize]
            }
        }
    }
}

fn compact_pieces(pieces: Vec<Piece>) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::with_capacity(pieces.len());
    for piece in pieces.into_iter().filter(|p| p.len > 0) {
        if let Some(last) = out.last_mut() {
            let same_source = match (&last.source, &piece.source) {
                (Source::Original, Source::Original) => true,
                (Source::Add(a), Source::Add(b)) => a == b,
                _ => false,
            };
            if same_source && last.end() == piece.start {
                last.len += piece.len;
                continue;
            }
        }
        out.push(piece);
    }
    out
}

#[derive(Debug, Clone)]
pub struct InspectorValue {
    pub label: &'static str,
    pub value: String,
}

pub fn inspector_values(
    bytes: &[u8],
    offset: u64,
    endian: Endianness,
    encoding: PreviewEncoding,
) -> Vec<InspectorValue> {
    let mut out = Vec::new();
    out.push(InspectorValue {
        label: "Offset",
        value: format!("0x{offset:016X} / {offset}"),
    });
    if let Some(byte) = bytes.first() {
        out.push(InspectorValue {
            label: "u8",
            value: format!("{byte} / 0x{byte:02X} / {:08b}", byte),
        });
        out.push(InspectorValue {
            label: "i8",
            value: (*byte as i8).to_string(),
        });
    }

    macro_rules! push_int {
        ($name:literal, $signed:ty, $unsigned:ty, $size:literal) => {
            if bytes.len() >= $size {
                let mut buf = [0u8; $size];
                buf.copy_from_slice(&bytes[..$size]);
                let unsigned = match endian {
                    Endianness::Little => <$unsigned>::from_le_bytes(buf),
                    Endianness::Big => <$unsigned>::from_be_bytes(buf),
                };
                let signed = match endian {
                    Endianness::Little => <$signed>::from_le_bytes(buf),
                    Endianness::Big => <$signed>::from_be_bytes(buf),
                };
                out.push(InspectorValue {
                    label: concat!($name, " unsigned"),
                    value: format!("{} / 0x{:X}", unsigned, unsigned),
                });
                out.push(InspectorValue {
                    label: concat!($name, " signed"),
                    value: signed.to_string(),
                });
            }
        };
    }

    push_int!("16-bit", i16, u16, 2);
    push_int!("32-bit", i32, u32, 4);
    push_int!("64-bit", i64, u64, 8);

    if bytes.len() >= 4 {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(&bytes[..4]);
        let bits = match endian {
            Endianness::Little => u32::from_le_bytes(buf),
            Endianness::Big => u32::from_be_bytes(buf),
        };
        out.push(InspectorValue {
            label: "f32",
            value: f32::from_bits(bits).to_string(),
        });
    }

    if bytes.len() >= 8 {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&bytes[..8]);
        let bits = match endian {
            Endianness::Little => u64::from_le_bytes(buf),
            Endianness::Big => u64::from_be_bytes(buf),
        };
        out.push(InspectorValue {
            label: "f64",
            value: f64::from_bits(bits).to_string(),
        });
    }

    out.push(InspectorValue {
        label: encoding.label(),
        value: decode_preview(bytes, encoding),
    });

    out
}

pub fn decode_preview(bytes: &[u8], encoding: PreviewEncoding) -> String {
    let take = min(bytes.len(), 64);
    let bytes = &bytes[..take];
    match encoding {
        PreviewEncoding::Ascii => bytes
            .iter()
            .map(|b| {
                if (0x20..=0x7e).contains(b) {
                    *b as char
                } else {
                    '.'
                }
            })
            .collect(),
        PreviewEncoding::Utf8 => decode_with(UTF_8, bytes),
        PreviewEncoding::Utf16Le => decode_with(UTF_16LE, bytes),
        PreviewEncoding::Utf16Be => decode_with(UTF_16BE, bytes),
        PreviewEncoding::ShiftJis => decode_with(SHIFT_JIS, bytes),
    }
}

fn decode_with(encoding: &'static encoding_rs::Encoding, bytes: &[u8]) -> String {
    let (cow, _, _) = encoding.decode(bytes);
    cow.chars()
        .map(|ch| {
            if ch.is_control() && ch != '\n' && ch != '\t' {
                '.'
            } else {
                ch
            }
        })
        .collect()
}

pub fn parse_hex_bytes(input: &str) -> Result<Vec<u8>> {
    let compact: String = input.chars().filter(|ch| !ch.is_whitespace()).collect();
    if compact.len() % 2 != 0 {
        bail!("hex input must contain an even number of digits");
    }
    let mut out = Vec::with_capacity(compact.len() / 2);
    for chunk in compact.as_bytes().chunks(2) {
        let s = std::str::from_utf8(chunk)?;
        out.push(u8::from_str_radix(s, 16).with_context(|| format!("invalid hex byte {s}"))?);
    }
    Ok(out)
}

pub fn find_bytes(haystack: &ByteDocument, needle: &[u8], start: u64) -> Option<u64> {
    if needle.is_empty() || start >= haystack.len() {
        return None;
    }

    let mut offset = start;
    let window_len = max(needle.len() * 2, 1024 * 1024);
    let overlap = needle.len().saturating_sub(1) as u64;

    while offset < haystack.len() {
        let bytes = haystack.read_range(offset, window_len);
        if let Some(pos) = bytes.windows(needle.len()).position(|w| w == needle) {
            return Some(offset + pos as u64);
        }
        if bytes.len() < window_len {
            return None;
        }
        offset += bytes.len() as u64 - overlap;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn piece_table_insert_delete_overwrite() {
        let mut doc = ByteDocument::from_bytes("sample.bin", b"abcdef".to_vec());
        doc.insert(3, b"XYZ".to_vec()).unwrap();
        assert_eq!(doc.read_range(0, 16), b"abcXYZdef");

        doc.delete(2..7).unwrap();
        assert_eq!(doc.read_range(0, 16), b"abef");

        doc.overwrite(1, b"123".to_vec()).unwrap();
        assert_eq!(doc.read_range(0, 16), b"a123");
    }

    #[test]
    fn undo_redo_tracks_user_level_edits() {
        let mut doc = ByteDocument::from_bytes("sample.bin", b"abcdef".to_vec());
        doc.overwrite(2, b"XYZ".to_vec()).unwrap();
        assert_eq!(doc.read_range(0, 16), b"abXYZf");
        assert!(doc.can_undo());
        assert!(!doc.can_redo());

        assert!(doc.undo());
        assert_eq!(doc.read_range(0, 16), b"abcdef");
        assert!(!doc.is_dirty());
        assert!(!doc.can_undo());
        assert!(doc.can_redo());

        assert!(doc.redo());
        assert_eq!(doc.read_range(0, 16), b"abXYZf");
        assert!(doc.is_dirty());
    }

    #[test]
    fn replace_range_is_one_undo_step() {
        let mut doc = ByteDocument::from_bytes("sample.bin", b"abcdef".to_vec());
        doc.replace_range(1..5, b"123456".to_vec()).unwrap();
        assert_eq!(doc.read_range(0, 16), b"a123456f");
        assert!(doc.undo());
        assert_eq!(doc.read_range(0, 16), b"abcdef");
    }

    #[test]
    fn hex_parser_ignores_spaces() {
        assert_eq!(
            parse_hex_bytes("DE AD be ef").unwrap(),
            [0xde, 0xad, 0xbe, 0xef]
        );
        assert!(parse_hex_bytes("ABC").is_err());
    }

    #[test]
    fn finds_across_virtual_document() {
        let mut doc = ByteDocument::from_bytes("sample.bin", b"abc___ghi".to_vec());
        doc.overwrite(3, b"def".to_vec()).unwrap();
        assert_eq!(find_bytes(&doc, b"defg", 0), Some(3));
    }

    #[test]
    fn large_document_viewport_edit_and_search_are_bounded() {
        let mut bytes = vec![0u8; 32 * 1024 * 1024];
        bytes[16 * 1024 * 1024..16 * 1024 * 1024 + 4].copy_from_slice(b"NEED");
        let mut doc = ByteDocument::from_bytes("large.bin", bytes);

        let started = std::time::Instant::now();
        assert_eq!(doc.read_range(16 * 1024 * 1024, 64)[..4], *b"NEED");
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "viewport read took {:?}",
            started.elapsed()
        );

        let started = std::time::Instant::now();
        doc.insert(10, vec![1, 2, 3, 4]).unwrap();
        doc.delete(20..24).unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(250),
            "piece edits took {:?}",
            started.elapsed()
        );

        let started = std::time::Instant::now();
        assert_eq!(find_bytes(&doc, b"NEED", 0), Some(16 * 1024 * 1024));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "linear search took {:?}",
            started.elapsed()
        );
    }
}
