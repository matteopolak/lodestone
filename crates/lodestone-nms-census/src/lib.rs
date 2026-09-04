//! # The NMS reference census
//!
//! ## What it is
//!
//! A pure-Rust scanner that reads a `.jar`, walks every class file's constant
//! pool and static bytecode, and reports every member of a target package
//! — by default `net/minecraft/` — that the jar actually uses.
//! It is the measurement that makes the Java-plugin bridge estimable:
//! `docs/java-plugin-bridge.md` explains what the number decides.
//!
//! No JVM, no `javap`, no decompiler. The constant pool retains symbolic
//! references required for descriptors and bootstrap sites, while a bounds-
//! checked bytecode walk counts member instructions. That matters practically
//! as well as aesthetically: this host has no Java runtime, so a census that
//! needed one could not be run here at all.
//!
//! ## How it works
//!
//! [`Census::scan_jar`] opens the archive and, for every `.class` entry,
//! parses the constant pool and `Code` attributes ([`classfile`]) and records
//! four populations:
//!
//! | population | source | what it answers |
//! |---|---|---|
//! | [`Census::members`] | `get*`, `put*`, and `invoke*` instruction sites | the statically encoded methods and field directions the bridge must back |
//! | [`Census::symbolic_members`] | `Fieldref`/`Methodref`/`InterfaceMethodref` pool entries | references preserved for descriptor/bootstrap context, but not proof of a use |
//! | [`Census::types`] | `CONSTANT_Class` | the classes that must *exist* — `new`, casts, `catch`, `instanceof` |
//! | [`Census::descriptor_types`] | object types inside every descriptor | the classes that must exist to make a signature loadable |
//!
//! The four are separate because they are separate obligations. A class named
//! only in a descriptor still has to be loadable or the *referring* method
//! fails verification, but it needs no method bodies; a class with 4,000 member
//! instruction sites is where the work is. The static-site table records field directions so
//! a real field write cannot be mistaken for a read.
//!
//! ### Two traps that were measured, not guessed
//!
//! **Jars nest.** The Mojang server jar is a *bundler*: its top level holds
//! four `net/minecraft/bundler/Main*` classes and the real server as a nested
//! jar under `META-INF/versions/`. Scanning it without recursion finds 4
//! classes where recursion finds tens of thousands. Paper ships the same shape
//! (a "paperclip" launcher wrapping the patched server), so recursion is a
//! requirement for the real census, not a nicety. [`ScanOptions::recurse_jars`]
//! controls it and defaults to on. `crates/lodestone-nms-census/tests/vanilla_jar.rs`
//! records the pinned Paper server's static-site baseline; recursion is
//! separately available to compare launcher-style nested archives.
//!
//! **Who refers matters more than what is referred to.** In a Paper jar,
//! `net.minecraft` is present *and* referenced by `net.minecraft` itself. Those
//! internal references are not work: they are calls within the layer the bridge
//! replaces wholesale. The surface that must be *implemented* is what the
//! non-`net.minecraft` classes — `org.bukkit.craftbukkit.*`, `io.papermc.*` —
//! reach for. [`MemberStat`] therefore splits every count into
//! [`MemberStat::external`] and a total, and [`Census::external_members`]
//! reports the former. Collapsing the two would overstate the surface by
//! roughly the ratio of engine to bridge code.
//!
//! ## How to change it
//!
//! - The target package is [`ScanOptions::target_prefix`], in **internal form**
//!   with a trailing slash (`net/minecraft/`), because that is how a constant
//!   pool spells it. A prefix without the slash would also match
//!   `net/minecraftforge/`.
//! - [`ScanOptions::internal_prefixes`] is what "external" means; anything a
//!   caller considers part of the replaced layer belongs there.
//! - A class file that fails to parse is **counted, not swallowed**
//!   ([`Census::parse_failures`]). A scanner that silently skipped unreadable
//!   classes would report a low number for a jar it could not read, which is
//!   the failure mode that looks like good news.
//!
//! ## Configuration
//!
//! None at build time. The binary (`nms-census`) takes the jar path and flags;
//! see its `--help`.
//!
//! ## Dependencies
//!
//! `zip` for the archive, `anyhow` for the binary's error reporting. Both are
//! already workspace dependencies. Deliberately **not** `jni` and nothing that
//! links `libjvm`: this crate must run on a machine with no Java at all.

pub mod classfile;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use std::path::Path;

use classfile::{ClassFile, MemberUseKind, RefKind};

