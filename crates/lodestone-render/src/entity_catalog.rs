use super::*;

/// The model-space lift that places a living entity's feet on the world origin.
pub const MODEL_FEET_OFFSET: f32 = 1.501;

/// Packed sky/block light meaning "full sky, no block light" (sky in the high
/// nibble), the value an entity carries when the caller has no world to sample.
///
/// This is a **fallback, not the normal path**. Vanilla samples the lightmap
/// once per entity at its block position, which is why light is one
/// byte per *instance* ([`EntityInstance::light`]) and not per vertex: a mob is
/// uniformly lit by the block it stands in. A caller that has a world supplies
/// the real byte via [`EntityInstance::with_light`] or
/// [`EntitySpawn::light`]; one that does not (the offline demo, a mesh-only
/// test) gets this and renders as it always did.
pub const ENTITY_FULLBRIGHT: u8 = 15 << 4;

/// The factor the **sky** half of the lightmap is scaled by at a given server
/// `time_of_day` — `1.0` at noon, `0.24` at midnight. Feed it to
/// [`EntityCameraUniform::with_sky_darken`](crate::entity_pipeline::EntityCameraUniform::with_sky_darken).
///
/// # Why this is needed even when world light is sampled correctly
///
/// A server's sky-light array is time-**invariant** — it records how much sky
/// reaches a block, not how bright the sky is right now. Measured live against a
/// vanilla 26.2 oracle at a single sky-lit position, with the server's own clock
/// as the control:
///
/// ```text
/// noon     clock= 6000  packed=0xF0  light_term=1.000
/// midnight clock=18000  packed=0xF0  light_term=1.000
/// ```
///
/// So a mob sampling world light perfectly is still full-bright at midnight.
/// Vanilla applies the darkening client-side only.
///
/// # The curve
///
/// 26.2 deleted the older fixed sky-darken curve and lightmap lift entirely,
/// replacing both with a data-driven timeline track for the sky-light factor.
/// This is a direct port of that track's sampling machinery, not a
/// re-derivation of a curve shape:
///
/// * Keyframes (tick → value): `730 → 1.0`, `11270 → 1.0`, `13140 → 0.24`,
///   `22860 → 0.24`, applied as a multiplier over the attribute's own default
///   of `1.0` — multiplying by `1.0` is a no-op, so the sampled keyframe value
///   *is* the final factor.
/// * The easing is **linear, not cubic-bezier**. The track builder defaults to
///   linear easing and the sky-light-factor track never opts out of that
///   default — only the neighbouring sun-angle, moon-angle and star-angle
///   tracks in the same data opt into a symmetric cubic-bezier easing with
///   control values `0.362`/`0.241`. An earlier note here claimed
///   "cubic-bezier eased"; that was a transcription error caught by reading
///   the source data itself rather than trusting the summary (exactly the
///   failure mode `CLAUDE.md` warns about) — see
///   `docs/time-of-day-lighting.md`.
/// * The sampler wraps the segment between the *last* and *first* keyframe
///   through the timeline's 24000-tick period, so the dawn ramp is **one
///   continuous 1870-tick segment running from 22860 through the tick-0 seam
///   to 730**, not a ramp that resets at midnight-wrap. The implementation
///   below collapses that wraparound into a single contiguous range by
///   shifting the day so it starts at the first keyframe, rather than
///   replicating the original two-segment split.
///
/// No `* 0.95 + 0.05` lift: that was specifically an older two-step darkening
/// pipeline's second step (a sky-darken curve into `[0.2, 1.0]`, then a lift
/// into `[0.24, 1.0]`). 26.2's keyframes are already expressed directly in
/// `[0.24, 1.0]`, and the vanilla lightmap shader applies no further affine
/// transform to the sampled value.
///
/// Verified against every one of the 24000 ticks in a real JVM's timeline
/// sampler — not hand-derived interpolation math, and not this function's own
/// output pasted back. See
/// `crates/lodestone-render/tests/sky_light_factor_timeline.rs` and
/// `oracle-java/SkyLightTimelineOracle.java` for provenance.
///
/// # How to change it
///
/// Rain and thunder further blend this factor toward `0.24` at the same
/// game-attribute layer — omitted here because the shell tracks neither yet.
/// Add them as arguments to this function rather than at the call site, so the
/// one place that knows vanilla's curve stays the one place. The
/// Negative-means-daylight sentinel lives in the shader, not here — this
/// function never returns `0.0`.
#[must_use]
pub fn sky_darken_for_time_of_day(time_of_day: i64) -> f32 {
    // The two ramps are symmetric and this many ticks long: 13140-11270 (dusk)
    // and (730+24000)-22860 (dawn, unwrapped across the tick-0 seam) are both
    // exactly 1870 ticks — not a coincidence, the track is built that way.
    const RAMP_LEN: f64 = 1_870.0;
    // Keyframe ticks, re-expressed relative to the first keyframe (730) so the
    // wraparound dawn ramp becomes one contiguous range instead of two
    // segments split across tick 0.
    const DUSK_START: f64 = 11_270.0 - 730.0; // 10540
    const DUSK_END: f64 = 13_140.0 - 730.0; // 12410
    const DAWN_START: f64 = 22_860.0 - 730.0; // 22130

    let day = time_of_day.rem_euclid(24_000);
    let shifted = (day - 730).rem_euclid(24_000) as f64;

    let factor = if shifted < DUSK_START {
        1.0
    } else if shifted < DUSK_END {
        let alpha = (shifted - DUSK_START) / RAMP_LEN;
        1.0 + (0.24 - 1.0) * alpha
    } else if shifted < DAWN_START {
        0.24
    } else {
        let alpha = (shifted - DAWN_START) / RAMP_LEN;
        0.24 + (1.0 - 0.24) * alpha
    };

    factor as f32
}

