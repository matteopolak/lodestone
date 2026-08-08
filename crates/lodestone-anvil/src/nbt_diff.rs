//! Canonical, path-reporting structural comparison of two NBT trees.
//!
//! # What it is
//!
//! The comparison primitive the world-save parity gate reports through
//! (`tests/vanilla_save_parity.rs`, issue
//! [#437](https://github.com/matteopolak/lodestone/issues/437)'s
//! both-directions standard). Given two [`lodestone_core::Nbt`] trees it
//! yields one [`Difference`] per differing leaf, each carrying the **full NBT
//! path** to that leaf — `Level.sections[3].block_states.palette[7].Name`,
//! not "chunks differ".
//!
//! # Why a byte compare cannot do this job
//!
//! Four independent reasons, each measured rather than assumed, and each one
//! on its own is enough to make a `.mca`-versus-`.mca` byte diff report a
//! difference for two files with identical *content*:
//!
//! | property | consequence for a byte compare |
//! |---|---|
//! | **Compound field order is not part of the value.** `write_named_nbt` emits [`Nbt::Compound`]'s `Vec` in its in-memory order, and nothing in the format constrains a writer's choice. | two writers agreeing on every field still produce different bytes |
//! | **Region chunk payloads are compressed**, under whatever `region-file-compression` the *writer* was configured with, at whatever zlib level it chose. | identical NBT compresses to different bytes |
//! | **Sector placement depends on write order**, not on content ([`crate::region::build_region`]'s first-fit allocator, and vanilla's `RegionBitmap`). | the same chunks land at different offsets |
//! | **The block-state `data` array's bit width is a function of palette length**, so a writer that orders its palette differently packs different `long`s for the same blocks. | see the note below — this one needs *semantic* decode, not just canonical NBT |
//!
//! So [`canonical`] sorts every compound's fields by key, and [`diff`]
//! compares compounds as key sets rather than as sequences. That handles the
//! first row. The other three are handled by comparing *decoded chunk NBT*
//! rather than file bytes — which is what the gate does.
//!
//! **This module deliberately knows nothing about the chunk schema.** It is
//! [`Nbt`]-in, paths-out. Per issue #298's stated trap (and
//! [`crate::region`]'s module doc), the container crate must not grow a
//! dependency on chunk internals; the palette/bit-storage decode the parity
//! gate needs lives in the gate, not here.
//!
//! # How it works
//!
//! [`diff`] walks both trees together:
//!
//! - **Compounds** are matched by field name. A field present on one side
//!   only becomes [`DifferenceKind::Added`] or [`DifferenceKind::Removed`] —
//!   the distinction is load-bearing, because "vanilla added a field we omit"
//!   is a legitimate, allowlistable behaviour while "vanilla dropped a field
//!   we wrote" is a data-loss defect, and a differ that reported both as
//!   "differs" could not tell them apart.
//! - **Lists and arrays** are compared element-wise by index, and a length
//!   mismatch is reported *once* at the list's own path rather than as N
//!   spurious per-element differences. NBT list order is part of the value,
//!   so this is correct in general; where a *schema* makes a list a set (a
//!   chunk's `sections`, `block_entities`), the caller normalizes first with
//!   [`sort_list_by_fields`].
//! - **Tag changes** (`Int` where the other side has `Long`) are
//!   [`DifferenceKind::TypeChanged`], never silently coerced. A writer that
//!   narrows a `Long` seed to an `Int` is a real defect this crate has
//!   already been bitten by (see [`crate::world_gen_settings`]'s
//!   `from_seed_carries_a_full_64_bit_seed`).
//!
//! Paths are built as `field`, `field.sub`, `field[7]`, `field[7].sub` — the
//! form a human can paste back into a debugger, with no leading separator.
//!
//! # How to change it, and the gotchas
//!
//! - **Float leaves compare on their bit patterns, not with `==`.** This is
//!   deliberate and it is *not* what [`Nbt`]'s own `PartialEq` does, which is
//!   IEEE equality. IEEE is wrong in both directions for a save format:
//!   `0.0 == -0.0` is **true**, so a writer that flipped a sign bit would go
//!   unreported, and `NaN == NaN` is **false**, so two byte-identical files
//!   carrying a `NaN` would be reported as differing. A save format is a byte
//!   channel; `to_bits()` is the comparison that matches it.
//!
//!   (This was found by the control `float_comparison_is_exact`, which was
//!   originally written asserting a `-0.0`/`0.0` difference that the
//!   `PartialEq`-based implementation did not report. The doc had claimed
//!   bit-exactness the code did not have.)
//! - **Do not make float comparison approximate.** A tolerance here would be
//!   exactly the "widening the tolerance is cutting the wire" failure
//!   CLAUDE.md warns about. If a caller genuinely has a legitimate float
//!   drift, it allowlists the *path*.
//! - **Keep the output ordered and stable.** [`diff`] emits differences in
//!   sorted-key, ascending-index order so a failure message is diffable
//!   across runs. A `HashMap` here would make every failure look different.
//! - **[`canonical`] is not needed for [`diff`] to be correct** — `diff`
//!   already matches compounds by name. It exists for *display*: printing a
//!   canonicalized subtree next to a path makes two trees visually
//!   comparable. Do not "optimize" it away on the grounds that `diff` does
//!   not call it.
//! - **A truncating cap on the reported difference count belongs in the
//!   caller, not here.** A differ that silently stopped at the first N
//!   differences would turn "we lost 15,000 blocks" into "we lost 20" — the
//!   shape CLAUDE.md records as a `diff | grep -c` control reporting 0 where
//!   the truth was ~15,000.
//!
//! # Configuration
//!
//! None.
//!
//! # Dependencies
//!
//! [`lodestone_core`]'s [`Nbt`] tree only. No I/O, no compression, no
//! filesystem.