/// How a scan treats packages and nesting.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Internal-form package prefix to census, **with** its trailing slash.
    ///
    /// The slash is load-bearing: `net/minecraft` without it also matches
    /// `net/minecraftforge/`.
    pub target_prefix: String,
    /// Prefixes whose classes count as part of the layer being replaced, so a
    /// reference *from* one of them is internal rather than external.
    pub internal_prefixes: Vec<String>,
    /// Descend into `.jar` entries found inside the archive.
    ///
    /// On by default because both the Mojang bundler and Paper's paperclip
    /// launcher hide the real classes one level down.
    pub recurse_jars: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            target_prefix: "net/minecraft/".to_owned(),
            internal_prefixes: vec!["net/minecraft/".to_owned()],
            recurse_jars: true,
        }
    }
}

/// A member of the target package, as the census keys it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MemberKey {
    /// Owning class in internal form, e.g. `net/minecraft/world/level/Level`.
    pub class: String,
    /// Member name, e.g. `getBlockState` or `<init>`.
    pub name: String,
    /// JVM descriptor, e.g. `(Lnet/minecraft/core/BlockPos;)Lnet/minecraft/world/level/block/state/BlockState;`.
    pub descriptor: String,
    /// Field direction or invocation opcode.
    pub kind: MemberUseKind,
}

/// A member mentioned in a constant pool, regardless of whether an
/// instruction uses it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SymbolicMemberKey {
    /// Owning class in internal form.
    pub class: String,
    /// Member name.
    pub name: String,
    /// Member descriptor.
    pub descriptor: String,
    /// Field, method, or interface-method pool-reference kind.
    pub kind: RefKind,
}

/// How often a member is referenced, split by whether the referring class is
/// part of the layer being replaced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemberStat {
    /// References from classes matching [`ScanOptions::internal_prefixes`].
    pub internal: u64,
    /// References from every other class — the ones the bridge must satisfy.
    pub external: u64,
}

impl MemberStat {
    /// Total references, both kinds.
    #[must_use]
    pub const fn total(self) -> u64 {
        self.internal + self.external
    }
}

/// The result of scanning one or more jars.
#[derive(Debug, Clone, Default)]
pub struct Census {
    /// Executable member uses into the target package.
    pub members: BTreeMap<MemberKey, MemberStat>,
    /// Symbolic pool member references into the target package. A member here
    /// but absent from [`Census::members`] can be bootstrap-only or otherwise
    /// unexecuted by this class's methods.
    pub symbolic_members: BTreeMap<SymbolicMemberKey, MemberStat>,
    /// `CONSTANT_Class` references into the target package — classes that must
    /// exist for a `new`, a cast, an `instanceof` or a `catch`.
    pub types: BTreeMap<String, MemberStat>,
    /// Target-package classes named inside a field or method descriptor.
    pub descriptor_types: BTreeMap<String, MemberStat>,
    /// Target-package classes the scanned jar **defines** itself.
    pub defined_target_classes: BTreeSet<String>,
    /// Every class file successfully parsed.
    pub classes_scanned: u64,
    /// Archives opened, including nested ones.
    pub jars_scanned: u64,
    /// Class entries that failed to parse, with the reason, capped for report
    /// size. The *count* is [`Census::parse_failure_count`].
    pub parse_failures: Vec<(String, String)>,
    parse_failure_count: u64,
}

/// The largest number of individual parse failures kept for reporting. The
/// count keeps rising past this; only the examples stop accumulating.
const MAX_RECORDED_FAILURES: usize = 32;