/// Look up the ported entity model for a built-in [`EntityType`] — the
/// registry identity the wire actually carries.
/// `EntityType as u8` **is** the `add_entity` registry id, so this call takes
/// no string at any point between the decoded id and the corpus lookup.
///
/// Returns the matching [`EntityModelEntry`] from the version-free
/// [`entity_models`] corpus, or `None` if we have no model for that type yet —
/// in which case the renderer skips the entity rather than substituting a wrong
/// mesh. `None` is also the correct answer for a type that is real but has no
/// rig by design (`experience_orb`, `tnt`) — see this module's tests for the
/// negative controls that pin that.
///
/// A plugin-supplied entity type — an
/// [`EntityTypeRef`](lodestone_data::entity_type::EntityTypeRef) whose
/// [`kind()`](lodestone_data::entity_type::EntityTypeRef::kind) is `Custom` —
/// has no place in [`entity_models`] at all: the corpus is a fixed,
/// hand-ported set of vanilla rigs, so no `EntityType` value could ever name
/// one. That is why this function takes `EntityType` rather than
/// `EntityTypeRef` — a caller holding an `EntityTypeRef` decides at its own
/// call site (`builtin_or_none()`) whether it even has a built-in type to look
/// up; folding that decision in here would hide it behind a silent `None`
/// instead of making the caller state it.
#[must_use]
pub fn model_for_type(entity_type: EntityType) -> Option<EntityModelEntry> {
    let name = canonical_model_name_for_type(entity_type)?;
    entity_models().into_iter().find(|e| e.name == name)
}

/// The corpus entry names, cached so the per-entity, per-frame
/// [`canonical_model_name`] lookup does not rebuild the whole `entity_models()`
/// vector. The corpus is a compile-time constant set, so caching it can never go
/// stale.
pub(crate) fn corpus_names() -> &'static [&'static str] {
    static NAMES: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| entity_models().into_iter().map(|e| e.name).collect())
}

/// [`corpus_names()`] as a set, computed once behind a `OnceLock` from that
/// same slice (so the two can never drift), for an O(1) "is this a corpus
/// entry" test — [`canonical_model_name`] resolves in O(1) instead of the
/// up-to-90 linear `&str` compares a naive per-entity, per-frame lookup would
/// cost, x4 for the base/armour/flame/wool passes in `gpu/entity_passes.rs`.
pub(crate) fn corpus_name_set() -> &'static std::collections::HashSet<&'static str> {
    static SET: std::sync::OnceLock<std::collections::HashSet<&'static str>> =
        std::sync::OnceLock::new();
    SET.get_or_init(|| corpus_names().iter().copied().collect())
}