use lodestone_core::Nbt;

/// Which side of the comparison a one-sided difference sits on, and how a
/// two-sided one differs.
///
/// The `Added`/`Removed` split is the whole reason this is an enum rather
/// than a pair of strings: a save-parity allowlist has to be able to permit
/// "the reference implementation added a field we deliberately omit" without
/// simultaneously permitting "the reference implementation dropped a field we
/// wrote", and those are the same *path*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DifferenceKind {
    /// Present on the right only — i.e. the second tree gained it.
    Added {
        /// A short rendering of the value that appeared.
        right: String,
    },
    /// Present on the left only — i.e. the second tree lost it.
    Removed {
        /// A short rendering of the value that vanished.
        left: String,
    },
    /// Present on both, but with different NBT tags.
    TypeChanged {
        /// The left side's tag name.
        left: &'static str,
        /// The right side's tag name.
        right: &'static str,
    },
    /// Present on both with the same tag, but different payloads.
    ValueChanged {
        /// A short rendering of the left payload.
        left: String,
        /// A short rendering of the right payload.
        right: String,
    },
    /// A list or array whose element counts differ. Reported once, at the
    /// list's own path, *instead of* per-element differences — see the module
    /// doc.
    LengthChanged {
        /// The left side's element count.
        left: usize,
        /// The right side's element count.
        right: usize,
    },
}

impl DifferenceKind {
    /// A stable one-word label, useful for grouping a long failure report.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Added { .. } => "added",
            Self::Removed { .. } => "removed",
            Self::TypeChanged { .. } => "type-changed",
            Self::ValueChanged { .. } => "value-changed",
            Self::LengthChanged { .. } => "length-changed",
        }
    }
}

impl std::fmt::Display for DifferenceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Added { right } => write!(f, "added: {right}"),
            Self::Removed { left } => write!(f, "removed: was {left}"),
            Self::TypeChanged { left, right } => write!(f, "type changed: {left} -> {right}"),
            Self::ValueChanged { left, right } => write!(f, "value changed: {left} -> {right}"),
            Self::LengthChanged { left, right } => {
                write!(f, "length changed: {left} -> {right} elements")
            }
        }
    }
}

/// One differing leaf, with the full NBT path that reaches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    /// Dotted/indexed path from the root, e.g.
    /// `sections[3].block_states.palette[7].Name`. Empty for a difference at
    /// the root itself.
    pub path: String,
    /// What differs.
    pub kind: DifferenceKind,
}

