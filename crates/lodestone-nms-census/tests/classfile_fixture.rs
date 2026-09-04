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

use lodestone_nms_census::classfile::{ClassFile, Entry, MemberUseKind, RefKind};
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
/// | 11 | `Utf8` `Code` |
/// | 12–15 | a field reference |
/// | 16–17 | the one method's name and descriptor |
/// | 18–23 | an interface-method reference |
/// | 24–26 | an `InvokeDynamic` call site |
///
/// Its sole method encodes one pool member more than once. It
/// also carries an opcode-valued operand, a `tableswitch`, a `lookupswitch`,
/// and `wide iinc`: a scanner that searches raw bytes for instruction values
/// would overcount, while a fixed-width-only walker would lose alignment.
fn fixture_class() -> Vec<u8> {
    const DESCRIPTOR: &str = "(Lnet/minecraft/core/BlockPos;)\
                              Lnet/minecraft/world/level/block/state/BlockState;";
    let mut out = Vec::new();
    out.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    out.extend_from_slice(&65u16.to_be_bytes()); // major: Java 21

    // constant_pool_count is one MORE than the number of usable entries, and the
    // count includes the dead slot: 26 entries occupying slots 1..=26 means 27.
    out.extend_from_slice(&27u16.to_be_bytes());

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
    utf8(&mut out, "Code"); // 11
    utf8(&mut out, "isClientSide"); // 12
    utf8(&mut out, "Z"); // 13
    out.push(12); // 14: NameAndType
    out.extend_from_slice(&12u16.to_be_bytes());
    out.extend_from_slice(&13u16.to_be_bytes());
    out.push(9); // 15: Fieldref
    out.extend_from_slice(&2u16.to_be_bytes());
    out.extend_from_slice(&14u16.to_be_bytes());
    utf8(&mut out, "run"); // 16
    utf8(&mut out, "()V"); // 17
    utf8(&mut out, "net/minecraft/world/level/LevelAccessor"); // 18
    out.push(7); // 19: Class
    out.extend_from_slice(&18u16.to_be_bytes());
    utf8(&mut out, "tick"); // 20
    utf8(&mut out, "()V"); // 21
    out.push(12); // 22: NameAndType
    out.extend_from_slice(&20u16.to_be_bytes());
    out.extend_from_slice(&21u16.to_be_bytes());
    out.push(11); // 23: InterfaceMethodref
    out.extend_from_slice(&19u16.to_be_bytes());
    out.extend_from_slice(&22u16.to_be_bytes());
    utf8(&mut out, "bootstrapCall"); // 24
    out.push(12); // 25: NameAndType
    out.extend_from_slice(&24u16.to_be_bytes());
    out.extend_from_slice(&21u16.to_be_bytes());
    out.push(18); // 26: InvokeDynamic
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&25u16.to_be_bytes());

    out.extend_from_slice(&0x0021u16.to_be_bytes()); // access_flags: public super
    out.extend_from_slice(&10u16.to_be_bytes()); // this_class -> 10
    out.extend_from_slice(&0u16.to_be_bytes()); // super_class (unread)
    out.extend_from_slice(&0u16.to_be_bytes()); // interfaces_count
    out.extend_from_slice(&0u16.to_be_bytes()); // fields_count
    out.extend_from_slice(&1u16.to_be_bytes()); // methods_count
    out.extend_from_slice(&0x0009u16.to_be_bytes()); // public static
    out.extend_from_slice(&16u16.to_be_bytes()); // name: run
    out.extend_from_slice(&17u16.to_be_bytes()); // descriptor: ()V
    out.extend_from_slice(&1u16.to_be_bytes()); // attributes_count
    out.extend_from_slice(&11u16.to_be_bytes()); // attribute_name: Code
    let code = [
        0x10, 0xb6, // bipush 182: an operand that must not be treated as invokevirtual
        0xb2, 0x00, 0x0f, // getstatic field
        0xb2, 0x00, 0x0f, // same field, second static use
        0xb3, 0x00, 0x0f, // putstatic field
        0xb4, 0x00, 0x0f, // getfield field
        0xb5, 0x00, 0x0f, // putfield field
        0xb6, 0x00, 0x08, // invokevirtual method
        0xb6, 0x00, 0x08, // same method, second static use
        0xb9, 0x00, 0x17, 0x01, 0x00, // invokeinterface
        0xba, 0x00, 0x1a, 0x00, 0x00, // invokedynamic: no member operation
        // Offset 33 has two padding bytes, then default/low/high/one target.
        0xaa,
        0, 0, // padding
        0, 0, 0, 0, // default
        0, 0, 0, 0, // low
        0, 0, 0, 0, // high
        0, 0, 0, 0, // branch target
        0xc4, 0x84, 0x00, 0xb6, 0x00, 0xb9, // wide iinc with opcode-like operands
        // `lookupswitch` at offset 58 has one padding byte, default, one
        // pair-count, then one match/target pair.
        0xab,
        0, // padding
        0, 0, 0, 0, // default
        0, 0, 0, 1, // pair count
        0, 0, 0, 0xb6, // match: an opcode-looking payload byte
        0, 0, 0, 0, // branch target
        0xb7, 0x00, 0x08, // invokespecial after both variable-width controls
        0xb8, 0x00, 0x08, // invokestatic after both variable-width controls
        0xb1, // return
    ];
    let attribute_length = 2 + 2 + 4 + code.len() + 2 + 2;
    out.extend_from_slice(
        &u32::try_from(attribute_length)
            .expect("fixture Code attribute fits")
            .to_be_bytes(),
    );
    out.extend_from_slice(&2u16.to_be_bytes()); // max_stack
    out.extend_from_slice(&2u16.to_be_bytes()); // max_locals
    out.extend_from_slice(
        &u32::try_from(code.len())
            .expect("fixture code fits")
            .to_be_bytes(),
    );
    out.extend_from_slice(&code);
    out.extend_from_slice(&0u16.to_be_bytes()); // exception_table_length
    out.extend_from_slice(&0u16.to_be_bytes()); // nested attributes_count
    out.extend_from_slice(&0u16.to_be_bytes()); // attributes_count
    out
}