/// Maps a built-in [`EntityType`] to the `name` of the [`entity_models`] entry
/// that renders it.
///
/// **The corpus is the source of truth**: an entity type whose [`path()`]
/// names a corpus entry resolves to *that* entry, and only the handful of
/// types whose registry path differs from the model name are listed here. The
/// inverse — an explicit table enumerating every drawable type — is what
/// shipped the "a drowned renders as an ordinary zombie" defect: `drowned` was
/// aliased onto `zombie` back when the corpus had no drowned mesh, and the
/// alias outlived the mesh's arrival by the whole tier-3 port. Deriving
/// identity from the corpus means a newly ported mob is drawable the moment
/// its mesh lands, and a wrong-mesh substitution has to be *written down*
/// rather than left behind.
///
/// The aliases that remain are genuine "vanilla renders this type with another
/// mob's model class" cases, not placeholders. They are matched on the
/// **enum discriminant**, not the path string, so an alias here can never be
/// reached by a plugin's namespaced-but-coincidentally-matching path — the
/// three arms below only ever fire for a real `minecraft:player`/`bogged`/
/// `*_minecart` registry entry.
///
/// [`path()`]: EntityType::path
pub(crate) fn canonical_model_name_for_type(entity_type: EntityType) -> Option<&'static str> {
    match entity_type {
        // The player renderer picks a skin model; wide/`steve` is the default.
        EntityType::Player => return Some("player_wide"),
        // A mannequin is the *same* renderer as a player: the dispatcher's
        // type switch routes both classes into the avatar renderer and picks
        // the rig from the entity's own skin, defaulting to wide when the skin
        // names no model. So this is the player arm's twin, not an
        // approximation — and it is the reason `renderer_is_avatar` already
        // lists both paths. Without it a mannequin was classified as an avatar
        // for arm-pose purposes and resolved no rig at all, which is the
        // "named all over the draw surface, draws nothing" shape.
        EntityType::Mannequin => return Some("player_wide"),
        // The bogged mob's model (a skeleton with mushrooms) is not ported yet;
        // the plain skeleton is the closest ported mesh. Unlike the drowned
        // alias this is deliberate and outlives no mesh — remove it when
        // `bogged` is ported.
        EntityType::Bogged => return Some("skeleton"),
        // Vanilla registers both the plain wind charge and the breeze's wind
        // charge against the same renderer, so a breeze's charge rides the
        // plain charge's rig rather than a second corpus entry — see
        // `wind_charge_model`'s doc for the rig itself and its known
        // simplifications.
        EntityType::BreezeWindCharge => return Some("wind_charge"),
        // Every minecart subclass shares vanilla's one cart-frame rig — the
        // subclasses differ only in the block state vanilla displays *inside*
        // the cart, which `gpu/moving_blocks.rs`'s `merge_minecart_contents`
        // draws as a second, independent block-model pass, not a second
        // corpus rig.
        //
        // All six subclasses, not the four this repo's own server spawns. The
        // arm used to stop at four, on the reasoning that `spawner_minecart`
        // and `command_block_minecart` are types "this server never produces"
        // and aliasing them would be untested. Both halves were wrong as a
        // reason to leave them out: the client joins *other people's* servers,
        // where a spawner minecart is an ordinary thing to meet, and an alias is
        // untested only until something tests it —
        // `tests/invisible_but_solid_rigs.rs` now resolves every one of the six
        // from its registry path and measures the drawn box against the
        // registry's own. Vanilla registers both of them through the same
        // minecart renderer as the other four, differing only in the layer that
        // supplies the *contents*, which is not this table's business.
        EntityType::ChestMinecart
        | EntityType::FurnaceMinecart
        | EntityType::TntMinecart
        | EntityType::HopperMinecart
        | EntityType::SpawnerMinecart
        | EntityType::CommandBlockMinecart => {
            return Some("minecart");
        }
        _ => {}
    }
    // The corpus first, then the boat family. Order matters both ways round:
    // `chest_boat` and `chest_raft` are corpus names that *also* satisfy
    // [`boat_model_name`]'s `_boat`/`_raft` suffix rules, so testing the suffixes
    // first would resolve the literal `"chest_boat"` to the plain boat rig.
    //
    // `entity_type.path()` is the one place this function still touches a
    // `&str`: the corpus (`lodestone_assets::entity_models`) and the boat-suffix
    // rule are both keyed by the *model*'s own name, a smaller, separately
    // hand-ported ~90-entry namespace that is not itself a registry, so there is
    // no enum to match against on that side — see `boat_model_name`'s doc for
    // why that space stays string/suffix-keyed rather than a 20-arm match.
    //
    // O(1) via `corpus_name_set()` rather than a linear scan over
    // `corpus_names()` — same corpus, same membership, just not re-walked for
    // every one of up to 90 entries on every call.
    let path = entity_type.path();
    corpus_name_set().get(path).copied().or_else(|| boat_model_name(path))
}

/// [`canonical_model_name_for_type`], reached from a raw type-path string
/// rather than an already-resolved [`EntityType`].
///
/// This is the one surviving `&str` hop in this module, and it exists for a
/// real reason rather than habit: [`EntityModelSet::resolve_animated`] and its
/// siblings are called with [`EntityDraw::model_type_path`]'s result, which is
/// **not always a registry entity type at all** — a slim-skinned player's
/// rig comes back as the literal corpus name `"player_slim"`, a string with no
/// `EntityType` variant to represent it, because vanilla's rig choice is
/// per-player skin data, not registry identity. Converting
/// `EntityModelSet::resolve*` to take `EntityType` would have to either drop
/// that case or grow a second parameter every mob-only caller would ignore,
/// and those methods are also called from `crates/lodestone-shell/src/
/// container/player_preview.rs`, which this module does not own. That is the
/// genuine boundary this function exists to stop at rather than force through.
///
/// A type path that **is** a real
/// registry entity resolves via one [`EntityType::from_name`] binary
/// search (`O(log 158)`) into [`canonical_model_name_for_type`] — the same
/// enum-keyed alias table [`model_for_type`] uses, so the two paths cannot
/// silently disagree — instead of re-testing the three alias literals as
/// strings and falling through to a second string scan. Only a **non**-entity-
/// type string (a corpus/rig pseudo-name, or a boat path handled by suffix)
/// still walks the corpus-name/boat-suffix fallback directly.
pub(crate) fn canonical_model_name(type_path: &str) -> Option<&'static str> {
    if let Some(entity_type) = EntityType::from_name(type_path) {
        return canonical_model_name_for_type(entity_type);
    }
    corpus_name_set()
        .get(type_path)
        .copied()
        .or_else(|| boat_model_name(type_path))
}

