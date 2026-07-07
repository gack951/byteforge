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
        let saving_current_mmap = self.path.as_deref().is_some_and(|current| current == path)
            && matches!(self.original, OriginalBytes::Mmap(_));

        if saving_current_mmap {
            let bytes = self.read_range(0, self.len as usize);
            self.original = if bytes.is_empty() {
                OriginalBytes::Empty
            } else {
                OriginalBytes::Memory(Arc::new(bytes.clone()))
            };
            self.additions.clear();
            self.pieces = if self.len == 0 {
                Vec::new()
            } else {
                vec![Piece {
                    source: Source::Original,
                    start: 0,
                    len: self.len,
                }]
            };

            let mut writer = BufWriter::new(
                File::create(path)
                    .with_context(|| format!("failed to create {}", path.display()))?,
            );
            writer.write_all(&bytes)?;
            writer.flush()?;
        } else {
            let mut writer = BufWriter::new(
                File::create(path)
                    .with_context(|| format!("failed to create {}", path.display()))?,
            );
            for piece in &self.pieces {
                writer.write_all(self.source_slice(&piece.source, piece.start..piece.end()))?;
            }
            writer.flush()?;
        }

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

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FormatField {
    pub offset: u64,
    pub len: u64,
    pub name: String,
    pub meaning: String,
    pub value: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FormatSummary {
    pub format: String,
    pub fields: Vec<FormatField>,
}

impl FormatField {
    pub fn contains(&self, offset: u64) -> bool {
        self.offset <= offset && offset < self.offset + self.len
    }
}

pub fn detect_format_fields(doc: &ByteDocument, max_fields: usize) -> Option<FormatSummary> {
    if max_fields == 0 || doc.is_empty() {
        return None;
    }

    let header = doc.read_range(0, 16);
    if header.starts_with(b"\x89PNG\r\n\x1A\n") {
        return Some(parse_png_fields(doc, max_fields));
    }
    if header.starts_with(b"BM") {
        return Some(parse_bmp_fields(doc, max_fields));
    }
    if header.len() >= 12 && &header[0..4] == b"RIFF" && &header[8..12] == b"WAVE" {
        return Some(parse_wav_fields(doc, max_fields));
    }
    if header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a") {
        return Some(parse_gif_fields(doc, max_fields));
    }
    if header.starts_with(b"\xFF\xD8") {
        return Some(parse_jpeg_fields(doc, max_fields));
    }
    if header.starts_with(b"PK\x03\x04") {
        return Some(parse_zip_fields(doc, max_fields));
    }

    None
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

fn parse_png_fields(doc: &ByteDocument, max_fields: usize) -> FormatSummary {
    let mut fields = Vec::new();
    push_field(
        &mut fields,
        max_fields,
        0,
        8,
        "PNG signature",
        "PNGファイル識別子",
        "89 50 4E 47 0D 0A 1A 0A",
    );

    let mut offset = 8;
    while offset + 12 <= doc.len() && fields.len() < max_fields {
        let Some(length) = read_u32_be(doc, offset) else {
            break;
        };
        let chunk_type = read_ascii(doc, offset + 4, 4).unwrap_or_else(|| "????".to_string());
        let data_offset = offset + 8;
        let crc_offset = data_offset + length as u64;
        if crc_offset + 4 > doc.len() {
            push_field(
                &mut fields,
                max_fields,
                offset,
                doc.len().saturating_sub(offset),
                "Truncated chunk",
                "途中で終わっているPNGチャンク",
                chunk_type,
            );
            break;
        }

        push_field(
            &mut fields,
            max_fields,
            offset,
            4,
            format!("{chunk_type} length"),
            "チャンクデータ長",
            format!("{} bytes", length),
        );
        push_field(
            &mut fields,
            max_fields,
            offset + 4,
            4,
            format!("{chunk_type} type"),
            "チャンク種別",
            png_chunk_meaning(&chunk_type),
        );

        if chunk_type == "IHDR" && length >= 13 {
            push_field(
                &mut fields,
                max_fields,
                data_offset,
                4,
                "IHDR width",
                "画像幅",
                read_u32_be(doc, data_offset)
                    .unwrap_or_default()
                    .to_string(),
            );
            push_field(
                &mut fields,
                max_fields,
                data_offset + 4,
                4,
                "IHDR height",
                "画像高さ",
                read_u32_be(doc, data_offset + 4)
                    .unwrap_or_default()
                    .to_string(),
            );
            push_field(
                &mut fields,
                max_fields,
                data_offset + 8,
                1,
                "IHDR bit depth",
                "ビット深度",
                read_u8(doc, data_offset + 8)
                    .unwrap_or_default()
                    .to_string(),
            );
            let color_type = read_u8(doc, data_offset + 9).unwrap_or_default();
            push_field(
                &mut fields,
                max_fields,
                data_offset + 9,
                1,
                "IHDR color type",
                "カラータイプ",
                format!("{color_type} ({})", png_color_type(color_type)),
            );
            push_field(
                &mut fields,
                max_fields,
                data_offset + 10,
                1,
                "IHDR compression",
                "圧縮方式",
                read_u8(doc, data_offset + 10)
                    .unwrap_or_default()
                    .to_string(),
            );
            push_field(
                &mut fields,
                max_fields,
                data_offset + 11,
                1,
                "IHDR filter",
                "フィルター方式",
                read_u8(doc, data_offset + 11)
                    .unwrap_or_default()
                    .to_string(),
            );
            let interlace = read_u8(doc, data_offset + 12).unwrap_or_default();
            push_field(
                &mut fields,
                max_fields,
                data_offset + 12,
                1,
                "IHDR interlace",
                "インターレース方式",
                format!("{interlace} ({})", png_interlace(interlace)),
            );
        } else if length > 0 {
            push_field(
                &mut fields,
                max_fields,
                data_offset,
                length as u64,
                format!("{chunk_type} data"),
                "チャンクデータ",
                format!("{} bytes", length),
            );
        }

        push_field(
            &mut fields,
            max_fields,
            crc_offset,
            4,
            format!("{chunk_type} CRC"),
            "チャンクCRC",
            format!("0x{:08X}", read_u32_be(doc, crc_offset).unwrap_or_default()),
        );

        offset = crc_offset + 4;
        if chunk_type == "IEND" {
            break;
        }
    }

    FormatSummary {
        format: "PNG image".to_string(),
        fields,
    }
}

fn parse_bmp_fields(doc: &ByteDocument, max_fields: usize) -> FormatSummary {
    let mut fields = Vec::new();
    push_field(
        &mut fields,
        max_fields,
        0,
        2,
        "Signature",
        "BMP識別子",
        read_ascii(doc, 0, 2).unwrap_or_default(),
    );
    push_field(
        &mut fields,
        max_fields,
        2,
        4,
        "File size",
        "ファイルサイズ",
        format!("{} bytes", read_u32_le(doc, 2).unwrap_or_default()),
    );
    push_field(
        &mut fields,
        max_fields,
        6,
        2,
        "Reserved 1",
        "予約領域1",
        read_u16_le(doc, 6).unwrap_or_default().to_string(),
    );
    push_field(
        &mut fields,
        max_fields,
        8,
        2,
        "Reserved 2",
        "予約領域2",
        read_u16_le(doc, 8).unwrap_or_default().to_string(),
    );
    let pixel_offset = read_u32_le(doc, 10).unwrap_or_default();
    push_field(
        &mut fields,
        max_fields,
        10,
        4,
        "Pixel data offset",
        "ピクセルデータ開始位置",
        format!("0x{pixel_offset:X} / {pixel_offset}"),
    );

    let dib_size = read_u32_le(doc, 14).unwrap_or_default();
    push_field(
        &mut fields,
        max_fields,
        14,
        4,
        "DIB header size",
        "DIBヘッダサイズ",
        format!("{} bytes", dib_size),
    );
    if dib_size >= 40 && doc.len() >= 54 {
        push_field(
            &mut fields,
            max_fields,
            18,
            4,
            "Width",
            "画像幅",
            read_i32_le(doc, 18).unwrap_or_default().to_string(),
        );
        push_field(
            &mut fields,
            max_fields,
            22,
            4,
            "Height",
            "画像高さ",
            read_i32_le(doc, 22).unwrap_or_default().to_string(),
        );
        push_field(
            &mut fields,
            max_fields,
            26,
            2,
            "Planes",
            "プレーン数",
            read_u16_le(doc, 26).unwrap_or_default().to_string(),
        );
        push_field(
            &mut fields,
            max_fields,
            28,
            2,
            "Bits per pixel",
            "1ピクセルあたりのビット数",
            read_u16_le(doc, 28).unwrap_or_default().to_string(),
        );
        let compression = read_u32_le(doc, 30).unwrap_or_default();
        push_field(
            &mut fields,
            max_fields,
            30,
            4,
            "Compression",
            "圧縮方式",
            format!("{compression} ({})", bmp_compression(compression)),
        );
        push_field(
            &mut fields,
            max_fields,
            34,
            4,
            "Image size",
            "画像データサイズ",
            format!("{} bytes", read_u32_le(doc, 34).unwrap_or_default()),
        );
        push_field(
            &mut fields,
            max_fields,
            38,
            4,
            "X pixels per meter",
            "水平解像度",
            read_i32_le(doc, 38).unwrap_or_default().to_string(),
        );
        push_field(
            &mut fields,
            max_fields,
            42,
            4,
            "Y pixels per meter",
            "垂直解像度",
            read_i32_le(doc, 42).unwrap_or_default().to_string(),
        );
        push_field(
            &mut fields,
            max_fields,
            46,
            4,
            "Colors used",
            "カラーテーブル使用数",
            read_u32_le(doc, 46).unwrap_or_default().to_string(),
        );
        push_field(
            &mut fields,
            max_fields,
            50,
            4,
            "Important colors",
            "重要色数",
            read_u32_le(doc, 50).unwrap_or_default().to_string(),
        );
    }

    FormatSummary {
        format: "BMP image".to_string(),
        fields,
    }
}

fn parse_wav_fields(doc: &ByteDocument, max_fields: usize) -> FormatSummary {
    let mut fields = Vec::new();
    push_field(
        &mut fields,
        max_fields,
        0,
        4,
        "RIFF id",
        "RIFF識別子",
        read_ascii(doc, 0, 4).unwrap_or_default(),
    );
    push_field(
        &mut fields,
        max_fields,
        4,
        4,
        "RIFF size",
        "RIFFデータサイズ",
        format!("{} bytes", read_u32_le(doc, 4).unwrap_or_default()),
    );
    push_field(
        &mut fields,
        max_fields,
        8,
        4,
        "Wave id",
        "WAVE識別子",
        read_ascii(doc, 8, 4).unwrap_or_default(),
    );

    let mut offset = 12;
    while offset + 8 <= doc.len() && fields.len() < max_fields {
        let chunk_id = read_ascii(doc, offset, 4).unwrap_or_else(|| "????".to_string());
        let size = read_u32_le(doc, offset + 4).unwrap_or_default();
        let data_offset = offset + 8;
        if data_offset + size as u64 > doc.len() {
            push_field(
                &mut fields,
                max_fields,
                offset,
                doc.len().saturating_sub(offset),
                "Truncated chunk",
                "途中で終わっているWAVチャンク",
                chunk_id,
            );
            break;
        }
        push_field(
            &mut fields,
            max_fields,
            offset,
            4,
            format!("{chunk_id} id"),
            "チャンク識別子",
            wav_chunk_meaning(&chunk_id),
        );
        push_field(
            &mut fields,
            max_fields,
            offset + 4,
            4,
            format!("{chunk_id} size"),
            "チャンクデータサイズ",
            format!("{} bytes", size),
        );

        if chunk_id == "fmt " && size >= 16 {
            let audio_format = read_u16_le(doc, data_offset).unwrap_or_default();
            push_field(
                &mut fields,
                max_fields,
                data_offset,
                2,
                "Audio format",
                "音声形式",
                format!("{audio_format} ({})", wav_audio_format(audio_format)),
            );
            push_field(
                &mut fields,
                max_fields,
                data_offset + 2,
                2,
                "Channels",
                "チャンネル数",
                read_u16_le(doc, data_offset + 2)
                    .unwrap_or_default()
                    .to_string(),
            );
            push_field(
                &mut fields,
                max_fields,
                data_offset + 4,
                4,
                "Sample rate",
                "サンプルレート",
                format!(
                    "{} Hz",
                    read_u32_le(doc, data_offset + 4).unwrap_or_default()
                ),
            );
            push_field(
                &mut fields,
                max_fields,
                data_offset + 8,
                4,
                "Byte rate",
                "平均バイトレート",
                format!(
                    "{} bytes/sec",
                    read_u32_le(doc, data_offset + 8).unwrap_or_default()
                ),
            );
            push_field(
                &mut fields,
                max_fields,
                data_offset + 12,
                2,
                "Block align",
                "ブロックアライン",
                read_u16_le(doc, data_offset + 12)
                    .unwrap_or_default()
                    .to_string(),
            );
            push_field(
                &mut fields,
                max_fields,
                data_offset + 14,
                2,
                "Bits per sample",
                "サンプルあたりビット数",
                read_u16_le(doc, data_offset + 14)
                    .unwrap_or_default()
                    .to_string(),
            );
        } else if chunk_id == "data" {
            push_field(
                &mut fields,
                max_fields,
                data_offset,
                size as u64,
                "Audio data",
                "PCMなどの音声データ",
                format!("{} bytes", size),
            );
        } else if size > 0 {
            push_field(
                &mut fields,
                max_fields,
                data_offset,
                size as u64,
                format!("{chunk_id} data"),
                "チャンクデータ",
                format!("{} bytes", size),
            );
        }

        offset = data_offset + size as u64 + (size as u64 % 2);
    }

    FormatSummary {
        format: "WAV audio".to_string(),
        fields,
    }
}

fn parse_gif_fields(doc: &ByteDocument, max_fields: usize) -> FormatSummary {
    let mut fields = Vec::new();
    push_field(
        &mut fields,
        max_fields,
        0,
        6,
        "Signature",
        "GIF署名とバージョン",
        read_ascii(doc, 0, 6).unwrap_or_default(),
    );
    push_field(
        &mut fields,
        max_fields,
        6,
        2,
        "Logical screen width",
        "論理画面幅",
        read_u16_le(doc, 6).unwrap_or_default().to_string(),
    );
    push_field(
        &mut fields,
        max_fields,
        8,
        2,
        "Logical screen height",
        "論理画面高さ",
        read_u16_le(doc, 8).unwrap_or_default().to_string(),
    );
    let packed = read_u8(doc, 10).unwrap_or_default();
    push_field(
        &mut fields,
        max_fields,
        10,
        1,
        "Packed fields",
        "グローバルカラーテーブル等のフラグ",
        format!("0b{packed:08b}"),
    );
    push_field(
        &mut fields,
        max_fields,
        11,
        1,
        "Background color index",
        "背景色インデックス",
        read_u8(doc, 11).unwrap_or_default().to_string(),
    );
    push_field(
        &mut fields,
        max_fields,
        12,
        1,
        "Pixel aspect ratio",
        "ピクセル縦横比",
        read_u8(doc, 12).unwrap_or_default().to_string(),
    );
    FormatSummary {
        format: "GIF image".to_string(),
        fields,
    }
}

fn parse_jpeg_fields(doc: &ByteDocument, max_fields: usize) -> FormatSummary {
    let mut fields = Vec::new();
    push_field(
        &mut fields,
        max_fields,
        0,
        2,
        "SOI",
        "JPEG開始マーカー",
        "FF D8",
    );
    let mut offset = 2;
    while offset + 1 < doc.len() && fields.len() < max_fields {
        if read_u8(doc, offset) != Some(0xFF) {
            offset += 1;
            continue;
        }
        while offset < doc.len() && read_u8(doc, offset) == Some(0xFF) {
            offset += 1;
        }
        let Some(marker) = read_u8(doc, offset) else {
            break;
        };
        let marker_offset = offset - 1;
        offset += 1;
        let marker_name = jpeg_marker_name(marker);
        if marker == 0xD9 {
            push_field(
                &mut fields,
                max_fields,
                marker_offset,
                2,
                marker_name,
                "JPEG終了マーカー",
                format!("FF {marker:02X}"),
            );
            break;
        }
        if marker == 0xDA {
            push_field(
                &mut fields,
                max_fields,
                marker_offset,
                2,
                marker_name,
                "スキャン開始マーカー",
                format!("FF {marker:02X}"),
            );
        }
        if jpeg_marker_has_no_length(marker) {
            push_field(
                &mut fields,
                max_fields,
                marker_offset,
                2,
                marker_name,
                "JPEGマーカー",
                format!("FF {marker:02X}"),
            );
            continue;
        }
        let Some(size) = read_u16_be(doc, offset) else {
            break;
        };
        if offset + size as u64 > doc.len() {
            break;
        }
        push_field(
            &mut fields,
            max_fields,
            marker_offset,
            2,
            marker_name,
            "JPEGセグメントマーカー",
            format!("FF {marker:02X}"),
        );
        push_field(
            &mut fields,
            max_fields,
            offset,
            2,
            format!("{marker_name} length"),
            "セグメント長",
            format!("{} bytes", size),
        );
        let data_len = size.saturating_sub(2) as u64;
        if data_len > 0 {
            push_field(
                &mut fields,
                max_fields,
                offset + 2,
                data_len,
                format!("{marker_name} data"),
                jpeg_marker_meaning(marker),
                format!("{} bytes", data_len),
            );
        }
        offset += size as u64;
        if marker == 0xDA {
            if let Some(eoi) = find_jpeg_eoi(doc, offset) {
                push_field(
                    &mut fields,
                    max_fields,
                    offset,
                    eoi.saturating_sub(offset),
                    "Entropy-coded data",
                    "圧縮画像データ",
                    format!("{} bytes", eoi.saturating_sub(offset)),
                );
                push_field(
                    &mut fields,
                    max_fields,
                    eoi,
                    2,
                    "EOI",
                    "JPEG終了マーカー",
                    "FF D9",
                );
            }
            break;
        }
    }
    FormatSummary {
        format: "JPEG image".to_string(),
        fields,
    }
}

fn parse_zip_fields(doc: &ByteDocument, max_fields: usize) -> FormatSummary {
    let mut fields = Vec::new();
    push_field(
        &mut fields,
        max_fields,
        0,
        4,
        "Local file header signature",
        "ZIPローカルファイルヘッダ識別子",
        "PK 03 04",
    );
    push_field(
        &mut fields,
        max_fields,
        4,
        2,
        "Version needed",
        "展開に必要なバージョン",
        read_u16_le(doc, 4).unwrap_or_default().to_string(),
    );
    push_field(
        &mut fields,
        max_fields,
        6,
        2,
        "General purpose flags",
        "汎用ビットフラグ",
        format!("0x{:04X}", read_u16_le(doc, 6).unwrap_or_default()),
    );
    let method = read_u16_le(doc, 8).unwrap_or_default();
    push_field(
        &mut fields,
        max_fields,
        8,
        2,
        "Compression method",
        "圧縮方式",
        format!("{method} ({})", zip_compression(method)),
    );
    push_field(
        &mut fields,
        max_fields,
        10,
        2,
        "Modified time",
        "最終更新時刻(DOS)",
        format!("0x{:04X}", read_u16_le(doc, 10).unwrap_or_default()),
    );
    push_field(
        &mut fields,
        max_fields,
        12,
        2,
        "Modified date",
        "最終更新日(DOS)",
        format!("0x{:04X}", read_u16_le(doc, 12).unwrap_or_default()),
    );
    push_field(
        &mut fields,
        max_fields,
        14,
        4,
        "CRC-32",
        "ファイルデータCRC",
        format!("0x{:08X}", read_u32_le(doc, 14).unwrap_or_default()),
    );
    push_field(
        &mut fields,
        max_fields,
        18,
        4,
        "Compressed size",
        "圧縮後サイズ",
        format!("{} bytes", read_u32_le(doc, 18).unwrap_or_default()),
    );
    push_field(
        &mut fields,
        max_fields,
        22,
        4,
        "Uncompressed size",
        "展開後サイズ",
        format!("{} bytes", read_u32_le(doc, 22).unwrap_or_default()),
    );
    let name_len = read_u16_le(doc, 26).unwrap_or_default() as u64;
    let extra_len = read_u16_le(doc, 28).unwrap_or_default() as u64;
    push_field(
        &mut fields,
        max_fields,
        26,
        2,
        "File name length",
        "ファイル名長",
        format!("{} bytes", name_len),
    );
    push_field(
        &mut fields,
        max_fields,
        28,
        2,
        "Extra field length",
        "拡張フィールド長",
        format!("{} bytes", extra_len),
    );
    if name_len > 0 {
        push_field(
            &mut fields,
            max_fields,
            30,
            name_len,
            "File name",
            "エントリ名",
            read_ascii(doc, 30, name_len as usize).unwrap_or_default(),
        );
    }
    if extra_len > 0 {
        push_field(
            &mut fields,
            max_fields,
            30 + name_len,
            extra_len,
            "Extra field",
            "拡張フィールド",
            format!("{} bytes", extra_len),
        );
    }
    let data_offset = 30 + name_len + extra_len;
    let compressed_size = read_u32_le(doc, 18).unwrap_or_default() as u64;
    if compressed_size > 0 {
        push_field(
            &mut fields,
            max_fields,
            data_offset,
            compressed_size,
            "File data",
            "圧縮済みファイルデータ",
            format!("{} bytes", compressed_size),
        );
    }
    FormatSummary {
        format: "ZIP archive".to_string(),
        fields,
    }
}

fn push_field(
    fields: &mut Vec<FormatField>,
    max_fields: usize,
    offset: u64,
    len: u64,
    name: impl Into<String>,
    meaning: impl Into<String>,
    value: impl Into<String>,
) {
    if fields.len() < max_fields && len > 0 {
        fields.push(FormatField {
            offset,
            len,
            name: name.into(),
            meaning: meaning.into(),
            value: value.into(),
        });
    }
}

fn read_u8(doc: &ByteDocument, offset: u64) -> Option<u8> {
    doc.read_range(offset, 1).first().copied()
}

fn read_u16_le(doc: &ByteDocument, offset: u64) -> Option<u16> {
    let bytes = doc.read_range(offset, 2);
    Some(u16::from_le_bytes(bytes.as_slice().try_into().ok()?))
}

fn read_u16_be(doc: &ByteDocument, offset: u64) -> Option<u16> {
    let bytes = doc.read_range(offset, 2);
    Some(u16::from_be_bytes(bytes.as_slice().try_into().ok()?))
}

fn read_u32_le(doc: &ByteDocument, offset: u64) -> Option<u32> {
    let bytes = doc.read_range(offset, 4);
    Some(u32::from_le_bytes(bytes.as_slice().try_into().ok()?))
}

fn read_i32_le(doc: &ByteDocument, offset: u64) -> Option<i32> {
    let bytes = doc.read_range(offset, 4);
    Some(i32::from_le_bytes(bytes.as_slice().try_into().ok()?))
}

fn read_u32_be(doc: &ByteDocument, offset: u64) -> Option<u32> {
    let bytes = doc.read_range(offset, 4);
    Some(u32::from_be_bytes(bytes.as_slice().try_into().ok()?))
}

fn read_ascii(doc: &ByteDocument, offset: u64, len: usize) -> Option<String> {
    let bytes = doc.read_range(offset, len);
    if bytes.len() != len {
        return None;
    }
    Some(
        bytes
            .iter()
            .map(|byte| {
                if (0x20..=0x7e).contains(byte) {
                    *byte as char
                } else {
                    '.'
                }
            })
            .collect(),
    )
}

fn png_chunk_meaning(chunk_type: &str) -> String {
    match chunk_type {
        "IHDR" => "IHDR: 画像ヘッダ".to_string(),
        "PLTE" => "PLTE: パレット".to_string(),
        "IDAT" => "IDAT: 圧縮画像データ".to_string(),
        "IEND" => "IEND: PNG終端".to_string(),
        "tEXt" => "tEXt: テキストメタデータ".to_string(),
        "zTXt" => "zTXt: 圧縮テキストメタデータ".to_string(),
        "iCCP" => "iCCP: ICCプロファイル".to_string(),
        "pHYs" => "pHYs: 物理ピクセル寸法".to_string(),
        "gAMA" => "gAMA: ガンマ値".to_string(),
        "cHRM" => "cHRM: 色度情報".to_string(),
        "sRGB" => "sRGB: 標準RGB色空間".to_string(),
        _ => format!("{chunk_type}: PNG chunk"),
    }
}

fn png_color_type(color_type: u8) -> &'static str {
    match color_type {
        0 => "grayscale",
        2 => "truecolor",
        3 => "indexed color",
        4 => "grayscale + alpha",
        6 => "truecolor + alpha",
        _ => "unknown",
    }
}