fn code_start(bytes: &[u8]) -> usize {
    bytes
        .windows(5)
        .position(|window| window == [0x10, 0xb6, 0xb2, 0x00, 0x0f])
        .expect("fixture Code start")
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

/// The largest legal `constant_pool_count` still has a finite final slot. A
/// two-slot entry there used to increment the `u16` index past its range in a
/// debug build; rejecting it before that increment is a format check, not a
/// build-mode accident.
#[test]
fn a_two_slot_constant_at_the_maximum_pool_boundary_is_refused() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&65u16.to_be_bytes());
    bytes.extend_from_slice(&u16::MAX.to_be_bytes());
    // Slots 1..=65,533 are valid one-slot integers. Slot 65,534 is the
    // malformed terminal Long and must fail before reading a nonexistent
    // second slot or overflowing the parser's 16-bit index.
    for _ in 1..u16::MAX - 1 {
        bytes.push(3);
        bytes.extend_from_slice(&0u32.to_be_bytes());
    }
    bytes.push(5);

    let err = ClassFile::parse(&bytes).expect_err("terminal Long must fail");
    assert!(
        err.to_string().contains("no declared second slot"),
        "unexpected error: {err}"
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

/// Pool membership is only a possible use. The static census must count
/// the two call instructions, four field-operation directions, and nothing
/// that merely resembles an opcode in an operand or switch payload.
#[test]
fn executable_bytecode_counts_each_instruction_and_field_direction() {
    let class = ClassFile::parse(&fixture_class()).expect("fixture parses");
    let uses = class.member_uses();

    assert_eq!(uses.len(), 10, "five calls plus five field operations: {uses:?}");
    assert_eq!(
        uses.iter()
            .filter(|use_| use_.kind == MemberUseKind::InvokeVirtual)
            .count(),
        2,
        "the one Methodref is invoked twice"
    );
    for kind in [
        MemberUseKind::InvokeSpecial,
        MemberUseKind::InvokeStatic,
        MemberUseKind::InvokeInterface,
    ] {
        assert_eq!(
            uses.iter().filter(|use_| use_.kind == kind).count(),
            1,
            "{kind:?} remains aligned after five-byte and variable-width instructions"
        );
    }
    for kind in [
        MemberUseKind::GetField,
        MemberUseKind::PutField,
        MemberUseKind::GetStatic,
        MemberUseKind::PutStatic,
    ] {
        let expected = usize::from(kind == MemberUseKind::GetStatic) + 1;
        assert_eq!(
            uses.iter().filter(|use_| use_.kind == kind).count(),
            expected,
            "{kind:?} count"
        );
    }
}

/// Reserved bytes are not instructions. A parser that treats unknown opcodes
/// as one-byte no-ops can drift into their operand payload and manufacture a
/// plausible but false member census.
#[test]
fn a_reserved_opcode_is_a_named_parse_failure() {
    let mut bytes = fixture_class();
    let at = code_start(&bytes);
    bytes[at] = 0xcb;
    let err = ClassFile::parse(&bytes).expect_err("reserved opcode must fail");
    assert!(
        err.to_string().contains("invalid bytecode opcode"),
        "unexpected error: {err}"
    );
}

/// A member-instruction opcode at the final code byte cannot borrow bytes from
/// the following exception table. The Code length is the hard boundary.
#[test]
fn a_truncated_instruction_is_a_named_parse_failure() {
    let mut bytes = fixture_class();
    let return_at = bytes.iter().rposition(|&byte| byte == 0xb1).expect("return");
    bytes[return_at] = 0xb6;
    let err = ClassFile::parse(&bytes).expect_err("incomplete invocation must fail");
    assert!(
        err.to_string().contains("truncated bytecode instruction"),
        "unexpected error: {err}"
    );
}

#[test]
fn a_tableswitch_with_high_below_low_is_a_named_parse_failure() {
    let mut bytes = fixture_class();
    let at = code_start(&bytes) + 33;
    // At this fixture offset the switch has two padding bytes, then default,
    // low, high. `low = 0`, `high = -1` is the smallest invalid range.
    bytes[at + 11..at + 15].copy_from_slice(&(-1i32).to_be_bytes());
    let err = ClassFile::parse(&bytes).expect_err("invalid table range must fail");
    assert!(
        err.to_string().contains("tableswitch high is below low"),
        "unexpected error: {err}"
    );
}

#[test]
fn a_lookupswitch_with_negative_pair_count_is_a_named_parse_failure() {
    let mut bytes = fixture_class();
    let at = code_start(&bytes) + 58;
    // One padding byte then default puts `npairs` at offset six.
    bytes[at + 6..at + 10].copy_from_slice(&(-1i32).to_be_bytes());
    let err = ClassFile::parse(&bytes).expect_err("negative pair count must fail");
    assert!(
        err.to_string().contains("lookupswitch pair count is negative"),
        "unexpected error: {err}"
    );
}

#[test]
fn a_wide_prefix_on_an_invocation_is_a_named_parse_failure() {
    let mut bytes = fixture_class();
    let at = bytes
        .windows(2)
        .position(|window| window == [0xc4, 0x84])
        .expect("wide iinc");
    bytes[at + 1] = 0xb6;
    let err = ClassFile::parse(&bytes).expect_err("wide invocation must fail");
    assert!(
        err.to_string().contains("cannot be widened"),
        "unexpected error: {err}"
    );
}

#[test]
fn five_byte_invocations_validate_reserved_operands_and_pool_kind() {
    let interface = [0xb9, 0x00, 0x17, 0x01, 0x00];
    let dynamic = [0xba, 0x00, 0x1a, 0x00, 0x00];

    let mut bad_interface = fixture_class();
    let at = bad_interface
        .windows(interface.len())
        .position(|window| window == interface)
        .expect("invokeinterface");
    bad_interface[at + 4] = 1;
    let err = ClassFile::parse(&bad_interface).expect_err("reserved byte must fail");
    assert!(
        err.to_string().contains("invokeinterface reserved byte"),
        "unexpected error: {err}"
    );

    let mut bad_dynamic = fixture_class();
    let at = bad_dynamic
        .windows(dynamic.len())
        .position(|window| window == dynamic)
        .expect("invokedynamic");
    bad_dynamic[at + 3] = 1;
    let err = ClassFile::parse(&bad_dynamic).expect_err("reserved bytes must fail");
    assert!(
        err.to_string().contains("invokedynamic reserved bytes"),
        "unexpected error: {err}"
    );

    let mut wrong_dynamic_kind = fixture_class();
    let at = wrong_dynamic_kind
        .windows(dynamic.len())
        .position(|window| window == dynamic)
        .expect("invokedynamic");
    wrong_dynamic_kind[at + 2] = 8;
    let err = ClassFile::parse(&wrong_dynamic_kind).expect_err("Methodref is not InvokeDynamic");
    assert!(
        err.to_string().contains("non-InvokeDynamic"),
        "unexpected error: {err}"
    );
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

/// End to end through a real zip: the static census counts repeated uses,
/// while the symbolic census retains one entry per pool reference.
///
/// This is the seam the two halves meet at — a parser that works and an archive
/// walk that never reaches it would leave both unit tests green.
#[test]
fn a_jar_containing_the_fixture_separates_executable_and_symbolic_references() {
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
        8,
        "each field direction is a separate static surface; got {external:?}"
    );
    let invoke = external
        .iter()
        .find(|(key, _)| key.kind == MemberUseKind::InvokeVirtual)
        .expect("virtual invocation is reported");
    assert_eq!(invoke.0.class, "net/minecraft/world/level/Level");
    assert_eq!(invoke.0.name, "getBlockState");
    assert_eq!(invoke.1.external, 2, "two bytecode invocations");
    let get_static = external
        .iter()
        .find(|(key, _)| key.kind == MemberUseKind::GetStatic)
        .expect("static read is reported");
    assert_eq!(get_static.1.external, 2, "two bytecode reads");
    assert_eq!(
        census.external_symbolic_members().len(),
        3,
        "pool membership remains separately visible"
    );
    assert_eq!(
        invoke.1.internal,
        0,
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
        10,
        "the static uses are still counted, just as internal"
    );

    std::fs::remove_file(&jar_path).ok();
}