/// The corpus rig for one of 26.2's twenty boat entity types.
///
/// **This is the one alias family the corpus cannot answer for itself.** The
/// registry has twenty types — nine wood species × (boat, chest boat), plus
/// `bamboo_raft` and `bamboo_chest_raft` (`lodestone_data::entity_types`, and
/// `lodestone_data::entity_census`'s per-type renderer-class column, which
/// groups them into four renderer classes) — while the corpus carries exactly
/// four rigs, one per *class*, because that is how vanilla builds them: the
/// species is a texture, not geometry (vanilla's own boat renderer takes its
/// model-layer location from the boat's own variant and its model from one of
/// four boat/raft model classes). With no alias, `model_for_type("oak_boat")`
/// returned `None` and the renderer skipped the entity entirely — a placed boat
/// was invisible.
///
/// # The two traps, both real
///
/// * **`_chest_boat` must be tested before `_boat`**, because `oak_chest_boat`
///   ends with `_boat` as well. Testing the shorter suffix first draws every chest
///   boat as a plain boat — geometry that is wrong by three cubes and, more
///   visibly, the wrong texture directory.
/// * **`bamboo_raft` and `bamboo_chest_raft` carry no `_boat` suffix at all**, so a
///   `_boat`-only rule silently misses two of the twenty. `lodestone_server`'s
///   `boat` module records the same trap from the item side.
///
/// Written as suffix rules rather than twenty arms so a new wood species is
/// drawable the moment the server sends it, matching how
/// [`canonical_model_name`]'s corpus fallback treats a newly ported mob. The
/// ordering above is what makes that safe.
///
/// The species texture is **not** resolved here: all nine wood boats draw the
/// corpus entry's `entity/boat/oak` sheet and both rafts draw
/// `entity/boat/bamboo`, because each corpus entry holds a single
/// `EntityTexture::Fixed`. That is a visible-but-minor wrong-colour hull, and
/// fixing it belongs in `lodestone-assets` (a variant texture on the four
/// entries), not in a name mapping.
pub(crate) fn boat_model_name(type_path: &str) -> Option<&'static str> {
    // Longest first: every `*_chest_boat` also ends with `_boat`.
    if type_path.ends_with("_chest_boat") {
        return Some("chest_boat");
    }
    if type_path.ends_with("_chest_raft") {
        return Some("chest_raft");
    }
    if type_path.ends_with("_boat") {
        return Some("boat");
    }
    if type_path.ends_with("_raft") {
        return Some("raft");
    }
    None
}

/// The [`entity_models`] entry name for a player's own body, chosen by skin
/// model rather than the `"player"`-type-path default [`canonical_model_name`]
/// falls back to.
///
/// Vanilla's player renderer (26.2) picks between `player_wide` and
/// `player_slim` per skin — a player's uploaded skin reports which model it
/// wants — so the choice is genuinely per-player data, not a constant. Both
/// rigs are already first-class [`entity_models`] entries (`player_wide` and
/// `player_slim` both appear as top-level corpus names, not just as
/// `canonical_model_name`'s hidden alias target), so a caller that already
/// knows which skin a player wears can pass this straight through as a
/// `type_path` — [`canonical_model_name`] resolves a literal `"player_wide"`/
/// `"player_slim"` via its corpus-name fallback with no extra plumbing.
///
/// `canonical_model_name("player")` deliberately keeps resolving to
/// `player_wide` alone: it has no per-instance signal to read, and the other
/// callers that go through it (the first-person arm, a remote player with no
/// skin data yet) want exactly that default.
///
/// No caller in this codebase has real skin-model data yet — see
/// `RenderState::prepare_first_person_hand`'s "the shell has no skin-model
/// signal" note in `lodestone-shell`, which is still true here. This function
/// exists so that the day that signal arrives (from the tab-list player-info
/// packet, decoded in the network layer), selecting the right rig for the
/// local player's own third-person body — or a remote one — is a one-line
/// change at the call site rather than new plumbing in this crate.
#[must_use]
pub fn player_model_name(slim: bool) -> &'static str {
    if slim { "player_slim" } else { "player_wide" }
}