impl std::fmt::Display for Difference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let path = if self.path.is_empty() {
            "<root>"
        } else {
            &self.path
        };
        write!(f, "{path}: {}", self.kind)
    }
}

/// The NBT tag name of a value, for [`DifferenceKind::TypeChanged`].
fn tag_name(nbt: &Nbt) -> &'static str {
    match nbt {
        Nbt::End => "End",
        Nbt::Byte(_) => "Byte",
        Nbt::Short(_) => "Short",
        Nbt::Int(_) => "Int",
        Nbt::Long(_) => "Long",
        Nbt::Float(_) => "Float",
        Nbt::Double(_) => "Double",
        Nbt::ByteArray(_) => "ByteArray",
        Nbt::String(_) => "String",
        Nbt::List { .. } => "List",
        Nbt::Compound(_) => "Compound",
        Nbt::IntArray(_) => "IntArray",
        Nbt::LongArray(_) => "LongArray",
    }
}

/// A short, single-line rendering of a value, for a failure message.
///
/// Containers are summarized by shape rather than dumped: a 256-`long`
/// `block_states.data` array printed in full would bury every other
/// difference in the report, and the *path* is what identifies it.
#[must_use]
pub fn summarize(nbt: &Nbt) -> String {
    match nbt {
        Nbt::End => "End".to_string(),
        Nbt::Byte(v) => format!("Byte({v})"),
        Nbt::Short(v) => format!("Short({v})"),
        Nbt::Int(v) => format!("Int({v})"),
        Nbt::Long(v) => format!("Long({v})"),
        Nbt::Float(v) => format!("Float({v})"),
        Nbt::Double(v) => format!("Double({v})"),
        Nbt::ByteArray(v) => format!("ByteArray[{}]", v.len()),
        Nbt::String(v) => format!("String({v:?})"),
        Nbt::List { elements, .. } => format!("List[{}]", elements.len()),
        Nbt::Compound(fields) => {
            let mut names: Vec<&str> = fields.iter().map(|(name, _)| name.as_str()).collect();
            names.sort_unstable();
            format!("Compound{{{}}}", names.join(","))
        }
        Nbt::IntArray(v) => format!("IntArray[{}]", v.len()),
        Nbt::LongArray(v) => format!("LongArray[{}]", v.len()),
    }
}

/// Returns `nbt` with every compound's fields sorted by name, recursively.
///
/// Compound field order is not part of an NBT value (see the module doc), so
/// this is the form in which two trees from two different writers are
/// visually comparable. [`diff`] does not need it — it matches compounds by
/// name — but a failure report that prints subtrees does.
#[must_use]
pub fn canonical(nbt: &Nbt) -> Nbt {
    match nbt {
        Nbt::Compound(fields) => {
            let mut sorted: Vec<(String, Nbt)> = fields
                .iter()
                .map(|(name, value)| (name.clone(), canonical(value)))
                .collect();
            sorted.sort_by(|(a, _), (b, _)| a.cmp(b));
            Nbt::Compound(sorted)
        }
        Nbt::List {
            element_type,
            elements,
        } => Nbt::List {
            element_type: *element_type,
            elements: elements.iter().map(canonical).collect(),
        },
        other => other.clone(),
    }
}

/// Sorts a `List<Compound>` in place by the named fields, in order, so two
/// writers that emitted the same *set* of elements in different orders
/// compare equal.
///
/// Needed because two chunk-NBT lists are semantically sets keyed by their
/// own contents rather than sequences — `sections` (keyed by `Y`) and
/// `block_entities` (keyed by `x`/`y`/`z`) — and nothing in the format makes
/// a writer emit them in any particular order. Without this, a differ
/// comparing index-wise would report every element of a reordered list as
/// changed.
///
/// A field missing from an element, or holding a non-numeric non-string tag,
/// sorts before every present one; elements are otherwise left in their
/// original relative order (the sort is stable), so a list this cannot key is
/// degraded rather than scrambled.
///
/// Does nothing if `nbt` is not a `List`.
pub fn sort_list_by_fields(nbt: &mut Nbt, fields: &[&str]) {
    let Nbt::List { elements, .. } = nbt else {
        return;
    };
    elements.sort_by_key(|element| sort_key(element, fields));
}

