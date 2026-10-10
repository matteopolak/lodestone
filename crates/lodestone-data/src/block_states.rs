//! Block-state id resolution for protocol 776 (Minecraft 26.2).
//!
//! A chunk section palette yields numeric *block state ids* straight off the
//! wire; rendering needs to turn each into a block name plus its property values
//! (`facing=north`, `snowy=false`, …), because the blockstate/model JSON is keyed
//! by exactly those. That id → (block, properties) mapping is generated data
//! specific to 26.2, the one canonical internal version. It is a
//! game-data census, not wire-format code, so it lives here in this data
//! crate rather than in `lodestone-v26-2` or in the fully
//! version-free `lodestone-model`.
//!
//! # Memory design
//!
//! There are 32,366 states across 1,196 blocks in 26.2. The naive shape — an
//! owned `String` name and a `Vec<(String, String)>` per state — would be
//! megabytes of heap and pointer-chasing for data that is 100% static. Instead
//! the generated table (in [`crate::generated_block_states`]) is **pure rodata,
//! zero heap**:
//!
//! * block names stay in the one registry-order canonical column, reached from
//!   the state table's name-sorted block index through a generated permutation;
//! * property sets are de-duplicated to the 6,454 that are actually distinct,
//!   each a `&'static [(&'static str, &'static str)]`;
//! * each state is a `(u16, u16)` pair — an alphabetical block-name index and
//!   an index into the property-set table.
//!
//! Lookup is O(1) indexing (ids are contiguous `0..STATE_COUNT`), not searching.
//! The zero-heap path is [`block_name`] and [`properties`], which hand back the
//! static slices directly.
//!
//! # The `BlockStateRegistry` trait, and why it costs heap
//!
//! [`lodestone_model::BlockStateRegistry`] — the version-free seam the asset
//! baker consumes — returns [`ResolvedBlockState`], which borrows an owned
//! [`Identifier`] and an owned `BTreeMap<String, String>`. Those owned types
//! cannot be produced from `&'static` data without materialising them, so
//! [`BlockStateTable`] builds a de-duplicated owned layer (1,196 identifiers +
//! 6,454 maps, **not** 32,366) on construction. It is a transient cost: build
//! one to bake, drop it to reclaim the heap, while the zero-heap static table
//! stays resident for the mesher. The `&BTreeMap` shape of `ResolvedBlockState`
//! is the reason this materialisation is unavoidable; see the crate report for a
//! proposed trait change that would let the static table satisfy the seam
//! directly.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Deref;

use lodestone_model::{BlockStateRegistry, Identifier, ResolvedBlockState};

use crate::block_properties::Properties;
use crate::generated_block_states as table;

pub use crate::generated_block_registry::BLOCK_COUNT;
pub use table::STATE_COUNT;

use crate::block::Block;

/// Resolves a block-state table's **alphabetical** block index through the
/// generated name-order permutation into the canonical registry-order names.
///
/// `STATES` deliberately keeps this index: it is the order of the name-keyed
/// report that supplies the state rows. The canonical name column deliberately
/// keeps registration order: it is the wire's block-type registry-id order. The
/// permutation is the only bridge; treating either index as the other silently
/// changes the block a state names.
fn block_name_at_alphabetical_index(index: u16) -> &'static str {
    let registry_id = crate::generated_block_enum::REGISTRY_IDS_BY_NAME[index as usize];
    crate::generated_block_registry::BLOCK_REGISTRY_NAMES[registry_id as usize]
}

