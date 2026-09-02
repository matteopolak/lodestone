//! The constant-pool parser against a class file built by hand from the
//! specification.
//!
//! # Why a hand-built fixture rather than a real class
//!
//! `CLAUDE.md`'s evidence standard: an expected value must originate **outside**
//! the code under test. A fixture produced by our own writer would satisfy
//! `decode(encode(x)) == x` under any pair of symmetric misunderstandings. There
//! is no Java on this host to compile a real class with, so the outside source
//! is the format specification itself (JVMS chapter 4), hand-expanded into bytes
//! here — the same route `CLAUDE.md` sanctions when a runtime is unavailable
//! ("the decompiled source read as a *record definition* and hand-expanded").
//!
//! The complementary evidence is `vanilla_jar.rs`, which runs the same parser
//! over tens of thousands of javac-produced classes in the real 26.2 server jar.
//! Neither alone is enough: this file proves the parser reads the shape the spec
//! describes, and that one proves the spec reading survives contact with a real
//! compiler.
//!
//! # The discriminating input
//!
//! The pool deliberately contains a `CONSTANT_Long`. JVMS 4.4.5 makes a `Long`
//! or `Double` occupy **two** pool slots, the second unusable — the single most
//! common way to get this format wrong, and one that does not error: a parser
//! that advances one slot per entry resolves every later index to its
//! *neighbour* and reports confident nonsense. A fixture with no `long` in it
//! passes under both the correct and the incorrect reading, so it would be
//! testing that the parser runs.
//!
//! Every index here is therefore chosen so the two readings disagree: with the
//! rule applied `utf8(9)` is a string and `this_class` resolves; without it,
//! index 9 is a `Class` and the whole tail shifts by one.

use lodestone_nms_census::classfile::{ClassFile, Entry, RefKind};
use lodestone_nms_census::{Census, ScanOptions, descriptor_object_types};

/// Assemble a class file whose constant pool exercises the entries the census
/// actually resolves, plus a `CONSTANT_Long` to force the two-slot rule.
///
/// Pool layout, chosen so a one-slot-per-entry reading disagrees at every index
/// from 8 onward:
///
/// | index | entry |
/// |---|---|
/// | 1 | `Utf8` `net/minecraft/world/level/Level` |
/// | 2 | `Class` → 1 |
/// | 3 | `Utf8` `getBlockState` |
/// | 4 | `Utf8` the descriptor |
/// | 5 | `NameAndType` → 3, 4 |
/// | 6 | `Long` — **and 7 is its dead second slot** |
/// | 8 | `Methodref` → class 2, name-and-type 5 |
/// | 9 | `Utf8` `org/example/Caller` |
/// | 10 | `Class` → 9 |
fn fixture_class() -> Vec<u8> {
    const DESCRIPTOR: &str = "(Lnet/minecraft/core/BlockPos;)\
                              Lnet/minecraft/world/level/block/state/BlockState;";
    let mut out = Vec::new();
    out.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    out.extend_from_slice(&65u16.to_be_bytes()); // major: Java 21

    // constant_pool_count is one MORE than the number of usable entries, and the
    // count includes the dead slot: 10 entries occupying slots 1..=10 means 11.
    out.extend_from_slice(&11u16.to_be_bytes());

    let utf8 = |out: &mut Vec<u8>, s: &str| {
        out.push(1);
        out.extend_from_slice(&u16::try_from(s.len()).expect("fixture string fits").to_be_bytes());
        out.extend_from_slice(s.as_bytes());
    };

    utf8(&mut out, "net/minecraft/world/level/Level"); // 1
    out.push(7); // 2: Class
    out.extend_from_slice(&1u16.to_be_bytes());
    utf8(&mut out, "getBlockState"); // 3
    utf8(&mut out, DESCRIPTOR); // 4
    out.push(12); // 5: NameAndType
    out.extend_from_slice(&3u16.to_be_bytes());
    out.extend_from_slice(&4u16.to_be_bytes());
    out.push(5); // 6: Long — eats slot 7 as well
    out.extend_from_slice(&0x0123_4567_89AB_CDEFu64.to_be_bytes());
    out.push(10); // 8: Methodref
    out.extend_from_slice(&2u16.to_be_bytes()); // class_index
    out.extend_from_slice(&5u16.to_be_bytes()); // name_and_type_index
    utf8(&mut out, "org/example/Caller"); // 9
    out.push(7); // 10: Class
    out.extend_from_slice(&9u16.to_be_bytes());

    out.extend_from_slice(&0x0021u16.to_be_bytes()); // access_flags: public super
    out.extend_from_slice(&10u16.to_be_bytes()); // this_class -> 10
    out.extend_from_slice(&0u16.to_be_bytes()); // super_class (unread)
    out.extend_from_slice(&0u16.to_be_bytes()); // interfaces_count
    out.extend_from_slice(&0u16.to_be_bytes()); // fields_count
    out.extend_from_slice(&0u16.to_be_bytes()); // methods_count
    out.extend_from_slice(&0u16.to_be_bytes()); // attributes_count
    out
}

#[test]
fn the_header_and_version_parse() {
    let class = ClassFile::parse(&fixture_class()).expect("fixture parses");
    assert_eq!(class.major, 65, "major version");
    assert_eq!(class.minor, 0, "minor version");
}