/// Which humanoid arm rig a model animates with — the render-crate side of
/// vanilla's zombie model overriding the plain humanoid model's arm swing.
///
/// [`AnimFamily`](crate::entity_anim::AnimFamily) is classified *structurally*
/// from part names, on purpose (see that module's docs). A zombie's skeleton is
/// part-for-part identical to a player's, so no structural rule can separate
/// them: the distinction is which model vanilla instantiates for the type.
/// That fact is a name mapping, so it lives here next to
/// [`canonical_model_name`] — the module that already owns "which vanilla
/// model draws this mob" — rather than being smuggled into the structural
/// classifier.
#[must_use]
pub fn humanoid_arms_for(model_name: &str) -> HumanoidArms {
    match model_name {
        // Every model that applies the zombie arm-drop animation after its
        // base pose, enumerated from the 26.2 client tree rather than from
        // the name "zombie": the shared zombie model family (used directly by
        // the zombie, and reused by the drowned and the husk), the zombie
        // villager model, and the zombified piglin model.
        //
        // `zombified_piglin` was missing and got `HumanoidArms::Swinging`,
        // i.e. a plain player arm swing where vanilla gives it the raised
        // undead arms. `giant` is deliberately absent: the giant mob's
        // renderer uses a bare humanoid model, not a zombie one, so its arms
        // hang. The illager model also applies the zombie arm-drop animation
        // but passes a hardcoded flag inside one arm-pose branch of a
        // different model family, so it is not this mapping (see
        // `mob_draws_bow_when_aggressive` for the illager gap).
        "zombie" | "husk" | "drowned" | "zombie_villager" | "zombified_piglin" => {
            HumanoidArms::Zombie
        }
        _ => HumanoidArms::Swinging,
    }
}

/// Whether this entity type's renderer maps **being aggressive with a bow in
/// the main hand** to the bow-and-arrow arm pose — i.e. whether vanilla draws
/// it with the skeleton family's renderer.
///
/// # Why this is a per-type rule and not a general one
///
/// The arm pose is chosen per *renderer*, not per model, and only the
/// skeleton-family renderer has this override:
///
/// ```text
/// same arm as main hand && is aggressive && main-hand item is a bow
///     ? bow-and-arrow pose : the base pose
/// ```
///
/// An aggressive **zombie** holding a bow does *not* get this pose — its
/// renderer only overrides the arm pose for the spear/stab case — and neither
/// does a pillager, whose whole arm-pose vocabulary is a different enum on a
/// different model class. So applying "aggressive + bow ⇒ draw" to every mob
/// would put half the hostile mobs in the world into a pose vanilla never
/// shows.
///
/// # The type set
///
/// Every subclass of the skeleton-family renderer in the 26.2 client tree:
/// the plain skeleton, wither skeleton, stray, bogged and parched renderers.
/// Keyed by entity type path (all five are registered types — ids 115, 147,
/// 128, 16, 97 in the census dump), because that is what the extract stage
/// has; note this is *not* the [`canonical_model_name`] space, where `bogged`
/// currently aliases to `skeleton`. Rendering `bogged` through the skeleton
/// mesh does not change which renderer vanilla would have used, so the rule
/// is keyed on the real type.
#[must_use]
pub fn mob_draws_bow_when_aggressive(type_path: &str) -> bool {
    matches!(
        type_path,
        "skeleton" | "wither_skeleton" | "stray" | "bogged" | "parched"
    )
}

/// Whether this entity type is drawn by the avatar renderer — the **only**
/// renderer whose arm-pose fallthrough reaches an "item" pose for a merely
/// *held* item.
///
/// # "every armed mob raises an arm in vanilla" is false, and this is the record
///
/// Two arm-pose fallthroughs sit at the bottom of the humanoid chain, and
/// they end differently:
///
/// ```text
/// avatar renderer's fallthrough        … held item is a spear ? spear pose : item pose;
/// humanoid-mob renderer's fallthrough  … held item is a spear ? spear pose : empty pose;
/// ```
///
/// A **player** holding a sword raises the arm; a **zombie** holding the same
/// sword does not. Reading only the avatar renderer — which is where the
/// "item" pose is naturally discovered, because it is the one that reaches
/// it — yields the opposite conclusion, and it was written down here as
/// "vanilla's fallthrough runs for any non-empty hand, so every armed mob has
/// a raised arm in vanilla and hangs its arms here". The first clause is true
/// *of that renderer*; the conclusion is wrong, because mobs never reach it.
///
/// Every humanoid-mob override delegates to the humanoid-mob renderer's
/// "empty" tail: the skeleton family (aggressive+bow, else fall through), the
/// zombie family (stab, else fall through), the drowned (aggressive+trident,
/// else fall through), and the piglin, whose pose comes from the piglin's own
/// server-side enum. So hanging arms on an armed mob is **correct today**,
/// and widening the fallthrough to all humanoids would have put every armed
/// zombie, skeleton, husk and armour stand into a pose vanilla never shows.
///
/// # The type set
///
/// Vanilla's renderer dispatch routes exactly two classes to the avatar
/// renderer: the player and the mannequin — the two subclasses of its common
/// base. Both are registered entity types, so this is keyed on the type path
/// the extract stage has, the same space as [`mob_draws_bow_when_aggressive`].
#[must_use]
pub fn renderer_is_avatar(type_path: &str) -> bool {
    matches!(type_path, "player" | "mannequin")
}

