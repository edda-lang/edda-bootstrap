//! Source spans, file identifiers, and source-map management.
//!
//! Every other crate in the workspace depends on this. A `Span` is the
//! universal "where in source" reference used by diagnostics, parser errors,
//! type errors, and IDE features.
//!
//! Bootstrap infrastructure — not a spec'd language feature.
//!
//! # Concurrency
//!
//! [`SourceMap`] is shared across worker threads (the daemon owns one and
//! hands `&SourceMap` references into parse, typecheck, and codegen jobs that
//! run in parallel). It uses `parking_lot::RwLock` internally:
//!   - readers are the common case (every diagnostic emission, every IDE
//!     hover request) and `parking_lot::RwLock` gives uncontested readers a
//!     single atomic op;
//!   - the lock does not poison on panic, which matters because a panic in
//!     one parse job must not deadlock the daemon's other jobs;
//!   - the type is smaller than `std::sync::RwLock` and the dependency is
//!     already standard across the wider Rust ecosystem.
//!
//! # Pointer-stability invariant
//!
//! Once a file is registered, the heap allocation holding its content must
//! never move and its bytes must never change. The implementation stores
//! contents as `Box<str>` inside a `Vec<FileEntry>`. Growing the `Vec` moves
//! the `Box` headers but **not** the `str` payloads — that is the whole point
//! of boxing. Entries are append-only: never removed, never mutated. This is
//! what makes it sound for [`SourceMap::file_content`] and
//! [`SourceMap::span_text`] to return `&str` tied to `&self` rather than to a
//! lock guard.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;

/// Opaque handle identifying a registered source file in a [`SourceMap`].
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub struct FileId(u32);

impl FileId {
    /// Construct a `FileId` from its raw index. Crate-internal: external
    /// code obtains `FileId`s only via [`SourceMap::add_file`].
    fn from_raw(index: u32) -> Self {
        FileId(index)
    }

    /// Return the raw u32 index. Crate-internal — kept private to preserve
    /// the opaque-handle pattern.
    fn as_index(self) -> usize {
        self.0 as usize
    }

    /// Return the raw 32-bit handle for byte-serialisation use cases. Used by
    /// `edda-refine`'s proof-certificate writer (per
    /// `docs/codegen/distribution/03-certificate.md` §3.4 / §3.5) to record
    /// the source position of `@unverified` / `@trust` annotations. The
    /// value is meaningless without an accompanying `SourceMap`; consumers
    /// reading certificates back must canonicalise via the file's path
    /// before re-issuing a `FileId` against their own `SourceMap`.
    pub const fn to_u32(self) -> u32 {
        self.0
    }
}

/// Byte offset from the start of a source file.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub struct BytePos(pub u32);

impl BytePos {
    /// Return the offset as a `u32`.
    pub fn to_u32(self) -> u32 {
        self.0
    }

    /// Return the offset as a `usize` for slicing.
    pub fn to_usize(self) -> usize {
        self.0 as usize
    }
}

/// Half-open byte range `[lo, hi)` inside a registered source file.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct Span {
    /// File this span refers to.
    pub file: FileId,
    /// Inclusive low byte offset.
    pub lo: BytePos,
    /// Exclusive high byte offset.
    pub hi: BytePos,
}

impl Span {
    /// Sentinel span used when synthetic AST nodes have no source location.
    pub const DUMMY: Span = Span {
        file: FileId(u32::MAX),
        lo: BytePos(0),
        hi: BytePos(0),
    };

    /// Construct a new span. Debug-asserts `lo <= hi`.
    pub fn new(file: FileId, lo: BytePos, hi: BytePos) -> Span {
        debug_assert!(
            lo.0 <= hi.0,
            "Span::new: lo ({}) > hi ({})",
            lo.0,
            hi.0
        );
        Span { file, lo, hi }
    }

    /// Length of the span in bytes.
    pub fn len(self) -> u32 {
        self.hi.0.saturating_sub(self.lo.0)
    }

    /// Whether the span has zero length.
    pub fn is_empty(self) -> bool {
        self.lo.0 == self.hi.0
    }

    /// Whether this is the [`Span::DUMMY`] sentinel.
    pub fn is_dummy(self) -> bool {
        self.file == Span::DUMMY.file
    }

    /// Merge two spans on the same file into the smallest span that covers both.
    pub fn join(self, other: Span) -> Span {
        debug_assert!(
            self.file == other.file,
            "Span::join: file mismatch ({:?} vs {:?})",
            self.file,
            other.file
        );
        Span {
            file: self.file,
            lo: BytePos(self.lo.0.min(other.lo.0)),
            hi: BytePos(self.hi.0.max(other.hi.0)),
        }
    }
}