/// A validated global block-state id — one of the 32,366 states of 26.2.
///
/// # Why a newtype and not an enum
///
/// [`Block`] is an enum because 1,196 hand-named block types is a set the
/// compiler can usefully check exhaustively. A block *state* is not that set: it
/// is the cross product of each block's property domains, 32,366 entries with no
/// individual names, and nothing ever wants to `match` on one. So the type's job
/// here is different — it is to make the *range* invariant true by construction
/// so that every downstream lookup can be total.
///
/// That is the payoff. `StateId::new` is the single fallible step; after it,
/// [`block`](Self::block), [`properties`](Self::properties) and
/// [`is_default`](Self::is_default) return values rather than `Option`s. The
/// free-function forms below ([`block_name`], [`properties`]) keep taking a raw
/// `u32` and keep returning `Option`, because they are the un-migrated wire-side
/// entry points; prefer the methods in new code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StateId(u32);

impl StateId {
    /// The canonical air state. The generated table reserves raw id zero for
    /// the default air state.
    pub const AIR: Self = Self(0);

    /// Validates a raw global block-state id, or `None` if it is not in
    /// `0..`[`STATE_COUNT`].
    #[must_use]
    pub fn new(raw: u32) -> Option<Self> {
        (raw < STATE_COUNT).then_some(Self(raw))
    }

    /// The raw global id, for the wire.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    #[must_use]
    pub const fn from_raw(raw: u16) -> Self {
        Self(raw as u32)
    }

    /// The canonical namespaced block name this state belongs to.
    ///
    /// Total, O(1), and zero-heap. A `StateId` is always one of this build's
    /// built-in states; names introduced by a plugin or data pack remain text
    /// at the registry/import boundary and therefore cannot be represented by
    /// this type.
    #[must_use]
    pub fn name(self) -> &'static str {
        self.block().name()
    }

    /// The block this state belongs to. Total, O(1), two array indexes.
    ///
    /// Goes through the generated registry-order join rather than treating a
    /// state's block index as a registry id: the state table is name-sorted and
    /// the registry is registration-ordered, so the two are unrelated
    /// permutations.
    #[must_use]
    pub fn block(self) -> Block {
        let registry_id = crate::generated_block_registry::STATE_BLOCK[self.0 as usize];
        Block::from_registry_id(registry_id)
            .expect("generated STATE_BLOCK column holds a valid registry id")
    }

    /// This state's property values as a sorted `(name, value)` slice; empty for
    /// a block with no properties. Total, O(1), zero-heap.
    #[must_use]
    pub fn properties(self) -> &'static [(&'static str, &'static str)] {
        let (_, set) = table::STATES[self.0 as usize];
        table::PROPERTY_SETS[set as usize]
    }

    /// This state's canonical `name[key=value,...]` spelling.
    ///
    /// This is the text form for storage and other serialized boundaries. Keep
    /// a [`StateId`] while state-specific work is in process; reconstruct this
    /// string only when a format requires it.
    #[must_use]
    pub fn canonical_state(self) -> String {
        let name = self.name();
        let properties = self.properties();
        if properties.is_empty() {
            return name.to_owned();
        }

        let mut out = String::with_capacity(name.len() + properties.len() * 12 + 2);
        out.push_str(name);
        out.push('[');
        for (index, (key, value)) in properties.iter().enumerate() {
            if index != 0 {
                out.push(',');
            }
            out.push_str(key);
            out.push('=');
            out.push_str(value);
        }
        out.push(']');
        out
    }

    /// Whether this is its block's own default-block-state. Total, O(1).
    #[must_use]
    pub fn is_default(self) -> bool {
        self == self.block().default_state()
    }

    /// Resolves one canonical block-state string into this build's state table.
    ///
    /// The parser deliberately accepts `&str`: a caller may hold a namespaced
    /// plugin or data-pack value that this built-in table does not contain. Such
    /// a value returns `None` here so its owning registry can preserve or
    /// handle it; it is never coerced into a built-in state.
    #[must_use]
    pub fn from_state_str(state: &str) -> Option<Self> {
        state_id(state).and_then(Self::new)
    }

    /// Finds an exact namespaced identity in the generated canonical census.
    ///
    /// Every property must be present with its generated value. Missing, extra,
    /// or invalid properties return `None`; no default-state fallback applies.
    /// This cold import lookup searches only the named block's generated span.
    #[must_use]
    pub fn from_exact_parts(name: &str, properties: &BTreeMap<String, String>) -> Option<Self> {
        let block = Block::from_name(name)?;
        if block.name() != name {
            return None;
        }
        state_span(block).find_map(|raw| {
            let state = Self(raw);
            let generated = state.properties();
            (generated.len() == properties.len()
                && generated.iter().zip(properties).all(|(&(key, value), (have_key, have_value))| {
                    key == have_key && value == have_value
                }))
            .then_some(state)
        })
    }
}