/// The load-bearing one: slot 7 must be dead, and every index after it must
/// still mean what the spec says it means.
#[test]
fn a_long_constant_consumes_two_pool_slots() {
    let class = ClassFile::parse(&fixture_class()).expect("fixture parses");

    assert!(
        matches!(class.pool.get(6), Some(Entry::Other)),
        "index 6 is the Long itself"
    );
    assert!(
        class.pool.get(7).is_none(),
        "index 7 is the Long's dead second slot and must not resolve — a parser \
         that advances one slot per entry would find the Methodref here"
    );
    assert_eq!(
        class.pool.utf8(9).expect("index 9 is a Utf8"),
        "org/example/Caller",
        "under a one-slot-per-entry reading index 9 would be the trailing Class"
    );
}

/// The three-link chase — `Methodref` → `Class` → `Utf8`, and `Methodref` →
/// `NameAndType` → two `Utf8`s — resolved end to end.
#[test]
fn a_method_reference_resolves_to_class_name_and_descriptor() {
    let class = ClassFile::parse(&fixture_class()).expect("fixture parses");
    let member = class.pool.member_ref(8).expect("index 8 is a Methodref");

    assert_eq!(member.kind, RefKind::Method);
    assert_eq!(member.class, "net/minecraft/world/level/Level");
    assert_eq!(member.name, "getBlockState");
    assert!(
        member.descriptor.starts_with("(Lnet/minecraft/core/BlockPos;)"),
        "descriptor was {:?}",
        member.descriptor
    );
    // `class_index` (2) and `name_and_type_index` (5) are deliberately
    // distinct and non-adjacent: two same-typed adjacent fields transpose
    // without a trace, and equal values would make a transposition invisible.
    assert_ne!(member.class, member.name);
}

/// The referrer is what splits "work the bridge must do" from "calls inside the
/// layer the bridge replaces", so reading it wrong silently rescales the whole
/// census.
#[test]
fn the_referring_class_is_read_from_this_class() {
    let class = ClassFile::parse(&fixture_class()).expect("fixture parses");
    assert_eq!(class.name().expect("this_class resolves"), "org/example/Caller");
}

/// Not a class file at all: the magic check must fire rather than the parser
/// wandering into a constant pool that is really zip data.
#[test]
fn a_non_class_file_is_refused_by_its_magic() {
    let err = ClassFile::parse(b"PK\x03\x04not a class").expect_err("must refuse");
    assert!(
        err.to_string().contains("not a class file"),
        "unexpected error: {err}"
    );
}

/// Truncation must be an error, never a partial pool reported as complete.
#[test]
fn a_truncated_class_file_is_an_error_not_a_short_census() {
    let full = fixture_class();
    let err = ClassFile::parse(&full[..full.len() / 2]).expect_err("must refuse");
    assert!(
        err.to_string().contains("truncated"),
        "unexpected error: {err}"
    );
}

/// The descriptor walk finds the types that only ever appear in a signature —
/// `BlockPos` here is a parameter and is named nowhere else in the pool.
#[test]
fn descriptor_types_are_found_where_no_class_constant_exists() {
    let class = ClassFile::parse(&fixture_class()).expect("fixture parses");
    let member = class.pool.member_ref(8).expect("Methodref");
    let named = descriptor_object_types(member.descriptor);
    assert!(
        named.contains(&"net/minecraft/core/BlockPos"),
        "BlockPos appears only inside the descriptor; got {named:?}"
    );
    assert!(
        !class
            .pool
            .iter()
            .any(|(i, _)| class.pool.class_name(i).is_ok_and(|n| n.contains("BlockPos"))),
        "premise: BlockPos must NOT have its own CONSTANT_Class, or this test \
         is not measuring the descriptor walk"
    );
}

/// End to end through a real zip: a jar carrying the fixture must produce a
/// census whose external count is 1, attributed to `org/example/Caller`.
///
/// This is the seam the two halves meet at — a parser that works and an archive
/// walk that never reaches it would leave both unit tests green.
#[test]
fn a_jar_containing_the_fixture_censuses_one_external_reference() {
    let dir = std::env::temp_dir().join(format!(
        "lodestone-nms-census-fixture-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let jar_path = dir.join("fixture.jar");

    let mut writer = zip::ZipWriter::new(std::fs::File::create(&jar_path).expect("create jar"));
    writer
        .start_file(
            "org/example/Caller.class",
            zip::write::SimpleFileOptions::default(),
        )
        .expect("start entry");
    std::io::Write::write_all(&mut writer, &fixture_class()).expect("write class");
    writer.finish().expect("finish jar");

    let census = Census::scan_jar(&jar_path, &ScanOptions::default()).expect("scan");

    assert_eq!(census.classes_scanned, 1, "one class in the jar");
    assert_eq!(census.parse_failure_count(), 0, "nothing failed to parse");

    let external = census.external_members();
    assert_eq!(
        external.len(),
        1,
        "exactly one net.minecraft member is referenced; got {external:?}"
    );
    let (key, stat) = external[0];
    assert_eq!(key.class, "net/minecraft/world/level/Level");
    assert_eq!(key.name, "getBlockState");
    assert_eq!(stat.external, 1);
    assert_eq!(
        stat.internal, 0,
        "the referrer is org/example/Caller, which is outside the replaced layer"
    );

    // The complement, and the reason the split exists: rerun with the referrer
    // itself declared internal and the same reference must move columns rather
    // than disappear.
    let census = Census::scan_jar(
        &jar_path,
        &ScanOptions {
            internal_prefixes: vec!["org/example/".to_owned()],
            ..ScanOptions::default()
        },
    )
    .expect("scan");
    assert!(
        census.external_members().is_empty(),
        "with the referrer declared internal there is no external surface left"
    );
    assert_eq!(
        census.members.values().map(|s| s.total()).sum::<u64>(),
        1,
        "the reference is still counted, just as internal"
    );

    std::fs::remove_file(&jar_path).ok();
}