fn png_interlace(value: u8) -> &'static str {
    match value {
        0 => "none",
        1 => "Adam7",
        _ => "unknown",
    }
}

fn bmp_compression(value: u32) -> &'static str {
    match value {
        0 => "BI_RGB",
        1 => "BI_RLE8",
        2 => "BI_RLE4",
        3 => "BI_BITFIELDS",
        4 => "BI_JPEG",
        5 => "BI_PNG",
        6 => "BI_ALPHABITFIELDS",
        _ => "unknown",
    }
}

fn wav_chunk_meaning(chunk_id: &str) -> String {
    match chunk_id {
        "fmt " => "fmt: 音声フォーマット".to_string(),
        "data" => "data: 音声サンプルデータ".to_string(),
        "LIST" => "LIST: メタデータリスト".to_string(),
        "fact" => "fact: 圧縮音声情報".to_string(),
        _ => format!("{chunk_id}: WAV chunk"),
    }
}

fn wav_audio_format(value: u16) -> &'static str {
    match value {
        1 => "PCM",
        3 => "IEEE float",
        6 => "A-law",
        7 => "mu-law",
        0xFFFE => "Extensible",
        _ => "unknown",
    }
}

fn jpeg_marker_name(marker: u8) -> &'static str {
    match marker {
        0xC0 => "SOF0",
        0xC2 => "SOF2",
        0xC4 => "DHT",
        0xD9 => "EOI",
        0xDA => "SOS",
        0xDB => "DQT",
        0xDD => "DRI",
        0xE0 => "APP0",
        0xE1 => "APP1",
        0xE2 => "APP2",
        0xE3 => "APP3",
        0xE4 => "APP4",
        0xE5 => "APP5",
        0xE6 => "APP6",
        0xE7 => "APP7",
        0xE8 => "APP8",
        0xE9 => "APP9",
        0xEA => "APP10",
        0xEB => "APP11",
        0xEC => "APP12",
        0xED => "APP13",
        0xEE => "APP14",
        0xEF => "APP15",
        0xFE => "COM",
        _ => "Marker",
    }
}