/// A block-state value that has crossed the text boundary without losing its
/// domain or extension status.
///
/// Built-in states carry a validated [`StateId`]. Values supplied by a plugin
/// or data pack remain an explicit extension value instead of being coerced to
/// a built-in default. The original spelling is retained for both variants so
/// callers can hand the value to a storage or wire boundary without changing
/// abbreviated property sets that were already accepted there.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BlockStateValue {
    /// A state present in this build's generated 26.2 census.
    Builtin { id: StateId, text: Box<str> },
    /// A state outside the generated census, retained for an extensible input.
    Extension(Box<str>),
}

impl BlockStateValue {
    /// Parses one canonical or abbreviated state spelling.
    ///
    /// A spelling is classified as [`Self::Builtin`] only when every property
    /// named by the input exists on the resolved built-in state. This extra
    /// check matters because [`StateId::from_state_str`] intentionally has a
    /// forgiving default-state fallback for lookup callers; a typed value must
    /// not turn an invalid or synthetic property into an unrelated built-in.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        match exact_state_id(text) {
            Some(id) => Self::Builtin { id, text: text.into() },
            None => Self::Extension(text.into()),
        }
    }

    /// Creates a value from a validated built-in id, using its full canonical
    /// spelling for the serialized boundary.
    #[must_use]
    pub fn from_id(id: StateId) -> Self {
        Self::Builtin {
            id,
            text: id.canonical_state().into_boxed_str(),
        }
    }

    /// Returns the validated built-in id, or `None` for an extension value.
    #[must_use]
    pub fn state_id(&self) -> Option<StateId> {
        match self {
            Self::Builtin { id, .. } => Some(*id),
            Self::Extension(_) => None,
        }
    }

    /// Returns the spelling retained at the text boundary.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Builtin { text, .. } | Self::Extension(text) => text,
        }
    }

    /// Consumes the value and returns its retained boundary spelling.
    #[must_use]
    pub fn into_string(self) -> String {
        match self {
            Self::Builtin { text, .. } | Self::Extension(text) => text.into(),
        }
    }

    /// Whether this value resolved to the generated built-in state table.
    #[must_use]
    #[cfg(test)]
    pub fn is_builtin(&self) -> bool {
        self.state_id().is_some()
    }
}

impl From<StateId> for BlockStateValue {
    fn from(id: StateId) -> Self {
        Self::from_id(id)
    }
}

impl From<&str> for BlockStateValue {
    fn from(text: &str) -> Self {
        Self::parse(text)
    }
}

impl From<String> for BlockStateValue {
    fn from(text: String) -> Self {
        Self::parse(&text)
    }
}

