//! [`ResourceSource`] and its directory, zip, and in-memory implementations.

use crate::error::AssetError;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::{Component, Path};

/// A single resource pack: a place full in-pack paths can be read from.
///
/// Paths are always the full in-pack path, for example
/// `assets/minecraft/textures/block/stone.png`, using `/` separators. Both a
/// directory tree and a zip/jar are the same on-disk format, so both back this
/// trait and are fully interchangeable.
pub trait ResourceSource: Send + Sync + std::fmt::Debug {
    /// Reads a full in-pack path, returning `None` if it is absent (a missing
    /// resource is never an error).
    fn read(&self, path: &str) -> Option<Vec<u8>>;

    /// Lists full in-pack paths under `prefix` (a plain string prefix match).
    fn list(&self, prefix: &str) -> Vec<String>;
}

// All filesystem-backed sources live in one gated module so filesystem access is
// confined to a single file that does not exist on wasm. See `source_native`.
#[cfg(not(target_arch = "wasm32"))]
#[path = "source_native.rs"]
mod source_native;
#[cfg(not(target_arch = "wasm32"))]
pub use source_native::DirectorySource;

/// Normalizes an in-pack path, rejecting traversal and absurd inputs.
///
/// Returns the canonical `a/b/c` form, or `None` if the path is empty, absolute,
/// contains a `.`/`..` component, an empty component (e.g. `//`), or a NUL byte.
fn normalize_pack_path(path: &str) -> Option<String> {
    if path.is_empty() || path.len() > 4096 || path.contains('\0') {
        return None;
    }
    let mut out: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." | ".." => return None,
            other => out.push(other),
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(out.join("/"))
}

/// The reader backing a [`ZipSource`]: a cursor over a shared, immutable,
/// in-memory copy of the whole archive. Cloning it is cheap (an `Arc` bump plus
/// a cursor copy), which is what makes lock-free per-read access possible.
type ZipReader = Cursor<std::sync::Arc<[u8]>>;

/// A resource pack backed by a zip/jar archive (the vanilla `client.jar`
/// format).
///
/// The archive's central directory is parsed exactly once at construction. The
/// parsed directory lives behind an `Arc` inside [`zip::ZipArchive`], and the
/// archive bytes live behind a second `Arc`, so [`read`](ZipSource::read) can
/// clone the archive cheaply and get its own independent, positioned reader
/// without re-parsing the directory or taking a lock. Reads are therefore
/// lock-free and run in parallel — important because the renderer loads assets
/// from a thread pool.
///
/// Entry names are normalized and any escaping (zip-slip) entries are dropped.
/// If the same name appears more than once, the last occurrence wins.
///
/// # Memory tradeoff
///
/// The entire archive is held in memory as a shared `Arc<[u8]>`. For the full
/// vanilla `client.jar` that is ~39 MiB, but most of that is JVM bytecode a
/// renderer never touches: the actual renderable-asset payload is only ~4.9 MiB
/// compressed (measured over the real 26.2 corpus — 10,967 entries, of which
/// 1,371 are block textures), and a browser fetches just that. This is what
/// makes reads lock-free: each read clones the `Arc` (a refcount bump, no copy)
/// and seeks its own cursor. A large third-party pack can still be several
/// hundred MiB, and users stack multiple packs, so the resident cost is the sum
/// of every open pack.
///
/// If that becomes a problem, a memory-mapped variant is a drop-in replacement:
/// `ZipReader` is just `Cursor<Arc<[u8]>>`, and the whole design only relies on
/// the backing store being cheaply clonable and independently seekable. Swapping
/// the `Arc<[u8]>` for an `Arc<Mmap>` (both `AsRef<[u8]>`) would keep reads
/// lock-free while letting the OS page the archive on demand, with no change to
/// the public API. It is deliberately not done now to avoid an `unsafe` mmap
/// dependency for a cost we do not yet pay.
#[derive(Debug, Clone)]
pub struct ZipSource {
    /// The archive with its central directory parsed once. Cloned per read; the
    /// clone shares the parsed directory (`Arc`) and the byte buffer (`Arc`).
    archive: zip::ZipArchive<ZipReader>,
    /// normalized entry name -> index within the archive.
    index: HashMap<String, usize>,
    /// Sorted normalized names, for `list`.
    names: Vec<String>,
}

impl ZipSource {
    /// Opens a zip/jar archive from an in-memory byte buffer.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, AssetError> {
        let bytes: std::sync::Arc<[u8]> = bytes.into();
        let reader = Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(reader).map_err(|source| AssetError::Zip {
            path: "<memory>".to_string(),
            source,
        })?;
        let mut index = HashMap::new();
        for i in 0..archive.len() {
            let entry = archive.by_index(i).map_err(|source| AssetError::Zip {
                path: "<memory>".to_string(),
                source,
            })?;
            if entry.is_dir() {
                continue;
            }
            // Prefer the archive's own sanitized name, then re-validate.
            let raw = entry
                .enclosed_name()
                .and_then(|p| pathbuf_to_pack_path(&p))
                .or_else(|| Some(entry.name().to_string()));
            if let Some(name) = raw.and_then(|n| normalize_pack_path(&n)) {
                index.insert(name, i); // last duplicate wins
            }
        }
        let mut names: Vec<String> = index.keys().cloned().collect();
        names.sort();
        Ok(Self {
            archive,
            index,
            names,
        })
    }
}

/// Converts a `PathBuf` produced by `enclosed_name` back into an in-pack path.
fn pathbuf_to_pack_path(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Normal(s) => parts.push(s.to_str()?.to_owned()),
            _ => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

impl ResourceSource for ZipSource {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        let normalized = normalize_pack_path(path)?;
        let &idx = self.index.get(&normalized)?;
        // Cheap clone: shares the parsed central directory (Arc) and the byte
        // buffer (Arc). No re-parse, no lock — each call reads independently.
        let mut archive = self.archive.clone();
        let mut entry = archive.by_index(idx).ok()?;
        // NOT `Vec::with_capacity(entry.size() as usize)`: `size()` is the
        // entry's own declared uncompressed-size field, read straight off the
        // archive before a single byte of it is decompressed or checked
        // against the CRC-32 the archive also carries — a hostile or merely
        // corrupt pack can declare almost 4 GiB for a few real bytes, and a
        // fuzz target over this exact function reproduced the resulting
        // allocator abort (`malloc(4294967294)`) on the first execution.
        // `read_to_end` grows the buffer to whatever the entry actually
        // decompresses to regardless of the hint, so dropping the
        // preallocation costs realloc overhead on a large *honest* entry and
        // costs nothing on a lying one.
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).ok()?;
        Some(buf)
    }

    fn list(&self, prefix: &str) -> Vec<String> {
        self.names
            .iter()
            .filter(|n| n.starts_with(prefix))
            .cloned()
            .collect()
    }
}

/// An in-memory resource pack, useful for tests and synthetic packs.
#[derive(Debug, Clone, Default)]
pub struct MemorySource {
    name: String,
    entries: HashMap<String, Vec<u8>>,
}

impl MemorySource {
    /// Creates an empty in-memory pack with a debug name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            entries: HashMap::new(),
        }
    }

    /// Inserts (or replaces) an entry at a full in-pack path.
    pub fn insert(&mut self, path: impl Into<String>, bytes: Vec<u8>) {
        self.entries.insert(path.into(), bytes);
    }

    /// The debug name given at construction.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl ResourceSource for MemorySource {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        let normalized = normalize_pack_path(path)?;
        self.entries.get(&normalized).cloned()
    }

    fn list(&self, prefix: &str) -> Vec<String> {
        self.entries
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect()
    }
}