/// Human-facing line/column position. **Both fields are 1-based.**
///
/// This convention matches every editor, terminal, and the LSP `Position`
/// type after the standard 0-based -> 1-based conversion. Internal byte
/// offsets remain 0-based; conversion happens exactly once, at
/// [`SourceMap::byte_to_linecol`].
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub struct LineCol {
    /// 1-based line number.
    pub line: u32,
    /// 1-based column number, counted in bytes from the start of the line.
    pub col: u32,
}

/// One registered file's metadata and content.
///
/// `name` and `content` are wrapped in heap allocations whose addresses are
/// stable for the lifetime of the entry. Once inserted into
/// `SourceMap::files`, an entry is never mutated or removed.
struct FileEntry {
    name: Arc<Path>,
    content: Box<str>,
}

/// Thread-safe registry of source files keyed by [`FileId`].
pub struct SourceMap {
    files: RwLock<Vec<FileEntry>>,
}

impl SourceMap {
    /// Construct an empty source map.
    pub fn new() -> Self {
        SourceMap {
            files: RwLock::new(Vec::new()),
        }
    }

    /// Register a file and return its newly issued [`FileId`].
    pub fn add_file(&self, name: PathBuf, content: String) -> FileId {
        let entry = FileEntry {
            name: Arc::from(name.as_path()),
            content: content.into_boxed_str(),
        };
        let mut files = self.files.write();
        let index = u32::try_from(files.len())
            .expect("edda-span: more than u32::MAX source files registered");
        files.push(entry);
        FileId::from_raw(index)
    }

    /// Number of files registered so far.
    pub fn len(&self) -> usize {
        self.files.read().len()
    }

    /// Whether no files have been registered.
    pub fn is_empty(&self) -> bool {
        self.files.read().is_empty()
    }

    /// Return the registered name of a file. The returned `Arc<Path>` shares
    /// ownership with the source map; cloning it is cheap.
    pub fn file_name(&self, id: FileId) -> Arc<Path> {
        let files = self.files.read();
        let entry = files
            .get(id.as_index())
            .unwrap_or_else(|| panic!("edda-span: unknown FileId {:?}", id));
        Arc::clone(&entry.name)
    }

    /// Return the file's content as a `&str` borrowed from the source map.
    pub fn file_content(&self, id: FileId) -> &str {
        let files = self.files.read();
        let entry = files
            .get(id.as_index())
            .unwrap_or_else(|| panic!("edda-span: unknown FileId {:?}", id));
        // SAFETY: `entry.content` is a `Box<str>` inside an append-only `Vec`.
        // The `Vec` may grow (reallocating the slice of `FileEntry` headers),
        // but the heap allocation holding the `str` payload does not move and
        // is never freed for the life of the `SourceMap`. We therefore extend
        // the borrow from the lock guard's lifetime up to `&self`. Aliasing
        // is sound because the bytes are immutable after insertion: no `&mut
        // str` is ever produced.
        let bytes: &str = &entry.content;
        unsafe { std::mem::transmute::<&str, &str>(bytes) }
    }

    /// Return the source text covered by `span`.
    pub fn span_text(&self, span: Span) -> &str {
        assert!(!span.is_dummy(), "edda-span: span_text called on Span::DUMMY");
        let text = self.file_content(span.file);
        &text[span.lo.to_usize()..span.hi.to_usize()]
    }

    /// Convert a byte offset inside `file` to a 1-based [`LineCol`].
    ///
    /// Column is counted in bytes from the start of the line — that matches
    /// LSP semantics with the byte-offset position encoding. Each `\n`
    /// terminates a line; `\r\n` is treated as a line break on the `\n` (the
    /// `\r` contributes to the previous line's column count).
    pub fn byte_to_linecol(&self, file: FileId, pos: BytePos) -> LineCol {
        let content = self.file_content(file);
        let offset = pos.to_usize();
        assert!(
            offset <= content.len(),
            "edda-span: BytePos {} out of range for file of length {}",
            offset,
            content.len()
        );
        compute_linecol(content, offset)
    }
}

impl Default for SourceMap {
    fn default() -> Self {
        SourceMap::new()
    }
}