impl Census {
    /// Scan the jar at `path`.
    ///
    /// # Errors
    ///
    /// If the file cannot be opened or is not a readable zip archive. An
    /// individual *class* that fails to parse is recorded in
    /// [`Census::parse_failures`] rather than aborting the scan.
    pub fn scan_jar(path: &Path, options: &ScanOptions) -> Result<Self, ScanError> {
        let bytes = std::fs::read(path).map_err(|source| ScanError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let mut census = Self::default();
        census.scan_archive_bytes(&bytes, &path.display().to_string(), options, 0)?;
        Ok(census)
    }

    /// Number of class entries that could not be parsed.
    ///
    /// Reported alongside every result: a scanner that quietly skipped
    /// unreadable classes would print a small, confident, wrong census.
    #[must_use]
    pub const fn parse_failure_count(&self) -> u64 {
        self.parse_failure_count
    }

    /// Members with at least one *external* reference, ordered most-referenced
    /// first — the implementation order the bridge should follow.
    #[must_use]
    pub fn external_members(&self) -> Vec<(&MemberKey, MemberStat)> {
        let mut out: Vec<_> = self
            .members
            .iter()
            .filter(|(_, stat)| stat.external > 0)
            .map(|(key, stat)| (key, *stat))
            .collect();
        out.sort_by(|a, b| b.1.external.cmp(&a.1.external).then_with(|| a.0.cmp(b.0)));
        out
    }

    /// Symbolic members with at least one external referring class.
    #[must_use]
    pub fn external_symbolic_members(&self) -> Vec<(&SymbolicMemberKey, MemberStat)> {
        let mut out: Vec<_> = self
            .symbolic_members
            .iter()
            .filter(|(_, stat)| stat.external > 0)
            .map(|(key, stat)| (key, *stat))
            .collect();
        out.sort_by(|a, b| b.1.external.cmp(&a.1.external).then_with(|| a.0.cmp(b.0)));
        out
    }

    /// Distinct target-package classes carrying at least one external member
    /// reference.
    #[must_use]
    pub fn external_classes(&self) -> BTreeMap<&str, u64> {
        let mut out: BTreeMap<&str, u64> = BTreeMap::new();
        for (key, stat) in &self.members {
            if stat.external > 0 {
                *out.entry(key.class.as_str()).or_default() += stat.external;
            }
        }
        out
    }

    /// Depth guard for nested archives. Two levels covers bundler-in-jar and
    /// library-in-bundler; a jar nested deeper than this is far likelier to be
    /// a zip quine than a real classpath.
    const MAX_NESTING: u32 = 3;

    fn scan_archive_bytes(
        &mut self,
        bytes: &[u8],
        origin: &str,
        options: &ScanOptions,
        depth: u32,
    ) -> Result<(), ScanError> {
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bytes)).map_err(|source| ScanError::Zip {
                path: origin.to_owned(),
                source: source.to_string(),
            })?;
        self.jars_scanned += 1;
        // Nested archives are collected first and scanned after the loop: an
        // entry borrows the archive mutably, so recursing inside the loop would
        // hold that borrow across the recursive call.
        let mut nested: Vec<(String, Vec<u8>)> = Vec::new();
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(|source| ScanError::Zip {
                path: format!("{origin}[{i}]"),
                source: source.to_string(),
            })?;
            if !entry.is_file() {
                continue;
            }
            let name = entry.name().to_owned();
            let is_class = name.ends_with(".class");
            let is_jar = options.recurse_jars && name.ends_with(".jar");
            if !is_class && !is_jar {
                continue;
            }
            let mut buf = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
            if entry.read_to_end(&mut buf).is_err() {
                self.note_failure(&format!("{origin}!{name}"), "entry could not be decompressed");
                continue;
            }
            if is_jar {
                if depth < Self::MAX_NESTING {
                    nested.push((format!("{origin}!{name}"), buf));
                }
                continue;
            }
            self.scan_class_bytes(&buf, &format!("{origin}!{name}"), options);
        }
        for (nested_origin, nested_bytes) in nested {
            // A nested archive that will not open is a recorded failure rather
            // than a fatal one: a jar can legitimately carry a non-archive with
            // a `.jar` name, and one of those must not lose the whole census.
            if self
                .scan_archive_bytes(&nested_bytes, &nested_origin, options, depth + 1)
                .is_err()
            {
                self.note_failure(&nested_origin, "nested entry is not a readable archive");
            }
        }
        Ok(())
    }

    fn scan_class_bytes(&mut self, bytes: &[u8], origin: &str, options: &ScanOptions) {
        let class = match ClassFile::parse(bytes) {
            Ok(class) => class,
            Err(e) => {
                self.note_failure(origin, &e.to_string());
                return;
            }
        };
        let referrer = class.name().unwrap_or("").to_owned();
        let external = !options
            .internal_prefixes
            .iter()
            .any(|prefix| referrer.starts_with(prefix.as_str()));
        self.classes_scanned += 1;
        if referrer.starts_with(options.target_prefix.as_str()) {
            self.defined_target_classes.insert(referrer.clone());
        }

        let bump = |stat: &mut MemberStat| {
            if external {
                stat.external += 1;
            } else {
                stat.internal += 1;
            }
        };

        for (index, entry) in class.pool.iter() {
            match entry {
                classfile::Entry::Ref { .. } => {
                    let Ok(member) = class.pool.member_ref(index) else {
                        continue;
                    };
                    if member.class.starts_with(options.target_prefix.as_str()) {
                        let key = SymbolicMemberKey {
                            class: member.class.to_owned(),
                            name: member.name.to_owned(),
                            descriptor: member.descriptor.to_owned(),
                            kind: member.kind,
                        };
                        bump(self.symbolic_members.entry(key).or_default());
                    }
                    for named in descriptor_object_types(member.descriptor) {
                        if named.starts_with(options.target_prefix.as_str()) {
                            bump(self.descriptor_types.entry(named.to_owned()).or_default());
                        }
                    }
                }
                classfile::Entry::Class { .. } => {
                    let Ok(name) = class.pool.class_name(index) else {
                        continue;
                    };
                    // An array type appears as a descriptor (`[Lfoo/Bar;`)
                    // rather than a bare internal name; unwrap it so
                    // `ServerLevel[]` counts as a reference to `ServerLevel`.
                    let bare = name.trim_start_matches('[');
                    let bare = bare
                        .strip_prefix('L')
                        .and_then(|s| s.strip_suffix(';'))
                        .unwrap_or(bare);
                    if bare.starts_with(options.target_prefix.as_str()) {
                        bump(self.types.entry(bare.to_owned()).or_default());
                    }
                }
                classfile::Entry::NameAndType {
                    descriptor_index, ..
                } => {
                    // Reached for `invokedynamic`, whose call site is a
                    // `NameAndType` with no owning `Class`. The descriptor is
                    // still a real loadability obligation.
                    let Ok(descriptor) = class.pool.utf8(*descriptor_index) else {
                        continue;
                    };
                    for named in descriptor_object_types(descriptor) {
                        if named.starts_with(options.target_prefix.as_str()) {
                            bump(self.descriptor_types.entry(named.to_owned()).or_default());
                        }
                    }
                }
                _ => {}
            }
        }

        for use_ in class.member_uses() {
            let Ok(member) = class.pool.member_ref(use_.pool_index) else {
                // `ClassFile::parse` already validates every static instruction index.
                // Keep this defensive guard for callers constructing a future
                // class representation through another path.
                continue;
            };
            if member.class.starts_with(options.target_prefix.as_str()) {
                let key = MemberKey {
                    class: member.class.to_owned(),
                    name: member.name.to_owned(),
                    descriptor: member.descriptor.to_owned(),
                    kind: use_.kind,
                };
                bump(self.members.entry(key).or_default());
            }
        }
    }

    fn note_failure(&mut self, origin: &str, reason: &str) {
        self.parse_failure_count += 1;
        if self.parse_failures.len() < MAX_RECORDED_FAILURES {
            self.parse_failures
                .push((origin.to_owned(), reason.to_owned()));
        }
    }
}