impl AsRef<str> for BlockStateValue {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Deref for BlockStateValue {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl fmt::Display for BlockStateValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl PartialEq<str> for BlockStateValue {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for BlockStateValue {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<String> for BlockStateValue {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other
    }
}

/// Resolves only a state whose named properties are all part of the generated
/// state domain. Missing properties are valid shorthand and are filled by the
/// same default-state lookup used by [`StateId::from_state_str`]. The property
/// text itself is parsed into [`Properties`], so duplicate keys, unknown keys,
/// and values from another property's domain cannot be mistaken for a state.
fn exact_state_id(state: &str) -> Option<StateId> {
    let Some((name, _)) = state.split_once('[') else {
        return StateId::from_state_str(state);
    };
    let property_text = &state[name.len()..];
    if !property_text.ends_with(']') {
        return None;
    }
    let parsed = Properties::parse(property_text).ok()?;
    if parsed.is_empty() {
        return None;
    }

    let id = StateId::from_state_str(state)?;
    if name != id.name() {
        return None;
    }
    let generated = Properties::from_state_id(id);
    parsed
        .iter()
        .all(|property| generated.get(property.key()) == Some(property.value()))
        .then_some(id)
}

/// The interned block identifier for `id` (for example `minecraft:oak_stairs`),
/// or `None` if `id` is not in `0..`[`STATE_COUNT`].
///
/// Zero-heap: returns a `&'static str` straight from rodata. O(1).
#[must_use]
pub fn block_name(id: u32) -> Option<&'static str> {
    let &(block, _) = table::STATES.get(id as usize)?;
    Some(block_name_at_alphabetical_index(block))
}

/// The property values for `id` as a sorted slice of `(name, value)` pairs, or
/// `None` if `id` is not in `0..`[`STATE_COUNT`]. An empty slice means the block
/// has no properties.
///
/// Zero-heap: returns a `&'static [(&'static str, &'static str)]` straight from
/// rodata. O(1).
#[must_use]
pub fn properties(id: u32) -> Option<&'static [(&'static str, &'static str)]> {
    let &(_, set) = table::STATES.get(id as usize)?;
    Some(table::PROPERTY_SETS[set as usize])
}

/// The canonical states belonging to one validated block, as a half-open range.
pub(crate) fn state_span(block: Block) -> std::ops::Range<u32> {
    let (start, count) = crate::generated_block_registry::BLOCK_STATE_SPANS[block.registry_id() as usize];
    start..start + count
}

/// The canonical `minecraft:air` state ID, for numeric boundaries.
#[must_use]
pub fn air_state_id() -> u32 {
    air_state().raw()
}

/// The canonical `minecraft:air` state as a validated [`StateId`].
///
/// Protocol encoders that need its numeric representation should call
/// [`StateId::raw`] at their wire boundary.
#[must_use]
pub fn air_state() -> StateId {
    StateId::AIR
}

/// Resolves a fully namespaced block-state spelling to its canonical state ID.
///
/// Lookup searches the generated block-name permutation and only that block's
/// half-open state span. Exact property sets are preferred. For an abbreviated
/// spelling, omitted properties retain the block's canonical default; unknown
/// properties are ignored, and an invalid value falls back to that default.
/// Bare paths and unknown resource names return `None`.
#[must_use]
pub fn state_id(state: &str) -> Option<u32> {
    let (name, raw_props) = match state.split_once('[') {
        Some((name, rest)) => (name, rest.strip_suffix(']').unwrap_or(rest)),
        None => (state, ""),
    };
    let mut wanted: Vec<(&str, &str)> = if raw_props.is_empty() {
        Vec::new()
    } else {
        raw_props
            .split(',')
            .filter_map(|pair| pair.split_once('='))
            .collect()
    };
    wanted.sort_unstable();

    let block = Block::from_name(name)?;
    if block.name() != name {
        return None;
    }
    let span = state_span(block);
    for id in span.clone() {
        if properties(id).unwrap_or(&[]) == wanted.as_slice() {
            return Some(id);
        }
    }

    let base = block.default_state().raw();
    if wanted.is_empty() {
        return Some(base);
    }

    let mut merged: Vec<(&str, &str)> = properties(base).unwrap_or(&[]).to_vec();
    let mut overridden = false;
    for &(key, value) in &wanted {
        if let Some(slot) = merged.iter_mut().find(|(have_key, _)| *have_key == key) {
            if slot.1 != value {
                slot.1 = value;
                overridden = true;
            }
        }
    }
    if !overridden {
        return Some(base);
    }
    merged.sort_unstable();
    for id in span {
        if properties(id).unwrap_or(&[]) == merged.as_slice() {
            return Some(id);
        }
    }
    Some(base)
}

/// A [`BlockStateRegistry`] implementation for protocol 776.
///
/// Holds the owned [`Identifier`]/`BTreeMap` layer that the trait's borrowing
/// shape requires (see the module docs). Construct it only when the trait is
/// needed — e.g. to drive the asset baker — and drop it afterwards; the
/// version-free [`block_name`]/[`properties`] accessors need no instance and
/// allocate nothing.
#[derive(Debug, Clone)]
pub struct BlockStateTable {
    /// One identifier per state-table alphabetical block index.
    identifiers: Vec<Identifier>,
    /// One map per distinct property set, indexed as [`table::PROPERTY_SETS`].
    property_maps: Vec<BTreeMap<String, String>>,
}

impl BlockStateTable {
    /// Materialises the owned identifier and property-map layer from the static
    /// table.
    ///
    /// # Panics
    ///
    /// Panics only if the generated table contains a block name that is not a
    /// valid [`Identifier`], which is a generation-time invariant — real data
    /// never triggers it.
    #[must_use]
    pub fn new() -> Self {
        let identifiers = (0..BLOCK_COUNT as u16)
            .map(|index| {
                let name = block_name_at_alphabetical_index(index);
                name.parse::<Identifier>()
                    .expect("generated block name is a valid identifier")
            })
            .collect();
        let property_maps = table::PROPERTY_SETS
            .iter()
            .map(|set| {
                set.iter()
                    .map(|&(key, value)| (key.to_owned(), value.to_owned()))
                    .collect()
            })
            .collect();
        Self {
            identifiers,
            property_maps,
        }
    }