/// Sort key for one list element: each requested field rendered as an
/// orderable value.
fn sort_key(element: &Nbt, fields: &[&str]) -> Vec<(i8, i64, String)> {
    fields
        .iter()
        .map(|field| {
            let value = match element {
                Nbt::Compound(entries) => entries
                    .iter()
                    .find(|(name, _)| name == field)
                    .map(|(_, value)| value),
                _ => None,
            };
            match value {
                // Rank 1: numeric, ordered by the number. All the integral
                // tags widen to `i64` losslessly, so `Y: Byte(-4)` and
                // `y: Int(-59)` sort against each other correctly.
                Some(Nbt::Byte(v)) => (1, i64::from(*v), String::new()),
                Some(Nbt::Short(v)) => (1, i64::from(*v), String::new()),
                Some(Nbt::Int(v)) => (1, i64::from(*v), String::new()),
                Some(Nbt::Long(v)) => (1, *v, String::new()),
                // Rank 2: string, ordered lexically.
                Some(Nbt::String(v)) => (2, 0, v.clone()),
                // Rank 0: absent or unorderable — sorts first, stably.
                _ => (0, 0, String::new()),
            }
        })
        .collect()
}

/// Compares two NBT trees structurally and returns one [`Difference`] per
/// differing leaf.
///
/// An empty result means the two trees are equal *as values* — same fields,
/// same tags, same payloads, same list orders — regardless of compound field
/// order or of how either was encoded.
///
/// Differences come back in a stable order (sorted field name, then ascending
/// index) so two runs produce diffable reports.
#[must_use]
pub fn diff(left: &Nbt, right: &Nbt) -> Vec<Difference> {
    let mut out = Vec::new();
    walk("", left, right, &mut out);
    out
}

/// Appends `segment` to `prefix` with the right separator, given that a
/// root-level field takes no leading `.`.
fn child_path(prefix: &str, segment: &str) -> String {
    if prefix.is_empty() {
        segment.to_string()
    } else {
        format!("{prefix}.{segment}")
    }
}