fn jpeg_marker_meaning(marker: u8) -> String {
    match marker {
        0xC0 => "ベースラインDCTフレーム".to_string(),
        0xC2 => "プログレッシブDCTフレーム".to_string(),
        0xC4 => "ハフマンテーブル".to_string(),
        0xDA => "スキャンヘッダ".to_string(),
        0xDB => "量子化テーブル".to_string(),
        0xDD => "リスタート間隔".to_string(),
        0xE0 => "JFIFなどのAPP0メタデータ".to_string(),
        0xE1 => "Exif/XMPなどのAPP1メタデータ".to_string(),
        0xFE => "コメント".to_string(),
        0xE2..=0xEF => "アプリケーション固有メタデータ".to_string(),
        _ => "JPEG segment data".to_string(),
    }
}

fn jpeg_marker_has_no_length(marker: u8) -> bool {
    marker == 0x01 || (0xD0..=0xD7).contains(&marker)
}

fn find_jpeg_eoi(doc: &ByteDocument, start: u64) -> Option<u64> {
    let mut offset = start;
    while offset + 1 < doc.len() {
        let bytes = doc.read_range(offset, 64 * 1024);
        if bytes.len() < 2 {
            return None;
        }
        if let Some(pos) = bytes.windows(2).position(|window| window == b"\xFF\xD9") {
            return Some(offset + pos as u64);
        }
        offset += bytes.len() as u64 - 1;
    }
    None
}