// Aggressive-driven poses vanilla has that this build does **not** model, and why
// each is left rather than approximated. Kept as a comment beside the rule it
// bounds, rather than as a doc on some function nobody calls.
//
// * **The drowned's arm pose**: aggressive + a trident ⇒ a throw-trident
//   pose. The pose body is two lines, but that pose is the first
//   **one-handed** pose in vanilla's table and every pose
//   [`crate::ArmPose`] models today is two-handed. One-handed means the base
//   setup-animation step's offhand-pose fork actually branches, and
//   `Skeleton::pose_arms_for_item` does not implement that fork. Adding the
//   pose without it would silently pose the wrong arm on an off-hand
//   trident — a defect class already hit once by folding the bow's two
//   branches into one signed expression.
// * **The illager renderer**: copies "is aggressive" into its render state,
//   but an illager's arms are driven by a different enum on a different
//   model class, and the value is computed server-side per subclass (the
//   vindicator returns an "attacking" pose when aggressive; the pillager the
//   same, behind two crossbow cases). Reaching it needs an illager arm
//   family in [`crate::entity_anim`], not a metadata bit.
// * **A mob's "left-handed" flag** (bit `0x02` of the same byte, decoded and
//   unconsumed): flips which arm is the main arm, which flips which arm
//   every pose applies to. See
//   `lodestone_entity::metadata::MobFlags::left_handed`.
//
// What *is* covered besides the bow: [`humanoid_arms_for`]'s
// `HumanoidArms::Zombie` family, whose arm drop reads the same flag (a
// steeper aggressive drop angle, `-PI/1.5`, vs a shallower one when not
// aggressive, `-PI/2.25`). That was a second island — the field existed on
// `AnimInput` and every shell call site passed `false`.

/// Which [`HandPoseOverride`] a model's own hand-translate step needs, keyed by the
/// same [`entity_models`] name [`humanoid_arms_for`] reads. The five corpus
/// models with an override; see [`HandPoseOverride`] and
/// `held_item_matrix`'s doc comment for the source table this was read from.
#[must_use]
pub fn hand_pose_override_for(model_name: &str) -> HandPoseOverride {
    match model_name {
        "skeleton" | "stray" | "wither_skeleton" => HandPoseOverride::PivotShiftTexels(1.0),
        "player_slim" => HandPoseOverride::PivotShiftTexels(0.5),
        "vex" => HandPoseOverride::Vex,
        "allay" => HandPoseOverride::Allay,
        _ => HandPoseOverride::Structural,
    }
}

/// The in-jar sheet path for a corpus texture reference (`"entity/zombie/zombie"`
/// → `"assets/minecraft/textures/entity/zombie/zombie.png"`).
pub(crate) fn sheet_path(reference: &str) -> &'static str {
    Box::leak(format!("assets/minecraft/textures/{reference}.png").into_boxed_str())
}