fn walk(path: &str, left: &Nbt, right: &Nbt, out: &mut Vec<Difference>) {
    if tag_name(left) != tag_name(right) {
        out.push(Difference {
            path: path.to_string(),
            kind: DifferenceKind::TypeChanged {
                left: tag_name(left),
                right: tag_name(right),
            },
        });
        return;
    }

    match (left, right) {
        (Nbt::Compound(left_fields), Nbt::Compound(right_fields)) => {
            // Union of both key sets, sorted, so the report is stable and a
            // field present on one side only is visible rather than skipped
            // by iterating just one side.
            let mut names: Vec<&str> = left_fields
                .iter()
                .map(|(name, _)| name.as_str())
                .chain(right_fields.iter().map(|(name, _)| name.as_str()))
                .collect();
            names.sort_unstable();
            names.dedup();

            for name in names {
                let child = child_path(path, name);
                let left_value = left_fields
                    .iter()
                    .find(|(field, _)| field == name)
                    .map(|(_, value)| value);
                let right_value = right_fields
                    .iter()
                    .find(|(field, _)| field == name)
                    .map(|(_, value)| value);
                match (left_value, right_value) {
                    (Some(l), Some(r)) => walk(&child, l, r, out),
                    (None, Some(r)) => out.push(Difference {
                        path: child,
                        kind: DifferenceKind::Added {
                            right: summarize(r),
                        },
                    }),
                    (Some(l), None) => out.push(Difference {
                        path: child,
                        kind: DifferenceKind::Removed { left: summarize(l) },
                    }),
                    (None, None) => unreachable!("name came from one of the two field lists"),
                }
            }
        }

        (
            Nbt::List {
                element_type: left_type,
                elements: left_elements,
            },
            Nbt::List {
                element_type: right_type,
                elements: right_elements,
            },
        ) => {
            // An empty list's element type is a writer's free choice —
            // `chunk_nbt` writes `End` for an empty `block_entities` while
            // vanilla may write the element tag — so it is only compared for
            // a list that actually has elements.
            if left_type != right_type && !left_elements.is_empty() && !right_elements.is_empty() {
                out.push(Difference {
                    path: child_path(path, "<element_type>"),
                    kind: DifferenceKind::ValueChanged {
                        left: format!("{left_type:?}"),
                        right: format!("{right_type:?}"),
                    },
                });
            }
            if left_elements.len() != right_elements.len() {
                out.push(Difference {
                    path: path.to_string(),
                    kind: DifferenceKind::LengthChanged {
                        left: left_elements.len(),
                        right: right_elements.len(),
                    },
                });
                return;
            }
            for (index, (l, r)) in left_elements.iter().zip(right_elements).enumerate() {
                walk(&format!("{path}[{index}]"), l, r, out);
            }
        }

        (Nbt::ByteArray(l), Nbt::ByteArray(r)) => compare_array(path, l, r, out),
        (Nbt::IntArray(l), Nbt::IntArray(r)) => compare_array(path, l, r, out),
        (Nbt::LongArray(l), Nbt::LongArray(r)) => compare_array(path, l, r, out),

        // Floats compare on bits rather than through `PartialEq`, because IEEE
        // equality is wrong in both directions here — see the module doc.
        (Nbt::Float(l), Nbt::Float(r)) => {
            if l.to_bits() != r.to_bits() {
                out.push(Difference {
                    path: path.to_string(),
                    kind: DifferenceKind::ValueChanged {
                        left: format!("Float({l} bits {:#010x})", l.to_bits()),
                        right: format!("Float({r} bits {:#010x})", r.to_bits()),
                    },
                });
            }
        }
        (Nbt::Double(l), Nbt::Double(r)) => {
            if l.to_bits() != r.to_bits() {
                out.push(Difference {
                    path: path.to_string(),
                    kind: DifferenceKind::ValueChanged {
                        left: format!("Double({l} bits {:#018x})", l.to_bits()),
                        right: format!("Double({r} bits {:#018x})", r.to_bits()),
                    },
                });
            }
        }

        // Every remaining pair is a same-tag integral or string scalar, for
        // which `PartialEq` on `Nbt` is exact.
        (l, r) => {
            if l != r {
                out.push(Difference {
                    path: path.to_string(),
                    kind: DifferenceKind::ValueChanged {
                        left: summarize(l),
                        right: summarize(r),
                    },
                });
            }
        }
    }
}

