//! Filesystem-backed resource sources, deliberately confined to one module.
//!
//! The entire file is gated on `cfg(not(target_arch = "wasm32"))`, so `std::fs`
//! appears *only* here. That is a hard architectural boundary a Cargo feature
//! cannot provide: feature unification lets any crate in the graph re-enable a
//! default-on feature silently, whereas a `cfg(target_arch)` split cannot be
//! turned back on by anyone. `scripts/wasm-check.sh` additionally asserts that
//! no other source file references `std::fs`, so reintroducing a filesystem
//! call outside this gated module fails the wasm guard rather than compiling
//! green and dying at runtime (note: `std::fs` *does* compile for wasm32 and
//! only fails at run time, so the grep guard — not the target alone — is what
//! catches a stray call).

use super::{ResourceSource, ZipSource, normalize_pack_path};
use crate::error::AssetError;
use std::path::{Component, Path, PathBuf};

/// A resource pack backed by a directory on disk.
///
/// Filesystem-backed, so it is gated on `cfg(not(target_arch = "wasm32"))` — a
/// hard architectural boundary that Cargo feature unification cannot re-enable.
/// A wasm/browser build does not compile this type at all: reaching for it there
/// is a compile error, not a green-compile/dead-at-runtime `std::fs` trap. The
/// browser reads packs via [`ZipSource::from_bytes`] instead.
#[derive(Debug, Clone)]
pub struct DirectorySource {
    root: PathBuf,
}

impl DirectorySource {
    /// Opens a directory as a resource pack. The path must exist and be a
    /// directory.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, AssetError> {
        let root = root.as_ref();
        let canonical = root.canonicalize().map_err(|source| AssetError::Io {
            path: root.display().to_string(),
            source,
        })?;
        if !canonical.is_dir() {
            return Err(AssetError::Io {
                path: root.display().to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotADirectory,
                    "resource pack root is not a directory",
                ),
            });
        }
        Ok(Self { root: canonical })
    }

    /// Resolves an in-pack path to an absolute filesystem path that is
    /// guaranteed to stay within the pack root, or `None` if it escapes.
    fn resolve(&self, path: &str) -> Option<PathBuf> {
        let normalized = normalize_pack_path(path)?;
        let mut full = self.root.clone();
        for part in normalized.split('/') {
            full.push(part);
        }
        // Defense in depth against symlink escapes: the resolved real path must
        // still live under the (already canonical) root.
        match full.canonicalize() {
            Ok(real) if real.starts_with(&self.root) => Some(real),
            _ => None,
        }
    }
}

impl ResourceSource for DirectorySource {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        let full = self.resolve(path)?;
        std::fs::read(full).ok()
    }

    fn list(&self, prefix: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let file_type = match entry.file_type() {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                let path = entry.path();
                if file_type.is_dir() {
                    stack.push(path);
                } else if let Some(rel) = relative_pack_path(&self.root, &path)
                    && rel.starts_with(prefix)
                {
                    out.push(rel);
                }
            }
        }
        out
    }
}

/// Renders a filesystem path relative to `root` as a forward-slash in-pack path.
fn relative_pack_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(s) => parts.push(s.to_str()?.to_owned()),
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

impl ZipSource {
    /// Opens a zip/jar archive from a file on disk.
    ///
    /// Filesystem-backed, so it is gated on `cfg(not(target_arch = "wasm32"))`.
    /// A wasm/browser build uses [`from_bytes`](ZipSource::from_bytes) instead,
    /// feeding it pack bytes acquired over `fetch`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AssetError> {
        let path = path.as_ref();
        let bytes = std::fs::read(path).map_err(|source| AssetError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_bytes(bytes).map_err(|e| match e {
            AssetError::Zip { source, .. } => AssetError::Zip {
                path: path.display().to_string(),
                source,
            },
            other => other,
        })
    }
}