/// The in-jar texture path(s) for a model, in priority order — the first that
/// the resource pack actually contains wins. Version-free: these are vanilla
/// resource-pack paths keyed by the model name [`canonical_model_name`]
/// produces, not protocol data.
///
/// Biome/variant-correct selection (a cold pig, a black horse) is a refinement:
/// this returns each entry's canonical sheet, which is the `_temperate` skin for
/// the mobs 26.2 split by climate. Returns an empty slice for a model with no
/// known sheet, so the caller falls back to a placeholder rather than failing.
///
/// **Derived from the corpus, not hand-listed.** Each entry already carries its
/// own [`EntityTexture`](lodestone_assets::entity::EntityTexture); a second
/// hand-written table here can only ever drift out of step with it, and did:
/// `drowned` had `entity/zombie/drowned` in the corpus while this table knew
/// only nine models. The per-model paths are interned once (the corpus is a
/// fixed compile-time set of ~90 entries) so the `&'static` signature holds.
#[must_use]
pub fn entity_texture_candidates(model_name: &str) -> &'static [&'static str] {
    static SHEETS: std::sync::OnceLock<Vec<(&'static str, &'static [&'static str])>> =
        std::sync::OnceLock::new();
    let sheets = SHEETS.get_or_init(|| {
        entity_models()
            .into_iter()
            .map(|entry| {
                let reference = entry.texture.default_path();
                let mut paths = vec![sheet_path(reference)];
                // 26.2 split several farm mobs into `_temperate`/`_cold`/`_warm`
                // and removed the bare sheet; older packs ship only the bare one.
                // Listing the legacy name second resolves both without this crate
                // learning a version.
                if let Some(legacy) = reference.strip_suffix("_temperate") {
                    paths.push(sheet_path(legacy));
                }
                let paths: &'static [&'static str] = Box::leak(paths.into_boxed_slice());
                (entry.name, paths)
            })
            .collect()
    });
    sheets
        .iter()
        .find(|(n, _)| *n == model_name)
        .map_or(&[], |(_, paths)| *paths)
}

/// The corpus sheet **reference** (`"entity/wolf/wolf_ashen"`, no `assets/` prefix
/// and no extension) for one model and one *wire* variant, or `None` when the
/// variant carries nothing this model's texture axis can use.
///
/// This is the production caller for
/// [`EntityTexture::resolve`](lodestone_assets::entity::EntityTexture::resolve),
/// which had none: the corpus modelled every wolf breed and every climate skin, and
/// the whole render path asked only for `default_path()`. A function with zero
/// production **readers** is the dual of the usual island, and `cargo xtask
/// connectedness` structurally cannot see it — the packet reaches the fold and the
/// fold reaches a component; what is missing is anything downstream *asking*.
///
/// # Which axes are lifted, and why not all of them
///
/// Only the axes whose wire form actually arrives at the client today. Both of
/// these come over as [`EntityVariant::Keyed`] — a registry-holder key — which the
/// v770 metadata decoder raises from the serializer alone:
///
/// | model | wire | assets axis |
/// |---|---|---|
/// | `wolf` | `Keyed("minecraft:ashen")` | [`WolfCoat`] |
/// | `pig`, `cow`, `chicken` | `Keyed("minecraft:cold")` | [`Temperature`] |
///
/// Horse colour, llama, cat, parrot and mooshroom have corpus entries and their own
/// axes; they are deliberately absent rather than half-lifted, because each needs
/// its own answer to "does this key/ordinal actually reach us", and guessing one
/// wrong produces a confidently wrong skin rather than a missing one.
///
/// # The wolf's tame state: wired end to end
///
/// The wire carries vanilla's tame bit:
/// [`EntityMetadataUpdate`](lodestone_model::EntityMetadataUpdate) declares
/// `tamed: Option<bool>` and `sitting: Option<bool>`, `v770`'s
/// `read_entity_metadata` populates both from the tamable-mob shared-flags
/// metadata field's low bits under `MetadataClass::Tamable`, and `SimMob::snapshot`
/// (`crates/lodestone-server/src/mobs/mod.rs`) pushes them for wolf/cat/parrot/
/// ocelot. `crates/lodestone-ecs/src/ingest.rs::apply_entity_metadata` now folds
/// `tamed` into `lodestone_ecs::entity::Tamed` (per-entity, alongside `Baby` —
/// not a `crate::session` scalar), and the shell's draw-time call site
/// (`crates/lodestone-shell/src/entities.rs::extract_entity_draws`) bridges that
/// component off the ingest entity, the same way it bridges `Variant`, and calls
/// [`entity_variant_sheet_for`] rather than the plain [`entity_variant_sheet`].
///
/// [`entity_variant_sheet_for`] is the render-side half of the fix: it takes the
/// tame bit as a parameter rather than pinning [`WolfState::Wild`] internally.
/// [`entity_variant_sheet`] itself is left alone (still always `Wild`) — its
/// remaining callers are fixtures and other models that have no tame axis, so
/// changing its signature would only add a parameter every other caller ignores.
///
/// [`WolfCoat`]: lodestone_assets::entity::WolfCoat
/// [`WolfState`]: lodestone_assets::entity::WolfState
/// [`WolfState::Wild`]: lodestone_assets::entity::WolfState::Wild
/// [`Temperature`]: lodestone_assets::entity::Temperature
/// [`EntityVariant::Keyed`]: lodestone_model::EntityVariant::Keyed
#[must_use]
pub fn entity_variant_sheet(
    model_name: &str,
    variant: &lodestone_model::EntityVariant,
) -> Option<&'static str> {
    entity_variant_sheet_for(model_name, variant, false)
}