fn compare_array<T: PartialEq + std::fmt::Display>(
    path: &str,
    left: &[T],
    right: &[T],
    out: &mut Vec<Difference>,
) {
    if left.len() != right.len() {
        out.push(Difference {
            path: path.to_string(),
            kind: DifferenceKind::LengthChanged {
                left: left.len(),
                right: right.len(),
            },
        });
        return;
    }
    for (index, (l, r)) in left.iter().zip(right).enumerate() {
        if l != r {
            out.push(Difference {
                path: format!("{path}[{index}]"),
                kind: DifferenceKind::ValueChanged {
                    left: l.to_string(),
                    right: r.to_string(),
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_core::NbtTag;

    fn compound(fields: &[(&str, Nbt)]) -> Nbt {
        Nbt::Compound(
            fields
                .iter()
                .map(|(name, value)| ((*name).to_string(), value.clone()))
                .collect(),
        )
    }

    fn list(element_type: NbtTag, elements: Vec<Nbt>) -> Nbt {
        Nbt::List {
            element_type,
            elements,
        }
    }

    #[test]
    fn identical_trees_have_no_differences() {
        let tree = compound(&[
            ("DataVersion", Nbt::Int(4903)),
            ("xPos", Nbt::Int(-1)),
            (
                "sections",
                list(
                    NbtTag::Compound,
                    vec![compound(&[
                        ("Y", Nbt::Byte(-4)),
                        ("data", Nbt::LongArray(vec![1, 2, 3])),
                    ])],
                ),
            ),
        ]);
        assert_eq!(diff(&tree, &tree), Vec::new());
    }

    #[test]
    fn compound_field_order_is_not_a_difference() {
        // The property that makes a byte compare unusable and this differ
        // usable. Two writers emitting the same fields in opposite orders
        // must compare equal.
        let forward = compound(&[("a", Nbt::Int(1)), ("b", Nbt::Int(2))]);
        let reverse = compound(&[("b", Nbt::Int(2)), ("a", Nbt::Int(1))]);
        assert_ne!(
            forward, reverse,
            "control: `Nbt`'s own `PartialEq` IS order-sensitive, so this differ \
             is doing real work rather than restating `==`"
        );
        assert_eq!(diff(&forward, &reverse), Vec::new());
    }

    #[test]
    fn a_changed_leaf_is_reported_at_its_exact_path() {
        // The whole point of the module: not "trees differ" but the path.
        let left = compound(&[(
            "sections",
            list(
                NbtTag::Compound,
                vec![
                    compound(&[("Y", Nbt::Byte(0))]),
                    compound(&[(
                        "block_states",
                        compound(&[(
                            "palette",
                            list(
                                NbtTag::Compound,
                                vec![
                                    compound(&[("Name", Nbt::String("minecraft:air".into()))]),
                                    compound(&[("Name", Nbt::String("minecraft:stone".into()))]),
                                ],
                            ),
                        )]),
                    )]),
                ],
            ),
        )]);
        let mut right = left.clone();
        // Reach in and change exactly one leaf: sections[1] palette[1] Name.
        if let Nbt::Compound(fields) = &mut right {
            if let Nbt::List { elements, .. } = &mut fields[0].1 {
                if let Nbt::Compound(section) = &mut elements[1] {
                    if let Nbt::Compound(states) = &mut section[0].1 {
                        if let Nbt::List { elements, .. } = &mut states[0].1 {
                            if let Nbt::Compound(entry) = &mut elements[1] {
                                entry[0].1 = Nbt::String("minecraft:deepslate".into());
                            }
                        }
                    }
                }
            }
        }

        let differences = diff(&left, &right);
        assert_eq!(differences.len(), 1, "{differences:#?}");
        assert_eq!(
            differences[0].path,
            "sections[1].block_states.palette[1].Name",
            "the path must locate the leaf, not the tree"
        );
        assert!(matches!(
            differences[0].kind,
            DifferenceKind::ValueChanged { .. }
        ));
    }

    #[test]
    fn added_and_removed_are_distinguished() {
        // Load-bearing for the parity allowlist: "vanilla added a field we
        // omit" is allowlistable, "vanilla dropped a field we wrote" is data
        // loss, and both are the same path.
        let ours = compound(&[("Status", Nbt::String("minecraft:full".into()))]);
        let theirs = compound(&[
            ("Status", Nbt::String("minecraft:full".into())),
            ("Heightmaps", compound(&[("MOTION_BLOCKING", Nbt::LongArray(vec![0; 37]))])),
        ]);

        let forward = diff(&ours, &theirs);
        assert_eq!(forward.len(), 1, "{forward:#?}");
        assert_eq!(forward[0].path, "Heightmaps");
        assert!(matches!(forward[0].kind, DifferenceKind::Added { .. }));

        let backward = diff(&theirs, &ours);
        assert_eq!(backward.len(), 1, "{backward:#?}");
        assert_eq!(backward[0].path, "Heightmaps");
        assert!(
            matches!(backward[0].kind, DifferenceKind::Removed { .. }),
            "the reverse direction must be Removed, not Added — otherwise an \
             allowlist permitting one silently permits the other"
        );
    }

    #[test]
    fn a_narrowed_integer_is_a_type_change_not_a_value_match() {
        // `Int(5)` and `Long(5)` are the same number and a different value.
        // A differ that coerced would miss exactly the seed-narrowing defect
        // `world_gen_settings` guards against.
        let differences = diff(
            &compound(&[("seed", Nbt::Long(5))]),
            &compound(&[("seed", Nbt::Int(5))]),
        );
        assert_eq!(differences.len(), 1, "{differences:#?}");
        assert_eq!(
            differences[0].kind,
            DifferenceKind::TypeChanged {
                left: "Long",
                right: "Int"
            }
        );
    }

    #[test]
    fn a_length_change_is_reported_once_not_per_element() {
        // A list that grew by one must not report N differences: the report
        // is the evidence, and burying the signal under per-index noise is
        // how a real failure gets read as "lots of small differences".
        let left = list(NbtTag::Int, (0..64).map(Nbt::Int).collect());
        let right = list(NbtTag::Int, (0..65).map(Nbt::Int).collect());
        let differences = diff(&left, &right);
        assert_eq!(differences.len(), 1, "{differences:#?}");
        assert_eq!(
            differences[0].kind,
            DifferenceKind::LengthChanged {
                left: 64,
                right: 65
            }
        );
    }

    #[test]
    fn every_differing_array_element_is_reported_with_its_index() {
        // The counter-case to the test above: same length, so each differing
        // element is its own located difference. A LongArray is how a
        // `block_states.data` array or a heightmap arrives, and "how many
        // longs differ, and which" is the measurement.
        let left = Nbt::LongArray(vec![1, 2, 3, 4]);
        let right = Nbt::LongArray(vec![1, 9, 3, 8]);
        let differences = diff(&left, &right);
        assert_eq!(differences.len(), 2, "{differences:#?}");
        assert_eq!(differences[0].path, "[1]");
        assert_eq!(differences[1].path, "[3]");
    }

    #[test]
    fn an_empty_lists_element_type_is_not_compared() {
        // `chunk_nbt` writes `End` for an empty `block_entities` list;
        // vanilla may write `Compound`. Both mean "no block entities", and a
        // differ that flagged it would produce a difference on every chunk
        // of every world, drowning the report.
        let ours = compound(&[("block_entities", list(NbtTag::End, vec![]))]);
        let theirs = compound(&[("block_entities", list(NbtTag::Compound, vec![]))]);
        assert_eq!(diff(&ours, &theirs), Vec::new());

        // Control: with elements present, a genuine element-type disagreement
        // IS reported — so the exemption above is scoped to empty lists and
        // has not disabled the check.
        let ours = compound(&[("v", list(NbtTag::Int, vec![Nbt::Int(1)]))]);
        let theirs = compound(&[("v", list(NbtTag::Long, vec![Nbt::Long(1)]))]);
        let differences = diff(&ours, &theirs);
        assert!(
            differences.iter().any(|d| d.path == "v.<element_type>"),
            "{differences:#?}"
        );
    }

    #[test]
    fn sort_list_by_fields_makes_a_reordered_set_compare_equal() {
        let mut ours = list(
            NbtTag::Compound,
            vec![
                compound(&[("Y", Nbt::Byte(2)), ("mark", Nbt::Int(20))]),
                compound(&[("Y", Nbt::Byte(-4)), ("mark", Nbt::Int(-40))]),
                compound(&[("Y", Nbt::Byte(0)), ("mark", Nbt::Int(0))]),
            ],
        );
        let mut theirs = list(
            NbtTag::Compound,
            vec![
                compound(&[("Y", Nbt::Byte(0)), ("mark", Nbt::Int(0))]),
                compound(&[("Y", Nbt::Byte(2)), ("mark", Nbt::Int(20))]),
                compound(&[("Y", Nbt::Byte(-4)), ("mark", Nbt::Int(-40))]),
            ],
        );
        assert!(
            !diff(&ours, &theirs).is_empty(),
            "control: unsorted, these DO differ — so the sort below is what \
             makes them agree, not an already-equal pair"
        );

        sort_list_by_fields(&mut ours, &["Y"]);
        sort_list_by_fields(&mut theirs, &["Y"]);
        assert_eq!(diff(&ours, &theirs), Vec::new());

        // And the negative `Y` really did sort first, rather than being
        // treated as a large unsigned value — the trap a `u8` key would hit.
        let Nbt::List { elements, .. } = &ours else {
            panic!("still a list");
        };
        assert_eq!(elements[0], compound(&[("Y", Nbt::Byte(-4)), ("mark", Nbt::Int(-40))]));
    }

    #[test]
    fn sort_list_by_fields_orders_block_entities_by_all_three_coordinates() {
        // The multi-field case: two block entities in the same column differ
        // only in `y`, so keying on `x` alone would leave them in writer
        // order and the differ would report both as changed.
        let mut ours = list(
            NbtTag::Compound,
            vec![
                entity(97, -59, 199),
                entity(97, -60, 199),
                entity(96, -59, 199),
            ],
        );
        let mut theirs = list(
            NbtTag::Compound,
            vec![
                entity(96, -59, 199),
                entity(97, -60, 199),
                entity(97, -59, 199),
            ],
        );
        sort_list_by_fields(&mut ours, &["x", "y", "z"]);
        sort_list_by_fields(&mut theirs, &["x", "y", "z"]);
        assert_eq!(diff(&ours, &theirs), Vec::new());
    }

    fn entity(x: i32, y: i32, z: i32) -> Nbt {
        compound(&[
            ("id", Nbt::String("minecraft:chest".into())),
            ("x", Nbt::Int(x)),
            ("y", Nbt::Int(y)),
            ("z", Nbt::Int(z)),
        ])
    }

    #[test]
    fn canonical_sorts_nested_compounds_without_touching_list_order() {
        let input = compound(&[
            ("z", Nbt::Int(1)),
            ("a", compound(&[("q", Nbt::Int(2)), ("b", Nbt::Int(3))])),
            ("m", list(NbtTag::Int, vec![Nbt::Int(9), Nbt::Int(1)])),
        ]);
        let Nbt::Compound(fields) = canonical(&input) else {
            panic!("still a compound");
        };
        let names: Vec<&str> = fields.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, vec!["a", "m", "z"]);
        let Nbt::Compound(nested) = &fields[0].1 else {
            panic!("nested compound");
        };
        assert_eq!(
            nested.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["b", "q"],
            "canonicalization must recurse"
        );
        let Nbt::List { elements, .. } = &fields[1].1 else {
            panic!("list");
        };
        assert_eq!(
            elements,
            &vec![Nbt::Int(9), Nbt::Int(1)],
            "list order is part of an NBT value and must NOT be sorted"
        );
    }

    #[test]
    fn float_comparison_is_bit_exact_not_ieee() {
        // `-0.0` and `0.0` are different bytes on disk and IEEE says they are
        // equal. A save-format differ must report the difference, so floats
        // compare on `to_bits()`.
        //
        // The control is the `assert_eq!` on `Nbt`'s own `PartialEq`: it shows
        // the naive implementation genuinely does NOT report this, so the
        // bit comparison is doing real work rather than restating `!=`. That
        // is how this was caught — the module doc claimed exactness the code
        // did not have.
        let zero = Nbt::Compound(vec![("yaw".into(), Nbt::Float(0.0))]);
        let negative_zero = Nbt::Compound(vec![("yaw".into(), Nbt::Float(-0.0))]);
        assert_eq!(
            zero, negative_zero,
            "control: IEEE equality says these are the same value, which is why \
             `PartialEq` cannot be the comparison here"
        );
        let differences = diff(&zero, &negative_zero);
        assert_eq!(differences.len(), 1, "{differences:#?}");
        assert_eq!(differences[0].path, "yaw");

        // And the other direction IEEE gets wrong: two byte-identical `NaN`s
        // must compare EQUAL, or every file carrying one would report a
        // spurious difference against itself.
        let nan = Nbt::Compound(vec![("yaw".into(), Nbt::Float(f32::NAN))]);
        assert_ne!(
            nan, nan,
            "control: IEEE says NaN != NaN, so a `PartialEq` differ would report \
             a file as differing from itself"
        );
        assert_eq!(diff(&nan, &nan), Vec::new());

        // Doubles take the same path.
        assert_eq!(
            diff(
                &Nbt::Double(0.0),
                &Nbt::Double(-0.0)
            )
            .len(),
            1
        );
        assert_eq!(diff(&Nbt::Double(f64::NAN), &Nbt::Double(f64::NAN)), Vec::new());
    }

    #[test]
    fn differences_come_back_in_a_stable_sorted_order() {
        // Two runs must produce diffable reports; a HashMap walk would not.
        let left = compound(&[("b", Nbt::Int(1)), ("a", Nbt::Int(1)), ("c", Nbt::Int(1))]);
        let right = compound(&[("c", Nbt::Int(2)), ("b", Nbt::Int(2)), ("a", Nbt::Int(2))]);
        let paths: Vec<String> = diff(&left, &right).into_iter().map(|d| d.path).collect();
        assert_eq!(paths, vec!["a", "b", "c"]);
    }
}