/// Why a scan could not start.
#[derive(Debug)]
pub enum ScanError {
    /// The jar could not be read from disk.
    Io {
        /// What was being opened.
        path: String,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The bytes were not a readable zip archive.
    Zip {
        /// What was being opened.
        path: String,
        /// The underlying error, rendered.
        source: String,
    },
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "could not read {path}: {source}"),
            Self::Zip { path, source } => write!(f, "could not open {path} as a jar: {source}"),
        }
    }
}

impl std::error::Error for ScanError {}

/// Every object type named inside a field or method descriptor.
///
/// `(Lnet/minecraft/core/BlockPos;I)Lnet/minecraft/world/level/block/state/BlockState;`
/// yields `net/minecraft/core/BlockPos` and
/// `net/minecraft/world/level/block/state/BlockState`. Primitives, array
/// brackets and parentheses are skipped; an unterminated `L` at the end is
/// ignored rather than panicking, because a malformed descriptor must not take
/// the scan down.
#[must_use]
pub fn descriptor_object_types(descriptor: &str) -> Vec<&str> {
    let bytes = descriptor.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'L' {
            let start = i + 1;
            match descriptor[start..].find(';') {
                Some(offset) => {
                    out.push(&descriptor[start..start + offset]);
                    i = start + offset + 1;
                }
                None => break,
            }
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_method_descriptor_yields_its_object_parameters_and_return() {
        let found = descriptor_object_types(
            "(Lnet/minecraft/core/BlockPos;I[Lnet/minecraft/world/item/ItemStack;)\
             Lnet/minecraft/world/level/block/state/BlockState;",
        );
        assert_eq!(
            found,
            vec![
                "net/minecraft/core/BlockPos",
                "net/minecraft/world/item/ItemStack",
                "net/minecraft/world/level/block/state/BlockState",
            ]
        );
    }

    /// A primitive-only descriptor has no object types, and a `V` return must
    /// not be mistaken for one. Chosen because `(IJZ)V` contains no `L` at all,
    /// so a scanner keying on "not a primitive" rather than on `L` would report
    /// spurious entries here.
    #[test]
    fn a_primitive_descriptor_yields_nothing() {
        assert!(descriptor_object_types("(IJZ)V").is_empty());
    }

    /// A truncated descriptor must return what it could read rather than
    /// panicking or looping: a scan over 40,000 classes cannot afford to abort
    /// on one malformed constant.
    #[test]
    fn an_unterminated_object_type_is_dropped_not_fatal() {
        assert_eq!(
            descriptor_object_types("(Lnet/minecraft/core/BlockPos;Lbroken"),
            vec!["net/minecraft/core/BlockPos"]
        );
    }
}