/// [`entity_variant_sheet`], plus the one bit that function cannot yet
/// receive: whether the entity is tamed (vanilla's tamable-mob shared-flags
/// metadata field, bit `4`, decoded into
/// [`EntityMetadataUpdate::tamed`](lodestone_model::EntityMetadataUpdate::tamed)
/// today — see [`entity_variant_sheet`]'s own doc for the wire/ECS chain and
/// exactly which piece downstream still has to change to reach this
/// parameter with a real value). Only `"wolf"` reads it; every other model
/// ignores it, matching [`entity_variant_sheet`]'s existing per-model table.
///
/// **The remaining wiring, for whoever picks this up**: a `Tamed(bool)`
/// component in `crates/lodestone-ecs/src/entity.rs`, folded from
/// `EntityMetadataUpdate::tamed` in
/// `crates/lodestone-ecs/src/ingest.rs::apply_entity_metadata` (an `ingest`
/// arm, not `session` — this is per-entity state, not a local-player scalar,
/// per this repo's router table), then read at the
/// `crates/lodestone-shell/src/entities.rs` draw-grouping call site and
/// passed here instead of the plain [`entity_variant_sheet`]. The wolf's
/// `sitting` bit has the same shape and no consumer yet either, but is not
/// part of this function's texture axis (vanilla renders a sitting wolf via
/// pose, not a different sheet).
#[must_use]
pub fn entity_variant_sheet_for(
    model_name: &str,
    variant: &lodestone_model::EntityVariant,
    tamed: bool,
) -> Option<&'static str> {
    let axis = variant_axis(model_name, variant, tamed)?;
    let entry = entity_models()
        .into_iter()
        .find(|entry| entry.name == model_name)?;
    Some(entry.texture.resolve(axis))
}

/// Lifts a wire variant (plus the tame bit, for a wolf) onto the
/// [`lodestone_assets`] texture axis this model's corpus entry selects on. See
/// [`entity_variant_sheet`] for the table and for the wolf tame-state wiring.
pub(crate) fn variant_axis(
    model_name: &str,
    variant: &lodestone_model::EntityVariant,
    tamed: bool,
) -> Option<lodestone_assets::entity::EntityVariant> {
    use lodestone_assets::entity::{EntityVariant as Axis, Temperature, WolfCoat, WolfState};

    let key = match variant {
        lodestone_model::EntityVariant::Keyed(id) => id,
        _ => return None,
    };
    // Namespace checked, not ignored: a data pack's `mypack:ashen` is a different
    // holder from `minecraft:ashen` and has no vanilla sheet.
    if key.namespace() != "minecraft" {
        return None;
    }
    match model_name {
        "wolf" => {
            let coat = match key.path() {
                "pale" => WolfCoat::Pale,
                "spotted" => WolfCoat::Spotted,
                "snowy" => WolfCoat::Snowy,
                "black" => WolfCoat::Black,
                "ashen" => WolfCoat::Ashen,
                "rusty" => WolfCoat::Rusty,
                "woods" => WolfCoat::Woods,
                "chestnut" => WolfCoat::Chestnut,
                "striped" => WolfCoat::Striped,
                _ => return None,
            };
            Some(Axis::Wolf {
                coat,
                state: if tamed { WolfState::Tame } else { WolfState::Wild },
            })
        }
        "pig" | "cow" | "chicken" => {
            let temperature = match key.path() {
                "temperate" => Temperature::Temperate,
                "cold" => Temperature::Cold,
                "warm" => Temperature::Warm,
                _ => return None,
            };
            Some(Axis::Temperature(temperature))
        }
        _ => None,
    }
}

/// Every in-jar sheet directory a variant-driven corpus entry can draw from, as
/// `"assets/minecraft/textures/entity/wolf/"`-shaped prefixes.
///
/// Derived from the corpus rather than hand-listed, for exactly the reason
/// [`entity_texture_candidates`] is: a second table here could only drift. A loader
/// walks these prefixes and keys what it finds by reference, so it needs no
/// enumeration of the variant enums — which is what keeps a new breed or a new
/// climate from needing a change here at all.
#[must_use]
pub fn entity_variant_sheet_dirs() -> Vec<&'static str> {
    let mut dirs: Vec<&'static str> = entity_models()
        .into_iter()
        .filter(|entry| entry.texture.is_variant())
        .filter_map(|entry| {
            let reference = entry.texture.default_path();
            let slash = reference.rfind('/')?;
            Some(sheet_dir(&reference[..=slash]))
        })
        .collect();
    dirs.sort_unstable();
    dirs.dedup();
    dirs
}

/// `"entity/wolf/"` → `"assets/minecraft/textures/entity/wolf/"`. Interned for the
/// same reason [`sheet_path`] is: the corpus is a fixed compile-time set, so the
/// `&'static` signature holds without a lifetime on the caller.
pub(crate) fn sheet_dir(reference: &str) -> &'static str {
    Box::leak(format!("assets/minecraft/textures/{reference}").into_boxed_str())
}

/// The corpus reference a jar path under one of [`entity_variant_sheet_dirs`]'
/// prefixes corresponds to: the inverse of [`sheet_path`].
///
/// `"assets/minecraft/textures/entity/wolf/wolf_ashen.png"` → `"entity/wolf/wolf_ashen"`.
/// `None` for a path that is not a texture under `assets/minecraft/textures/`, so a
/// loader can skip a stray `.mcmeta` without knowing this module's layout.
#[must_use]
pub fn sheet_reference_of(jar_path: &str) -> Option<&str> {
    jar_path
        .strip_prefix("assets/minecraft/textures/")?
        .strip_suffix(".png")
}