fn zip_compression(value: u16) -> &'static str {
    match value {
        0 => "stored",
        8 => "deflate",
        12 => "bzip2",
        14 => "lzma",
        93 => "zstd",
        98 => "ppmd",
        _ => "unknown",
    }
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
    fn save_opened_mmap_document_back_to_same_path() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "byteforge-core-save-same-path-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"opened").unwrap();

        let mut doc = ByteDocument::open(&path).unwrap();
        doc.overwrite(0, b"X".to_vec()).unwrap();
        doc.save_as(&path).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"Xpened");
        assert!(!doc.is_dirty());
        let _ = std::fs::remove_file(path);
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

    #[test]
    fn detects_png_chunks_and_ihdr_fields() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x89PNG\r\n\x1A\n");
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&320u32.to_be_bytes());
        bytes.extend_from_slice(&200u32.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes.extend_from_slice(&0x12345678u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(b"IEND");
        bytes.extend_from_slice(&0u32.to_be_bytes());
        let doc = ByteDocument::from_bytes("image.png", bytes);

        let summary = detect_format_fields(&doc, 64).expect("png summary");
        assert_eq!(summary.format, "PNG image");
        assert!(summary.fields.iter().any(|field| {
            field.name == "IHDR width" && field.meaning == "画像幅" && field.value == "320"
        }));
        assert!(
            summary
                .fields
                .iter()
                .any(|field| field.name == "IHDR color type" && field.value.contains("alpha"))
        );
    }

    #[test]
    fn detects_bmp_header_fields() {
        let mut bytes = vec![0u8; 54];
        bytes[0..2].copy_from_slice(b"BM");
        bytes[2..6].copy_from_slice(&54u32.to_le_bytes());
        bytes[10..14].copy_from_slice(&54u32.to_le_bytes());
        bytes[14..18].copy_from_slice(&40u32.to_le_bytes());
        bytes[18..22].copy_from_slice(&2i32.to_le_bytes());
        bytes[22..26].copy_from_slice(&3i32.to_le_bytes());
        bytes[26..28].copy_from_slice(&1u16.to_le_bytes());
        bytes[28..30].copy_from_slice(&24u16.to_le_bytes());
        let doc = ByteDocument::from_bytes("image.bmp", bytes);

        let summary = detect_format_fields(&doc, 64).expect("bmp summary");
        assert_eq!(summary.format, "BMP image");
        assert!(
            summary
                .fields
                .iter()
                .any(|field| field.name == "Width" && field.value == "2")
        );
        assert!(summary.fields.iter().any(|field| {
            field.name == "Bits per pixel" && field.meaning == "1ピクセルあたりのビット数"
        }));
    }

    #[test]
    fn detects_wav_fmt_and_data_chunks() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&36u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&44100u32.to_le_bytes());
        bytes.extend_from_slice(&176400u32.to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        let doc = ByteDocument::from_bytes("sound.wav", bytes);

        let summary = detect_format_fields(&doc, 64).expect("wav summary");
        assert_eq!(summary.format, "WAV audio");
        assert!(
            summary
                .fields
                .iter()
                .any(|field| { field.name == "Sample rate" && field.value == "44100 Hz" })
        );
        assert!(
            summary
                .fields
                .iter()
                .any(|field| field.name == "Audio data" && field.meaning.contains("音声データ"))
        );
    }

    #[test]
    fn detects_common_gif_jpeg_and_zip_headers() {
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&16u16.to_le_bytes());
        gif.extend_from_slice(&8u16.to_le_bytes());
        gif.extend_from_slice(&[0b1000_0001, 0, 0]);
        let gif = ByteDocument::from_bytes("image.gif", gif);
        assert_eq!(
            detect_format_fields(&gif, 16).expect("gif summary").format,
            "GIF image"
        );

        let jpeg = ByteDocument::from_bytes(
            "image.jpg",
            vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, b'J', b'F', 0xFF, 0xD9],
        );
        let jpeg_summary = detect_format_fields(&jpeg, 16).expect("jpeg summary");
        assert_eq!(jpeg_summary.format, "JPEG image");
        assert!(jpeg_summary.fields.iter().any(|field| field.name == "APP0"));

        let mut zip = b"PK\x03\x04".to_vec();
        zip.extend_from_slice(&20u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&8u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u32.to_le_bytes());
        zip.extend_from_slice(&4u32.to_le_bytes());
        zip.extend_from_slice(&4u32.to_le_bytes());
        zip.extend_from_slice(&8u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(b"file.txt");
        zip.extend_from_slice(&[1, 2, 3, 4]);
        let zip = ByteDocument::from_bytes("archive.zip", zip);
        let zip_summary = detect_format_fields(&zip, 32).expect("zip summary");
        assert_eq!(zip_summary.format, "ZIP archive");
        assert!(
            zip_summary
                .fields
                .iter()
                .any(|field| field.name == "File name" && field.value == "file.txt")
        );
    }

    #[test]
    fn format_detection_uses_latest_edited_bytes() {
        let mut doc = ByteDocument::from_bytes("edited.bin", b"??\0\0\0\0".to_vec());
        assert!(detect_format_fields(&doc, 16).is_none());

        doc.overwrite(0, b"BM".to_vec()).unwrap();
        let summary = detect_format_fields(&doc, 16).expect("edited bmp summary");
        assert_eq!(summary.format, "BMP image");
        assert!(
            summary
                .fields
                .iter()
                .any(|field| field.name == "Signature" && field.value == "BM")
        );
    }
}