/// Walk `content[..offset]` counting newlines to produce a 1-based [`LineCol`].
fn compute_linecol(content: &str, offset: usize) -> LineCol {
    let prefix = &content.as_bytes()[..offset];
    let mut line: u32 = 1;
    let mut last_newline: usize = 0;
    let mut seen_newline = false;
    for (i, &b) in prefix.iter().enumerate() {
        if b == b'\n' {
            line = line.saturating_add(1);
            last_newline = i + 1;
            seen_newline = true;
        }
    }
    let col_start = if seen_newline { last_newline } else { 0 };
    let col = u32::try_from(offset - col_start)
        .unwrap_or(u32::MAX)
        .saturating_add(1);
    LineCol { line, col }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_file_round_trip() {
        let map = SourceMap::new();
        let id = map.add_file(PathBuf::from("a.ea"), "fn main() {}".to_string());
        assert_eq!(map.file_content(id), "fn main() {}");
        assert_eq!(map.file_name(id).as_ref(), Path::new("a.ea"));
        assert_eq!(map.len(), 1);
        assert!(!map.is_empty());
    }

    #[test]
    fn distinct_ids_for_distinct_files() {
        let map = SourceMap::new();
        let a = map.add_file(PathBuf::from("a.ea"), "a".to_string());
        let b = map.add_file(PathBuf::from("b.ea"), "bb".to_string());
        assert_ne!(a, b);
        assert_eq!(map.file_content(a), "a");
        assert_eq!(map.file_content(b), "bb");
    }

    #[test]
    fn byte_to_linecol_first_byte() {
        let map = SourceMap::new();
        let id = map.add_file(PathBuf::from("x"), "abc\ndef\nghi".to_string());
        assert_eq!(
            map.byte_to_linecol(id, BytePos(0)),
            LineCol { line: 1, col: 1 }
        );
    }

    #[test]
    fn byte_to_linecol_after_newline() {
        let map = SourceMap::new();
        let id = map.add_file(PathBuf::from("x"), "abc\ndef\nghi".to_string());
        // Byte 4 is 'd' — first byte of line 2.
        assert_eq!(
            map.byte_to_linecol(id, BytePos(4)),
            LineCol { line: 2, col: 1 }
        );
        // Byte 8 is 'g' — first byte of line 3.
        assert_eq!(
            map.byte_to_linecol(id, BytePos(8)),
            LineCol { line: 3, col: 1 }
        );
    }

    #[test]
    fn byte_to_linecol_mid_line() {
        let map = SourceMap::new();
        let id = map.add_file(PathBuf::from("x"), "abc\ndefgh\nij".to_string());
        // Byte 6 is 'f' — third byte of line 2.
        assert_eq!(
            map.byte_to_linecol(id, BytePos(6)),
            LineCol { line: 2, col: 3 }
        );
        // Byte at end of file.
        let last = u32::try_from("abc\ndefgh\nij".len()).unwrap();
        let pos = map.byte_to_linecol(id, BytePos(last));
        assert_eq!(pos.line, 3);
    }

    #[test]
    fn span_join_overlapping_and_disjoint() {
        let map = SourceMap::new();
        let id = map.add_file(PathBuf::from("x"), "abcdefghij".to_string());
        let a = Span::new(id, BytePos(0), BytePos(3));
        let b = Span::new(id, BytePos(2), BytePos(5));
        let joined_overlap = a.join(b);
        assert_eq!(joined_overlap.lo, BytePos(0));
        assert_eq!(joined_overlap.hi, BytePos(5));

        let c = Span::new(id, BytePos(3), BytePos(4));
        let joined_adjacent = a.join(c);
        assert_eq!(joined_adjacent.lo, BytePos(0));
        assert_eq!(joined_adjacent.hi, BytePos(4));

        let d = Span::new(id, BytePos(7), BytePos(9));
        let joined_disjoint = a.join(d);
        assert_eq!(joined_disjoint.lo, BytePos(0));
        assert_eq!(joined_disjoint.hi, BytePos(9));
    }

    #[test]
    fn span_dummy_is_dummy() {
        assert!(Span::DUMMY.is_dummy());
        assert!(Span::DUMMY.is_empty());
        assert_eq!(Span::DUMMY.len(), 0);

        let map = SourceMap::new();
        let id = map.add_file(PathBuf::from("x"), "abc".to_string());
        let real = Span::new(id, BytePos(0), BytePos(3));
        assert!(!real.is_dummy());
    }

    #[test]
    fn span_text_slices_correctly() {
        let map = SourceMap::new();
        let id = map.add_file(PathBuf::from("x"), "hello world".to_string());
        let span = Span::new(id, BytePos(6), BytePos(11));
        assert_eq!(map.span_text(span), "world");
    }

    #[test]
    fn pointer_stability_across_growth() {
        let map = SourceMap::new();
        let first = map.add_file(PathBuf::from("first"), "first-content".to_string());
        // Hold a borrow into the first file's content.
        let first_text: &str = map.file_content(first);
        // Force the inner Vec to grow several times.
        for i in 0..64 {
            let name = format!("file-{}", i);
            let body = format!("body-{}", i);
            map.add_file(PathBuf::from(name), body);
        }
        // The original borrow must still read the right bytes.
        assert_eq!(first_text, "first-content");
        // And re-fetching by id must still be consistent.
        assert_eq!(map.file_content(first), "first-content");
    }

    #[test]
    fn is_empty_and_len() {
        let map = SourceMap::new();
        assert!(map.is_empty());
        assert_eq!(map.len(), 0);
        map.add_file(PathBuf::from("a"), String::new());
        assert!(!map.is_empty());
        assert_eq!(map.len(), 1);
    }
}