    /// Approximate heap bytes owned by the materialised layer, for measurement.
    ///
    /// Counts the two backing `Vec`s plus every owned string in the identifiers
    /// and property maps. Ignores `BTreeMap` node overhead, so it is a lower
    /// bound on true resident heap.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        let idents: usize = self
            .identifiers
            .iter()
            .map(|id| id.namespace().len() + id.path().len())
            .sum();
        let maps: usize = self
            .property_maps
            .iter()
            .flat_map(|map| map.iter())
            .map(|(key, value)| key.len() + value.len())
            .sum();
        self.identifiers.capacity() * size_of::<Identifier>()
            + self.property_maps.capacity() * size_of::<BTreeMap<String, String>>()
            + idents
            + maps
    }
}

impl Default for BlockStateTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_properties::{ParseError, PropertiesError, PropertyError};

    #[test]
    fn canonical_spans_have_half_open_official_boundaries() {
        assert_eq!(state_span(Block::Air), 0..1);
        assert_eq!(state_span(Block::GrassBlock), 8..10);
        assert_eq!(state_span(Block::Water), 86..102);
        assert_eq!(state_span(Block::OakLog), 136..139);
        assert_eq!(StateId::new(10).unwrap().block(), Block::Dirt);
        assert!(!state_span(Block::GrassBlock).contains(&10));
    }

    #[test]
    fn canonical_spans_partition_every_state_and_contain_each_default() {
        let mut covered = vec![false; STATE_COUNT as usize];
        for block in Block::all() {
            let span = state_span(block);
            assert!(!span.is_empty(), "{} has no states", block.name());
            assert!(
                span.contains(&block.default_state().raw()),
                "{} default lies outside its span",
                block.name()
            );
            for raw in span {
                let state = StateId::new(raw).expect("span holds a valid canonical state");
                assert_eq!(state.block(), block, "state {raw} has the wrong span owner");
                assert!(!covered[raw as usize], "state {raw} appears in two spans");
                covered[raw as usize] = true;
            }
        }
        assert!(covered.into_iter().all(|present| present));
    }

    #[test]
    fn generated_property_sets_have_unique_sorted_keys() {
        for (index, set) in table::PROPERTY_SETS.iter().enumerate() {
            assert!(
                set.windows(2).all(|pair| pair[0].0 < pair[1].0),
                "property set {index} is not strictly sorted: {set:?}"
            );
        }
    }

    #[test]
    fn canonical_state_round_trips_a_non_default_property_set() {
        let state = StateId::from_state_str("minecraft:redstone_wire[power=7]")
            .expect("the generated census contains redstone wire power 7");

        let text = state.canonical_state();
        assert_eq!(
            StateId::from_state_str(&text),
            Some(state),
            "canonical output must preserve every generated property, including defaults omitted by the input"
        );
        assert!(text.contains("power=7"), "the requested property must survive the boundary spelling");
    }

    /// The discriminating case for a partial-property lookup: on a
    /// block with many states it must land on the *named* state, not on the
    /// lowest id sharing the block's name.
    ///
    /// Redstone dust is the case that shipped wrong. Before `state_id`
    /// existed, `lodestone-v26-2`'s hand-rolled scan fell back to the lowest
    /// id sharing a block name whenever the caller's property set did not
    /// exactly match a real state — which, for a block this server only ever
    /// partially describes (`minecraft:redstone_wire[power=N]`, never the
    /// other four connection properties), was *every* update. That lowest id
    /// is state **4011**, confirmed as this block's first (lowest) id by the
    /// committed JVM dump `tests/support/snow_support_jvm.txt` ("`B 4011
    /// minecraft:redstone_wire`" — the dump's own header defines `B` as "the
    /// first state id of a block"). Its `power` is `0`. So every dust update
    /// used to resolve to the same id regardless of the power the server
    /// actually wanted to send, and rendered as unpowered wire.
    ///
    /// `power=7` is chosen because it discriminates the two hypotheses: the
    /// old code returns id 4011 (`power=0`) for *any* power value, so an
    /// input that also produced 4011 would not tell the implementations
    /// apart.
    #[test]
    fn state_id_resolves_redstone_dust_by_power_not_to_the_lowest_id() {
        const WRONG_OLD_ANSWER: u32 = 4011;

        assert_eq!(
            properties(WRONG_OLD_ANSWER)
                .and_then(|props| props.iter().find(|(k, _)| *k == "power"))
                .map(|(_, v)| *v),
            Some("0"),
            "fixture sanity: the old broken fallback's wrong answer must actually carry power=0"
        );

        let power_seven = state_id("minecraft:redstone_wire[power=7]")
            .expect("minecraft:redstone_wire[power=7] must resolve");

        // The assertion that matters: not the old wrong answer.
        assert_ne!(
            power_seven, WRONG_OLD_ANSWER,
            "state_id(\"minecraft:redstone_wire[power=7]\") returned the old lowest-id fallback \
             ({WRONG_OLD_ANSWER}, power=0) instead of resolving power=7 to its own state — this \
             is the exact defect issues #465/#511 describe"
        );
        assert_eq!(
            properties(power_seven)
                .and_then(|props| props.iter().find(|(k, _)| *k == "power"))
                .map(|(_, v)| *v),
            Some("7"),
            "state_id must resolve the requested power value exactly, not merely to a different id"
        );

        // Tier 2's contract: every property the caller did *not* name keeps
        // the block's jar-marked default value.
        let default_id = state_id("minecraft:redstone_wire")
            .expect("minecraft:redstone_wire must resolve to its jar-marked default");
        let default_props = properties(default_id).expect("default state has properties");
        let resolved_props = properties(power_seven).expect("resolved state has properties");
        for &(key, default_value) in default_props {
            if key == "power" {
                continue;
            }
            let resolved_value = resolved_props
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| *v);
            assert_eq!(
                resolved_value,
                Some(default_value),
                "property `{key}` should keep its default value when the caller only named `power`"
            );
        }
    }

    /// A bare name with no properties at all resolves to the block's
    /// jar-marked default (tier 3) — **not** the lowest id (4011). Cross-
    /// checked against the committed JVM dump's own `D` (`state ==
    /// default_block_state()`) bitstring for this block's id range in
    /// `tests/support/snow_support_jvm.txt`, which marks id 5171 as the one
    /// true bit — an id this test derives independently of `state_id` itself
    /// by walking the dump's `P D` line, not by trusting the function under
    /// test.
    #[test]
    fn state_id_resolves_a_bare_name_to_the_jar_marked_default() {
        assert_eq!(state_id("minecraft:redstone_wire"), Some(5171));
    }

    /// An unknown block name resolves to nothing — `state_id` does not paper
    /// over a name this version's census does not carry.
    #[test]
    fn state_id_returns_none_for_an_unknown_block() {
        assert_eq!(state_id("minecraft:not_a_real_block"), None);
    }

    #[test]
    fn block_state_value_keeps_a_typed_partial_state_and_its_boundary_spelling() {
        let value = BlockStateValue::parse("minecraft:target[power=12]");
        assert_eq!(value.state_id(), StateId::from_state_str("minecraft:target[power=12]"));
        assert_eq!(value.as_str(), "minecraft:target[power=12]");
        assert_eq!(value.to_string(), "minecraft:target[power=12]");
        assert!(value.is_builtin());
    }

    #[test]
    fn block_state_value_preserves_an_extension_instead_of_defaulting_it() {
        let value = BlockStateValue::parse("example:custom_block[variant=blue]");
        assert_eq!(value.state_id(), None);
        assert_eq!(value.as_str(), "example:custom_block[variant=blue]");
        assert!(!value.is_builtin());
    }

    #[test]
    fn block_state_value_rejects_a_synthetic_or_malformed_property() {
        let synthetic = BlockStateValue::parse("minecraft:comparator[output=9]");
        assert_eq!(synthetic.state_id(), None);
        let malformed = BlockStateValue::parse("minecraft:stone[not-a-property]");
        assert_eq!(malformed.state_id(), None);
    }

    #[test]
    fn block_state_value_rejects_duplicate_unknown_and_wrong_schema_properties() {
        let duplicate = BlockStateValue::parse("minecraft:target[power=12,power=12]");
        assert_eq!(duplicate.state_id(), None);
        assert!(matches!(
            Properties::parse("[power=12,power=12]"),
            Err(ParseError::InvalidProperties(PropertiesError::DuplicateKey(_)))
        ));

        let unknown = BlockStateValue::parse("minecraft:target[definitely_unknown=true]");
        assert_eq!(unknown.state_id(), None);
        assert!(matches!(
            Properties::parse("[definitely_unknown=true]"),
            Err(ParseError::UnknownKey)
        ));

        let wrong_schema = BlockStateValue::parse("minecraft:comparator[power=12]");
        assert_eq!(wrong_schema.state_id(), None);
        assert!(matches!(
            Properties::parse("[power=north]"),
            Err(ParseError::InvalidProperties(
                PropertiesError::InvalidProperty(PropertyError::ValueNotAllowed { .. })
            ))
        ));
        assert!(Properties::parse("[power=12]").is_ok());
    }

    #[test]
    fn block_state_value_from_id_uses_the_full_canonical_spelling() {
        let id = StateId::from_state_str("minecraft:redstone_wire[power=7]").expect("known state");
        let value = BlockStateValue::from_id(id);
        assert_eq!(value.state_id(), Some(id));
        assert_eq!(value.as_str(), id.canonical_state());
    }
}

impl BlockStateRegistry for BlockStateTable {
    fn resolve(&self, id: u32) -> Option<ResolvedBlockState<'_>> {
        let &(block, set) = table::STATES.get(id as usize)?;
        Some(ResolvedBlockState {
            block: &self.identifiers[block as usize],
            properties: &self.property_maps[set as usize],
        })
    }

    fn state_count(&self) -> u32 {
        table::STATE_COUNT
    }
}
