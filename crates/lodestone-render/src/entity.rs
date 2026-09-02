//! The entity render plan: the version-free layer that turns *tracked entities*
//! into *a culled, instanced set of draws* for one frame.
//!
//! Entities are the second geometry pipeline (terrain being the first). They are
//! fundamentally different from blocks and reuse none of the mesher:
//!
//! * Their geometry is a **cuboid part hierarchy**, not paletted voxels, and it
//!   is already baked version-free by [`lodestone_assets::entity::bake_entity`]
//!   from the hand-ported [`entity_models`](lodestone_assets::entity_models)
//!   corpus. This module never re-derives geometry; it converts the baked quads
//!   into the shared [`ModelVertex`] format once per model type.
//! * They are **instanced**: a mob farm is hundreds of the same model at
//!   different transforms, so the mesh is uploaded once and each visible entity
//!   contributes only a per-instance model matrix (see
//!   [`crate::entity_pipeline`]). This module produces those matrices.
//!
//! # The placement transform (why it is exactly this and not eyeballed)
//!
//! Vanilla places a living entity with a fixed sequence of pose-stack ops,
//! read here from the decompiled 26.2 client:
//!
//! ```text
//!   translate(feetPos)                     // move to the entity's feet
//!   rotateY(180° - bodyYaw)                // face the body's yaw
//!   scale(-1, -1, 1)                       // model space is Y-down, Z-forward
//!   translate(0, -1.501, 0)                // lift feet to the ground plane
//! ```
//!
//! Model space has **Y pointing down** (the head cube spans `y ∈ [-8, 0]`, the
//! feet reach `y = +24`), which is why the `scale(-1, -1, 1)` flip is load
//! bearing rather than cosmetic: without it every mob renders upside down but
//! still recognisable, the exact "looks plausible, is wrong" trap. The
//! composition order is copied from source, not inferred, and
//! [`entity_model_matrix`] is unit-tested to put feet on the ground and the head
//! above them. The `scale(-1,-1,1)` has determinant `+1`, so it preserves
//! winding — a front face in model space stays a front face in world space, and
//! back-face culling remains valid.
//!
//! Per-part animation (head tracking, walk cycles) is a layer *above* this: it
//! adjusts each [`PartPose`](lodestone_assets::entity::PartPose) before baking.
//! This module renders the static rest pose posed only by body yaw, which is
//! what the incoming [`EntityView`](../../lodestone_client/state/struct.EntityView.html)
//! data supports today (position + rotation, no limb angles).

use glam::{Mat4, Vec3, Vec4};
use lodestone_assets::entity::{EntityModelDef, bake_entity_parts};
use lodestone_assets::entity_models::{EntityModelEntry, entity_models};
use lodestone_assets::equipment::{ArmourLayer, ArmourSlot, armour_item, humanoid_armour_model};
use lodestone_assets::{BakedQuad, DisplaySlot, DisplayTransform, DisplayTransforms, GuiLight};
use lodestone_data::entity_type::EntityType;

use crate::camera::Frustum;
use crate::entity_anim::{AnimInput, HandPoseOverride, HumanoidArms, Skeleton};
use crate::item_render::{UNITS_PER_BLOCK, display_matrix, display_matrix_for_hand};
use crate::models::{ModelMesh, ModelVertex, mesh_item_quads};

/// The vanilla feet-to-model lift (the pose-stack `translate(0, -1.501, 0)`
/// applied when placing a living entity), in blocks.
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
/// `0.0`-means-daylight sentinel lives in the shader, not here — this
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
/// registry identity the wire actually carries (issue #523's third pass:
/// `EntityType as u8` **is** the `add_entity` registry id, so this call takes
/// no string at any point between the decoded id and the corpus lookup).
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
fn corpus_names() -> &'static [&'static str] {
    static NAMES: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| entity_models().into_iter().map(|e| e.name).collect())
}

/// [`corpus_names()`] as a set, computed once behind a `OnceLock` from that
/// same slice (so the two can never drift), for an O(1) "is this a corpus
/// entry" test — the up-to-90 linear `&str` compares
/// [`canonical_model_name`] used to run per entity per frame (issue #523),
/// x4 for the base/armour/flame/wool passes in `gpu/entity_passes.rs`.
fn corpus_name_set() -> &'static std::collections::HashSet<&'static str> {
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
fn canonical_model_name_for_type(entity_type: EntityType) -> Option<&'static str> {
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
    // every one of up to 90 entries on every call (issue #523).
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
/// container/player_preview.rs`, which this pass does not own. That is the
/// genuine boundary issue #523 asks to stop at rather than force through.
///
/// What *did* change here (issue #523): a type path that **is** a real
/// registry entity now resolves via one [`EntityType::from_name`] binary
/// search (`O(log 158)`) into [`canonical_model_name_for_type`] — the same
/// enum-keyed alias table [`model_for_type`] uses, so the two paths cannot
/// silently disagree — instead of re-testing the three alias literals as
/// strings and falling through to a second string scan. Only a **non**-entity-
/// type string (a corpus/rig pseudo-name, or a boat path handled by suffix)
/// still walks the corpus-name/boat-suffix fallback directly.
fn canonical_model_name(type_path: &str) -> Option<&'static str> {
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
/// `lodestone_data::entity_census`'s per-type Java class column, which names them
/// `Boat`/`ChestBoat`/`Raft`/`ChestRaft`) — while the corpus carries exactly four
/// rigs, one per *class*, because that is how vanilla builds them: the species is
/// a texture, not geometry (`BoatRenderer` takes its `ModelLayerLocation` from the
/// boat's `getVariant()` and its model from `BoatModel`/`ChestBoatModel`/
/// `RaftModel`/`ChestRaftModel`). With no alias, `model_for_type("oak_boat")`
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
fn boat_model_name(type_path: &str) -> Option<&'static str> {
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
/// `AvatarRenderer` (26.2's player renderer) picks between `player_wide` and
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

/// Which [`HandPoseOverride`] a model's `translateToHand` needs, keyed by the
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
fn sheet_path(reference: &str) -> &'static str {
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
fn variant_axis(
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
fn sheet_dir(reference: &str) -> &'static str {
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

/// A CPU entity mesh in the shared wide [`ModelVertex`] format, plus the model's
/// local-space bounding box for culling.
///
/// One of these is built per *model type* (not per entity) and uploaded once;
/// every instance of that type reuses it. Positions are in the baked model frame
/// (blocks, Y-down): the per-instance [`entity_model_matrix`] moves them into the
/// world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartRange {
    /// First index belonging to this part.
    pub index_start: u32,
    /// Number of indices in this part.
    pub index_count: u32,
    /// First vertex belonging to this part.
    pub vertex_start: u32,
    /// Number of vertices in this part.
    pub vertex_count: u32,
}

/// A baked entity model split into animatable parts.
///
/// Vertices are in **part-local** space: the part's own rest pose is *not* folded
/// in, so a joint stays a joint. Multiplying a part's vertices by its matrix from
/// [`Skeleton::rest_pose`] reproduces the whole-model bake exactly — asserted
/// over the entire corpus by `lodestone-assets`' `part_bake_recomposes_to_the_
/// whole_model_bake`. That equivalence is what lets the renderer keep one static
/// vertex buffer per model *type* and move only matrices per frame.
#[derive(Debug, Clone)]
pub struct EntityMesh {
    /// Four vertices per quad, in the shared model-vertex format, part-local.
    pub vertices: Vec<ModelVertex>,
    /// Six indices per quad, wound so front faces point outward.
    pub indices: Vec<u32>,
    /// One index sub-range per part, in [`Skeleton`] part order.
    pub parts: Vec<PartRange>,
    /// The part hierarchy and its animator.
    pub skeleton: Skeleton,
    /// Local-space AABB minimum (model frame, blocks), at rest.
    pub local_min: Vec3,
    /// Local-space AABB maximum (model frame, blocks), at rest.
    pub local_max: Vec3,
    /// This model's `translateToHand` override, if vanilla's subclass departs
    /// from `HumanoidModel`'s. See [`HandPoseOverride`] and
    /// [`hand_pose_override_for`]; consumed by [`EntityInstance::new`] to fill
    /// [`EntityInstance::hand_transforms`].
    pub hand_override: HandPoseOverride,
}

impl EntityMesh {
    /// Bake a model definition into a renderable mesh.
    ///
    /// Each [`EntityQuad`](lodestone_assets::entity::EntityQuad) becomes four
    /// [`ModelVertex`] and six indices. The winding is chosen per quad from the
    /// baked outward normal so front faces are counter-clockwise (matching the
    /// pipeline's `Ccw`/back-cull), independent of the quad's corner order or
    /// mirror flag.
    #[must_use]
    pub fn from_model(def: &EntityModelDef) -> Self {
        Self::from_named_model("", def)
    }

    /// Bake a model definition into a renderable mesh, applying the arm rig
    /// [`humanoid_arms_for`] assigns to `model_name`.
    ///
    /// The name has to be known *here* rather than at pose time because a zombie
    /// rig moves the arms in its **resting** pose, and the mesh's local AABB is
    /// taken from that resting pose. Choosing the rig later would leave every
    /// zombie with a culling box drawn around a mob standing to attention while
    /// the drawn one has its arms out in front — the classic "correct until it
    /// clips at the screen edge" bug.
    #[must_use]
    pub fn from_named_model(model_name: &str, def: &EntityModelDef) -> Self {
        let baked = bake_entity_parts(def);
        let skeleton =
            Skeleton::from_parts(&baked).with_humanoid_arms(humanoid_arms_for(model_name));
        let rest = skeleton.rest_pose();

        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut parts = Vec::with_capacity(baked.len());
        let mut local_min = Vec3::splat(f32::INFINITY);
        let mut local_max = Vec3::splat(f32::NEG_INFINITY);

        for (part_index, part) in baked.iter().enumerate() {
            let index_start = indices.len() as u32;
            let vertex_start = vertices.len() as u32;
            // The rest matrix is used only for the local AABB: the vertices
            // themselves stay part-local so the animator can rotate the joint.
            let rest_m = rest[part_index];
            for quad in &part.quads {
                for p in &quad.positions {
                    let posed = rest_m.transform_point3(Vec3::from(*p));
                    local_min = local_min.min(posed);
                    local_max = local_max.max(posed);
                }
            }
            push_part_quads(&part.quads, &mut vertices, &mut indices);
            parts.push(PartRange {
                index_start,
                index_count: indices.len() as u32 - index_start,
                vertex_start,
                vertex_count: vertices.len() as u32 - vertex_start,
            });
        }

        if indices.is_empty() {
            local_min = Vec3::ZERO;
            local_max = Vec3::ZERO;
        }

        // A creeper's drawn size is not its rest size: `pose_swelling` scales the
        // whole model by up to `MAX_SWELL_SCALE` (~41% horizontally) about the
        // model-space feet plane while its fuse burns. Everything above derives the
        // box from `rest_pose` alone, which is this function's own doc comment's
        // "correct until it clips at the screen edge" bug — a swelling creeper at
        // the frustum edge would be culled while still visibly on screen.
        //
        // Padded once here, at bake time, rather than recomputed per frame in
        // `EntityInstance::placed`: one constant box that always contains the drawn
        // model costs a slightly conservative cull and cannot drift from the pose,
        // where a per-frame exact box is a second derivation of the same geometry.
        //
        // The y term is conjugated about `MODEL_FEET_OFFSET` because that is what
        // `swell_root_affine` does — a plain scale about the model origin would let
        // the padded box sink below the feet plane rather than grow upward. `min`/
        // `max` over both corners, so the widening is correct whatever the signs.
        if model_name == "creeper" {
            let s = crate::entity_anim::MAX_SWELL_SCALE;
            let swollen = |v: Vec3| {
                Vec3::new(
                    v.x * s,
                    MODEL_FEET_OFFSET + (v.y - MODEL_FEET_OFFSET) * s,
                    v.z * s,
                )
            };
            let (a, b) = (swollen(local_min), swollen(local_max));
            local_min = local_min.min(a.min(b));
            local_max = local_max.max(a.max(b));
        }

        EntityMesh {
            vertices,
            indices,
            parts,
            skeleton,
            local_min,
            local_max,
            hand_override: hand_pose_override_for(model_name),
        }
    }

    /// Number of quads in the mesh.
    #[must_use]
    pub fn quad_count(&self) -> usize {
        self.indices.len() / 6
    }
}

/// Append one part's baked quads to a shared vertex/index buffer as
/// **part-local** geometry, winding each triangle pair from the quad's own baked
/// outward normal.
///
/// The one implementation of that winding rule, shared by [`EntityMesh`],
/// [`ArmourMesh`] and [`crate::block_entity::BlockEntityMesh`]. It has to be
/// shared rather than copied: an armour layer whose winding disagreed with the
/// mob it sits on would be invisible from exactly the half of the angles the mob
/// is visible from, and only once back-face culling is eventually turned on — a
/// defect that cannot be seen today and would land later, on somebody else's
/// change. A chest whose winding disagreed with the mobs beside it would have
/// the same property, which is why `block_entity` reaches in here rather than
/// keeping a "simple" local copy.
pub(crate) fn push_part_quads(
    quads: &[lodestone_assets::entity::EntityQuad],
    vertices: &mut Vec<ModelVertex>,
    indices: &mut Vec<u32>,
) {
    for quad in quads {
        let base = vertices.len() as u32;
        for i in 0..4 {
            vertices.push(ModelVertex {
                position: quad.positions[i],
                uv: quad.uvs[i],
                ao: 1.0,
                // The entity shader does **not** read this byte: entity light is
                // per *instance* (one lightmap sample per mob, as vanilla does),
                // so it arrives on the instance buffer, not here. The field is
                // filled anyway because the vertex layout is shared with
                // terrain, and a full-bright value keeps a mis-wired reader
                // honest rather than rendering every mob black.
                light: ENTITY_FULLBRIGHT,
                tint: 255,
                anim: 0,
                cutout_bypass: 0,
                // No biome tint on entities (see `models` module docs' D1
                // note: entities share `ModelVertex`'s layout but carry no
                // tint), so this override is always inert.
                tint_rgb_override: [0, 0, 0, 0],
            });
        }
        // Wind the two triangles so the geometric normal agrees with the baked
        // outward normal; otherwise back-face culling would drop the visible
        // side.
        let n = Vec3::from(quad.normal);
        let p0 = Vec3::from(quad.positions[0]);
        let p1 = Vec3::from(quad.positions[1]);
        let p2 = Vec3::from(quad.positions[2]);
        if (p1 - p0).cross(p2 - p0).dot(n) >= 0.0 {
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        } else {
            indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
        }
    }
}

/// The world placement transform for a standing mob, matching vanilla's
/// living-entity pose-stack order exactly (see the module docs).
///
/// `feet` is the entity's world position (its feet, as the protocol reports it),
/// `body_yaw_deg` its body yaw in degrees (Minecraft convention: `0` faces `+Z`),
/// and `scale` a uniform size multiplier (`1.0` for a normal adult; babies and
/// scaled mobs pass a smaller value). Applying this to a baked model vertex
/// yields its world position.
#[must_use]
pub fn entity_model_matrix(feet: Vec3, body_yaw_deg: f32, scale: f32) -> Mat4 {
    dying_entity_model_matrix(feet, body_yaw_deg, scale, 0.0)
}

/// [`entity_model_matrix`] with the **death fall-over** — vanilla's
/// living-entity pose-stack setup rotation's Z-axis term, in degrees, from
/// [`death_fall_over_degrees`](crate::entity_anim::death_fall_over_degrees).
///
/// # The roll's position in the product is the whole of this function
///
/// It sits between the body yaw and the Y-down flip, because that is where
/// vanilla's pose stack puts it:
///
/// ```text
///   setup rotations:  rotate about Y by (180 - bodyRot)          // `rotate`
///                     rotate about Z by (fall * 90)               // this term
///   render:           scale(-1, -1, 1)                            // `flip_scale`
///                     translate(0, -1.501, 0)                     // `lift`
/// ```
///
/// Two consequences that a "just multiply a Z rotation on" reading gets wrong:
///
/// * It is applied **before** the `lift`, so the mob rotates about the plane its
///   feet stand on and topples sideways. Composing the roll on the *outside*
///   (`T(feet) · Rz · Ry · …`) rotates about the same point but in the wrong frame,
///   so the fall direction stops tracking the body yaw; composing it after the lift
///   swings the mob about its own mid-height and leaves its feet in the air.
/// * `Rz` commutes with `flip_scale` (a `diag(-s, -s, s)` is `diag(-1,-1,1)` times a
///   uniform scale, and the sign flips cancel across the xy block), so it is *only*
///   the `lift` that fixes the position — which is exactly why the roll cannot be
///   folded into the caller's matrix afterwards and this is a separate function
///   rather than a multiply at the call site.
///
/// `fall_over_deg` of `0.0` is an exact identity (`Mat4::from_rotation_z(0)` is the
/// identity), so every living entity gets the bit-identical matrix
/// [`entity_model_matrix`] returned before this existed.
#[must_use]
pub fn dying_entity_model_matrix(
    feet: Vec3,
    body_yaw_deg: f32,
    scale: f32,
    fall_over_deg: f32,
) -> Mat4 {
    let translate_feet = Mat4::from_translation(feet);
    let rotate = Mat4::from_rotation_y((180.0 - body_yaw_deg).to_radians());
    let fall_over = Mat4::from_rotation_z(fall_over_deg.to_radians());
    // scale(-1,-1,1) folded with the uniform entity scale.
    let flip_scale = Mat4::from_scale(Vec3::new(-scale, -scale, scale));
    let lift = Mat4::from_translation(Vec3::new(0.0, -MODEL_FEET_OFFSET, 0.0));
    translate_feet * rotate * fall_over * flip_scale * lift
}

/// The vertical bob and extra spin a **non-living vehicle** rig needs in place
/// of [`MODEL_FEET_OFFSET`], keyed by model name — the second switch beside
/// [`projectile_pitch_offset_deg`] that decides which of three placements a
/// corpus model gets. `None` for every model that really is drawn through the
/// living-entity renderer (every mob, the player, and — despite the name —
/// `armor_stand`, which is a living entity in vanilla and keeps the
/// 1.501 lift).
///
/// Read from the 26.2 decompile, not inferred:
///
/// * `boat`/`chest_boat`/`raft`/`chest_raft` — the boat renderer's submit
///   step does `translate(0, 0.375, 0)`, `rotateY(180 - yRot)`,
///   `scale(-1, -1, 1)`, **then a fixed `rotateY(90)`** — a boat is drawn
///   through the plain entity renderer, not the living-entity one, so there
///   is no 1.501 lift at all, and the model (hull length along local `+X`,
///   matching the boat model's own pivots) needs that trailing spin to face
///   the right way once the yaw and flip are applied. Dropping it would
///   leave every boat floating at the right height but broadside to its
///   heading.
/// * `minecart` — the minecart renderer's submit step also does a `0.375`
///   bob before its own `scale(-1, -1, 1)` and no lift. Vanilla composes the
///   cart's yaw as a bare `rotateY(yRot)` (no `180 -`, no rail curve
///   tracking, both because this engine has no per-tick rail-curve state to
///   feed it) rather than the mob convention this crate already applies
///   elsewhere; reusing the existing `180 - yaw` term here rather than
///   porting that difference keeps the change scoped to the lift bug this
///   function exists to fix, so the extra spin is `0.0` (an exact identity)
///   rather than a second unverified rotation formula.
///
/// `end_crystal` is deliberately **not** in this table: its renderer has no
/// `scale(-1, -1, 1)` flip at all, so it is not a small variation on this
/// placement the way the vehicles are — fixing it needs its own
/// investigation into whether the corpus geometry was even authored for the
/// flipped frame, not a table entry here.
#[must_use]
pub fn non_living_vehicle_placement(model_name: &str) -> Option<(f32, f32)> {
    match model_name {
        // `"boat_water_patch"` joins this arm rather than getting its own:
        // the boat renderer's submit step submits the water-patch geometry
        // **inside the same pushed pose-stack block**
        // as the main model, after the identical bob/rotate/flip/spin
        // sequence — so the patch's placement transform is not merely
        // *similar* to the boat's, it is the same pose-stack state the boat
        // model itself just submitted through. Omitting it here would leave
        // the mask floating at the wrong height and facing broadside, right
        // back to the "water shows through the bottom" symptom this exists
        // to fix, just from a mask sitting nowhere near the hull instead of
        // no mask at all.
        "boat" | "chest_boat" | "raft" | "chest_raft" | "boat_water_patch" => Some((0.375, 90.0)),
        "minecart" => Some((0.375, 0.0)),
        // A leash knot's renderer flips the model and submits it, and does
        // nothing else — no bob, no yaw, and (because it is not a living-entity
        // renderer) no 1.501 feet lift. So it belongs in this table rather than
        // on the mob placement, which would bury it 1.501 blocks under the fence
        // post it is tied to; the bob is a genuine `0.0` rather than a stand-in.
        //
        // The `180 - yaw` this placement applies is a harmless surplus here: the
        // entity's yaw is always zero and the knot is a 6×6 box centred on its
        // own pivot, so the half-turn only mirrors the sheet across X.
        "leash_knot" => Some((0.0, 0.0)),
        // A wither skull's renderer applies the flip and *nothing else*: the
        // skull's facing comes from its model's own head rotation, set from the
        // entity's yaw. Composed under the flip that is `Ry(-yaw)`, and this
        // placement's `Ry(180 - yaw)` reaches it exactly at an extra spin of
        // 180° — an identity, not a fitted number. The head's matching *pitch*
        // is dropped: this placement has no pitch term, so a skull climbing or
        // diving stays level.
        "wither_skull" => Some((0.0, 180.0)),
        // A shulker bullet is lifted `0.15` and then tumbled on all three axes
        // at three rates off its own age, over which a second translucent copy
        // is drawn at 1.5×. Neither the tumble nor the halo is available to a
        // rig with one mesh, one sheet and no clock, so the bullet gets a fixed
        // orientation — tolerable only because its three slabs make it symmetric
        // under any quarter turn. The bob is real and is kept.
        "shulker_bullet" => Some((0.15, 0.0)),
        // The wind charge's renderer applies neither a flip nor a rotation —
        // the dispatcher's bare translate is all it gets, so vanilla's box union
        // never turns to face travel direction, only its (unported) internal
        // counter-spin moves. There is no "translate only" placement in this
        // table to route it through instead, so it takes the ordinary flip and a
        // zero extra yaw, same as `wither_skull`/`shulker_bullet` above. See
        // `wind_charge_model`'s doc for why that is tolerable here: both boxes
        // are close to rotationally symmetric, so the wrong flip and the
        // yaw-following are not an obvious mirroring defect.
        "wind_charge" => Some((0.0, 0.0)),
        _ => None,
    }
}

/// The world placement transform for a **non-living vehicle** — a model
/// [`non_living_vehicle_placement`] recognises — matching the vanilla pose-stack
/// order that function documents: bob, yaw, flip, then the model's own extra
/// spin. `vertical_offset` is the bob (in world-Y, applied before the yaw
/// rotate — the two commute since the bob is Y-only and the rotation is about
/// Y) and `extra_yaw_deg` is the trailing spin, `0.0` for models with none.
#[must_use]
pub fn non_living_vehicle_matrix(
    feet: Vec3,
    yaw_deg: f32,
    scale: f32,
    vertical_offset: f32,
    extra_yaw_deg: f32,
) -> Mat4 {
    let translate_feet = Mat4::from_translation(feet);
    let bob = Mat4::from_translation(Vec3::new(0.0, vertical_offset, 0.0));
    let rotate = Mat4::from_rotation_y((180.0 - yaw_deg).to_radians());
    let flip_scale = Mat4::from_scale(Vec3::new(-scale, -scale, scale));
    let spin = Mat4::from_rotation_y(extra_yaw_deg.to_radians());
    translate_feet * bob * rotate * flip_scale * spin
}

/// The extra pitch, in degrees, a projectile rig needs on top of the entity's
/// own `xRot` — or `None` for a model that is **not** placed by
/// [`projectile_model_matrix`].
///
/// This is the one switch that decides which of the two placements a corpus
/// model gets, so it is also the thing that would put every arrow 1.5 blocks
/// **above** where it belongs and mirrored if it returned `None` by mistake — see
/// [`projectile_model_matrix`] for why the offset points *up* and not down,
/// which is a direction that was initially recorded backwards. It is keyed on
/// the *model name*, not the entity type path, because that is what
/// [`EntityModelSet`] already keys everything else by, and because vanilla's
/// own distinction is which renderer *type* draws the entity:
///
/// * `arrow`, `spectral_arrow` — a shared arrow renderer (used by both the
///   tippable and spectral arrow variants). Pitch about the Z axis with
///   **no** offset: the arrow model's shaft already lies along `+X`.
/// * `trident` — the thrown-trident renderer applies a Z-axis rotation of
///   `xRot + 90`. The trident model's pole lies along `Y` with the spikes at
///   negative `Y`; the `+90°` is exactly what rotates that axis onto the
///   arrow's `+X`, so one matrix serves both rigs and the whole difference
///   between them is this number.
///
/// Every other model — every mob, the player, and the block-entity rigs — is
/// drawn through the living-entity renderer (or a block entity) and returns
/// `None`.
#[must_use]
pub fn projectile_pitch_offset_deg(model_name: &str) -> Option<f32> {
    match model_name {
        "arrow" | "spectral_arrow" => Some(0.0),
        "trident" => Some(90.0),
        // A llama spit's renderer is this placement term for term —
        // `Ry(yRot - 90°)` then `Rz(xRot)`, no flip and no feet lift — so it
        // belongs here rather than on the mob path, with no offset for the same
        // reason as the arrow: its cluster is authored around the shot axis.
        //
        // One deviation, and it is not expressible here: vanilla lifts the spit
        // `0.15` blocks in **world** space *before* the two rotations. A mesh
        // offset would rotate with the spit instead of staying vertical, and
        // this matrix has no pre-rotation translation, so the spit draws 0.15
        // blocks low. Fixing it means a bob term on this placement, which every
        // other user would pass zero for.
        "llama_spit" => Some(0.0),
        _ => None,
    }
}

/// The world placement transform for a **projectile**, matching vanilla's
/// arrow-renderer submit step's pose-stack order.
///
/// ```text
///   translate(pos)                       // move to the entity's position
///   rotateY(yRot - 90°)                  // face travel direction
///   rotateZ(xRot + pitch_offset)         // pitch about Z, not X
/// ```
///
/// # Why this is not [`entity_model_matrix`] with a pitch bolted on
///
/// A projectile is drawn through the plain entity renderer, **not** the
/// living-entity one. The plain entity renderer applies no scale at all; the
/// `scale(-1, -1, 1)` and the `translate(0, -1.501, 0)` that
/// [`entity_model_matrix`] carries both belong to the living-entity renderer
/// alone. So a projectile gets **neither**, and there is consequently no flip
/// here: the projectile meshes in
/// [`entity_models`](lodestone_assets::entity_models) are authored `+Y` **up**
/// rather than in the mob rigs' `Y`-down frame.
///
/// Reusing the mob matrix would draw every arrow [`MODEL_FEET_OFFSET`] = 1.501
/// blocks **above** its reported position, and pointing along a reflected axis.
/// Note the direction: the lift is applied *before* the `scale(-1, -1, 1)`, so
/// `-1.501` comes back out as `+1.501` — an earlier note here said "below",
/// and so did the first draft of the test that now pins it
/// (`reusing_the_mob_matrix_would_lift_an_arrow_and_reverse_it`). Either way it
/// reads as a texture bug rather than a placement bug, which is why it is worth
/// the separate function.
///
/// # Rotations, and why the axis matters
///
/// `pos` is the entity's world position, `yaw_deg` its `yRot` and `pitch_deg` its
/// `xRot` — both as the server reports them, both derived by vanilla from
/// `atan2` on the projectile's own velocity when it is shot, which is *not*
/// the yaw convention a mob's body uses: vanilla's shoot step sets
/// `yRot = atan2(mx, mz)`, so a projectile fired by a player looking at yaw
/// `Y` carries `yRot = -Y`. `Ry(yRot - 90°)` maps model
/// `+X` to `(sin yRot, 0, cos yRot)`, which is exactly that motion direction —
/// the two conventions agree only because both halves are taken from vanilla
/// together.
///
/// Both signs are the **opposite** of a player's, so they were measured against
/// Mojang's own 26.2 server over RCON rather than only read: `+X` motion gives
/// `yRot = +90` (a player facing `-X` has yaw `+90`), and *rising* motion gives a
/// **positive** `xRot` (a player looking up has a *negative* pitch). Nine
/// direction cases, nine exact matches — see `docs/projectile-renderers.md`,
/// which also records why the first run of that probe read zero for all nine.
///
/// Pitch is a rotation about **`Z`**, not `X`, because the shaft runs along `+X`.
/// A pitch applied about `X` would spin the arrow about its own axis and leave
/// the silhouette almost unchanged while every arrow flew level — the "looks
/// plausible, is wrong" shape this file's module docs warn about for the mob
/// flip.
#[must_use]
pub fn projectile_model_matrix(pos: Vec3, yaw_deg: f32, pitch_deg: f32, scale: f32) -> Mat4 {
    Mat4::from_translation(pos)
        * Mat4::from_rotation_y((yaw_deg - 90.0).to_radians())
        * Mat4::from_rotation_z(pitch_deg.to_radians())
        * Mat4::from_scale(Vec3::splat(scale))
}

/// A single entity to render: which model type draws it, its world transform,
/// and its world-space AABB for frustum culling.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityInstance {
    /// The [`entity_models`] entry name that supplies this entity's mesh.
    pub model: &'static str,
    /// The per-instance model → world matrix (whole-entity placement).
    pub transform: Mat4,
    /// One `entity → world` matrix per skeleton part, in mesh part order:
    /// `transform * part_matrix`. Drawing part `p`'s index range instanced over
    /// `part_transforms[p]` is what makes a limb swing.
    pub part_transforms: Vec<Mat4>,
    /// The `entity → world` `translateToHand` matrix for `[Arm::Right,
    /// Arm::Left]`, honoring this model's [`HandPoseOverride`] — `None` for an
    /// arm the model doesn't have.
    ///
    /// **Prefer this over indexing `part_transforms` by
    /// `skeleton.index_of(arm.part_name())` when placing a held item.** For
    /// [`HandPoseOverride::Structural`] the two are numerically identical, but
    /// for the five corpus models with a real override they are not, and
    /// cannot be made to be: the override is scoped to `translateToHand`
    /// alone, while `part_transforms[arm]` is shared with the whole-body
    /// instanced draw and also places the arm's own visible mesh. See
    /// [`HandPoseOverride`]'s doc comment for why folding the override into
    /// `part_transforms` would be a new bug, not a fix.
    pub hand_transforms: [Option<Mat4>; 2],
    /// World-space AABB minimum.
    pub aabb_min: Vec3,
    /// World-space AABB maximum.
    pub aabb_max: Vec3,
    /// Packed sky/block light (`sky << 4 | block`, `0..=15` each) sampled once
    /// at this entity's block position, exactly as vanilla samples it — one
    /// value for the whole mob, not per vertex. Defaults to
    /// [`ENTITY_FULLBRIGHT`]; set the real world value with
    /// [`with_light`](Self::with_light).
    pub light: u8,
}

impl EntityInstance {
    /// Build an instance for `model` at `feet`/`yaw`/`scale`, computing both the
    /// transform and a world AABB by transforming the model's local-bounds
    /// corners. `local_min`/`local_max` come from the model's [`EntityMesh`].
    #[must_use]
    pub fn new(
        model: &'static str,
        mesh: &EntityMesh,
        feet: Vec3,
        yaw_deg: f32,
        scale: f32,
        anim: &AnimInput,
    ) -> Self {
        Self::new_animated(model, mesh, feet, yaw_deg, scale, anim, 0.0, 0.0)
    }

    /// [`new`](Self::new) with the two per-entity animation states that are neither
    /// placement nor skeletal pose: a creeper's **swell** fraction (vanilla's
    /// per-tick swelling progress) and a dying entity's **`death_time`**
    /// (`deathTime + partialTicks`, `0.0` while alive).
    ///
    /// A separate constructor rather than two more arguments on [`new`](Self::new),
    /// for [`new_projectile`](Self::new_projectile)'s reason inverted: here the
    /// variants really *are* one placement with options, and **both** extras have a
    /// documented exact identity at `0.0` —
    /// [`Skeleton::pose_swelling`](crate::entity_anim::Skeleton::pose_swelling)
    /// delegates `pose` to itself at zero swell, and
    /// [`dying_entity_model_matrix`] reduces to [`entity_model_matrix`] at zero
    /// roll. So the five call sites with nothing to pass keep working
    /// bit-identically instead of being widened for symmetry.
    ///
    /// The two land in different places, which is why they are not one value: the
    /// swell reaches the **pose** (a scale about the model-space feet plane composed
    /// above the root part, so a creeper grows upward out of the ground rather than
    /// moving), while the fall-over reaches the **placement**, between the body yaw
    /// and the Y-down flip.
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "one argument per independent piece of a live entity's placement, \
                  pose and animation state; a bundle struct would move the same \
                  fields behind a name that adds nothing"
    )]
    pub fn new_animated(
        model: &'static str,
        mesh: &EntityMesh,
        feet: Vec3,
        yaw_deg: f32,
        scale: f32,
        anim: &AnimInput,
        swell: f32,
        death_time: f32,
    ) -> Self {
        Self::placed(
            model,
            mesh,
            dying_entity_model_matrix(
                feet,
                yaw_deg,
                scale,
                crate::entity_anim::death_fall_over_degrees(death_time),
            ),
            anim,
            swell,
        )
    }

    /// Build an instance for a **projectile** — a model
    /// [`projectile_pitch_offset_deg`] recognises — at `pos`/`yaw`/`pitch`/`scale`,
    /// placed by [`projectile_model_matrix`] instead of [`entity_model_matrix`].
    ///
    /// Separate constructor rather than a `pitch` argument on [`new`](Self::new)
    /// because the two placements share no ops at all: no flip, no
    /// [`MODEL_FEET_OFFSET`] lift, a different yaw term, and a rotation
    /// [`new`](Self::new) has no concept of. A single function with a "is it a
    /// projectile" branch inside would read as one placement with an option, when
    /// it is two placements from two different vanilla classes.
    ///
    /// `anim` is accepted (and forwarded) for uniformity, but every projectile rig
    /// classifies as [`AnimFamily::Static`](crate::entity_anim::AnimFamily::Static)
    /// — an arrow has no `head`, no legs and no arms — so the pose is its rest pose
    /// whatever is passed.
    #[must_use]
    pub fn new_projectile(
        model: &'static str,
        mesh: &EntityMesh,
        pos: Vec3,
        yaw_deg: f32,
        pitch_deg: f32,
        scale: f32,
        anim: &AnimInput,
    ) -> Self {
        Self::placed(
            model,
            mesh,
            projectile_model_matrix(pos, yaw_deg, pitch_deg, scale),
            anim,
            // No projectile is a creeper, and `0.0` is `pose_swelling`'s exact
            // identity case — see [`new_swelling`](Self::new_swelling).
            0.0,
        )
    }

    /// Build an instance for a **non-living vehicle** — a model
    /// [`non_living_vehicle_placement`] recognises — placed by
    /// [`non_living_vehicle_matrix`] instead of [`dying_entity_model_matrix`].
    ///
    /// Separate constructor for the same reason [`new_projectile`](Self::new_projectile)
    /// is: the placements share the yaw-rotate and the flip, but not the
    /// vertical term (a small bob in world-Y rather than the 1.501
    /// [`MODEL_FEET_OFFSET`] lift) or the trailing spin some of these rigs need.
    /// No vehicle is dying or swelling, so both extras `new_animated` carries are
    /// their documented identities here.
    #[must_use]
    pub fn new_non_living(
        model: &'static str,
        mesh: &EntityMesh,
        feet: Vec3,
        yaw_deg: f32,
        scale: f32,
        anim: &AnimInput,
        vertical_offset: f32,
        extra_yaw_deg: f32,
    ) -> Self {
        Self::placed(
            model,
            mesh,
            non_living_vehicle_matrix(feet, yaw_deg, scale, vertical_offset, extra_yaw_deg),
            anim,
            0.0,
        )
    }

    /// The half of instance construction that is placement-independent: pose the
    /// skeleton, hang the hands off it, and derive the world AABB — all from an
    /// already-built model→world `transform`.
    ///
    /// Shared by [`new`](Self::new) and [`new_projectile`](Self::new_projectile)
    /// rather than copied, so an arrow's culling box, part matrices and light
    /// default can never drift from a mob's. The *only* thing the two callers
    /// disagree about is the matrix.
    ///
    /// `swell` is a creeper's swell fraction and `0.0` for everything else, which
    /// [`Skeleton::pose_swelling`](crate::entity_anim::Skeleton::pose_swelling)
    /// documents as its exact identity case. The AABB is deliberately **not**
    /// recomputed from the swollen pose: `mesh.local_min`/`local_max` already
    /// contain a fully swollen creeper (see
    /// [`EntityMesh::from_named_model`](EntityMesh::from_named_model)), so a
    /// creeper's culling box is constant across its fuse rather than growing frame
    /// by frame — one box that always contains the drawn model, instead of a box
    /// that is exactly right and has to be rebuilt every frame.
    /// `pub(crate)` rather than private: [`EntityModelSet::resolve_at`] (a
    /// different module in this same crate) needs to build an instance from a
    /// caller-supplied transform rather than the ordinary feet/yaw placement
    /// every other constructor here derives one from.
    pub(crate) fn placed(
        model: &'static str,
        mesh: &EntityMesh,
        transform: Mat4,
        anim: &AnimInput,
        swell: f32,
    ) -> Self {
        let (aabb_min, aabb_max) = transformed_aabb(&transform, mesh.local_min, mesh.local_max);
        let part_transforms = mesh
            .skeleton
            .pose_swelling(anim, swell)
            .into_iter()
            .map(|part| transform * part)
            .collect();
        // `false`/`true` here is `Arm::Right`/`Arm::Left`'s own `is_left()` —
        // spelled out rather than iterating `[Arm::Right, Arm::Left]` because
        // `Arm` is defined below this impl and `entity_anim::Skeleton` takes
        // the mirror sign as a bare bool, not this crate's `Arm` type.
        let hand_transforms = [false, true].map(|left| {
            mesh.skeleton
                .translate_to_hand(anim, left, mesh.hand_override)
                .map(|local| transform * local)
        });
        EntityInstance {
            model,
            transform,
            part_transforms,
            hand_transforms,
            aabb_min,
            aabb_max,
            light: ENTITY_FULLBRIGHT,
        }
    }

    /// The `entity → world` `translateToHand` matrix for `arm`, honoring this
    /// model's [`HandPoseOverride`]. `None` only if the model has no such arm
    /// at all. See [`Self::hand_transforms`]'s doc for why this is not the
    /// same value as `part_transforms[skeleton.index_of(arm.part_name())]` for
    /// five corpus models.
    #[must_use]
    pub fn hand_transform(&self, arm: Arm) -> Option<Mat4> {
        self.hand_transforms[if arm.is_left() { 1 } else { 0 }]
    }

    /// Set this instance's packed sky/block light (`sky << 4 | block`).
    ///
    /// Builder-style rather than a seventh argument to [`new`](Self::new)
    /// because the great majority of call sites (mesh tests, the offline demo)
    /// have no world to sample and want the [`ENTITY_FULLBRIGHT`] default; only
    /// a caller wired to a real light source has anything to pass.
    #[must_use]
    pub fn with_light(mut self, light: u8) -> Self {
        self.light = light;
        self
    }
}

/// The version-free description of one tracked entity to render this frame: the
/// minimal fields the render layer needs, deliberately decoupled from any
/// client or wire type.
///
/// This is the seam a live scene loop adapts its entities into — e.g. mapping
/// each `EntityView` from the client handle into one of these. Keeping it a
/// small borrow of a type-path string plus world placement means the render
/// crate never depends on the client or a protocol version: the caller owns the
/// mapping from *its* entity representation, and this crate owns everything from
/// a type path to pixels. That is why [`EntityModelSet::plan`] takes these
/// rather than an `EntityView` directly — the dependency would point the wrong
/// way (render → client), and this crate must stay usable headless.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntitySpawn<'a> {
    /// The entity type's resource path, e.g. `"pig"` (namespace stripped).
    pub type_path: &'a str,
    /// Feet position in world space — the entity's on-ground origin.
    pub feet: Vec3,
    /// Whole-body yaw in degrees. Head tracking and limb poses are a layer above
    /// this and are not yet applied, so this is the body facing.
    pub body_yaw_deg: f32,
    /// Uniform model scale: `1.0` for an adult; babies and slimes pass a smaller
    /// value. The caller owns the baby/variant → scale decision.
    pub scale: f32,
    /// Per-part animation drive: head tracking, walk phase, attack swing, age.
    /// Pass [`AnimInput::REST`] for a static pose.
    pub anim: AnimInput,
    /// Packed sky/block light (`sky << 4 | block`) at this entity's block
    /// position — the caller's one job on the lighting side, because only the
    /// caller has a world to sample. Pass [`ENTITY_FULLBRIGHT`] when there is
    /// no world (the offline demo); passing it *because it is convenient*
    /// against a live server is the "mobs are super bright" defect.
    pub light: u8,
}

/// corners and takes their component-wise min/max. Correct for the entity flip
/// and yaw rotation (an axis-aligned box stays conservative under rotation).
fn transformed_aabb(m: &Mat4, local_min: Vec3, local_max: Vec3) -> (Vec3, Vec3) {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for i in 0..8 {
        let corner = Vec3::new(
            if i & 1 == 0 { local_min.x } else { local_max.x },
            if i & 2 == 0 { local_min.y } else { local_max.y },
            if i & 4 == 0 { local_min.z } else { local_max.z },
        );
        let world = m.transform_point3(corner);
        min = min.min(world);
        max = max.max(world);
    }
    (min, max)
}

/// A version-free baked corpus of every entity model the renderer can draw,
/// baked once on the CPU (no GPU) so the local bounds needed for culling and the
/// meshes needed for upload live in one place.
///
/// The GPU side ([`crate::entity_pipeline`]) uploads one buffer per entry here;
/// this pure set is what makes type→instance resolution testable headlessly.
#[derive(Debug, Clone)]
pub struct EntityModelSet {
    models: Vec<(&'static str, EntityMesh)>,
    /// `name -> index into models`, built once in [`Self::load`] from the same
    /// vector so it cannot drift from it. Turns [`Self::get`] — called for
    /// every drawn entity in every one of the base/armour/flame/wool passes,
    /// every frame — from an O(90) linear scan into an O(1) lookup (issue
    /// #523).
    index: std::collections::HashMap<&'static str, usize>,
}

impl Default for EntityModelSet {
    fn default() -> Self {
        Self::load()
    }
}

impl EntityModelSet {
    /// Bake every entry in the [`entity_models`] corpus into a renderable mesh.
    #[must_use]
    pub fn load() -> Self {
        let models: Vec<(&'static str, EntityMesh)> = entity_models()
            .into_iter()
            .map(|entry| {
                (
                    entry.name,
                    EntityMesh::from_named_model(entry.name, &(entry.build)()),
                )
            })
            .collect();
        let index = models
            .iter()
            .enumerate()
            .map(|(i, (name, _))| (*name, i))
            .collect();
        Self { models, index }
    }

    /// The baked mesh for a model name, if present.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&EntityMesh> {
        self.index.get(name).map(|&i| &self.models[i].1)
    }

    /// Every `(name, mesh)` pair, in corpus order (for uploading each once).
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &EntityMesh)> {
        self.models.iter().map(|(n, m)| (*n, m))
    }

    /// Number of baked models.
    #[must_use]
    pub fn len(&self) -> usize {
        self.models.len()
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// Resolve a tracked entity (its type path, feet position, body yaw and
    /// scale) into a renderable [`EntityInstance`], or `None` if its type has no
    /// model yet. This is the type→geometry seam: it consumes only version-free
    /// data (a type path string and world coordinates), never a wire type.
    ///
    /// **A projectile resolved through here is drawn level.** This is
    /// [`resolve_posed`](Self::resolve_posed) with `pitch_deg = 0`, which is the
    /// right answer for every mob (a mob's pitch is head tracking, and that
    /// arrives through `anim`, not through the placement) and a flat one for an
    /// arrow. Callers that have a pitch — the live frame path — should use
    /// `resolve_posed`; the mesh-only tests and the offline demo that do not have
    /// nothing to pass and keep working unchanged.
    #[must_use]
    pub fn resolve(
        &self,
        type_path: &str,
        feet: Vec3,
        yaw_deg: f32,
        scale: f32,
        anim: &AnimInput,
    ) -> Option<EntityInstance> {
        self.resolve_posed(type_path, feet, yaw_deg, 0.0, scale, anim)
    }

    /// [`resolve`](Self::resolve) with the entity's **pitch**, which is what a
    /// projectile needs and a mob ignores.
    ///
    /// The pitch selects the placement, not just a rotation: a model
    /// [`projectile_pitch_offset_deg`] recognises is placed by
    /// [`projectile_model_matrix`] (no Y flip, no [`MODEL_FEET_OFFSET`] lift,
    /// `Ry(yaw − 90°) · Rz(pitch + offset)`), and everything else by
    /// [`entity_model_matrix`]. Sending an arrow down the mob path draws it 1.501
    /// blocks **high** and mirrored — see [`projectile_model_matrix`] for the
    /// direction of that offset, which is not the one it looks like.
    ///
    /// `yaw_deg`/`pitch_deg` are the entity's own reported rotation. For a
    /// projectile those are vanilla's velocity-derived `yRot`/`xRot`
    /// (recomputed from `atan2` on the projectile's own velocity every tick,
    /// and the server broadcasts the result), *not* a body yaw and a head
    /// pitch — the two use different conventions and
    /// [`projectile_model_matrix`] documents the one it expects.
    #[must_use]
    pub fn resolve_posed(
        &self,
        type_path: &str,
        feet: Vec3,
        yaw_deg: f32,
        pitch_deg: f32,
        scale: f32,
        anim: &AnimInput,
    ) -> Option<EntityInstance> {
        self.resolve_animated(type_path, feet, yaw_deg, pitch_deg, scale, anim, 0.0, 0.0)
    }

    /// [`resolve_posed`](Self::resolve_posed) with a creeper's **swell** fraction
    /// and a dying entity's **`death_time`** — see
    /// [`EntityInstance::new_animated`] for what each one reaches.
    ///
    /// This is the seam the live frame path wants, and the reason it exists is worth
    /// keeping, because both animations had the *same* defect. Every piece of the
    /// creeper swell — [`creeper_swell_scale`], the `swell` parameter on
    /// [`Skeleton::pose_swelling`](crate::entity_anim::Skeleton::pose_swelling),
    /// the `MAX_SWELL_SCALE` bounds pad, the white-flash blink
    /// ([`creeper_white_overlay_progress`]), its alpha byte
    /// ([`crate::entity_pipeline::creeper_overlay_alpha_from_progress`]), the
    /// instance lane that carries it and the shader that reads it — was built,
    /// individually tested and **reached zero pixels**, because the one hop from a
    /// decoded fuse to this call did not exist: the shell resolved every entity
    /// through `resolve_posed`, whose extras are a hard `0.0`. Both extras being an
    /// exact identity at `0.0` is what made that invisible, and is why a *live*
    /// call site passing them is the thing to check rather than either formula.
    ///
    /// [`creeper_swell_scale`]: crate::entity_anim::creeper_swell_scale
    /// [`creeper_white_overlay_progress`]: crate::entity_anim::creeper_white_overlay_progress
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "one argument per independent piece of a live entity's placement, pose \
                  and animation state; bundling them into a struct would move the same \
                  fields behind a name that adds nothing"
    )]
    pub fn resolve_animated(
        &self,
        type_path: &str,
        feet: Vec3,
        yaw_deg: f32,
        pitch_deg: f32,
        scale: f32,
        anim: &AnimInput,
        swell: f32,
        death_time: f32,
    ) -> Option<EntityInstance> {
        let name = canonical_model_name(type_path)?;
        let mesh = self.get(name)?;
        Some(if let Some(offset) = projectile_pitch_offset_deg(name) {
            EntityInstance::new_projectile(
                name,
                mesh,
                feet,
                yaw_deg,
                pitch_deg + offset,
                scale,
                anim,
            )
        } else if let Some((vertical_offset, extra_yaw_deg)) =
            non_living_vehicle_placement(name)
        {
            EntityInstance::new_non_living(
                name,
                mesh,
                feet,
                yaw_deg,
                scale,
                anim,
                vertical_offset,
                extra_yaw_deg,
            )
        } else {
            EntityInstance::new_animated(name, mesh, feet, yaw_deg, scale, anim, swell, death_time)
        })
    }

    /// Resolve a tracked entity's model at a caller-supplied `transform`,
    /// bypassing [`entity_model_matrix`]/[`dying_entity_model_matrix`]
    /// entirely — the seam a **nested** placement needs.
    ///
    /// Every other `resolve*` here derives its placement from `(feet, yaw,
    /// scale)` under vanilla's ordinary entity convention. That is the wrong
    /// shape for a mob drawn *inside* another transform chain — the mob
    /// spawner's miniature display entity, whose renderer builds vanilla's
    /// own pose stack (translate, spin, tilt, shrink) and then hands the
    /// entity's *own* renderer that already-transformed stack, rather than a
    /// `(feet, yaw)` pair. [`crate::spawner::spawner_display_outer_matrix`]
    /// builds that outer chain; the caller composes it with
    /// `entity_model_matrix(Vec3::ZERO, entity_yaw_deg, 1.0)` for the
    /// entity's own flip/lift, exactly the nesting vanilla's two render calls
    /// produce, and passes the product here.
    ///
    /// `None` for a `type_path` with no baked model, the same miss every
    /// other `resolve*` here has.
    #[must_use]
    pub fn resolve_at(
        &self,
        type_path: &str,
        transform: Mat4,
        anim: &AnimInput,
    ) -> Option<EntityInstance> {
        let name = canonical_model_name(type_path)?;
        let mesh = self.get(name)?;
        Some(EntityInstance::placed(name, mesh, transform, anim, 0.0))
    }

    /// Resolve, frustum-cull and group a set of tracked entities into an
    /// [`EntityFrame`] in one call — the one-shot entry point for a live scene
    /// loop, so a caller with a list of entities never has to hand-assemble the
    /// intermediate [`EntityInstance`] vector or call [`plan_entities`] itself.
    ///
    /// Each [`EntitySpawn`] whose `type_path` has a baked model becomes an
    /// instance; a type with no model yet (e.g. `ender_dragon`) is silently
    /// skipped, exactly as [`resolve`](Self::resolve) skips it, so an
    /// unsupported mob never aborts the frame. Survivors are culled and grouped
    /// by [`plan_entities`], producing one [`EntityBatch`] per visible model
    /// type. Note [`EntityCullStats::total`] counts entities that *have a model*
    /// — modelless types are dropped before culling, not counted as culled.
    ///
    /// # Instance-buffer contract (the GPU side of the seam)
    ///
    /// Each [`EntityBatch::transforms`] entry is a model→world [`Mat4`]. Upload a
    /// batch's transforms column-major with
    /// [`upload_instances`](crate::entity_pipeline::upload_instances); the entity
    /// shader reads each as the `mat4x4` spanning vertex locations 4–7 (four
    /// `Float32x4` columns, `step_mode: Instance`). Draw the uploaded mesh for
    /// [`EntityBatch::model`] instanced over that buffer, one instanced draw per
    /// batch. That is the whole contract — the caller supplies entities and a
    /// frustum; this crate owns type→mesh→matrix→pixels.
    #[must_use]
    pub fn plan<'a, I>(&self, spawns: I, frustum: &Frustum) -> EntityFrame
    where
        I: IntoIterator<Item = EntitySpawn<'a>>,
    {
        let instances: Vec<EntityInstance> = spawns
            .into_iter()
            .filter_map(|s| {
                self.resolve(s.type_path, s.feet, s.body_yaw_deg, s.scale, &s.anim)
                    .map(|i| i.with_light(s.light))
            })
            .collect();
        plan_entities(&instances, frustum)
    }
}

/// Per-frame entity culling accounting. Mirrors [`crate::scene::CullStats`]'s
/// anti-vacuity discipline: a frame that drew nothing, or culled nothing while a
/// populated set straddles the frustum, is a bug rather than a fast frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntityCullStats {
    /// Entities considered this frame.
    pub total: usize,
    /// Entities whose model is known *and* survived frustum culling.
    pub drawn: usize,
    /// Entities frustum-culled.
    pub culled_frustum: usize,
}

impl EntityCullStats {
    /// A frame is *meaningful* only if it both drew and culled something, so a
    /// benchmark or gate cannot pass by culling (or drawing) everything.
    #[must_use]
    pub fn is_meaningful(&self) -> bool {
        self.drawn > 0 && self.culled_frustum > 0
    }
}

/// One model type's visible instances for a frame: the model name and the
/// per-instance transforms to draw instanced.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityBatch {
    /// The [`entity_models`] entry name.
    pub model: &'static str,
    /// One whole-entity model → world matrix per visible instance.
    pub transforms: Vec<Mat4>,
    /// Per-part instance matrices: `parts[p][i]` places part `p` of instance
    /// `i`. Outer length equals the mesh's part count; every inner vector has
    /// one entry per visible instance, in the same order as `transforms`.
    pub parts: Vec<Vec<Mat4>>,
    /// One packed sky/block light byte per visible instance, in the same order
    /// as `transforms` — widened to `u32` because that is what the instance
    /// vertex attribute carries. The *same* slice is uploaded alongside every
    /// part's matrices: a mob's light is per entity, so each of its parts reads
    /// the identical value.
    pub lights: Vec<u32>,
}

/// The visible entity draws for one frame, grouped by model type, plus the
/// accounting that produced them.
#[derive(Debug, Clone, Default)]
pub struct EntityFrame {
    /// Visible instances grouped by model type (one [`EntityBatch`] per model
    /// that has at least one visible instance).
    pub batches: Vec<EntityBatch>,
    /// Culling accounting for this frame.
    pub stats: EntityCullStats,
}

impl EntityFrame {
    /// Total visible instances across all batches.
    #[must_use]
    pub fn instance_count(&self) -> usize {
        self.batches.iter().map(|b| b.transforms.len()).sum()
    }
}

/// Cull a set of entity instances against `frustum` and group the survivors by
/// model type for instanced drawing.
///
/// Instances whose world AABB is outside the frustum are dropped; the rest are
/// bucketed by [`EntityInstance::model`] preserving first-seen model order, so
/// the caller issues one instanced draw per model type. Cost is `O(instances)`
/// frustum tests plus the grouping.
#[must_use]
pub fn plan_entities(instances: &[EntityInstance], frustum: &Frustum) -> EntityFrame {
    let mut batches: Vec<EntityBatch> = Vec::new();
    let mut stats = EntityCullStats {
        total: instances.len(),
        ..EntityCullStats::default()
    };

    for inst in instances {
        if !frustum.intersects_aabb(inst.aabb_min, inst.aabb_max) {
            stats.culled_frustum += 1;
            continue;
        }
        stats.drawn += 1;
        match batches.iter_mut().find(|b| b.model == inst.model) {
            Some(batch) => {
                batch.transforms.push(inst.transform);
                batch.lights.push(u32::from(inst.light));
                for (slot, m) in batch.parts.iter_mut().zip(&inst.part_transforms) {
                    slot.push(*m);
                }
            }
            None => batches.push(EntityBatch {
                model: inst.model,
                transforms: vec![inst.transform],
                parts: inst.part_transforms.iter().map(|m| vec![*m]).collect(),
                lights: vec![u32::from(inst.light)],
            }),
        }
    }

    EntityFrame { batches, stats }
}

// ---------------------------------------------------------------------------
// Humanoid armour
// ---------------------------------------------------------------------------
//
// Armour is the one drawable in this module that is **not** an entity. It is a
// layer over somebody else's rig, and the whole design follows from one
// consequence of that:
//
// # Every armour piece is posed by the *wearer's* part matrix, never its own
//
// Vanilla does this too, and does it by a route we cannot copy: the armour
// model is an instance of the wearer's own model *class*
// (`AbstractZombieRenderer` builds an `ArmorModelSet<M extends ZombieModel>`),
// and `submitModel` calls `setupAnim` on it with the wearer's render state. A
// zombie's chestplate therefore reaches out in front with `animateZombieArms`,
// because the chestplate ran the same animator.
//
// Here there is one animator per *mesh*, so the faithful equivalent is to skip
// the second pose entirely and read the wearer's already-composed
// `EntityInstance::part_transforms[i]` for the part of the same name. That is
// exact, because [`ArmourMesh`]'s geometry is part-local and its pivots come
// from the very same `humanoid_root` builder the wearer's rig does
// (`lodestone_assets::equipment` shares it deliberately).
//
// **Reading, never mutating.** `EntityInstance::hand_transforms` exists because
// folding a held item's pivot shift into `part_transforms` would have dragged
// the mob's visible arm along with the item. The same discipline applies with
// less effort here: an armour layer needs *exactly* the wearer's matrix with
// nothing added, so there is nothing to fold in and nothing to copy — see
// [`ArmourMesh::attach`], which hands back `(range, wearer part index)` pairs
// and leaves the caller indexing the wearer's own slice.
//
// # Two measured deviations from vanilla, both sub-texel
//
// Reusing the wearer's pivot rather than the armour model's own means a rig
// whose pivots differ from the plain humanoid model's gets its armour at
// *its* pivot, not at vanilla's:
//
// * `skeleton`/`stray`/`wither_skeleton` put their legs at `x = ±2.0` where
//   the humanoid model has `±1.9`, so skeleton leg armour sits 0.1 texel
//   (0.00625 blocks) further out than vanilla draws it.
// * `player_slim`'s arms pivot 0.5 texel lower than the wide rig's, and
//   vanilla bakes only *one* player armour set (its armour-mesh-set builder
//   takes no slim flag and adds only empty sleeve/pants/jacket nodes), so a
//   slim player's sleeves get armour 0.5 texel (0.03 blocks) low.
//
// Both are deliberate: following the visible limb is worth more than matching
// vanilla's pivot to a thirtieth of a block, and the alternative — posing a
// second skeleton — would reintroduce exactly the zombie-arm divergence vanilla
// avoids by construction.

/// One armour slot's baked mesh, in the shared part-local [`ModelVertex`]
/// format, with its parts keyed by the **wearer's** part names.
///
/// One of these per [`ArmourSlot`], not per material: the geometry depends only
/// on the slot's inflation, and every material paints the same four meshes with
/// a different sheet.
#[derive(Debug, Clone)]
pub struct ArmourMesh {
    /// Four vertices per quad, part-local (the part's own pose is *not* folded
    /// in — the wearer's matrix supplies it).
    pub vertices: Vec<ModelVertex>,
    /// Six indices per quad, wound so front faces point outward.
    pub indices: Vec<u32>,
    /// `(wearer part name, index range)` for every part that actually carries
    /// geometry, in bake order. Parts pruned by the slot's retention rule are
    /// absent rather than present-and-empty, so a caller cannot accidentally
    /// issue a zero-index draw.
    pub parts: Vec<(&'static str, PartRange)>,
}

impl ArmourMesh {
    /// Bake the mesh for one slot.
    #[must_use]
    pub fn for_slot(slot: ArmourSlot) -> Self {
        let def = humanoid_armour_model(slot);
        let baked = bake_entity_parts(&def);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut parts = Vec::new();
        for part in &baked {
            if part.quads.is_empty() {
                continue;
            }
            // Resolve the baked name back to the `&'static str` the slot
            // declares, so the pairing in `attach` is a pointer-cheap compare
            // and a name this mesh carries but the slot does not is a bake bug
            // that shows up here rather than as a missing draw.
            let Some(name) = slot
                .part_names()
                .iter()
                .find(|n| **n == part.name.as_str())
                .copied()
            else {
                continue;
            };
            let index_start = indices.len() as u32;
            let vertex_start = vertices.len() as u32;
            push_part_quads(&part.quads, &mut vertices, &mut indices);
            parts.push((
                name,
                PartRange {
                    index_start,
                    index_count: indices.len() as u32 - index_start,
                    vertex_start,
                    vertex_count: vertices.len() as u32 - vertex_start,
                },
            ));
        }
        ArmourMesh {
            vertices,
            indices,
            parts,
        }
    }

    /// Number of quads in the mesh.
    #[must_use]
    pub fn quad_count(&self) -> usize {
        self.indices.len() / 6
    }

    /// Pair each of this mesh's parts with the index of the wearer's part of the
    /// same name, dropping any part the wearer's rig does not have.
    ///
    /// The caller then reads `instance.part_transforms[wearer_index]` — the
    /// wearer's own, already-animated, already-world-space matrix — and draws
    /// `range` instanced over it. Nothing is written back: see this section's
    /// header for why an armour layer must not touch `part_transforms`.
    ///
    /// A non-humanoid rig yields nothing — see [`wearer_carries_armour`], which
    /// this enforces so a caller cannot forget it.
    pub fn attach<'a>(
        &'a self,
        wearer: &'a Skeleton,
    ) -> impl Iterator<Item = (PartRange, usize)> + 'a {
        let humanoid = wearer_carries_armour(wearer);
        self.parts
            .iter()
            .filter(move |_| humanoid)
            .filter_map(|(name, range)| wearer.index_of(name).map(|i| (*range, i)))
    }
}

/// Whether a rig wears humanoid armour at all.
///
/// Vanilla's real gate is which *renderer* owns a `HumanoidArmorLayer`
/// (`HumanoidMobRenderer`, `AvatarRenderer`, `ArmorStandRenderer`, the piglin
/// and zombie families), and the structural equivalent here is the animation
/// family: [`AnimFamily::Humanoid`] is exactly "has both arms and both legs",
/// which is what `HumanoidModel` means.
///
/// **Part names alone are not sufficient and that is the trap.** A pig has both
/// `head` and `body`, so a chestplate keyed on part names would attach its
/// `body` cube to a pig's torso and draw a floating breastplate on a farm
/// animal — geometry that resolves perfectly and is completely wrong. Vanilla
/// draws nothing there.
#[must_use]
pub fn wearer_carries_armour(wearer: &Skeleton) -> bool {
    wearer.family() == crate::entity_anim::AnimFamily::Humanoid
}

/// The four baked humanoid armour meshes, one per [`ArmourSlot`].
///
/// Built once (CPU only, like [`EntityModelSet`]) and uploaded once; a mob's
/// armour costs one instance matrix per drawn part, exactly as its own body
/// does.
#[derive(Debug, Clone)]
pub struct ArmourModelSet {
    meshes: Vec<(ArmourSlot, ArmourMesh)>,
}

impl Default for ArmourModelSet {
    fn default() -> Self {
        Self::load()
    }
}

impl ArmourModelSet {
    /// Bake all four slot meshes, in [`ArmourSlot::ALL`] order — which is
    /// vanilla's own armour-layer submit order, so a caller that walks
    /// [`iter`](Self::iter) draws in vanilla's sequence.
    #[must_use]
    pub fn load() -> Self {
        Self {
            meshes: ArmourSlot::ALL
                .into_iter()
                .map(|slot| (slot, ArmourMesh::for_slot(slot)))
                .collect(),
        }
    }

    /// The baked mesh for a slot.
    #[must_use]
    pub fn get(&self, slot: ArmourSlot) -> Option<&ArmourMesh> {
        self.meshes
            .iter()
            .find(|(s, _)| *s == slot)
            .map(|(_, m)| m)
    }

    /// Every `(slot, mesh)` pair, in submit order (for uploading each once).
    pub fn iter(&self) -> impl Iterator<Item = (ArmourSlot, &ArmourMesh)> {
        self.meshes.iter().map(|(s, m)| (*s, m))
    }
}

// ---------------------------------------------------------------------------
// Sheep wool (issue #53)
// ---------------------------------------------------------------------------
//
// The wool layer follows exactly the humanoid-armour discipline above — a
// second, independently-baked mesh posed off the *wearer's* already-animated
// `part_transforms`, never a second skeleton — with one structural
// simplification and one structural trap that armour does not have:
//
// * **One mesh, not one per slot.** Armour needs [`ArmourModelSet`] because
//   the four slots bake different geometry; wool is a single overlay over the
//   whole sheep body, so [`WoolMesh`] has no per-slot table.
// * **The gate cannot live inside the mesh geometry the way `ArmourMesh`'s
//   does.** [`wearer_carries_armour`] reads the wearer's *animation family*,
//   which is a structural property `sheep`, `pig`, `cow` and `wolf` all share
//   — a farm animal has no `head`/`body` parts that would make a chestplate
//   attach fail. Wool cannot reuse that gate: it must be keyed on the
//   wearer's **resolved model name being exactly `"sheep"`**
//   (`docs/entity-rendering.md`'s "pig/cow trap, worse"), so [`WoolMesh::attach`]
//   takes the resolved model name as a second argument rather than reading it
//   off the [`Skeleton`] the way armour's `wearer.family()` check does.

/// [`sheep_wool_model`](lodestone_assets::entity_models::sheep_wool_model)'s
/// six named parts, in the order [`WoolMesh::load`] bakes them — the same
/// pre-order `sheep_model`'s body shares (pinned by
/// `sheep_wool_model_shares_sheep_body_part_names_and_pivots` in
/// `lodestone-assets/tests/entity_models.rs`).
const SHEEP_WOOL_PART_NAMES: [&str; 6] = [
    "head",
    "body",
    "right_hind_leg",
    "left_hind_leg",
    "right_front_leg",
    "left_front_leg",
];

/// The sheep wool overlay's baked mesh, in the shared part-local
/// [`ModelVertex`] format, with its parts keyed by the **sheep body's** part
/// names — the same shape as [`ArmourMesh`], minus the per-slot table, since
/// wool has only one variant.
#[derive(Debug, Clone)]
pub struct WoolMesh {
    /// Four vertices per quad, part-local (the part's own pose is *not*
    /// folded in — the wearer's matrix supplies it).
    pub vertices: Vec<ModelVertex>,
    /// Six indices per quad, wound so front faces point outward.
    pub indices: Vec<u32>,
    /// `(sheep body part name, index range)` for every part that carries
    /// geometry, in bake order.
    pub parts: Vec<(&'static str, PartRange)>,
}

impl WoolMesh {
    /// Bake the wool overlay mesh.
    #[must_use]
    pub fn load() -> Self {
        let def = lodestone_assets::entity_models::sheep_wool_model();
        let baked = bake_entity_parts(&def);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut parts = Vec::new();
        for part in &baked {
            if part.quads.is_empty() {
                continue;
            }
            // Same discipline as `ArmourMesh::for_slot`: resolve the baked
            // name back to the `&'static str` this module owns, so a name
            // this mesh carries but the constant list does not is a bake bug
            // caught here rather than as a missing draw.
            let Some(name) = SHEEP_WOOL_PART_NAMES
                .iter()
                .find(|n| **n == part.name.as_str())
                .copied()
            else {
                continue;
            };
            let index_start = indices.len() as u32;
            let vertex_start = vertices.len() as u32;
            push_part_quads(&part.quads, &mut vertices, &mut indices);
            parts.push((
                name,
                PartRange {
                    index_start,
                    index_count: indices.len() as u32 - index_start,
                    vertex_start,
                    vertex_count: vertices.len() as u32 - vertex_start,
                },
            ));
        }
        WoolMesh {
            vertices,
            indices,
            parts,
        }
    }

    /// Number of quads in the mesh.
    #[must_use]
    pub fn quad_count(&self) -> usize {
        self.indices.len() / 6
    }

    /// Pair each of this mesh's parts with the index of the wearer's part of
    /// the same name, dropping every part when `wearer_model` is not
    /// `"sheep"`.
    ///
    /// The caller then reads `instance.part_transforms[wearer_index]` and
    /// draws `range` instanced over it, exactly [`ArmourMesh::attach`]'s
    /// contract. `wearer_model` is the resolved
    /// [`EntityModelSet::resolve`] model name (`instance.model` /
    /// [`EntityBatch::model`]) — **never** [`Skeleton::family`], because
    /// `AnimFamily::Quadruped` is shared by `pig`, `cow` and `wolf`: gating on
    /// family alone would grow wool on a pig exactly as an ungated armour
    /// attach once drew a breastplate on one. See this section's header.
    pub fn attach<'a>(
        &'a self,
        wearer: &'a Skeleton,
        wearer_model: &str,
    ) -> impl Iterator<Item = (PartRange, usize)> + 'a {
        let is_sheep = wearer_model == "sheep";
        self.parts
            .iter()
            .filter(move |_| is_sheep)
            .filter_map(|(name, range)| wearer.index_of(name).map(|i| (*range, i)))
    }
}

/// The sheep wool overlay's CPU model, loaded once. There is only one
/// [`WoolMesh`] (wool has no per-material variant the way armour does), so
/// unlike [`ArmourModelSet`] this holds a single mesh rather than a table —
/// the wrapper exists for symmetry with the armour loading path and so a
/// future second wool variant (e.g. a baby rig) has somewhere to live.
#[derive(Debug, Clone)]
pub struct SheepWoolModelSet {
    mesh: WoolMesh,
}

impl Default for SheepWoolModelSet {
    fn default() -> Self {
        Self::load()
    }
}

impl SheepWoolModelSet {
    /// Bake the wool mesh.
    #[must_use]
    pub fn load() -> Self {
        Self {
            mesh: WoolMesh::load(),
        }
    }

    /// The baked wool mesh.
    #[must_use]
    pub fn mesh(&self) -> &WoolMesh {
        &self.mesh
    }
}

// ---------------------------------------------------------------------------
// The player's cape (`PlayerCapeModel`/`CapeLayer`)
// ---------------------------------------------------------------------------
//
// Structurally the same "second, independently-baked mesh posed off the
// wearer's already-animated `part_transforms`" discipline as armour and wool
// above, with one difference from both: the cape needs an **extra** local
// transform on top of the wearer's body matrix (the per-frame lean/flap
// rotation), where armour and wool reuse the wearer's part matrix verbatim.
// `attach` therefore hands back the same `(PartRange, wearer_index)` pairing
// as the other two — the caller is what composes the extra matrix in, once
// per instance, via [`cape_local_rotation`].

/// The player cape's baked mesh: one part, `"cape"`, in the wearer's
/// **body-pivot-local** space (see [`lodestone_assets::entity::player_cape_model`]
/// for why no rotation is baked in).
#[derive(Debug, Clone)]
pub struct CapeMesh {
    /// Four vertices per quad, part-local.
    pub vertices: Vec<ModelVertex>,
    /// Six indices per quad.
    pub indices: Vec<u32>,
    /// Always exactly one entry, `("cape", range)` — kept as a list rather
    /// than a bare range for the same reason [`ArmourMesh::parts`] is: a bake
    /// that produced no quads (a malformed model) yields an empty list rather
    /// than a range into nothing.
    pub parts: Vec<(&'static str, PartRange)>,
}

impl CapeMesh {
    /// Bake the cape mesh.
    #[must_use]
    pub fn load() -> Self {
        let def = lodestone_assets::entity::player_cape_model();
        let baked = bake_entity_parts(&def);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut parts = Vec::new();
        for part in &baked {
            if part.name.as_str() != "cape" || part.quads.is_empty() {
                continue;
            }
            let index_start = indices.len() as u32;
            let vertex_start = vertices.len() as u32;
            push_part_quads(&part.quads, &mut vertices, &mut indices);
            parts.push((
                "cape",
                PartRange {
                    index_start,
                    index_count: indices.len() as u32 - index_start,
                    vertex_start,
                    vertex_count: vertices.len() as u32 - vertex_start,
                },
            ));
        }
        CapeMesh {
            vertices,
            indices,
            parts,
        }
    }

    /// Pair the cape part with the wearer's `"body"` part, dropping it
    /// entirely for a non-humanoid rig — same gate [`wearer_carries_armour`]
    /// uses, and for the same reason (a farm animal has no `body` pivot a
    /// cape should hang from).
    pub fn attach<'a>(
        &'a self,
        wearer: &'a Skeleton,
    ) -> impl Iterator<Item = (PartRange, usize)> + 'a {
        let humanoid = wearer_carries_armour(wearer);
        self.parts
            .iter()
            .filter(move |_| humanoid)
            .filter_map(|(name, range)| wearer.index_of(name).map(|i| (*range, i)))
    }
}

/// The per-frame cape placement, relative to the wearer's **body** part
/// transform: translate to the pivot vanilla's cape-layer builder gives it
/// (`(0, 0, 2)` model texels), then rotate.
///
/// `lean`/`lean2`/`flap` are vanilla's per-frame cape-lean, cape-lean2 and
/// cape-flap values, in **degrees** — see
/// `lodestone_shell::entities::cape_sway` for how those three are derived
/// from the lagged "cloak" position each frame.
///
/// # The rotation, derived from a rotate-onto-existing-pose composition
///
/// Vanilla's per-frame animation step does not set a rotation on the cape, it
/// **composes** one onto the cape's existing pose:
///
/// ```text
/// old_rotation = rotationZYX(zRot, yRot, xRot)
/// new_rotation = old_rotation.rotate(rotation)
/// ```
///
/// i.e. `new = old * rotation` (the underlying rotation composition always
/// post-multiplies). The cape's `old` rotation is the static pose the model
/// builder gives it, `Ry(pi)` (it hangs facing backward), and the `rotation`
/// argument is itself built by chained rotate-Y/X/Z calls — each one *also* a
/// post-multiply — so:
///
/// ```text
/// rotation = Ry(-pi) * Rx(theta_x) * Rz(theta_z) * Ry(theta_y2)
/// new      = Ry(pi) * rotation
///          = [Ry(pi) * Ry(-pi)] * Rx(theta_x) * Rz(theta_z) * Ry(theta_y2)
///          = Rx(theta_x) * Rz(theta_z) * Ry(theta_y2)
/// ```
///
/// The static `Ry(pi)` and the quaternion's leading `Ry(-pi)` are exact
/// inverses on the same axis and cancel — which is exactly why
/// [`lodestone_assets::entity::player_cape_model`] bakes no rotation at all:
/// baking `Ry(pi)` here would double it instead of cancelling it.
///
/// `theta_x = 6 + lean/2 + flap`, `theta_z = lean2/2`,
/// `theta_y2 = 180 - lean2/2`, all degrees, straight out of vanilla's
/// per-frame cape animation step.
#[must_use]
pub fn cape_local_rotation(lean: f32, lean2: f32, flap: f32) -> Mat4 {
    let theta_x = (6.0 + lean / 2.0 + flap).to_radians();
    let theta_z = (lean2 / 2.0).to_radians();
    let theta_y2 = (180.0 - lean2 / 2.0).to_radians();
    let translate = Mat4::from_translation(Vec3::new(0.0, 0.0, 2.0 / 16.0));
    let rotate = Mat4::from_rotation_x(theta_x) * Mat4::from_rotation_z(theta_z) * Mat4::from_rotation_y(theta_y2);
    translate * rotate
}

// ---------------------------------------------------------------------------
// The elytra
// ---------------------------------------------------------------------------
//
// The same "second mesh posed off the wearer's already-animated
// `part_transforms`" discipline as armour, wool and the cape above. It sits
// closest to the cape: both hang off the wearer's `"body"` part and both need
// an **extra** local transform the caller composes in per frame.
//
// It differs from the cape in three ways that each cost something to get
// wrong, so they are named here rather than left to be rediscovered:
//
//  * **Two parts, not one**, and their transforms are not equal — the right
//    wing negates the left's Y and Z rotation. A single shared matrix draws
//    both wings folded the same way, which reads as "one wing is inside out".
//  * **The draw gate is the chest equipment slot**, not a texture URL. An
//    elytra is worn where a chestplate goes, and vanilla's real gate is that
//    the piece's `equipment/<asset>.json` declares a `wings` layer at all —
//    which is why a diamond chestplate, whose asset declares `humanoid` and
//    `humanoid_leggings` and no `wings`, draws nothing here.
//  * **The elytra layer's submit step translates the whole layer `+0.125` on
//    Z** before anything else, to clear the wearer's own body. That is 0.125
//    *blocks* (the pose stack is in blocks at layer level; the underlying
//    part-render step is what divides texels by 16), i.e. 2 texels —
//    numerically the same as the cape's `z = 2` pivot, and a different
//    quantity with a different origin.

/// The elytra's baked mesh: two parts, `"left_wing"` and `"right_wing"`, in
/// the wearer's **body-pivot-local** space.
///
/// See [`lodestone_assets::entity::elytra_model`] for why neither the static
/// pose rotation nor the crouch `y` is baked in, and
/// [`elytra_wing_transform`] for what the caller must compose per wing.
#[derive(Debug, Clone)]
pub struct ElytraMesh {
    /// Four vertices per quad, part-local.
    pub vertices: Vec<ModelVertex>,
    /// Six indices per quad.
    pub indices: Vec<u32>,
    /// One entry per wing, in `("left_wing", _), ("right_wing", _)` order —
    /// a list rather than a fixed pair for the same reason
    /// [`CapeMesh::parts`] is one: a bake that produced no quads yields an
    /// empty list rather than a range into nothing.
    pub parts: Vec<(ElytraWing, PartRange)>,
}

/// Which wing a [`ElytraMesh::parts`] range belongs to.
///
/// A named side rather than a `&'static str`, because the side is not just a
/// label here — it *selects* the sign of two of the three rotation terms in
/// [`elytra_wing_transform`], and a stringly-typed version invites the
/// silently-symmetric bug where both wings get the left one's matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElytraWing {
    /// `left_wing` — pivot `x = +5`, rotations used as given.
    Left,
    /// `right_wing` — pivot `x = -5`, Y and Z rotations negated.
    Right,
}

impl ElytraWing {
    /// The part name in [`lodestone_assets::entity::elytra_model`].
    #[must_use]
    pub const fn part_name(self) -> &'static str {
        match self {
            ElytraWing::Left => "left_wing",
            ElytraWing::Right => "right_wing",
        }
    }

    /// The wing pivot's X in **model texels** (vanilla's elytra model builder
    /// offsets each wing part by `±5, 0, 0`).
    #[must_use]
    pub const fn pivot_x(self) -> f32 {
        match self {
            ElytraWing::Left => 5.0,
            ElytraWing::Right => -5.0,
        }
    }
}

impl ElytraMesh {
    /// Bake the elytra mesh.
    #[must_use]
    pub fn load() -> Self {
        let def = lodestone_assets::entity::elytra_model();
        let baked = lodestone_assets::entity::bake_entity_parts(&def);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut parts = Vec::new();
        for wing in [ElytraWing::Left, ElytraWing::Right] {
            let Some(part) = baked
                .iter()
                .find(|p| p.name.as_str() == wing.part_name() && !p.quads.is_empty())
            else {
                continue;
            };
            let index_start = indices.len() as u32;
            let vertex_start = vertices.len() as u32;
            push_part_quads(&part.quads, &mut vertices, &mut indices);
            parts.push((
                wing,
                PartRange {
                    index_start,
                    index_count: indices.len() as u32 - index_start,
                    vertex_start,
                    vertex_count: vertices.len() as u32 - vertex_start,
                },
            ));
        }
        ElytraMesh {
            vertices,
            indices,
            parts,
        }
    }

    /// Pair both wings with the wearer's `"body"` part, dropping them
    /// entirely for a non-humanoid rig — the same gate [`CapeMesh::attach`]
    /// and [`wearer_carries_armour`] use, and for the same reason: a rig with
    /// no `body` pivot has nothing for the wings to hang off, and attaching
    /// by part name alone would strap an elytra to a pig.
    pub fn attach<'a>(
        &'a self,
        wearer: &'a Skeleton,
    ) -> impl Iterator<Item = (ElytraWing, PartRange, usize)> + 'a {
        let humanoid = wearer_carries_armour(wearer);
        let body = wearer.index_of("body");
        self.parts
            .iter()
            .filter(move |_| humanoid)
            .filter_map(move |(wing, range)| body.map(|i| (*wing, *range, i)))
    }
}

/// Vanilla's elytra animation state's resting rotation triple
/// `(x_rot, y_rot, z_rot)` in radians — the target it lerps toward when the
/// wearer is neither fall-flying nor crouching, `(PI/12, 0, -PI/12)`.
///
/// This is also the elytra model's authored rest pose, which is why it is
/// what a standing player's wings look like. A caller that keeps no animation
/// state at all can pass this straight to [`elytra_wing_transform`] and get
/// the correct wings for every wearer who is standing, walking or running —
/// everything except a glide and a crouch.
#[must_use]
pub fn elytra_rest_rotations() -> (f32, f32, f32) {
    (
        std::f32::consts::PI / 12.0,
        0.0,
        -std::f32::consts::PI / 12.0,
    )
}

/// The per-tick *target* `(x_rot, y_rot, z_rot)` an elytra's animation state
/// lerps toward — vanilla's per-tick elytra-state update's three-way branch,
/// in radians.
///
/// `motion` is the wearer's delta movement in blocks per tick. Only its
/// **normalised Y** is read, and only when it is negative: a steeper dive
/// folds the wings back further, which is the whole visual point of the
/// gliding pose.
///
/// This is the pure half of vanilla's elytra animation state. The impure half
/// is two lerped triples (`rot*` and `rot*Old`) advanced once per game tick
/// by `current += (target - current) * ` [`ELYTRA_ROTATION_LERP`] and read
/// back interpolated by partial ticks — that state belongs wherever entity
/// ticks live, not here, exactly as `cape_sway`'s lagged cloak position does.
///
/// # Precedence
///
/// Fall-flying wins over crouching, not the other way round: vanilla checks
/// fall-flying first, and a player can be both.
#[must_use]
pub fn elytra_target_rotations(fall_flying: bool, crouching: bool, motion: Vec3) -> (f32, f32, f32) {
    use std::f32::consts::PI;
    if fall_flying {
        // `ratio = 1 - (-normalize(motion).y)^1.5` while descending, else 1.
        // Computed in f64 because vanilla's own vector type is
        // double-precision and its pow call operates in double too, and the
        // exponent is fractional, so the f32 round trip is not free.
        let ratio = if motion.y < 0.0 {
            let len = f64::from(motion.x).hypot(f64::from(motion.y)).hypot(f64::from(motion.z));
            // Vanilla's vector normalize returns ZERO for a zero-length
            // vector, whose `y` is 0 and so leaves `ratio` at 1 — matching
            // the guard rather than dividing by zero.
            let ny = if len < 1.0e-4 { 0.0 } else { f64::from(motion.y) / len };
            1.0 - (-ny).max(0.0).powf(1.5)
        } else {
            1.0
        };
        let ratio = ratio as f32;
        // Vanilla's lerp helper: `start + delta * (end - start)`.
        let lerp = |start: f32, end: f32| start + ratio * (end - start);
        (
            lerp(PI / 12.0, PI / 9.0),
            0.0,
            lerp(-PI / 12.0, -PI / 2.0),
        )
    } else if crouching {
        // Transcribed from the branch, not derived: the Y term is vanilla's
        // own float literal `0.08726646F` (5 degrees), and it is the only one
        // of the nine constants in this function that is not a fraction of PI.
        (PI * 2.0 / 9.0, 0.08726646, -PI / 4.0)
    } else {
        elytra_rest_rotations()
    }
}

/// The per-tick approach rate in vanilla's per-tick elytra-state update
/// (`rot += (target - rot) * 0.3`).
pub const ELYTRA_ROTATION_LERP: f32 = 0.3;

/// The wearer's crouching wing `y` offset in **model texels** — vanilla's
/// per-frame elytra animation step's `isCrouching ? 3.0F : 0.0F`, which it
/// assigns to *both* wings.
#[must_use]
pub const fn elytra_wing_y(crouching: bool) -> f32 {
    if crouching { 3.0 } else { 0.0 }
}

/// The per-frame placement of one wing, relative to the wearer's **body**
/// part transform.
///
/// `x_rot`/`y_rot`/`z_rot` are the *left* wing's angles in radians — the
/// triple [`elytra_target_rotations`] produces, after the caller's own
/// lerping. The right wing's negations are applied here rather than by the
/// caller so there is exactly one place that knows the sign convention.
///
/// # The composition
///
/// ```text
/// T(0, 0, 0.125) * T(pivot_x/16, y/16, 0) * Rz(z) * Ry(y) * Rx(x)
/// ```
///
/// * The leading translate is the elytra layer's own submit-step translate
///   of `(0, 0, 0.125)`, applied to the layer as a whole and therefore
///   **outside** the wing's own pivot. In blocks.
/// * `T(pivot_x/16, y/16, 0)` is the wing's pivot: `x` is authored and
///   constant (`±5` texels), `y` is assigned per frame by the per-frame
///   animation step and is `3` texels only while crouching. `z` is `0`.
/// * The rotation order is `Rz * Ry * Rx`, matching vanilla's own
///   translate-and-rotate part composition — not the `Rx * Rz * Ry` the cape
///   ends up with, which is a *composed* quaternion chain rather than a part
///   pose.
///
/// # The right wing
///
/// Vanilla's per-frame animation step gives it `yRot = -left.yRot` and
/// `zRot = -left.zRot`, and
/// leaves `xRot` and `y` shared. Two of three negated, and it is the two that
/// are *not* negated that make a "just mirror everything" version wrong: a
/// mirrored `xRot` pitches one wing up and the other down.
#[must_use]
pub fn elytra_wing_transform(
    wing: ElytraWing,
    x_rot: f32,
    y_rot: f32,
    z_rot: f32,
    crouching: bool,
) -> Mat4 {
    let (y_rot, z_rot) = match wing {
        ElytraWing::Left => (y_rot, z_rot),
        ElytraWing::Right => (-y_rot, -z_rot),
    };
    let layer = Mat4::from_translation(Vec3::new(0.0, 0.0, 0.125));
    let pivot = Mat4::from_translation(Vec3::new(
        wing.pivot_x() / 16.0,
        elytra_wing_y(crouching) / 16.0,
        0.0,
    ));
    let rotate =
        Mat4::from_rotation_z(z_rot) * Mat4::from_rotation_y(y_rot) * Mat4::from_rotation_x(x_rot);
    layer * pivot * rotate
}

/// The texture layers to draw for an item sitting in `slot`, in draw order —
/// empty when this item is not humanoid armour, or is armour for a *different*
/// slot, or its material declares no layers for this slot's layer type.
///
/// The slot equality check is vanilla's own armour-layer render gate: a
/// helmet's declared slot must equal the slot it is worn in, so a plugin can
/// put a helmet in the boots slot, and vanilla draws nothing rather than
/// drawing a helmet around the ankles.
#[must_use]
pub fn armour_layers(slot: ArmourSlot, item_path: &str) -> &'static [ArmourLayer] {
    match armour_item(item_path) {
        Some((item_slot, asset)) if item_slot == slot => asset.layers(slot.layer_type()),
        _ => &[],
    }
}

/// The gamma-space RGB a layer multiplies its texel by: vanilla's
/// dyeable-layer "colour when undyed" for a dyeable layer, white for any
/// other.
///
/// This is [`armour_layer_tint_with_dye`] with the stack's own
/// `minecraft:dyed_color` **absent** — kept as a zero-argument convenience
/// because every call site today (`gpu.rs::prepare_armour`) has no dye value
/// to hand it: the wire component is dropped at the shell's
/// `entity_snapshot` boundary. See `docs/armour-rendering.md` for the wiring
/// that would change that.
#[must_use]
pub fn armour_layer_tint(layer: &ArmourLayer) -> [u8; 3] {
    armour_layer_tint_with_dye(layer, None)
}

/// The gamma-space RGB a layer multiplies its texel by, given the wearer
/// stack's own `minecraft:dyed_color` component if it decoded one.
///
/// This is vanilla's own per-layer colour resolution, transcribed exactly:
///
/// ```text
/// fn color_for_layer(layer, dye_color) -> color {
///    if let Some(dyeable) = layer.dyeable() {
///       let color_when_undyed = dyeable.color_when_undyed().map(opaque).unwrap_or(0);
///       if dye_color != 0 { dye_color } else { color_when_undyed }
///    } else {
///       WHITE // no tint
///    }
/// }
/// ```
///
/// where `dye_color` is the stack's dyed-color component, opacity-forced, if
/// present, or `0` (not `color_when_undyed` — that fallback lives here, one
/// call up) if it does not carry one.
///
/// A non-dyeable layer (`layer.dye` is [`None`]) ignores `dyed_color`
/// entirely and returns white (opaque white, i.e. "no tint") — matching the
/// non-dyeable branch above, which never reads `dye_color`.
///
/// **A leather piece dyed pure black (`0x000000`) is indistinguishable from
/// an undyed one**, and this is vanilla's own behaviour, not a port bug:
/// forcing opacity only touches the alpha byte, so a `0x000000` dye still
/// reads as `dye_color == 0` and the `dye_color != 0 ? dye_color :
/// color_when_undyed` fallback falls through to
/// [`UNDYED_LEATHER_RGB`](lodestone_assets::equipment::UNDYED_LEATHER_RGB)
/// exactly as if the component were absent. `dyed_color_zero_reads_as_undyed`
/// pins this so a future "fix" that special-cases black does not quietly
/// diverge from the game it is porting.
#[must_use]
pub fn armour_layer_tint_with_dye(layer: &ArmourLayer, dyed_color: Option<u32>) -> [u8; 3] {
    let Some(undyed) = layer.dye else {
        // The layer is not dyeable: vanilla returns opaque white
        // unconditionally, never consulting the dye colour.
        return [255, 255, 255];
    };
    // Vanilla's dyed-color read forces only the alpha byte, so the low 24
    // bits of `dyed_color` are already the RGB vanilla reads. `0` (component
    // absent, or present-but-black) falls through to the undyed colour.
    match dyed_color.filter(|&rgb| rgb & 0x00FF_FFFF != 0) {
        Some(rgb) => [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8],
        None => undyed,
    }
}

// ---------------------------------------------------------------------------
// Dropped items
// ---------------------------------------------------------------------------
//
// A dropped item is an entity that is **not** a cuboid part rig, so none of the
// machinery above applies to it: it has no skeleton, no per-mob sheet, and no
// `entity_models` corpus entry. What it has is an *item model* — the same baked
// geometry [`BlockModels::item_forms`](crate::BlockModels::item_forms) already
// supplies for a hotbar icon — drawn in the world through the ordinary
// [`ModelPipeline`](crate::ModelPipeline) rather than the entity pipeline.
//
// This section owns the *placement*: where in the world that geometry goes, and
// how it bobs and spins. Transcribed from the 26.2 client's dropped-item
// submit step:
//
// ```text
//   box          = the item's ground-posed model bounding box
//   minOffsetY   = -box.minY + 0.0625
//   bob          = sin(ageInTicks / 10 + bobOffs) * 0.1 + 0.1
//   translate(0, bob + minOffsetY, 0)
//   rotateY(getSpin(ageInTicks, bobOffs))   // radians
//   // then the item is drawn under its display.ground transform
// ```
//
// and `getSpin(age, bobOffs) = age / 20 + bobOffs`.
//
// # The winding invariant, stated for a *world* pose
//
// The GUI item path composes `gui_ortho * gui_item_pose`, and each of those two
// matrices has a negative determinant so that the **product**'s determinant sign
// matches [`Camera::view_projection`](crate::Camera::view_projection)'s — which
// is itself negative, because `glam`'s DirectX right-handed perspective is.
// That is a statement about the *composed* matrix, and it does not transfer to
// this path.
//
// Here the pose is a **world-space model matrix** left-multiplied by the very
// same `Camera::view_projection`, exactly like a terrain section's. So the pose
// must not flip anything: its determinant has to be **positive**, and the
// composed `view_projection * pose` then inherits the camera's negative sign.
// Reading the GUI rule as "the pose determinant must be negative" and coding to
// it would ship an item you are looking at the *inside* of — which spins
// convincingly in a screenshot. `dropped_item_pose_preserves_winding` derives
// the reference sign from the camera rather than hardcoding either answer.

/// Vanilla's dropped-item renderer constant for how far the lowest point of
/// the posed model floats above the entity's own position, in blocks.
pub const ITEM_MIN_HOVER_HEIGHT: f32 = 0.0625;

/// Vanilla's dropped-item renderer constant: a posed model thinner than this
/// in `z` is treated as a flat sprite and a stack of them is
/// fanned along `z` rather than jittered in three axes.
pub const FLAT_ITEM_DEPTH_THRESHOLD: f32 = 0.0625;

/// Bob amplitude in blocks (`… * 0.1F + 0.1F`), so the bob spans `0.0..=0.2`.
pub const ITEM_BOB_AMPLITUDE: f32 = 0.1;

/// Ticks per radian of bob phase (`sin(ageInTicks / 10.0F + bobOffs)`).
pub const ITEM_BOB_TICKS_PER_RADIAN: f32 = 10.0;

/// Ticks per radian of spin (`getSpin = ageInTicks / 20.0F + bobOffs`).
pub const ITEM_SPIN_TICKS_PER_RADIAN: f32 = 20.0;

/// `display.ground` of `minecraft:block/block`, which **every** block item model
/// inherits (verified against 26.2's `client.jar`).
///
/// # This is now a *fallback*, not the only source
///
/// It used to be the only one: `icon.rs` did `resolved.display.get("gui")` and
/// dropped every other slot, so [`ItemGeometry`](crate::ItemGeometry) carried
/// the isometric inventory pose and nothing else. The asset layer now carries
/// all nine slots ([`ItemGeometry::display`](crate::ItemGeometry::display)), and
/// [`ground_transform`] reads the real declared `ground` in preference to this.
///
/// The constants stay because the *fallback still has to be right*: an item
/// whose model chain declares no `ground` at all would otherwise be posed with
/// the identity, i.e. a full-size 1×1×1 block lying in the grass. Being wrong by
/// a factor of four in scale is the visible signature.
///
/// Verified against 26.2's `client.jar`: `models/block/block.json` declares
/// `ground` as `translation [0, 3, 0]`, `scale 0.25`.
pub const BLOCK_ITEM_GROUND: DisplayTransform = DisplayTransform {
    rotation: [0.0, 0.0, 0.0],
    translation: [0.0, 3.0, 0.0],
    scale: [0.25, 0.25, 0.25],
};

/// `display.ground` of `minecraft:item/generated`, the parent of every flat
/// sprite item. See [`BLOCK_ITEM_GROUND`] for why this is a constant.
pub const GENERATED_ITEM_GROUND: DisplayTransform = DisplayTransform {
    rotation: [0.0, 0.0, 0.0],
    translation: [0.0, 2.0, 0.0],
    scale: [0.5, 0.5, 0.5],
};

/// The `display.ground` transform to pose an item under, chosen by its GUI
/// lighting mode: `side` is the block-model family (`block/block`), `front` the
/// flat-sprite family (`item/generated`). Vanilla makes the same split — the two
/// `gui_light` values partition the item models almost exactly along the same
/// line — and it is the only signal reachable from a baked
/// [`ItemGeometry`](crate::ItemGeometry) today.
#[must_use]
pub fn ground_transform_for(gui_light: GuiLight) -> DisplayTransform {
    match gui_light {
        GuiLight::Side => BLOCK_ITEM_GROUND,
        GuiLight::Front => GENERATED_ITEM_GROUND,
    }
}

/// The `display.ground` transform to pose a **dropped** item under: the one the
/// item's own model chain declares, falling back to
/// [`ground_transform_for`]`(gui_light)` when it declares none.
///
/// This is the accessor a drop should use.
/// [`DisplayTransforms::declared`] rather than `get` is the whole point: `get`
/// answers an undeclared slot with the identity, which for `ground` means a
/// full-size block lying in the grass rather than vanilla's quarter-scale one.
/// Distinguishing "the pack said identity" from "we found nothing" is what makes
/// the [`GuiLight`]-keyed guess a fallback instead of dead code.
///
/// # How to change it
///
/// The other slots want exactly this shape — a `hand_transform(&DisplayTransforms,
/// Arm, /* first person */ bool)` for held items, reading
/// `thirdperson_righthand`/`firstperson_righthand` with
/// [`DisplaySlot::left_hand_fallback`](lodestone_assets::DisplaySlot::left_hand_fallback)
/// already handled inside `DisplayTransforms::get`. There is **no** sensible
/// `GuiLight`-keyed fallback for those (`block/block` and `item/generated`
/// disagree on far more than scale), so an undeclared hand slot should draw the
/// identity and be counted, not guessed at.
#[must_use]
pub fn ground_transform(display: &DisplayTransforms, gui_light: GuiLight) -> DisplayTransform {
    display
        .declared(DisplaySlot::Ground)
        .unwrap_or_else(|| ground_transform_for(gui_light))
}

/// A stable per-entity bob/spin phase in `[0, 2π)`, standing in for vanilla's
/// `bobOffs = random.nextFloat() * PI * 2`.
///
/// Vanilla seeds it from the client's RNG at spawn; we cannot observe that, and
/// re-rolling it every frame would make an item jitter instead of spin. Hashing
/// the server-assigned entity id gives the same *property* that matters — two
/// items dropped together do not bob in lockstep — while staying a pure function
/// of data both the renderer and a test can see.
#[must_use]
pub fn item_bob_offset(entity_id: i32) -> f32 {
    // A single multiplicative-hash round over the id, taken as a fraction.
    let mixed = (entity_id as u32).wrapping_mul(0x9E37_79B9);
    let frac = f32::from(u16::try_from(mixed >> 16).unwrap_or(0)) / 65536.0;
    frac * std::f32::consts::TAU
}

/// Vanilla's vertical bob at `age_ticks`: `sin(age / 10 + offs) * 0.1 + 0.1`,
/// so the result is in `0.0..=0.2` blocks and never negative.
#[must_use]
pub fn item_bob_height(age_ticks: f32, bob_offset: f32) -> f32 {
    (age_ticks / ITEM_BOB_TICKS_PER_RADIAN + bob_offset).sin() * ITEM_BOB_AMPLITUDE
        + ITEM_BOB_AMPLITUDE
}

/// Vanilla's dropped-item spin function: the item's yaw in **radians** at `age_ticks`.
#[must_use]
pub fn item_spin_radians(age_ticks: f32, bob_offset: f32) -> f32 {
    age_ticks / ITEM_SPIN_TICKS_PER_RADIAN + bob_offset
}

/// The model-space `y` extent of `quads` once posed by `ground`, as
/// `(min_y, max_y)`. `(0, 0)` for an empty quad list.
///
/// This is vanilla's posed-model bounding box for the `y` axis: it is
/// measured on the **posed** model, which is why it cannot be a constant — a
/// scaled-down cube and a full-size one hover differently.
#[must_use]
pub fn posed_item_y_extent(quads: &[BakedQuad], ground: &DisplayTransform) -> (f32, f32) {
    let pose = display_matrix(ground);
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for quad in quads {
        for p in &quad.positions {
            let y = pose.transform_point3(Vec3::from(*p)).y;
            min = min.min(y);
            max = max.max(y);
        }
    }
    if min > max { (0.0, 0.0) } else { (min, max) }
}

/// The posed model's `z` extent, the mirror of [`posed_item_y_extent`].
///
/// This is the input to vanilla's flat-versus-solid branch in its
/// multi-copy item-cluster submit step: a model whose depth exceeds
/// [`FLAT_ITEM_DEPTH_THRESHOLD`] is a block-ish thing whose extra stack copies
/// jitter in all three axes, while a flat sprite instead fans its copies evenly
/// along `z`. Measured on the *posed* model for the same reason the `y` version
/// is — the branch is about the drawn depth, not the model's declared one.
#[must_use]
pub fn posed_item_z_extent(quads: &[BakedQuad], ground: &DisplayTransform) -> (f32, f32) {
    let pose = display_matrix(ground);
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for quad in quads {
        for p in &quad.positions {
            let z = pose.transform_point3(Vec3::from(*p)).z;
            min = min.min(z);
            max = max.max(z);
        }
    }
    if min > max { (0.0, 0.0) } else { (min, max) }
}

/// How many copies of a stack vanilla draws — its item-cluster render-state
/// count: 1, then 2 above 1, 3 above 16, 4 above 32, 5 above 48.
#[must_use]
pub fn rendered_amount(count: u32) -> u32 {
    match count {
        0..=1 => 1,
        2..=16 => 2,
        17..=32 => 3,
        33..=48 => 4,
        _ => 5,
    }
}

/// Per-copy scatter for a stack's extra copies, in the idiom
/// [`item_bob_offset`] set.
///
/// Vanilla seeds this from a random source keyed on the item's registry id
/// plus its damage value, which we cannot observe. So this hashes `(entity_id, copy)` for
/// the same *property* — no two drops and no two copies scatter in lockstep —
/// rather than chasing bytes we have no way to reproduce. `copy == 0` is exactly
/// zero, matching vanilla's unperturbed first `submit`.
///
/// `extent` is the half-range on each axis: `0.15` for a solid model (all three
/// axes), `0.075` for a flat sprite (x and y only, hence the zero `z` the caller
/// discards).
#[must_use]
pub fn item_cluster_jitter(entity_id: i32, copy: u32, extent: f32) -> Vec3 {
    if copy == 0 {
        return Vec3::ZERO;
    }
    // Three decorrelated hash rounds over the same (id, copy) key — one per
    // axis, so a copy does not move along the diagonal.
    let key = (entity_id as u32)
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(copy.wrapping_mul(0x85EB_CA6B));
    let axis = |salt: u32| {
        let mixed = key.wrapping_add(salt).wrapping_mul(0xC2B2_AE35);
        let frac = f32::from(u16::try_from(mixed >> 16).unwrap_or(0)) / 65536.0;
        (frac * 2.0 - 1.0) * extent
    };
    Vec3::new(axis(0x1656_67B1), axis(0x27D4_EB2F), axis(0x1656_67B5))
}

/// Vanilla's lowest-point offset: the lift that puts the posed model's
/// lowest point exactly [`ITEM_MIN_HOVER_HEIGHT`] above the entity's own
/// position.
#[must_use]
pub fn item_hover_lift(quads: &[BakedQuad], ground: &DisplayTransform) -> f32 {
    -posed_item_y_extent(quads, ground).0 + ITEM_MIN_HOVER_HEIGHT
}

/// The world placement matrix for a dropped item, matching vanilla's
/// dropped-item submit step's pose-stack order exactly:
///
/// ```text
/// T(position) · T(0, bob + hover_lift, 0) · Ry(spin) · display_matrix(ground)
/// ```
///
/// `position` is the item entity's reported world position, `age_ticks` its
/// continuous age (`ageInTicks`, fractional between server ticks), `bob_offset`
/// its per-entity phase ([`item_bob_offset`]) and `hover_lift`
/// [`item_hover_lift`] for the same quads and transform.
///
/// The determinant is **positive** (a translation, a rotation and a positive
/// uniform scale), so this composes with `Camera::view_projection` to the same
/// winding as terrain. See the section note above for why "negative" is the
/// tempting wrong answer.
#[must_use]
pub fn dropped_item_matrix(
    position: Vec3,
    age_ticks: f32,
    bob_offset: f32,
    ground: &DisplayTransform,
    hover_lift: f32,
) -> Mat4 {
    let bob = item_bob_height(age_ticks, bob_offset);
    let spin = item_spin_radians(age_ticks, bob_offset);
    Mat4::from_translation(position)
        * Mat4::from_translation(Vec3::new(0.0, bob + hover_lift, 0.0))
        * Mat4::from_rotation_y(spin)
        * display_matrix(ground)
}

/// Mesh one dropped item's baked geometry into a world-space [`ModelMesh`],
/// ready for [`GpuModelMesh::upload`](crate::GpuModelMesh) and a draw through
/// the ordinary [`ModelPipeline`](crate::ModelPipeline) with a *world* camera
/// uniform (`section_origin` zero).
///
/// The geometry and the shading come from [`mesh_item_quads`], which the hotbar
/// already uses, so a dropped stone and a stone in slot 0 are textured and shaded
/// from the identical quads. The one thing overridden afterwards is the packed
/// light byte: `mesh_item_quads` nails every vertex to
/// [`GUI_ITEM_LIGHT`](crate::GUI_ITEM_LIGHT) because an inventory slot is
/// full-bright by definition, and a dropped item in a dark cave is emphatically
/// not. Pass the world sample (see [`EntityLightSource`](crate::EntityLightSource)
/// on the shell side); pass `GUI_ITEM_LIGHT` when there is no world to sample.
#[must_use]
pub fn dropped_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    ground: &DisplayTransform,
    position: Vec3,
    age_ticks: f32,
    bob_offset: f32,
    light: u8,
) -> ModelMesh {
    let lift = item_hover_lift(quads, ground);
    let pose = dropped_item_matrix(position, age_ticks, bob_offset, ground, lift);
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

/// Vanilla's vault display-item rotation speed: degrees per client tick the
/// display item spins, unbounded (vanilla wraps the stored angle before
/// storing it, but the rotation applied from it is periodic mod 360, so the
/// unwrapped running total below is the same rotation and needs no wrap).
pub const VAULT_SPIN_DEGREES_PER_TICK: f32 = 10.0;

/// Vanilla's per-tick vault display-item spin update, evaluated at a
/// continuous tick: a shortest-path rotation lerp between the previous and
/// current spin, where `currentSpin = previousSpin + 10°` every tick, which
/// for a constant per-tick step is exactly the unwrapped linear form below
/// (the shortest-path wrap only matters when the two ends are more than 180°
/// apart, and adjacent ticks here are always exactly 10° apart).
///
/// # A deliberate simplification: tied to absolute world time, not per-vault age
///
/// Real vanilla starts each vault's own counter at `0` when its block entity
/// is constructed — effectively when its chunk first loads — so two vaults
/// loaded at different moments spin out of phase with each other. This client
/// has no record of *when* a given vault's block entity was constructed, only
/// the world's current game time, so every vault here shares one clock instead
/// (the same limitation `crate::beacon`'s rotating core and
/// `block_entity::banner_phase` already accept, per those functions' docs).
/// Decorative only — nothing about a vault's function reads this phase.
#[must_use]
pub fn vault_spin_degrees(game_time: i64, partial_tick: f32) -> f32 {
    (game_time as f32 + partial_tick) * VAULT_SPIN_DEGREES_PER_TICK
}

/// The world placement matrix for one copy of a vault's floating display-item
/// cluster, matching vanilla's vault-renderer submit step's pose stack —
///
/// ```text
/// T(block_pos) · T(0.5, 0.4, 0.5) · Ry(spin) · T(offset) · display_matrix(ground)
/// ```
///
/// — composed with vanilla's own per-copy translate for a multi-copy item
/// cluster (`offset`, zero for the first copy) and the item's own
/// `display.ground` transform on the right, the same composition
/// [`dropped_item_matrix`] uses for the identical reason: vanilla applies the
/// display transform *inside* its item render-state submit step, after every
/// pose this function's caller pushes.
#[must_use]
pub fn vault_display_item_matrix(
    block_pos: Vec3,
    spin_deg: f32,
    offset: Vec3,
    ground: &DisplayTransform,
) -> Mat4 {
    Mat4::from_translation(block_pos)
        * Mat4::from_translation(Vec3::new(0.5, 0.4, 0.5))
        * Mat4::from_rotation_y(spin_deg.to_radians())
        * Mat4::from_translation(offset)
        * display_matrix(ground)
}

/// Mesh one copy of a vault's display-item cluster into a world-space
/// [`ModelMesh`], for the same model-pipeline draw [`dropped_item_mesh`] feeds
/// — see that function's doc for why a vault's floating reward is an *item
/// model* on the model pipeline rather than a cuboid rig on
/// [`EntityPipeline`](crate::EntityPipeline).
#[must_use]
pub fn vault_display_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    ground: &DisplayTransform,
    block_pos: Vec3,
    spin_deg: f32,
    offset: Vec3,
    light: u8,
) -> ModelMesh {
    let pose = vault_display_item_matrix(block_pos, spin_deg, offset, ground);
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

/// Mesh one campfire's cooking item into a world-space [`ModelMesh`], for the
/// same model-pipeline draw [`dropped_item_mesh`] feeds.
///
/// The placement is [`campfire_item_matrix`](crate::block_entity::campfire_item_matrix)
/// — a pure port of vanilla's campfire-renderer submit step's pose stack —
/// composed with the item's own `display.fixed` on the right, because that is
/// where vanilla applies the item transform: its item render-state submit
/// step applies it *after* the renderer's own pushes. Composing it on the
/// left instead would rotate the campfire's corner offset by the item's
/// display rotation, which for a food sprite (`fixed` is a `180°` Y turn on
/// most of them) mirrors all four items into the wrong corners while still
/// looking like four items on a campfire.
///
/// `fixed` is `display.get(DisplaySlot::Fixed)` — vanilla resolves a
/// campfire's stack in the item-frame display context, **not** the ground
/// one. That is the one thing this does not share with the drop path.
#[must_use]
pub fn campfire_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    fixed: &DisplayTransform,
    pos: [i32; 3],
    facing_yaw_deg: f32,
    slot: usize,
    light: u8,
) -> ModelMesh {
    let pose = crate::block_entity::campfire_item_matrix(pos, facing_yaw_deg, slot)
        * display_matrix(fixed);
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

/// Mesh one item on a shelf's slot into a world-space [`ModelMesh`], for the
/// same model-pipeline draw [`dropped_item_mesh`] feeds.
///
/// The placement is
/// [`shelf_slot_matrix`](crate::block_entity::shelf_slot_matrix) —
/// ported from vanilla's shelf-renderer item-submit pose stack up to its
/// final translate — composed with **two** further transforms this function
/// alone can supply, both requiring the item's own baked quads:
///
/// 1. The bounding-box correction (a vertical offset in the real jar):
///    `-box.minY`, plus `-(box.maxY - box.minY) / 2` when the shelf is
///    *not* aligned to the bottom. `box` is the item's posed-model bounding
///    box — the item's extents *after* its own on-shelf display-context
///    transform, which is exactly what [`posed_item_y_extent`] measures.
///    Applied as a translate **inside** the `0.25×` scale
///    [`shelf_slot_matrix`] already applied (vanilla translates after it
///    scales), which is why this is a right-hand factor rather than folded
///    into that function's own world-space translate.
/// 2. The item's own `display.on_shelf` transform, composed on the right for
///    the same reason [`campfire_item_mesh`] composes there: vanilla applies
///    it *inside* its item render-state submit step, after every pose the
///    renderer itself pushes.
#[must_use]
pub fn shelf_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    on_shelf: &DisplayTransform,
    pos: [i32; 3],
    facing_yaw_deg: f32,
    slot: usize,
    align_to_bottom: bool,
    light: u8,
) -> ModelMesh {
    let (min_y, max_y) = posed_item_y_extent(quads, on_shelf);
    let mut offset_y = -min_y;
    if !align_to_bottom {
        offset_y += -(max_y - min_y) / 2.0;
    }
    let pose = crate::block_entity::shelf_slot_matrix(pos, facing_yaw_deg, slot, align_to_bottom)
        * Mat4::from_translation(Vec3::new(0.0, offset_y, 0.0))
        * display_matrix(on_shelf);
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

/// Mesh a suspicious sand/gravel block's revealed item into a world-space
/// [`ModelMesh`], for the same model-pipeline draw [`dropped_item_mesh`] feeds.
///
/// The placement is
/// [`brushable_item_matrix`](crate::block_entity::brushable_item_matrix) —
/// ported from vanilla's brushable-block renderer submit step's pose stack —
/// composed with the item's own `display.fixed` on the right, for the
/// identical reason [`campfire_item_mesh`] composes there: vanilla's
/// brushable-block render-state extraction resolves the item in the
/// item-frame display context, not the ground one.
#[must_use]
pub fn brushable_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    fixed: &DisplayTransform,
    pos: [i32; 3],
    hit_direction: lodestone_assets::Direction,
    dust_progress: u8,
    light: u8,
) -> ModelMesh {
    let pose = crate::block_entity::brushable_item_matrix(pos, hit_direction, dust_progress)
        * display_matrix(fixed);
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

/// [`mesh_item_quads`] followed by the world-light override both
/// [`dropped_item_mesh`] and [`held_item_mesh`] need: the baked geometry nails
/// every vertex to [`GUI_ITEM_LIGHT`](crate::GUI_ITEM_LIGHT) (an inventory slot
/// is full-bright by definition), and a world-placed item is not, so the caller's
/// own world sample overwrites it here, in one place, after meshing.
pub(crate) fn mesh_item_quads_with_light(
    quads: &[BakedQuad],
    pose: Mat4,
    gui_light: GuiLight,
    light: u8,
) -> ModelMesh {
    let mut mesh = mesh_item_quads(quads, pose, gui_light);
    for vertex in &mut mesh.vertices {
        vertex.light = light;
    }
    mesh
}

// ---------------------------------------------------------------------------
// Thrown item projectiles
// ---------------------------------------------------------------------------
//
// A snowball is not a cuboid rig and not a dropped item either: it is the item's
// *own* model, posed by `display.ground`, turned to face the camera, and drawn at
// the entity's position with no bob, no spin and no hover lift. Transcribed from
// the 26.2 client's thrown-item renderer, whose whole submit step is
//
// ```text
// poseStack.scale(scale, scale, scale);
// poseStack.mulPose(camera.orientation);
// state.item.submit(...)                  // resolved in the ground display context
// ```
//
// with the entity's position already on the pose stack by the dispatcher. The
// ground context is why [`ground_transform`] is shared with the drop path
// rather than duplicated: vanilla's render-state extraction for a thrown item
// resolves the item's display in that same ground context.

/// One entity type's thrown-item-renderer registration: which item's model to
/// draw, at what scale, and whether the renderer forces full-bright block light.
///
/// The `scale` and `full_bright` columns are **not** uniform, and reading them as
/// uniform is the visible bug: a `fireball` is `3.0` and a `small_fireball`
/// `0.75`, so the two would otherwise be the same size on screen even though the
/// large one is four times the small one in vanilla.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThrownItem {
    /// The item id whose baked geometry to draw, e.g. `"minecraft:snowball"`.
    ///
    /// This is vanilla's default-item fallback. It is only the *fallback*: the
    /// entity's real stack rides entity metadata (the same item-stack
    /// metadata field a dropped item uses), and a caller that has it
    /// should prefer it — a dispenser-fired arrow-of-harming analogue for
    /// potions is exactly the case where the two differ.
    pub item: &'static str,
    /// Vanilla's thrown-item-renderer scale, applied *before* the billboard
    /// rotation.
    pub scale: f32,
    /// Vanilla's full-bright flag, which overrides the block light level to
    /// `15`. A fireball glows; a snowball does not.
    pub full_bright: bool,
}

/// The [`ThrownItem`] registration for an entity type path (`"snowball"`), or
/// `None` for every entity that is not drawn by `ThrownItemRenderer`.
///
/// This is the **complete** 26.2 registration list, read out of
/// `client/renderer/entity/EntityRenderers.java` rather than guessed from the
/// name. Two entries commonly assumed to be here are not, and adding them would
/// draw the wrong thing:
///
/// * **`wind_charge` and `breeze_wind_charge` use `WindChargeRenderer`**, a real
///   cuboid model — not an item billboard, and there is no `wind_charge` *item*
///   sprite to draw either. Both are now in the
///   [`entity_models`](lodestone_assets::entity_models) corpus (`wind_charge`
///   model, `breeze_wind_charge` aliased onto it — see `wind_charge_model`'s
///   doc), placed by [`non_living_vehicle_matrix`] rather than by
///   [`entity_model_matrix`]. This entry stays for the same reason the
///   arrow/trident one below does: it is what stops either type being added to
///   the table below, which would draw an item billboard over the mesh.
/// * **`arrow`, `spectral_arrow` and `trident` use `ArrowRenderer`/`ThrownTridentRenderer`**,
///   a 3-D cuboid rig, not an item billboard. Those three are now in the
///   [`entity_models`](lodestone_assets::entity_models) corpus and are placed by
///   [`projectile_model_matrix`] rather than by [`entity_model_matrix`]; see
///   `docs/projectile-renderers.md`. This entry stayed here after they landed
///   because the fact it records — that they are *not* `ThrownItemRenderer`
///   entries — is what stops them being added to the table below, which would
///   draw an item sprite over the mesh.
///
///   The note this replaced said the orientation "needs a velocity the draw
///   record does not carry". That was the wrong conclusion from a true premise:
///   vanilla derives `yRot`/`xRot` from `atan2` on velocity, but it does so on
///   the *server* too (`Projectile.shoot`, `AbstractArrow.tick`) and then
///   broadcasts the result as ordinary entity rotation. The draw record's
///   existing `yaw`/`pitch` **are** those velocity-derived angles, so no velocity
///   plumbing was needed.
///
/// # "Not a `ThrownItemRenderer` entry" and "not drawn as an item" are two claims
///
/// The note this paragraph replaced ran them together, listing
/// `dragon_fireball`, `wither_skull`, `llama_spit`, `shulker_bullet`,
/// `fishing_bobber`, `firework_rocket` and `end_crystal` as "dedicated renderers
/// too, and likewise absent". The first claim is true of all seven and this
/// table's membership is exactly that set — a parity gate in
/// `tests/thrown_and_held_item_pixels.rs` checks it against the vanilla
/// registration list, so widening the table is widening what the table *means*.
///
/// The second claim is false of one, and knowing which matters to whoever picks
/// the rest up. `FireworkEntityRenderer` **does** draw an item model billboarded
/// on `camera.orientation`, exactly the way these entries are drawn. What keeps
/// it out is not its geometry but its inputs: the stack comes from the entity
/// rather than from a default, and a rocket fired from a crossbow is spun onto
/// its flight axis by a metadata bit the draw record does not carry. Adding it
/// here would make the table mean "types drawn as a billboarded item", which is
/// a *different* table and would take the parity gate's premise with it.
///
/// Four of the other six now have corpus rigs of their own — `wither_skull`,
/// `llama_spit`, `shulker_bullet` and `evoker_fangs` — placed by
/// [`non_living_vehicle_matrix`] or [`projectile_model_matrix`]. They stay out of
/// this table for the same reason the arrows do: an item sprite drawn over a mesh
/// is two wrong things at once.
///
/// `dragon_fireball` and `fishing_bobber` are still absent and neither is an
/// item: the first is a single camera-facing quad built vertex by vertex from a
/// texture, the second a billboard plus a line back to the caster. Both need a
/// draw path this crate does not have.
#[must_use]
pub fn thrown_item_for(type_path: &str) -> Option<ThrownItem> {
    // `(entity type, default item, scale, full_bright)`.
    const TABLE: &[(&str, &str, f32, bool)] = &[
        ("egg", "minecraft:egg", 1.0, false),
        ("ender_pearl", "minecraft:ender_pearl", 1.0, false),
        (
            "experience_bottle",
            "minecraft:experience_bottle",
            1.0,
            false,
        ),
        // `EyeOfEnder.getDefaultItem()` is `Items.ENDER_EYE` — the *item* id is
        // `ender_eye`, not `eye_of_ender`, which is the entity type. Using the
        // entity name here resolves no item and draws nothing.
        ("eye_of_ender", "minecraft:ender_eye", 1.0, true),
        ("fireball", "minecraft:fire_charge", 3.0, true),
        ("lingering_potion", "minecraft:lingering_potion", 1.0, false),
        ("small_fireball", "minecraft:fire_charge", 0.75, true),
        ("snowball", "minecraft:snowball", 1.0, false),
        ("splash_potion", "minecraft:splash_potion", 1.0, false),
    ];
    TABLE
        .iter()
        .find(|(name, ..)| *name == type_path)
        .map(|&(_, item, scale, full_bright)| ThrownItem {
            item,
            scale,
            full_bright,
        })
}

/// The **camera→world rotation**, which is what vanilla's `camera.orientation`
/// is: apply it to a model authored facing camera-space `+Z` and the model faces
/// the eye.
///
/// # Derived from the view matrix, not written out as `Ry(yaw)·Rx(pitch)`
///
/// Every hand-written form of this was wrong on the first try, in a different way
/// each time, because three conventions stack: vanilla's own quaternion is
/// `rotationYXZ(π - yRot, -xRot, 0)` (note the `π -`, which exists because MC's
/// camera space is rotated 180° from its world space), `glam`'s right-handed view
/// looks down **-Z**, and [`Camera::forward`](crate::Camera::forward) is
/// Minecraft's convention (`yaw 0` faces `+Z`). Taking the view matrix and
/// inverting its rotation cannot get any of those backwards: a view matrix is
/// `R · T` with `R` orthonormal, so `R⁻¹ = Rᵀ`.
///
/// Pass [`Camera::view_matrix`](crate::Camera::view_matrix). The determinant is
/// `+1`, so this does not flip winding — see [`thrown_item_matrix`].
///
/// # Why the item's front face lands the right way round either way
///
/// A flat sprite item is [`extruded_sprite_geometry`](crate::BlockModels)'s slab,
/// whose `SOUTH` face (outward normal `+Z`) carries UVs `(0, 0, 16, 16)` and whose
/// `NORTH` face carries `(16, 0, 0, 16)` — the reversed `u`. That flip is exactly
/// what makes *both* faces read unmirrored from their own side, so a 180°
/// yaw error here is invisible on the sprite items, which is every entity in
/// [`thrown_item_for`]. What is **not** invisible is getting the *pitch* term
/// wrong (an upside-down snowball) or dropping the rotation entirely (a slab seen
/// edge-on from the side, i.e. a near-invisible sliver).
#[must_use]
pub fn camera_orientation(view_matrix: Mat4) -> Mat4 {
    let mut rotation = view_matrix;
    rotation.w_axis = Vec4::W;
    rotation.transpose()
}

/// The world placement matrix for a thrown item projectile, matching
/// `ThrownItemRenderer.submit`'s pose-stack order exactly:
///
/// ```text
/// T(position) · S(scale) · camera_orientation · display_matrix(ground)
/// ```
///
/// `orientation` is [`camera_orientation`]`(camera.view_matrix())` and `ground`
/// the item's own [`ground_transform`] — the `GROUND` display context
/// `extractRenderState` resolves the item in.
///
/// **No bob, no spin, no hover lift.** Those three are `ItemEntityRenderer`'s and
/// are the tempting thing to reuse from [`dropped_item_matrix`]; a bobbing,
/// spinning snowball in flight is the signature of having done so.
///
/// The determinant is **positive** — a translation, a positive uniform scale, a
/// rotation and `display_matrix`'s positive scale — so this composes with
/// `Camera::view_projection` to the same winding as terrain, exactly like a drop.
#[must_use]
pub fn thrown_item_matrix(
    position: Vec3,
    orientation: Mat4,
    scale: f32,
    ground: &DisplayTransform,
) -> Mat4 {
    Mat4::from_translation(position)
        * Mat4::from_scale(Vec3::splat(scale))
        * orientation
        * display_matrix(ground)
}

/// Mesh one thrown item projectile into a world-space [`ModelMesh`], for the same
/// pass and the same camera uniform [`dropped_item_mesh`] feeds.
///
/// `light` is the packed sky/block sample at the projectile, or
/// [`GUI_ITEM_LIGHT`](crate::GUI_ITEM_LIGHT) when [`ThrownItem::full_bright`] is
/// set — vanilla's `getBlockLightLevel` override returns `15` for the fireballs
/// and the eye of ender, which is what makes a fireball readable against a dark
/// Nether ceiling.
#[must_use]
pub fn thrown_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    ground: &DisplayTransform,
    position: Vec3,
    orientation: Mat4,
    scale: f32,
    light: u8,
) -> ModelMesh {
    let pose = thrown_item_matrix(position, orientation, scale, ground);
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

// ---------------------------------------------------------------------------
// `minecraft:special` items on the 3-D world surfaces
// ---------------------------------------------------------------------------
//
// A chest, shulker box or skull has no item model and no block model — every
// triangle comes from a block-entity rig, resolved once by
// [`crate::special_item_rig`]. The *poses* are the ordinary item poses; only the
// geometry source differs. So the two helpers here are the pieces that
// [`dropped_item_matrix`]/[`held_item_matrix`] need and that a rig cannot supply
// the way a quad list can:
//
// * a hover lift measured from the rig's own AABB rather than from quads;
// * an item-frame pose, which no baked-item path needed because a framed item
//   drew nothing at all before.
//
// Everything else is shared with the baked path *by calling the same function*,
// which is the point: a chest and a pickaxe must bob, spin and hang on identical
// arcs. See `docs/held-block-entity-items.md`.

/// Vanilla's `minOffsetY` for a **rig** rather than a quad list: the lift that
/// puts the posed rig's lowest point [`ITEM_MIN_HOVER_HEIGHT`] above the drop's
/// own position.
///
/// `local_min`/`local_max` are a `BlockEntityMesh`'s rest-pose AABB, and `ground`
/// the item's own `display.ground`. All eight corners are transformed, not just
/// `local_min`: `display_matrix` can rotate, and under a rotation the lowest
/// point of the posed box is not the image of the lowest point of the original
/// one. Transforming `local_min` alone is the plausible wrong version — it agrees
/// exactly whenever the transform has no rotation, which is true of most `ground`
/// transforms and false of the ones that matter.
#[must_use]
pub fn special_item_hover_lift(local_min: Vec3, local_max: Vec3, ground: &DisplayTransform) -> f32 {
    let pose = display_matrix(ground);
    let mut min_y = f32::INFINITY;
    for i in 0..8u8 {
        let corner = Vec3::new(
            if i & 1 == 0 { local_min.x } else { local_max.x },
            if i & 2 == 0 { local_min.y } else { local_max.y },
            if i & 4 == 0 { local_min.z } else { local_max.z },
        );
        min_y = min_y.min(pose.transform_point3(corner).y);
    }
    if min_y.is_finite() {
        -min_y + ITEM_MIN_HOVER_HEIGHT
    } else {
        ITEM_MIN_HOVER_HEIGHT
    }
}

// ---------------------------------------------------------------------------
// Item frames
// ---------------------------------------------------------------------------
//
// `ItemFrameRenderer` — the frame body, and whatever hangs in it. Both are posed
// out of one shared chain, [`item_frame_space`], for the reason the boat's hull
// and its water patch share one: two matrices that must agree can only be
// guaranteed to agree by being the same matrix.

/// `poseStack.translate(0.0F, 0.0F, 0.4375F)` — how far in front of the frame's
/// own plane a *visible* frame's contents sit.
const ITEM_FRAME_CONTENT_LIFT: f32 = 0.4375;

/// The same step for an **invisible** frame, vanilla's `translate(0, 0, 0.5F)`.
/// A frame with no body to hold it clear needs the extra 1/16 to stay out of the
/// wall behind it.
const ITEM_FRAME_INVISIBLE_CONTENT_LIFT: f32 = 0.5;

/// `poseStack.scale(0.5F, 0.5F, 0.5F)` in `ItemFrameRenderer.submit`'s item
/// branch — the one number here a reader is likely to assume is `1.0` from the
/// framed *map* path. A map is drawn a full block across by its own separate
/// branch, and copying that would draw a chest twice the size of the frame
/// around it.
const FRAMED_ITEM_SCALE: f32 = 0.5;

/// One eighth turn, `rotation * 360 / 8` from `Axis.ZP.rotationDegrees` in the
/// item branch.
const FRAMED_ITEM_ROTATION_STEP_DEG: f32 = 45.0;

/// The frame's own rotation: `Rx(xRot) · Ry(yRot)` exactly as
/// `ItemFrameRenderer.submit` pushes it, re-expressed in the `(yaw, pitch)` the
/// wire actually carries.
///
/// # Why `180 - yaw`, and why the pitch passes through unchanged
///
/// The renderer derives its two angles from the frame's `Direction`, which is not
/// on the wire; `ItemFrame.setDirection` derives the entity's own `yRot`/`xRot`
/// from that same `Direction`, and those *are*. Composing the two derivations
/// eliminates it:
///
/// | direction | renderer | entity |
/// |---|---|---|
/// | horizontal | `xRot = 0`, `yRot = 180 - dir.toYRot()` | `xRot = 0`, `yRot = dir.get2DDataValue() * 90` |
/// | vertical | `xRot = -90 * step`, `yRot = 180` | `xRot = -90 * step`, `yRot = 0` |
///
/// `Direction.toYRot()` **is** `get2DDataValue() * 90`, so the horizontal row is
/// `yRot_render = 180 - yRot_entity`; and the vertical row's `yRot_entity` is `0`,
/// so `180 - yRot_entity` is `180` there too. One expression covers both, and the
/// pitch is the entity's own in either case.
///
/// The `180 -` is the half of this that a reader will want to drop, because
/// dropping it still produces a frame flat against a wall — just the *wrong* wall,
/// with its back plate facing the room.
#[must_use]
pub fn item_frame_facing(yaw_deg: f32, pitch_deg: f32) -> Mat4 {
    Mat4::from_rotation_x(pitch_deg.to_radians())
        * Mat4::from_rotation_y((180.0 - yaw_deg).to_radians())
}

/// The unit vector along the frame's `Direction` — the way it faces out of its
/// wall — from the same two angles.
///
/// Derived from [`item_frame_facing`] rather than from a `Direction` table, and
/// the `NEG_Z` is why: the frame's model has its back plate at local `+z` (the
/// `template_item_frame` element spans `z = 15.5..16`), so after the facing
/// rotation local `-z` is *by construction* the direction the frame looks. A
/// separate table would be a second place for the same fact to be wrong, and it
/// would agree with this one at yaw `0` and `180` — the two inputs a test is most
/// likely to pick.
#[must_use]
pub fn item_frame_facing_step(yaw_deg: f32, pitch_deg: f32) -> Vec3 {
    item_frame_facing(yaw_deg, pitch_deg).transform_vector3(Vec3::NEG_Z)
}

/// The frame's own space: origin at the **centre of its attachment block**, `+z`
/// into the wall behind it, in the `(packet_anchor, yaw, pitch)` terms the shell
/// has for a `HangingEntity`.
///
/// ```text
/// T(floor(packet_anchor) + (0.5, 0.5, 0.5)) · Rx(pitch) · Ry(180 - yaw)
/// ```
///
/// Everything `ItemFrameRenderer.submit` draws is posed relative to this: the
/// body at `T(-0.5, -0.5, -0.5)` (block models are corner-origin), the contents
/// at `T(0, 0, 0.4375)`. `ItemFrame.getAddEntityPacket` sends `getPos()` — the
/// integer attachment `BlockPos` — rather than the entity centre created by
/// `recalculateBoundingBox`. The dispatcher offset and the renderer's matching
/// negative offset cancel, then the renderer's `direction * .46875` cancels the
/// entity-centre displacement, leaving exactly this block centre.
#[must_use]
pub fn item_frame_space(packet_anchor: Vec3, yaw_deg: f32, pitch_deg: f32) -> Mat4 {
    Mat4::from_translation(packet_anchor.floor() + Vec3::splat(0.5))
        * item_frame_facing(yaw_deg, pitch_deg)
}

/// Vanilla's item-frame entity bounds after `EntityRenderer.shouldRender`
/// inflates them by half a block, returned as `(min, max)`.
///
/// The entity is not centred on its packet attachment anchor: `ItemFrame`
/// moves its bounding-box centre `0.46875` blocks away from the attachment
/// block centre, gives the wall-normal axis a thickness of `1/16`, and uses a
/// one-block square when it holds a map (`3/4` otherwise). The renderer then
/// inflates that exact box by `0.5`. A symmetric box around the packet anchor
/// misses the inflated room-facing edge and can cull visible contents at a
/// grazing camera angle.
#[must_use]
pub fn item_frame_culling_aabb(
    packet_anchor: Vec3,
    yaw_deg: f32,
    pitch_deg: f32,
    has_map: bool,
) -> (Vec3, Vec3) {
    let facing = item_frame_facing_step(yaw_deg, pitch_deg);
    let centre = packet_anchor.floor() + Vec3::splat(0.5) - facing * 0.46875;
    let face_size = if has_map { 1.0 } else { 0.75 };
    let thin = 0.0625;
    let axis = facing.abs();
    let size = if axis.x >= axis.y && axis.x >= axis.z {
        Vec3::new(thin, face_size, face_size)
    } else if axis.y >= axis.z {
        Vec3::new(face_size, thin, face_size)
    } else {
        Vec3::new(face_size, face_size, thin)
    };
    let half = size * 0.5 + Vec3::splat(0.5);
    (centre - half, centre + half)
}

/// The world placement for the frame **body** — the wooden border and back
/// plate — as a transform over block-local `0.0..=1.0` model quads.
///
/// `ItemFrameRenderer.submit`'s `pushPose(); translate(-0.5, -0.5, -0.5);
/// frameModel.submitWithZOffset(...)`. The `-0.5`s are the block model's
/// corner-origin convention, not a centring fudge — the same pair
/// `falling_block_pose` applies for the same reason.
#[must_use]
pub fn item_frame_body_matrix(packet_anchor: Vec3, yaw_deg: f32, pitch_deg: f32) -> Mat4 {
    item_frame_space(packet_anchor, yaw_deg, pitch_deg) * Mat4::from_translation(Vec3::splat(-0.5))
}

/// How far in front of the frame's plane its contents sit, `invisible` selecting
/// between vanilla's two `translate` calls.
#[must_use]
pub fn item_frame_content_lift(invisible: bool) -> f32 {
    if invisible {
        ITEM_FRAME_INVISIBLE_CONTENT_LIFT
    } else {
        ITEM_FRAME_CONTENT_LIFT
    }
}

/// The world placement for an item hanging in an item frame:
///
/// ```text
/// item_frame_space · T(0, 0, lift) · Rz(rotation · 45°) · S(0.5) · display_matrix(fixed)
/// ```
///
/// `rotation` is `ItemFrame.getRotation()`, `0..8`; `invisible` is
/// `state.isInvisible`, which swaps the lift (see [`item_frame_content_lift`]).
///
/// # The sign trap
///
/// The lift is along the frame's own local `+z`, which after
/// [`item_frame_facing`] points **into** the wall. From the packet's attachment
/// block centre, visible contents therefore land `0.4375` toward its wall face;
/// equivalently, they are `1/16` outside that face. Getting that sign wrong (or
/// dropping `180 - yaw`) sends contents through the attachment block instead.
#[must_use]
pub fn framed_item_matrix(
    packet_anchor: Vec3,
    yaw_deg: f32,
    pitch_deg: f32,
    rotation: u8,
    invisible: bool,
    fixed: &DisplayTransform,
) -> Mat4 {
    item_frame_space(packet_anchor, yaw_deg, pitch_deg)
        * Mat4::from_translation(Vec3::new(0.0, 0.0, item_frame_content_lift(invisible)))
        * Mat4::from_rotation_z(
            (f32::from(rotation % 8) * FRAMED_ITEM_ROTATION_STEP_DEG).to_radians(),
        )
        * Mat4::from_scale(Vec3::splat(FRAMED_ITEM_SCALE))
        * display_matrix(fixed)
}

/// Mesh an ordinary (baked-quad) item hanging in an item frame into a
/// world-space [`ModelMesh`], for the same model-pipeline draw
/// [`dropped_item_mesh`] feeds.
///
/// The rig-shaped items — a chest, a shulker box, a skull — go through
/// [`framed_item_matrix`] and the block-entity pass instead; this is the other
/// 99% of the item registry, and its absence is why a sword in a frame drew
/// nothing while a chest in one drew fine.
///
/// `fixed` is the item's own `display.fixed`, composed on the right for the
/// identical reason [`campfire_item_mesh`] composes there:
/// `ItemFrameRenderer.extractRenderState` resolves the stack in
/// `ItemDisplayContext.FIXED`, and vanilla applies that transform inside
/// `ItemStackRenderState.submit`, after every pose the renderer itself pushes.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn framed_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    fixed: &DisplayTransform,
    packet_anchor: Vec3,
    yaw_deg: f32,
    pitch_deg: f32,
    rotation: u8,
    invisible: bool,
    light: u8,
) -> ModelMesh {
    let pose = framed_item_matrix(packet_anchor, yaw_deg, pitch_deg, rotation, invisible, fixed);
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

// ---------------------------------------------------------------------------
// Experience orbs
// ---------------------------------------------------------------------------
//
// `ExperienceOrbRenderer`, which is one camera-facing quad and nothing else. It
// is **not** an [`entity_models`] rig and never will be, exactly as a dropped
// item is not: `model_for_type("experience_orb")` and
// `entity_texture_candidates("experience_orb")` are both deliberately empty and
// stay that way (see `unknown_entity_type_has_no_model`). The corpus holds cuboid
// part hierarchies; this is a sprite, and it draws through the same
// billboard-with-its-own-sheet shape the mob-fire layer uses.

/// Where the orb sheet lives in the vanilla jar.
///
/// A **standalone 64×64 sheet**, not a slice of the stitched block atlas — which
/// is why the orb pass binds its own group-1 texture rather than riding the model
/// pipeline. `textures/entity/experience/experience_orb.png`, from
/// `ExperienceOrbRenderer.EXPERIENCE_ORB_LOCATION`.
pub const EXPERIENCE_ORB_TEXTURE: &str =
    "assets/minecraft/textures/entity/experience/experience_orb.png";

/// Where the entity ground-shadow sprite lives in the vanilla jar — a
/// standalone radial-gradient decal, not a slice of any atlas, matching
/// [`EXPERIENCE_ORB_TEXTURE`]'s own shape. `textures/misc/shadow.png`, from
/// `ShadowFeatureRenderer`'s own
/// `Identifier.withDefaultNamespace("textures/misc/shadow.png")`.
pub const SHADOW_TEXTURE: &str = "assets/minecraft/textures/misc/shadow.png";

/// How many distinct sprite cells [`experience_orb_icon`] can return, i.e. the
/// number of baked orb quads a renderer needs.
pub const EXPERIENCE_ORB_ICON_COUNT: u32 = 11;

/// The 16-pixel sprite cell edge inside the 64-pixel sheet, so four cells per row.
const ORB_CELL: f32 = 16.0;
/// The sheet edge both cell axes are normalised against.
const ORB_SHEET: f32 = 64.0;
/// Cells per row — `icon % 4` picks the column, `icon / 4` the row.
const ORB_CELLS_PER_ROW: u32 = 4;

/// Vanilla's `ExperienceOrb.getIcon()` — which of the eleven sprite cells an orb
/// worth `value` draws.
///
/// # It is a bucketed ladder, not a linear map, and the buckets are the *orb
/// denominations*
///
/// The thresholds are `2477, 1237, 617, 307, 149, 73, 37, 17, 7, 3` — the same
/// irregular, roughly-doubling ladder `ExperienceOrb.award` splits a payout over,
/// read top-down with `>=`. So the cell is constant *across* a bucket: an orb worth
/// 7, one worth 8 and one worth 16 all draw cell 2, and 17 is the first value that
/// draws cell 3. Any gate that observes only one value, or two values inside one
/// bucket, cannot tell this function from a linear `value / 250` — the pairs that
/// discriminate are the ones straddling a threshold.
///
/// Values below 3 (including `0`, which is what an orb whose `DATA_VALUE` never
/// reached us reads as) draw cell 0. A negative value cannot occur on the wire but
/// falls into the same arm rather than panicking or wrapping.
#[must_use]
pub fn experience_orb_icon(value: i32) -> u32 {
    // Written as the same descending `>=` ladder vanilla uses rather than as a
    // `match` on ranges: a range table has to restate every threshold twice
    // (as one arm's end and the next arm's start) and an off-by-one there is
    // invisible except at exactly the boundary value.
    if value >= 2477 {
        10
    } else if value >= 1237 {
        9
    } else if value >= 617 {
        8
    } else if value >= 307 {
        7
    } else if value >= 149 {
        6
    } else if value >= 73 {
        5
    } else if value >= 37 {
        4
    } else if value >= 17 {
        3
    } else if value >= 7 {
        2
    } else if value >= 3 {
        1
    } else {
        0
    }
}

/// The four `(u, v)` corners of one orb sprite cell, in the bottom-left,
/// bottom-right, top-right, top-left order [`experience_orb_mesh`] winds.
///
/// `ExperienceOrbRenderer.submit`'s own arithmetic: `u0 = (icon % 4 * 16) / 64`,
/// `v0 = (icon / 4 * 16) / 64`, each `+16` for the far edge — and note vanilla
/// pairs the quad's **bottom** vertices with `v1` (the cell's larger v) and its top
/// with `v0`, so the sprite is not flipped. Getting that pair the other way round
/// draws an upside-down orb, which is invisible on a radially symmetric cell and
/// visible on the higher-value ones.
fn experience_orb_cell_uvs(icon: u32) -> [[f32; 2]; 4] {
    let column = (icon % ORB_CELLS_PER_ROW) as f32;
    let row = (icon / ORB_CELLS_PER_ROW) as f32;
    let u0 = column * ORB_CELL / ORB_SHEET;
    let u1 = (column * ORB_CELL + ORB_CELL) / ORB_SHEET;
    let v0 = row * ORB_CELL / ORB_SHEET;
    let v1 = (row * ORB_CELL + ORB_CELL) / ORB_SHEET;
    [[u0, v1], [u1, v1], [u1, v0], [u0, v0]]
}

/// One orb's quad in *local* space, for the sprite cell `icon`, ready to be posed
/// by [`experience_orb_matrix`].
///
/// The corners are vanilla's literally: `x ∈ [-0.5, 0.5]`, `y ∈ [-0.25, 0.75]`,
/// `z = 0`. The y range is **not** centred on zero — vanilla's four `vertex` calls
/// are `(-0.5, -0.25)`, `(0.5, -0.25)`, `(0.5, 0.75)`, `(-0.5, 0.75)` — so the
/// quad sits three-quarters above its own origin and, after the `0.3` scale and the
/// `+0.1` lift in [`experience_orb_matrix`], spans `y ∈ [0.025, 0.325]` above the
/// orb's feet. Centring it would sink half the sprite into the floor.
///
/// The vertex `light`/`tint`/`anim` lanes are inert defaults exactly as
/// [`crate::entity_pipeline::flame_mesh`]'s are: the orb pass carries its light and
/// its colour **per instance**, because both change per orb and per tick.
#[must_use]
pub fn experience_orb_mesh(icon: u32) -> (Vec<ModelVertex>, Vec<u32>) {
    const CORNERS: [[f32; 2]; 4] = [[-0.5, -0.25], [0.5, -0.25], [0.5, 0.75], [-0.5, 0.75]];
    let uvs = experience_orb_cell_uvs(icon);
    let vertices = CORNERS
        .iter()
        .zip(uvs)
        .map(|([x, y], uv)| ModelVertex {
            position: [*x, *y, 0.0],
            uv,
            ao: 1.0,
            light: 0,
            tint: 255,
            anim: 0,
            cutout_bypass: 0,
            tint_rgb_override: [0, 0, 0, 0],
        })
        .collect();
    // The same two-triangle winding every other baked quad in this crate uses.
    (vertices, vec![0, 1, 2, 0, 2, 3])
}

/// The world placement for one orb, matching `ExperienceOrbRenderer.submit`'s
/// pose-stack order:
///
/// ```text
/// T(feet) · T(0, 0.1, 0) · camera_orientation · S(0.3)
/// ```
///
/// `orientation` is [`camera_orientation`]`(camera.view_matrix())` — the same one
/// matrix every orb this frame shares, since a billboard's rotation depends only
/// on the camera. The `0.1` lift is applied in **world** space, before the
/// orientation, so it is straight up whatever way the camera is looking; folding it
/// into the local quad instead would tilt it with the view.
///
/// Determinant is positive (a translation, a rotation and a positive uniform
/// scale), so this composes to terrain's winding — and `EntityPipeline` is
/// `cull_mode: None` regardless, so a sign error here would show as wrong depth
/// order rather than as a vanished quad.
#[must_use]
pub fn experience_orb_matrix(feet: Vec3, orientation: Mat4) -> Mat4 {
    /// `poseStack.scale(0.3F, 0.3F, 0.3F)`.
    const ORB_SCALE: f32 = 0.3;
    /// `poseStack.translate(0.0F, 0.1F, 0.0F)`.
    const ORB_LIFT: f32 = 0.1;
    Mat4::from_translation(feet + Vec3::new(0.0, ORB_LIFT, 0.0))
        * orientation
        * Mat4::from_scale(Vec3::splat(ORB_SCALE))
}

/// Vanilla's pulsing orb colour, as the gamma-space `[r, g, b]` bytes an
/// `InstanceTint` carries.
///
/// `ExperienceOrbRenderer.submit`, verbatim, with `rr = ageInTicks / 2`:
///
/// ```text
/// r = (sin(rr) + 1) * 0.5 * 255
/// g = 255
/// b = (sin(rr + 4π/3) + 1) * 0.1 * 255
/// ```
///
/// The two amplitudes differ — `0.5` for red, `0.1` for blue — and the phase
/// offset is `4π/3`, not `2π/3`, so the orb cycles green→yellow→green rather than
/// through a full hue wheel. Green is pinned at full and never modulates.
///
/// These are **gamma-space** bytes multiplied into a gamma-encoded texel, which is
/// where `entity.wgsl` applies an `InstanceTint`; vanilla is not colour-managed and
/// converting them to linear first would pull the whole cycle toward white.
#[must_use]
pub fn experience_orb_tint(age_ticks: f32) -> [u8; 3] {
    let phase = age_ticks / 2.0;
    let channel = |value: f32| -> u8 {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped into 0..=255 first, and vanilla truncates too"
        )]
        {
            (value.clamp(0.0, 255.0)) as u8
        }
    };
    let red = (phase.sin() + 1.0) * 0.5 * 255.0;
    let blue = ((phase + std::f32::consts::PI * 4.0 / 3.0).sin() + 1.0) * 0.1 * 255.0;
    [channel(red), 255, channel(blue)]
}

/// An orb's packed light, from the sample at its own position.
///
/// `ExperienceOrbRenderer.getBlockLightLevel` is
/// `clamp(super.getBlockLightLevel(..) + 7, 0, 15)` — a **+7 boost to the block
/// nibble only**, which is what keeps an orb readable on a cave floor. The sky
/// nibble is passed through untouched; boosting both would make an orb in a lit
/// room brighter than the room.
#[must_use]
pub fn experience_orb_light(packed: u8) -> u8 {
    let sky = packed & 0xF0;
    let block = ((packed & 0x0F) + 7).min(15);
    sky | block
}

// ---------------------------------------------------------------------------
// Held items, and the first-person arm
// ---------------------------------------------------------------------------
//
// Both are *item/part geometry hung off an arm*, and both are transcribed from
// the 26.2 client rather than tuned by eye. The two chains are deliberately kept
// separate (`held_item_matrix` vs `first_person_arm_pose`) because vanilla's are:
// one hangs off the third-person part hierarchy, the other replaces it entirely.

/// Which arm of a humanoid rig something is attached to — vanilla's
/// `HumanoidArm`.
///
/// A mob's `getMainArm()` is `RIGHT` for every `Mob` (only a `Player` can be
/// left-handed), so the wire's `MainHand` maps to [`Arm::Right`] and `OffHand`
/// to [`Arm::Left`]. That mapping belongs to the caller, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arm {
    /// The right arm — a mob's main hand.
    Right,
    /// The left arm — a mob's off hand.
    Left,
}

impl Arm {
    /// The `entity_models` part name for this arm.
    #[must_use]
    pub const fn part_name(self) -> &'static str {
        match self {
            Arm::Right => "right_arm",
            Arm::Left => "left_arm",
        }
    }

    /// The overlay ("sleeve") part parented to this arm at `PartPose::ZERO`, for
    /// the models that have one (the two player rigs). It shares the arm's matrix
    /// exactly — see [`first_person_arm_pose`].
    #[must_use]
    pub const fn sleeve_part_name(self) -> &'static str {
        match self {
            Arm::Right => "right_sleeve",
            Arm::Left => "left_sleeve",
        }
    }

    /// Whether this is a left-hand context, i.e. whether
    /// [`display_matrix_for_hand`]'s mirror applies.
    #[must_use]
    pub const fn is_left(self) -> bool {
        matches!(self, Arm::Left)
    }

    /// Vanilla's `invert`/`isLeftHand ? -1 : 1` sign, used for every mirrored
    /// term in both chains below.
    #[must_use]
    pub const fn invert(self) -> f32 {
        match self {
            Arm::Right => 1.0,
            Arm::Left => -1.0,
        }
    }

    /// The `display` slot an item held in this arm is posed by.
    #[must_use]
    pub const fn display_slot(self, first_person: bool) -> DisplaySlot {
        match (self, first_person) {
            (Arm::Right, false) => DisplaySlot::ThirdPersonRightHand,
            (Arm::Left, false) => DisplaySlot::ThirdPersonLeftHand,
            (Arm::Right, true) => DisplaySlot::FirstPersonRightHand,
            (Arm::Left, true) => DisplaySlot::FirstPersonLeftHand,
        }
    }
}

/// `ItemInHandLayer.submitArmWithItem`'s adult hand offset, in model texels
/// (`offsetX`, `offsetY`, `offsetZ`). `x` is mirrored by [`Arm::invert`].
///
/// Read from 26.2's
/// `client/renderer/entity/layers/ItemInHandLayer.java:45-48`, where the three
/// values are `1.0F`, `2.0F` and `-10.0F` and the translate is
/// `((isLeftHand ? -1 : 1) * offsetX / 16, offsetY / 16, offsetZ / 16)`.
pub const HELD_ITEM_OFFSET_TEXELS: [f32; 3] = [1.0, 2.0, -10.0];

/// The same offsets for a **baby** (`useBabyOffset`): `0.0`, `1.0`, `-4.5`.
///
/// Vanilla's predicate is `state.isBaby && state.entityType != ARMOR_STAND`; an
/// armour stand is never a baby in the shell's data, so the caller's
/// "is this mob drawn small?" test is sufficient.
pub const HELD_ITEM_BABY_OFFSET_TEXELS: [f32; 3] = [0.0, 1.0, -4.5];

/// The `display` transform to pose an item held in `arm` under.
///
/// Uses [`DisplayTransforms::get`] rather than `declared`, because unlike
/// `ground` there is **no** sensible fallback constant for a hand slot:
/// `block/block` and `item/generated` disagree on far more than scale, so an
/// undeclared hand slot should get vanilla's own answer — the identity
/// (`ItemTransform.NO_TRANSFORM`, which is only the `-0.5` centring) — and not a
/// guess. `get` also applies
/// [`DisplaySlot::left_hand_fallback`](lodestone_assets::DisplaySlot::left_hand_fallback),
/// which matters in practice: neither `block/block` nor `item/generated` declares
/// `thirdperson_lefthand`.
#[must_use]
pub fn hand_transform(
    display: &DisplayTransforms,
    arm: Arm,
    first_person: bool,
) -> DisplayTransform {
    display.get(arm.display_slot(first_person))
}

/// The world placement matrix for an item held in a mob's hand, matching
/// `ItemInHandLayer.submitArmWithItem`'s pose-stack order exactly:
///
/// ```text
/// part_transforms[arm] · Rx(-90°) · Ry(180°) · T(±ox/16, oy/16, oz/16)
///                      · display_matrix_for_hand(thirdperson_?hand, is_left)
/// ```
///
/// `arm_transform` is vanilla's `translateToHand(arm)` result, an
/// **entity→world** matrix: [`EntityInstance::hand_transform`]`(arm)` — *not*
/// `part_transforms[skeleton.index_of(arm.part_name())]`, which is the same
/// value only for the models with no override (see the table below and
/// [`HandPoseOverride`](crate::entity_anim::HandPoseOverride)).
///
/// # Verified against source, and the three offsets are not the whole story
///
/// Read from the 26.2 decompile, not transcribed from a summary. Two things the
/// short form hides:
///
/// * The item's own `display` transform is **not** applied by the layer — it
///   happens one level down, inside `ItemStackRenderState.LayerRenderState.submit`
///   → `applyTransform` → `itemTransform.apply(displayContext.leftHand(), pose)`.
///   That is why the left-hand mirror lives in [`display_matrix_for_hand`] and is
///   applied here even when the transform came from the right-hand fallback:
///   `ItemDisplayContext.leftHand()` is a property of the *context*, not of where
///   the numbers came from.
/// * `submitArmWithItem` has two further pose steps this does not model, both
///   gated on state the shell does not track: `SpearAnimations.thirdPersonAttackItem`
///   (a `STAB` swing mid-attack) and `ArmPose.animateUseItem` (`ticksUsingItem != 0`,
///   i.e. drawing a bow, eating, blocking with a shield). Both are the identity in
///   the resting case this renders.
///
/// # How to change it: the per-model `translateToHand` overrides
///
/// For most models `arm_transform` is `HumanoidModel.translateToHand`, which
/// `IllagerModel` and `ArmorStandModel` use too, and — because the composed
/// part matrix already carries the *whole* parent chain — also covers models
/// whose arms hang off `body` rather than `root` (`CopperGolemModel` spells out
/// `root · body · arm`). Five corpus models in 26.2 append or prepend more, and
/// [`Skeleton::translate_to_hand`](crate::entity_anim::Skeleton::translate_to_hand)
/// now models every one of them, selected per model name by
/// [`hand_pose_override_for`]:
///
/// | model | override |
/// |---|---|
/// | `skeleton`, `stray`, `wither_skeleton` | pivot `x += ±1` texel *before* the arm's own matrix |
/// | `player_slim` | pivot `x += ±0.5` texel, same position |
/// | `vex` | then `scale(0.55)`, then `translate(±0.046875, -0.15625, 0.078125)` |
/// | `allay` | a different chain entirely: `root · body`, then `T(0, 1/16, 3/16) · Rx(right_arm.xRot) · S(0.7) · T(1/16, 0, 0)` — the arm's matrix is never used |
/// | `copper_golem` | not in the corpus |
///
/// The two *pivot-shift* rows cannot be expressed as a pre- or post-multiplication
/// of the arm's already-composed matrix, because the shift goes between the
/// parent chain and the arm's own rotation, which that matrix has already
/// folded together. That is why the fix lives in `entity_anim`
/// ([`Skeleton::translate_to_hand`](crate::entity_anim::Skeleton::translate_to_hand)),
/// operating on the posed-but-not-yet-composed parts, rather than as a
/// correction applied to `arm_transform` here.
///
/// **Not yet wired to a live server.** `lodestone-shell`'s `merge_held_items`
/// (`crates/lodestone-shell/src/gpu.rs`) still builds `arm_transform` by
/// indexing `instance.part_transforms[skeleton.index_of(arm.part_name())]`
/// directly, which is exactly [`EntityInstance::hand_transform`]'s
/// [`HandPoseOverride::Structural`](crate::entity_anim::HandPoseOverride::Structural)
/// case and therefore still correct for every model but these five. Swapping
/// that one lookup for `instance.hand_transform(arm)` is the remaining step —
/// deliberately left undone here because this file's remit was
/// `lodestone-render` only.
#[must_use]
pub fn held_item_matrix(
    arm_transform: Mat4,
    arm: Arm,
    baby: bool,
    transform: &DisplayTransform,
) -> Mat4 {
    let [ox, oy, oz] = if baby {
        HELD_ITEM_BABY_OFFSET_TEXELS
    } else {
        HELD_ITEM_OFFSET_TEXELS
    };
    arm_transform
        * Mat4::from_rotation_x((-90.0f32).to_radians())
        * Mat4::from_rotation_y(180.0f32.to_radians())
        * Mat4::from_translation(Vec3::new(
            arm.invert() * ox / UNITS_PER_BLOCK,
            oy / UNITS_PER_BLOCK,
            oz / UNITS_PER_BLOCK,
        ))
        * display_matrix_for_hand(transform, arm.is_left())
}

/// Mesh one held item's baked geometry into a world-space [`ModelMesh`], ready
/// for the ordinary [`ModelPipeline`](crate::ModelPipeline) with a *world* camera
/// uniform — the same treatment [`dropped_item_mesh`] gives a drop, and for the
/// same reason (the pose is folded into vertex positions, so there is no
/// per-instance matrix to batch on).
///
/// `light` is the holder's own packed sky/block sample: the geometry comes from
/// [`mesh_item_quads`], which nails every vertex to
/// [`GUI_ITEM_LIGHT`](crate::GUI_ITEM_LIGHT) because an inventory slot is
/// full-bright by definition, and a sword in a zombie's hand in a cave is not.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn held_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    arm_transform: Mat4,
    arm: Arm,
    baby: bool,
    transform: &DisplayTransform,
    light: u8,
) -> ModelMesh {
    let pose = held_item_matrix(arm_transform, arm, baby, transform);
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

/// The arm's forced `zRot` in `AvatarRenderer.renderHand`, in **radians**
/// (`model.rightArm.zRot = 0.1F`, `model.leftArm.zRot = -0.1F`). Mirrored by
/// [`Arm::invert`].
pub const FIRST_PERSON_ARM_Z_ROT: f32 = 0.1;

/// `renderPlayerArm`'s `inverseArmHeight` coefficient on `y`
/// (`ARM_HEIGHT_SCALE = -0.6F`, applied at `ItemInHandRenderer.java:270`).
///
/// Numerically equal to [`FIRST_PERSON_ITEM_EQUIP_DIP`], and deliberately a
/// separate constant: the two live in different vanilla methods over different base
/// offsets (`-0.6` for the arm, `-0.52` for the item), so the equality is a
/// coincidence of 26.2's numbers rather than a shared rule.
pub const FIRST_PERSON_ARM_EQUIP_DIP: f32 = -0.6;

/// Vertical FOV the first-person arm is projected with, in degrees.
///
/// **Not the player's FOV.** `GameRenderer.renderLevel` sets a *separate*
/// projection for the hand — `hudProjection.setupPerspective(0.05F, 100.0F,
/// cameraState.hudFov, w, h)` — and `Camera.calculateHudFov` is a hard-coded
/// `70.0F` passed through `modifyFovBasedOnDeathOrFluid`. So the arm keeps a
/// constant apparent size while the world FOV changes (sprinting, the FOV
/// slider), which is exactly the behaviour players expect and would be lost by
/// reusing `Camera::projection_matrix`.
pub const HAND_FOV_Y_DEGREES: f32 = 70.0;

/// Near plane for [`hand_projection`] (vanilla's `0.05F`).
pub const HAND_NEAR: f32 = 0.05;

/// Far plane for [`hand_projection`] (vanilla's `100.0F` — *not* the world's
/// render-distance-derived far plane).
pub const HAND_FAR: f32 = 100.0;

/// The projection the first-person arm is drawn with: vanilla's `hudProjection`.
///
/// This is the **whole** transform for the hand pass. `GameRenderer.renderItemInHand`
/// does `poseStack.mulPose(modelViewMatrix.invert())` while pushing
/// `modelViewStack.mul(modelViewMatrix)`, and the shader multiplies
/// `Proj · ModelViewStack · PoseStack` — so the view rotation cancels exactly and
/// the arm pose is already in **camera space**. `modelViewMatrix` there is
/// `cameraState.viewRotationMatrix`, rotation-only, which is why nothing has to
/// undo a camera translation either.
///
/// A view matrix is orthonormal-plus-translation, so `det(view) = +1` and
/// `sign(det(hand_projection)) == sign(det(Camera::view_projection))` — which
/// is why this is built from [`Camera`](crate::Camera) rather than assembled
/// separately. The arm pose must therefore have a **positive** determinant,
/// exactly like a world model matrix and unlike the GUI item pose — see
/// `first_person_arm_pose_preserves_winding`.
#[must_use]
pub fn hand_projection(aspect: f32) -> Mat4 {
    // Built through `Camera::projection_matrix` itself rather than through an
    // equivalent constructor, so the two cannot disagree about depth range or
    // handedness. That is not hypothetical: this function used to call glam's
    // *forward* `directx::perspective` directly, and when the world projection
    // became reversed-Z the hand pass was left projecting the other way — near
    // at 0 against a depth buffer cleared to 0 and compared with "nearer is
    // greater", which discards the entire arm. Only the position and the two
    // angles are unused here; every field the projection reads is set below.
    crate::Camera {
        fov_y_degrees: HAND_FOV_Y_DEGREES,
        aspect: if aspect.is_finite() && aspect > 0.0 {
            aspect
        } else {
            1.0
        },
        near: HAND_NEAR,
        far: HAND_FAR,
        ..crate::Camera::default()
    }
    .projection_matrix()
}

/// The camera-space chain `ItemInHandRenderer.renderPlayerArm` builds, driven by
/// `attack_anim`.
///
/// `attack_anim` is vanilla's `attackValue` — `Player.getAttackAnim(partialTick)`,
/// i.e. swing progress in `0.0..=1.0`, interpolated from the **tick** clock
/// (`lodestone_entity::pose::EntityPose::attack_anim_lerp`). `0.0` is a fully
/// rested arm and reproduces this function's behaviour before the swing existed,
/// byte for byte, which is what `arm_chain_at_rest_matches_the_static_chain`
/// pins. Values outside the range are clamped rather than extrapolated: the
/// shaping functions below are periodic, so an out-of-range value does not fail,
/// it silently animates something else.
///
/// ```text
/// s  = sqrt(a)                     -- `Mth.sqrt(attackValue)`
/// xs = -0.3 · sin(s·π)
/// ys =  0.4 · sin(s·2π)
/// zs = -0.4 · sin(a·π)
/// yr =  sin(s·π)                   -- `ySwingRotation`
/// zr =  sin(a²·π)                  -- `zSwingRotation`
///
/// T(i·(xs + 0.64000005), ys - 0.6, zs - 0.71999997)
///   · Ry(i·45°) · Ry(i·yr·70°) · Rz(i·zr·-20°)
///   · T(i·-1, 3.6, 3.5) · Rz(i·120°) · Rx(200°) · Ry(i·-135°) · T(i·5.6, 0, 0)
/// ```
///
/// with `i` = [`Arm::invert`].
///
/// # The `sqrt` is the shape of the animation, not a detail
///
/// Three of the five terms are driven by `sqrt(a)` and one by `a²`, and only
/// `zSwingPosition` is linear in `a`. `sin(sqrt(a)·π)` rises far faster than
/// `sin(a·π)` and decays slowly — the arm snaps out and eases back, which is what
/// a swing *reads* as. Substituting a linear ramp gives a symmetric, sluggish
/// pendulum that is visibly not Minecraft, so this is transcribed term by term
/// from `ItemInHandRenderer.renderPlayerArm` in
/// `.cache/mc/26.2/client-src` rather than eyeballed.
///
/// Note `ySwingPosition` uses `2π`, not `π`: over one swing the arm's vertical
/// offset goes up, back through zero, and down again, rather than making a single
/// hump like `x` and `z`.
///
/// The dropped terms and why:
///
/// * `submitHandsWithItems` prefixes `Rx((viewXRot - xBob) · 0.1°)` and
///   `Ry((viewYRot - yBob) · 0.1°)`, and `renderItemInHand` prefixes `bobHurt` and
///   `bobView`. All four need state the shell does not have (`xBob`/`yBob`, hurt
///   time, walk distance); all four are the identity when standing still.
/// * `applyItemArmAttackTransform` — the *item*-in-hand swing (`45° + yr·-20°`,
///   `zr'·-20°`, `xzr·-80°`) — is a **different** chain for the case where the
///   main hand is not empty and vanilla draws the item instead of the arm. It is
///   not this one and must not be folded in; see
///   `RenderState::prepare_first_person_hand`'s `FirstPersonHand::Item` branch,
///   which is the *other* half of vanilla's `isEmpty()` fork — see
///   [`first_person_item_chain`].
///
/// There is no `scale` anywhere in the chain, and that is not an omission — the
/// large constants (`3.6`, `3.5`, `5.6`) are in blocks and largely cancel through
/// the three rotations. At rest the composed arm cube lands roughly `0.35..0.9`
/// blocks right, `0.29..0.99` down and `0.44..1.19` forward of the eye, i.e.
/// bottom-right of frame, which is what
/// `the_first_person_arm_lands_in_the_bottom_right_of_frame` pins.
#[must_use]
pub fn first_person_arm_chain(arm: Arm, attack_anim: f32) -> Mat4 {
    first_person_arm_chain_with_equip(arm, attack_anim, 0.0)
}

/// [`first_person_arm_chain`] with vanilla's `inverseArmHeight` — the equip/swap
/// dip (issue #366).
///
/// `renderPlayerArm` (`ItemInHandRenderer.java:270`) translates `y` by
/// `ySwingPosition + -0.6F + inverseArmHeight * -0.6F`, so the dip coefficient is
/// [`FIRST_PERSON_ARM_EQUIP_DIP`] and it is **the same `-0.6`** the item chain uses
/// ([`FIRST_PERSON_ITEM_EQUIP_DIP`]) even though the two chains' *base* offsets
/// differ (`-0.6` here against the item's `-0.52`). Two constants rather than one
/// shared alias, because the equality is a coincidence of vanilla's numbers and
/// not a rule: they sit in different methods and either could move.
///
/// `inverse_arm_height` runs `0.0` (fully equipped, at rest) to `1.0` (fully
/// lowered, mid-swap). Passing a value outside that range is not clamped here —
/// the caller owns the ramp, and clamping in the matrix would hide a broken one.
///
/// [`first_person_arm_chain`] is this function at `0.0` and is kept as the name
/// every existing caller and gate uses, so adding the dip changed no call site's
/// behaviour and no test's expected matrix.
#[must_use]
pub fn first_person_arm_chain_with_equip(
    arm: Arm,
    attack_anim: f32,
    inverse_arm_height: f32,
) -> Mat4 {
    let i = arm.invert();
    let ArmSwingTerms {
        x_position,
        y_position,
        z_position,
        y_rotation,
        z_rotation,
    } = ArmSwingTerms::new(attack_anim);
    Mat4::from_translation(Vec3::new(
        i * (x_position + 0.640_000_05),
        y_position - 0.6 + inverse_arm_height * FIRST_PERSON_ARM_EQUIP_DIP,
        z_position - 0.719_999_97,
    )) * Mat4::from_rotation_y((i * 45.0).to_radians())
        * Mat4::from_rotation_y((i * y_rotation * 70.0).to_radians())
        * Mat4::from_rotation_z((i * z_rotation * -20.0).to_radians())
        * Mat4::from_translation(Vec3::new(i * -1.0, 3.6, 3.5))
        * Mat4::from_rotation_z((i * 120.0).to_radians())
        * Mat4::from_rotation_x(200.0f32.to_radians())
        * Mat4::from_rotation_y((i * -135.0).to_radians())
        * Mat4::from_translation(Vec3::new(i * 5.6, 0.0, 0.0))
}

/// The five scalars `renderPlayerArm` derives from `attackValue`, split out from
/// [`first_person_arm_chain`] so the *shaping* can be asserted against
/// hand-evaluated vanilla values on its own. Buried inside the matrix product,
/// swapping a `sqrt(a)` for an `a` is invisible: the matrix still moves, still has
/// determinant +1, and still keeps the arm on screen — it just animates wrong.
///
/// Every field is `0.0` at `attack_anim == 0.0`, which is what makes the swing
/// purely additive on top of the rest chain.
struct ArmSwingTerms {
    /// `xSwingPosition`, pre-`invert`: `-0.3 · sin(sqrt(a)·π)`.
    x_position: f32,
    /// `ySwingPosition`: `0.4 · sin(sqrt(a)·2π)` — note the `2π`.
    y_position: f32,
    /// `zSwingPosition`: `-0.4 · sin(a·π)`, the one linear-in-`a` term.
    z_position: f32,
    /// `ySwingRotation`: `sin(sqrt(a)·π)`, scaled by `70°` at the call site.
    y_rotation: f32,
    /// `zSwingRotation`: `sin(a²·π)`, scaled by `-20°` at the call site.
    z_rotation: f32,
}

impl ArmSwingTerms {
    /// `attack_anim` outside `0.0..=1.0` is clamped — see
    /// [`first_person_arm_chain`] on why extrapolating a periodic shaping
    /// function is worse than clamping it.
    fn new(attack_anim: f32) -> Self {
        use std::f32::consts::{PI, TAU};
        let a = attack_anim.clamp(0.0, 1.0);
        let s = a.sqrt();
        Self {
            x_position: -0.3 * (s * PI).sin(),
            y_position: 0.4 * (s * TAU).sin(),
            z_position: -0.4 * (a * PI).sin(),
            y_rotation: (s * PI).sin(),
            z_rotation: (a * a * PI).sin(),
        }
    }
}

/// The camera-space matrix to draw the first-person arm (and its sleeve) with, or
/// `None` if `mesh` has no such arm part.
///
/// ```text
/// first_person_arm_chain(arm, attack_anim) · rest_pose()[arm] · Rz(±0.1)
/// ```
///
/// `AvatarRenderer.renderHand` calls `arm.resetPose()` and then forces
/// `zRot = ±0.1F`, so the arm part itself is drawn from its **authored rest pose**
/// with one rotation replaced — never from the third-person `setupAnim` result.
/// That is why this is a separate function from [`EntityInstance::part_transforms`]
/// and must stay one: the third-person player body needs the animated chain
/// (`HumanoidModel.setupAttackAnimation`, which is
/// [`crate::entity_anim::Skeleton::pose`]'s `attack_anim`), and sharing a code
/// path would silently give one of the two the other's pose.
///
/// **The swing lives in the chain, not in the part pose**, and that is the whole
/// reason both can be animated by the same `attack_anim` number without sharing
/// any code: first person swings the *camera-space chain* the rested arm hangs
/// off, third person swings the *arm part* inside a rested body. Feeding this
/// function's `attack_anim` to `Skeleton::pose`, or vice versa, produces a
/// plausible-looking wrong answer, so the two paths take the same scalar and
/// nothing else.
///
/// `rest_pose()[arm] · Rz(0.1)` is *exact* rather than approximate because
/// `player_wide`'s `right_arm` is `PartPose::offset(-5, 2, 0)` with **zero** rest
/// rotation and hangs directly off an identity root — asserted by
/// `the_player_arm_rest_pose_is_a_pure_translation`, not commented.
///
/// `right_sleeve` is a child of `right_arm` at `PartPose::ZERO`, so it shares this
/// matrix exactly; [`first_person_arm_parts`] returns both indices for one matrix.
#[must_use]
pub fn first_person_arm_pose(mesh: &EntityMesh, arm: Arm, attack_anim: f32) -> Option<Mat4> {
    first_person_arm_pose_with_equip(mesh, arm, attack_anim, 0.0)
}

/// [`first_person_arm_pose`] with vanilla's `inverseArmHeight` equip dip — see
/// [`first_person_arm_chain_with_equip`] (issue #366).
#[must_use]
pub fn first_person_arm_pose_with_equip(
    mesh: &EntityMesh,
    arm: Arm,
    attack_anim: f32,
    inverse_arm_height: f32,
) -> Option<Mat4> {
    let index = mesh.skeleton.index_of(arm.part_name())?;
    let rest = mesh.skeleton.rest_pose();
    let local = *rest.get(index)?;
    Some(
        first_person_arm_chain_with_equip(arm, attack_anim, inverse_arm_height)
            * local
            * Mat4::from_rotation_z(arm.invert() * FIRST_PERSON_ARM_Z_ROT),
    )
}

/// The mesh part indices [`first_person_arm_pose`]'s matrix draws: the arm, and
/// its sleeve overlay when the model has one.
///
/// Empty when the model has no such arm, so a caller can treat "no first-person
/// arm for this rig" as "draw nothing" without a second lookup.
#[must_use]
pub fn first_person_arm_parts(mesh: &EntityMesh, arm: Arm) -> Vec<usize> {
    let Some(index) = mesh.skeleton.index_of(arm.part_name()) else {
        return Vec::new();
    };
    let mut parts = vec![index];
    if let Some(sleeve) = mesh.skeleton.index_of(arm.sleeve_part_name()) {
        parts.push(sleeve);
    }
    parts
}

// ---------------------------------------------------------------------------
// The item in the first-person hand
// ---------------------------------------------------------------------------
//
// Vanilla draws the arm **or** the item, never both: `submitArmWithItem` branches
// on `itemStack.isEmpty()` and calls `renderPlayerArm` only in the empty case.
// So this is not a layer on top of `first_person_arm_chain` — it is the *other*
// branch, with its own translation and its own swing shaping, and folding one into
// the other produces a plausible-looking wrong pose. The two share only the
// `attackValue` scalar.

/// `ItemInHandRenderer.applyItemArmTransform`'s translation, in blocks
/// (`invert * 0.56F`, `-0.52F`, `-0.72F`). `x` is mirrored by [`Arm::invert`] and
/// `y` additionally takes `inverseArmHeight * -0.6F`.
///
/// Note these are **not** [`first_person_arm_chain`]'s `0.64000005 / -0.6 /
/// -0.71999997`. The two chains are 0.08 blocks apart in `x`, which is small
/// enough to look like a rounding difference and is in fact the difference between
/// an item held in view and one clipping the frame edge.
pub const FIRST_PERSON_ITEM_OFFSET: [f32; 3] = [0.56, -0.52, -0.72];

/// `applyItemArmTransform`'s `inverseArmHeight` coefficient on `y` (`-0.6F`).
pub const FIRST_PERSON_ITEM_EQUIP_DIP: f32 = -0.6;

/// The three scalars `ItemInHandRenderer.swingArm` derives from `attackValue`.
///
/// **Different coefficients from [`ArmSwingTerms`]** (`-0.4 / 0.2 / -0.2` against
/// the arm's `-0.3 / 0.4 / -0.4`) and no rotation terms of its own — the rotation
/// comes from [`first_person_item_attack_chain`]. Kept as its own type so the two
/// cannot be swapped by autocomplete.
struct ItemSwingTerms {
    /// `xSwingPosition`, pre-`invert`: `-0.4 · sin(sqrt(a)·π)`.
    x_position: f32,
    /// `ySwingPosition`: `0.2 · sin(sqrt(a)·2π)` — the `2π`, as in the arm chain.
    y_position: f32,
    /// `zSwingPosition`: `-0.2 · sin(a·π)`.
    z_position: f32,
}

impl ItemSwingTerms {
    fn new(attack_anim: f32) -> Self {
        use std::f32::consts::{PI, TAU};
        let a = attack_anim.clamp(0.0, 1.0);
        let s = a.sqrt();
        Self {
            x_position: -0.4 * (s * PI).sin(),
            y_position: 0.2 * (s * TAU).sin(),
            z_position: -0.2 * (a * PI).sin(),
        }
    }
}

/// `ItemInHandRenderer.applyItemArmAttackTransform`:
///
/// ```text
/// Ry(i·(45 + yr·-20)) · Rz(i·xzr·-20) · Rx(xzr·-80) · Ry(i·-45)
/// ```
///
/// with `yr = sin(a²·π)`, `xzr = sin(sqrt(a)·π)` and `i` = [`Arm::invert`].
///
/// **This is the identity at `attack_anim == 0.0`** — both shaping terms vanish and
/// the leading `Ry(i·45)` is cancelled exactly by the trailing `Ry(i·-45)`. That is
/// what makes the resting pose independent of the swing, and it is the property to
/// check first if a held item sits at a strange angle while standing still: a
/// dropped `Ry(i·-45)` looks like a permanent 45° twist, not like a broken swing.
#[must_use]
pub fn first_person_item_attack_chain(arm: Arm, attack_anim: f32) -> Mat4 {
    use std::f32::consts::PI;
    let i = arm.invert();
    let a = attack_anim.clamp(0.0, 1.0);
    let y_rotation = (a * a * PI).sin();
    let xz_rotation = (a.sqrt() * PI).sin();
    Mat4::from_rotation_y((i * (45.0 + y_rotation * -20.0)).to_radians())
        * Mat4::from_rotation_z((i * xz_rotation * -20.0).to_radians())
        * Mat4::from_rotation_x((xz_rotation * -80.0).to_radians())
        * Mat4::from_rotation_y((i * -45.0).to_radians())
}

/// The camera-space chain an item in the first-person hand is posed by, matching
/// `submitArmWithItem`'s generic (`SwingAnimation.Type.WHACK`) branch:
///
/// ```text
/// T(i·0.56, -0.52 + h·-0.6, -0.72)          -- applyItemArmTransform
///   · T(i·xs, ys, zs) · applyItemArmAttackTransform(arm, a)   -- swingArm
/// ```
///
/// `inverse_arm_height` is vanilla's `inverseArmHeight` — the equip/swap dip,
/// `swapAnimationScale(item) · (1 - lerp(oHeight, height))`. Pass `0.0` for a
/// fully-equipped hand; the shell tracks neither height, the same gap
/// [`first_person_arm_chain`] documents.
///
/// # The three swing animation types, and why `WHACK` is the one modelled
///
/// 26.2 branches on `itemStack.getSwingAnimation().type()`: `WHACK` runs
/// `swingArm`, `STAB` runs `SpearAnimations.firstPersonAttack`, and `NONE` runs
/// nothing. At `attack_anim == 0.0` **all three are the identity**
/// ([`first_person_item_attack_chain`] cancels and the translations vanish), so a
/// resting hand is correct for every item whatever its type. Mid-swing, a spear
/// (`STAB`) and the handful of `NONE` items get `WHACK`'s motion here, which is
/// wrong but is a wrong *animation*, not a wrong resting pose — and it needs the
/// item's `SwingAnimation` component, which the item pipeline does not decode.
///
/// The determinant is **positive** (translations and rotations only), matching
/// [`hand_projection`]'s requirement — see `first_person_arm_pose_preserves_winding`
/// for why the hand pass takes the world rule and not the GUI one.
#[must_use]
pub fn first_person_item_chain(arm: Arm, attack_anim: f32, inverse_arm_height: f32) -> Mat4 {
    let i = arm.invert();
    let [ox, oy, oz] = FIRST_PERSON_ITEM_OFFSET;
    let ItemSwingTerms {
        x_position,
        y_position,
        z_position,
    } = ItemSwingTerms::new(attack_anim);
    Mat4::from_translation(Vec3::new(
        i * ox,
        oy + inverse_arm_height * FIRST_PERSON_ITEM_EQUIP_DIP,
        oz,
    )) * Mat4::from_translation(Vec3::new(i * x_position, y_position, z_position))
        * first_person_item_attack_chain(arm, attack_anim)
}

/// The full camera-space pose for an item in the first-person hand:
/// [`first_person_item_chain`] followed by the item's own
/// `firstperson_?hand` display transform.
///
/// `transform` is [`hand_transform`]`(&geometry.display, arm, true)` — note the
/// `true`. Passing `false` there is the silent failure mode: it reads
/// `thirdperson_righthand` instead, which for `item/generated` is a *different*
/// rotation and scale and puts the item at a visibly wrong angle without ever
/// putting it off screen.
#[must_use]
pub fn first_person_item_matrix(
    arm: Arm,
    attack_anim: f32,
    inverse_arm_height: f32,
    transform: &DisplayTransform,
) -> Mat4 {
    first_person_item_chain(arm, attack_anim, inverse_arm_height)
        * display_matrix_for_hand(transform, arm.is_left())
}

/// `ItemInHandRenderer.applyEatTransform`'s `Math.pow(scaledUsageTime, 27.0)`.
///
/// The exponent is the whole character of the animation and the one number a
/// "reasonable" simplification destroys. `1 - t` (a linear approach) and `1 - t^27`
/// agree only at the two endpoints: at `remaining = 30` of a 32-tick food the real
/// jiggle is `0.5755` and the linear reading is `0.03125`, an 18× difference, and by
/// `remaining = 24` the real one is already `0.9985` against `0.21875`. Linear reads
/// as the item *drifting* toward the mouth over the whole use; vanilla snaps it
/// there within about two ticks and then bobs.
pub const EAT_JIGGLE_EXPONENT: f64 = 27.0;

/// `applyEatTransform`'s `scaledUsageTime < 0.8F` gate on the vertical bob.
///
/// **This is a bound on *remaining* time, so it opens *late*, not early.**
/// `scaledUsageTime` is `currUsageTime / useDuration` where `currUsageTime` counts
/// **down**, so the bob is suppressed for the first 20% of a use and runs for the
/// last 80% of it. Reading the comparison as "only near the start" — the natural
/// reading of `< 0.8` — inverts the animation and is invisible in a still frame.
pub const EAT_BOB_SCALED_LIMIT: f32 = 0.8;

/// `applyEatTransform`'s `currUsageTime`:
/// `player.getUseItemRemainingTicks() - frameInterp + 1.0F`.
///
/// Named rather than inlined because the `+ 1.0` and the sign of `frameInterp` are
/// both easy to lose and neither is checkable from a screenshot: the result is a
/// bob one tick out of phase. Note it can exceed `useDuration` on the first tick of
/// a use (`remaining == duration` gives `duration + 1`), which makes
/// `scaledUsageTime > 1` and the jiggle **negative** for that instant. That is
/// vanilla, not a clamp we forgot — the item flicks away from the mouth before
/// coming to it.
#[must_use]
pub fn eat_usage_time(remaining_ticks: u32, partial_tick: f32) -> f32 {
    remaining_ticks as f32 - partial_tick + 1.0
}

/// `ItemInHandRenderer.applyEatTransform`, verbatim:
///
/// ```java
/// float currUsageTime = player.getUseItemRemainingTicks() - frameInterp + 1.0F;
/// float scaledUsageTime = currUsageTime / itemStack.getUseDuration(player);
/// if (scaledUsageTime < 0.8F) {
///    float extraHeightOffset = Mth.abs(Mth.cos(currUsageTime / 4.0F * (float)Math.PI) * 0.1F);
///    poseStack.translate(0.0F, extraHeightOffset, 0.0F);
/// }
/// float eatJiggle = 1.0F - (float)Math.pow(scaledUsageTime, 27.0);
/// int invert = arm == HumanoidArm.RIGHT ? 1 : -1;
/// poseStack.translate(eatJiggle * 0.6F * invert, eatJiggle * -0.5F, eatJiggle * 0.0F);
/// poseStack.mulPose(Axis.YP.rotationDegrees(invert * eatJiggle * 90.0F));
/// poseStack.mulPose(Axis.XP.rotationDegrees(eatJiggle * 10.0F));
/// poseStack.mulPose(Axis.ZP.rotationDegrees(invert * eatJiggle * 30.0F));
/// ```
///
/// # Vanilla has no third-person counterpart
///
/// This is the *entire* eating animation. `HumanoidModel.ArmPose` has no `EAT` or
/// `DRINK` variant and `AvatarRenderer.getArmPose` omits both from its chain, so
/// another player eating is drawn with the ordinary [`ArmPose::Item`](crate::ArmPose::Item)
/// raise plus crumbs. The dip, the twist and the bob exist only here.
///
/// # `EAT` and `DRINK` are the same transform
///
/// They are one `switch` case in `submitArmWithItem`, so a potion and a carrot move
/// identically in the hand. The two animations differ only in duration (via the
/// item's `consumeSeconds`), sound, and whether particles are emitted at all.
///
/// # The `z` term is `eatJiggle * 0.0F`
///
/// Kept as a literal zero rather than dropped, because it is the one axis vanilla
/// deliberately does not move and a reader comparing against the Java otherwise has
/// to prove the omission was intentional.
#[must_use]
pub fn first_person_eat_transform(arm: Arm, curr_usage_time: f32, use_duration: u32) -> Mat4 {
    let i = arm.invert();
    let scaled = curr_usage_time / use_duration.max(1) as f32;
    let bob = if scaled < EAT_BOB_SCALED_LIMIT {
        // `Mth.abs(Mth.cos(currUsageTime / 4.0F * PI) * 0.1F)` — a 8-tick period,
        // and the absolute value is what makes it a *bounce* rather than a
        // sinusoid: it never goes below the resting height.
        let height = ((curr_usage_time / 4.0 * std::f32::consts::PI).cos() * 0.1).abs();
        Mat4::from_translation(Vec3::new(0.0, height, 0.0))
    } else {
        Mat4::IDENTITY
    };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "vanilla's own `(float)Math.pow(..., 27.0)`: the pow is evaluated in double and narrowed"
    )]
    let jiggle = 1.0 - f64::from(scaled).powf(EAT_JIGGLE_EXPONENT) as f32;
    bob * Mat4::from_translation(Vec3::new(jiggle * 0.6 * i, jiggle * -0.5, jiggle * 0.0))
        * Mat4::from_rotation_y((i * jiggle * 90.0).to_radians())
        * Mat4::from_rotation_x((jiggle * 10.0).to_radians())
        * Mat4::from_rotation_z((i * jiggle * 30.0).to_radians())
}

/// The camera-space chain for an item being **eaten or drunk**, replacing
/// [`first_person_item_chain`] for as long as the use lasts.
///
/// ```text
/// applyEatTransform(arm, currUsageTime, useDuration)
///   · T(i·0.56, -0.52 + h·-0.6, -0.72)      -- applyItemArmTransform
/// ```
///
/// # Two differences from [`first_person_item_chain`], both from the same `switch`
///
/// * **`applyItemArmTransform` comes *last*, not first.** `ItemUseAnimation.EAT` and
///   `DRINK` have `hasCustomArmTransform() == true`, so `submitArmWithItem` skips the
///   pre-switch `applyItemArmTransform` and the case applies it *after*
///   `applyEatTransform`. Putting the offset first instead — the order every other
///   pose here uses — rotates the item about the camera rather than about the hand,
///   which swings it across the whole screen.
/// * **There is no swing.** The `player.isUsingItem()` branch never reaches
///   `swingArm`, so [`ItemSwingTerms`] and [`first_person_item_attack_chain`] do not
///   apply. Left-clicking while eating must not move the item.
#[must_use]
pub fn first_person_eat_chain(
    arm: Arm,
    curr_usage_time: f32,
    use_duration: u32,
    inverse_arm_height: f32,
) -> Mat4 {
    let i = arm.invert();
    let [ox, oy, oz] = FIRST_PERSON_ITEM_OFFSET;
    first_person_eat_transform(arm, curr_usage_time, use_duration)
        * Mat4::from_translation(Vec3::new(
            i * ox,
            oy + inverse_arm_height * FIRST_PERSON_ITEM_EQUIP_DIP,
            oz,
        ))
}

/// [`first_person_eat_chain`] followed by the item's own `firstperson_?hand`
/// display transform — the eating counterpart of [`first_person_item_matrix`].
#[must_use]
pub fn first_person_eat_matrix(
    arm: Arm,
    curr_usage_time: f32,
    use_duration: u32,
    inverse_arm_height: f32,
    transform: &DisplayTransform,
) -> Mat4 {
    first_person_eat_chain(arm, curr_usage_time, use_duration, inverse_arm_height)
        * display_matrix_for_hand(transform, arm.is_left())
}

/// The first-person item-use pose selected by
/// `ItemInHandRenderer.submitArmWithItem` after it has resolved the item's
/// geometry.
///
/// `Bow` deliberately carries elapsed use ticks, not a precomputed power: the
/// same clock drives the pulling-model thresholds and vanilla's nonlinear draw
/// power, while keeping that conversion in the pose owner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FirstPersonItemUse {
    /// `ItemUseAnimation.EAT` / `DRINK`'s custom arm transform.
    Eat {
        /// Vanilla's interpolated elapsed use time.
        curr_usage_time: f32,
        /// The item's configured use duration.
        use_duration: u32,
    },
    /// `ItemUseAnimation.BOW`'s aimed, charging transform.
    Bow {
        /// Ticks elapsed since the bow use began.
        held_ticks: f32,
    },
}

/// `BowItem.getPowerForTime`: the nonlinear charge fraction shared by its
/// launch velocity and `ItemInHandRenderer`'s first-person pose.
#[must_use]
pub fn first_person_bow_power(held_ticks: f32) -> f32 {
    let charge = (held_ticks / 20.0).max(0.0);
    ((charge * charge + charge * 2.0) / 3.0).min(1.0)
}

/// `ItemInHandRenderer`'s `ItemUseAnimation.BOW` transform, before the item's
/// own `firstperson_?hand` display transform.
///
/// ```text
/// T(i·0.56, -0.52 + h·-0.6, -0.72)          -- applyItemArmTransform
///   · T(i·-0.2785682, 0.18344387, 0.15731531)
///   · Rx(-13.935) · Ry(i·35.3) · Rz(i·-9.785)
///   · T(0, shake, 0) · T(0, 0, power·0.04)
///   · S(1, 1, 1 + power·0.2) · Ry(i·-45)
/// ```
///
/// # The leading arm transform is not optional, and omitting it hides the bow
///
/// `submitArmWithItem` applies `applyItemArmTransform` **before** entering the
/// use-animation switch for every animation whose `hasCustomArmTransform()` is
/// false, and `BOW`'s is false — only `EAT`, `DRINK` and `SPEAR` opt out, and the
/// first two then re-apply it themselves *after* their own transform (which is
/// why [`first_person_eat_chain`] composes it last and this one composes it
/// first). Starting the chain at the BOW-specific translation therefore drops
/// `z = -0.72` and the item sits on, or behind, the near plane: the bow vanishes
/// the instant the use begins rather than being drawn in the wrong place, which
/// is what makes the omission read as a use-state bug.
///
/// `inverse_arm_height` is vanilla's `inverseArmHeight`, the same equip/swap dip
/// [`first_person_item_chain`] takes; a charging bow still dips while swapping.
///
/// # The shake follows the rotations
///
/// Vanilla's `poseStack.translate(0, shake · 0.004, 0)` sits *after* the three
/// `mulPose` calls, so it displaces the bow along the **rotated** local Y, not
/// along camera-space Y. Folding it into the leading translation's `y` — the
/// arithmetically tempting simplification, since it is the only non-zero
/// component — tilts the wobble into the wrong plane. `Mth.sin` rather than
/// `f32::sin`: vanilla's is a quantized lookup table and this repo's ported
/// trigonometry goes through `lodestone_physics::mth` for that reason.
#[must_use]
pub fn first_person_bow_chain(arm: Arm, held_ticks: f32, inverse_arm_height: f32) -> Mat4 {
    let i = arm.invert();
    let [ox, oy, oz] = FIRST_PERSON_ITEM_OFFSET;
    let held_ticks = held_ticks.max(0.0);
    let power = first_person_bow_power(held_ticks);
    let shake = if power > 0.1 {
        lodestone_physics::mth::sin(f64::from((held_ticks - 0.1) * 1.3)) * (power - 0.1) * 0.004
    } else {
        0.0
    };
    Mat4::from_translation(Vec3::new(
        i * ox,
        oy + inverse_arm_height * FIRST_PERSON_ITEM_EQUIP_DIP,
        oz,
    )) * Mat4::from_translation(Vec3::new(i * -0.278_568_2, 0.183_443_87, 0.157_315_31))
        * Mat4::from_rotation_x((-13.935f32).to_radians())
        * Mat4::from_rotation_y((i * 35.3).to_radians())
        * Mat4::from_rotation_z((i * -9.785).to_radians())
        * Mat4::from_translation(Vec3::new(0.0, shake, 0.0))
        * Mat4::from_translation(Vec3::new(0.0, 0.0, power * 0.04))
        * Mat4::from_scale(Vec3::new(1.0, 1.0, 1.0 + power * 0.2))
        * Mat4::from_rotation_y((i * -45.0).to_radians())
}

/// [`first_person_bow_chain`] followed by the item's own
/// `firstperson_?hand` display transform.
#[must_use]
pub fn first_person_bow_matrix(
    arm: Arm,
    held_ticks: f32,
    inverse_arm_height: f32,
    transform: &DisplayTransform,
) -> Mat4 {
    first_person_bow_chain(arm, held_ticks, inverse_arm_height)
        * display_matrix_for_hand(transform, arm.is_left())
}

/// Mesh the item in the first-person hand into a camera-space [`ModelMesh`], to be
/// drawn through the ordinary [`ModelPipeline`](crate::ModelPipeline) with
/// [`hand_projection`] alone as the camera uniform (the same uniform the bare arm
/// uses, and for the same reason: the pose is already camera-space).
///
/// `item_use` selects the pose. It is a parameter rather than separate mesh
/// functions so the use cases cannot diverge in lighting or quad-meshing.
#[must_use]
pub fn first_person_item_mesh_with_use(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    arm: Arm,
    attack_anim: f32,
    inverse_arm_height: f32,
    transform: &DisplayTransform,
    light: u8,
    item_use: Option<FirstPersonItemUse>,
) -> ModelMesh {
    let pose = match item_use {
        Some(FirstPersonItemUse::Eat {
            curr_usage_time,
            use_duration,
        }) => first_person_eat_matrix(
            arm,
            curr_usage_time,
            use_duration,
            inverse_arm_height,
            transform,
        ),
        Some(FirstPersonItemUse::Bow { held_ticks }) => {
            first_person_bow_matrix(arm, held_ticks, inverse_arm_height, transform)
        }
        None => first_person_item_matrix(arm, attack_anim, inverse_arm_height, transform),
    };
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

/// Mesh the item in the first-person hand into a camera-space [`ModelMesh`], to be
/// drawn through the ordinary [`ModelPipeline`](crate::ModelPipeline) with
/// [`hand_projection`] alone as the camera uniform (the same uniform the bare arm
/// uses, and for the same reason: the pose is already camera-space).
#[must_use]
pub fn first_person_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    arm: Arm,
    attack_anim: f32,
    inverse_arm_height: f32,
    transform: &DisplayTransform,
    light: u8,
) -> ModelMesh {
    let pose = first_person_item_matrix(arm, attack_anim, inverse_arm_height, transform);
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_assets::Direction;

    fn pig_mesh() -> EntityMesh {
        EntityMesh::from_model(&lodestone_assets::entity_models::pig_model())
    }

    // -----------------------------------------------------------------------
    // Humanoid armour
    // -----------------------------------------------------------------------

    /// Every armour slot must bake real geometry, and every *load-bearing* part
    /// it bakes must attach to a real part of the humanoid rigs that wear
    /// armour. An armour mesh whose parts do not attach draws nothing at all —
    /// the island defect, with a green mesh test.
    ///
    /// `hat` is the one excusable miss: it is the helmet's outermost shell, it
    /// unwraps onto a region measured empty in all nine of 26.2's humanoid
    /// armour sheets, and the corpus `armor_stand` rig deliberately has no `hat`
    /// part at all (vanilla forces `hat.visible = false` there). So it is
    /// required to attach *only* where the wearer has one — which is itself an
    /// assertion, not a shrug.
    #[test]
    fn every_armour_slot_attaches_to_every_humanoid_rig() {
        let set = ArmourModelSet::load();
        let models = EntityModelSet::load();
        for wearer_name in [
            "player_wide",
            "player_slim",
            "zombie",
            "skeleton",
            "armor_stand",
        ] {
            let wearer = models
                .get(wearer_name)
                .unwrap_or_else(|| panic!("{wearer_name} must be in the corpus"));
            assert!(
                wearer_carries_armour(&wearer.skeleton),
                "{wearer_name} must classify as humanoid, or it wears nothing"
            );
            for (slot, mesh) in set.iter() {
                assert!(mesh.quad_count() > 0, "{slot:?} baked no geometry at all");
                let attached: Vec<&'static str> = mesh
                    .attach(&wearer.skeleton)
                    .map(|(range, wearer_index)| {
                        assert!(range.index_count > 0, "{slot:?} attached an empty range");
                        assert!(wearer_index < wearer.skeleton.len());
                        mesh.parts
                            .iter()
                            .find(|(_, r)| *r == range)
                            .map(|(n, _)| *n)
                            .expect("range came from this mesh")
                    })
                    .collect();
                let expected: Vec<&'static str> = mesh
                    .parts
                    .iter()
                    .map(|(n, _)| *n)
                    .filter(|n| *n != "hat" || wearer.skeleton.index_of("hat").is_some())
                    .collect();
                assert_eq!(
                    attached, expected,
                    "{wearer_name} cannot carry every part of {slot:?}"
                );
            }
        }
    }

    /// **The water-clip mask reaches `resolve_animated` through the real
    /// corpus loader, not just through its own standalone builder.** Owner
    /// report: "placing down a boat still shows water through the bottom".
    /// This is the render-layer half of the island check
    /// `lodestone_assets::entity_models`'s own `the_water_patch_is_a_real_corpus_entry`
    /// makes at the data layer: a name present in `entity_models()` still has
    /// to survive `EntityModelSet::load()`'s baking and `canonical_model_name`'s
    /// resolution before anything can draw it.
    #[test]
    fn boat_water_patch_resolves_through_the_real_corpus_loader() {
        let models = EntityModelSet::load();
        let anim = AnimInput::REST;
        let instance = models
            .resolve_animated("boat_water_patch", Vec3::new(1.0, 64.0, 2.0), 30.0, 0.0, 1.0, &anim, 0.0, 0.0)
            .expect(
                "\"boat_water_patch\" must resolve through the same corpus loader every real \
                 boat instance goes through, or the fix is present in source and reaches no pixel",
            );
        assert_eq!(instance.model, "boat_water_patch");
    }

    /// **The mask must sit exactly where the boat itself is drawn, not merely
    /// somewhere plausible.** `AbstractBoatRenderer.submit` calls
    /// `submitTypeAdditions` (the water-patch submit) *inside* the same
    /// `pushPose`/`popPose` block as the main model, after the identical
    /// bob/rotate/flip/spin sequence — so the two must share one placement
    /// transform, not two similar ones. Both hypotheses are checked: the
    /// right one (`"boat_water_patch"` joins `non_living_vehicle_placement`'s
    /// `"boat"` arm) and the wrong one this fix could easily have shipped
    /// (falling through to the *living-entity* placement — a bare
    /// `resolve`/`resolve_posed` matrix with no `0.375` bob and no `90°`
    /// spin), which would leave the mask floating at the wrong height and
    /// facing broadside to the hull it exists to seal.
    #[test]
    fn the_water_patch_shares_the_boats_own_placement_transform() {
        let models = EntityModelSet::load();
        let anim = AnimInput::REST;
        let feet = Vec3::new(-4.0, 70.0, 11.0);
        let boat = models
            .resolve_animated("boat", feet, 217.0, 0.0, 1.0, &anim, 0.0, 0.0)
            .expect("\"boat\" must resolve");
        let patch = models
            .resolve_animated("boat_water_patch", feet, 217.0, 0.0, 1.0, &anim, 0.0, 0.0)
            .expect("\"boat_water_patch\" must resolve");
        assert_eq!(
            boat.transform, patch.transform,
            "the mask's placement transform must be bit-identical to the boat's own, not a \
             separately-derived approximation"
        );

        // The wrong hypothesis, computed from the *other* placement rule this
        // fix could have fallen through to (a living-entity matrix, no bob,
        // no spin) — must disagree, or this test cannot tell the two apart.
        let living_matrix = crate::entity::EntityInstance::new(
            "boat_water_patch",
            models.get("boat_water_patch").expect("resolved above"),
            feet,
            217.0,
            1.0,
            &anim,
        )
        .transform;
        assert_ne!(
            boat.transform, living_matrix,
            "the living-entity placement must differ from the vehicle one at this yaw, or \
             the positive assertion above proves nothing"
        );
    }

    /// A non-humanoid rig carries no armour, and that is the correct answer
    /// rather than a fallback: `HumanoidArmorLayer` is only attached to
    /// renderers whose model is a `HumanoidModel`, so a pig handed a chestplate
    /// by a plugin wears nothing in vanilla either.
    ///
    /// The negative control matters here: a pig **does** have `head` and
    /// `body`, so a name-keyed attach would happily bolt a chestplate to it.
    /// That is why the gate is the animation family, and why this asserts the
    /// name lookup would otherwise have succeeded.
    #[test]
    fn a_pig_attaches_no_armour_despite_having_a_body_part() {
        let set = ArmourModelSet::load();
        let pig = pig_mesh();
        assert!(!wearer_carries_armour(&pig.skeleton));
        assert!(
            pig.skeleton.index_of("body").is_some() && pig.skeleton.index_of("head").is_some(),
            "control: the pig must have the parts a name-keyed attach would match"
        );
        for (_, mesh) in set.iter() {
            assert_eq!(mesh.attach(&pig.skeleton).count(), 0);
        }
    }

    // -----------------------------------------------------------------------
    // Sheep wool (issue #53)
    // -----------------------------------------------------------------------

    fn cow_mesh() -> EntityMesh {
        EntityMesh::from_model(&lodestone_assets::entity_models::cow_model())
    }

    fn sheep_mesh() -> EntityMesh {
        EntityMesh::from_model(&lodestone_assets::entity_models::sheep_model())
    }

    /// A sheep attaches every one of the wool mesh's six parts to its own
    /// body — the positive half of the pig/cow trap check below: if this did
    /// not attach, the negative checks would be proving nothing.
    #[test]
    fn a_sheep_attaches_every_wool_part_to_its_own_body() {
        let wool = WoolMesh::load();
        let sheep = sheep_mesh();
        assert_eq!(wool.parts.len(), 6, "sheep_wool_model must bake all six named parts");
        let attached: Vec<_> = wool.attach(&sheep.skeleton, "sheep").collect();
        assert_eq!(
            attached.len(),
            6,
            "every wool part must attach to the real sheep body rig"
        );
        for (range, wearer_index) in &attached {
            assert!(range.index_count > 0, "an attached wool part baked no geometry");
            assert!(*wearer_index < sheep.skeleton.len());
        }
    }

    /// **The pig/cow trap, for wool.** `sheep`, `pig`, `cow` and `wolf` are all
    /// `AnimFamily::Quadruped` and all four share the exact part *names*
    /// [`sheep_wool_model`] uses (`head`, `body`, `*_hind_leg`, `*_front_leg`)
    /// — `quadruped_root` builds every one of them from the same generator.
    /// So a pig or a cow genuinely **does** have every name [`WoolMesh::attach`]
    /// looks up, which is exactly why gating on `wearer.family()` (armour's own
    /// discipline) would be wrong here: it would resolve cleanly and grow a
    /// fleece on a farm animal. The control matters for the same reason
    /// `a_pig_attaches_no_armour_despite_having_a_body_part` asserts it does:
    /// without it, this test could pass by accident (a rig with no matching
    /// parts at all) rather than by the `wearer_model` gate actually working.
    #[test]
    fn a_pig_and_a_cow_attach_no_wool_despite_sharing_every_part_name() {
        let wool = WoolMesh::load();
        for (name, mesh) in [("pig", pig_mesh()), ("cow", cow_mesh())] {
            for part_name in SHEEP_WOOL_PART_NAMES {
                assert!(
                    mesh.skeleton.index_of(part_name).is_some(),
                    "control: {name} must have a {part_name} part, or this test proves \
                     nothing about the wearer_model gate specifically"
                );
            }
            // The real would-be-wrong call: gating on family alone, exactly the
            // mistake `docs/entity-rendering.md` names.
            assert_eq!(
                mesh.skeleton.family(),
                crate::entity_anim::AnimFamily::Quadruped,
                "{name} must share the sheep's animation family for this control to be real"
            );
            assert_eq!(
                wool.attach(&mesh.skeleton, name).count(),
                0,
                "{name} must attach no wool part when gated on its own resolved model name"
            );
        }
    }

    /// The armour a wearer draws with is *its own* posed part matrix, so the
    /// world-pose determinant invariant is inherited rather than re-derived:
    /// every matrix an armour layer is drawn under has to be **positive**,
    /// orientation-preserving like any world model matrix, so that composing it
    /// with `view_projection` leaves the camera's own sign untouched and the
    /// same faces survive culling as for un-armoured geometry.
    ///
    /// The camera's own polarity is deliberately not asserted. It follows from
    /// which end of `[0, 1]` the near plane sits at — negative under a forward
    /// projection, positive under this renderer's reversed-Z one — and it is not
    /// what the rasterizer reads. The claim that matters is that the *pose* does
    /// not reverse orientation, and that is absolute.
    #[test]
    fn armour_is_drawn_under_positive_determinant_wearer_matrices() {
        let camera = crate::camera::Camera::default();
        let view_proj = camera.view_projection().determinant();
        assert!(
            view_proj.abs() > 1.0e-6,
            "the reference camera's projection is degenerate ({view_proj}), so \
             composition through it says nothing"
        );

        let set = ArmourModelSet::load();
        let models = EntityModelSet::load();
        let instance = models
            .resolve("zombie", Vec3::new(3.0, 64.0, -7.0), 37.0, 1.0, &AnimInput {
                head_yaw_deg: 12.0,
                head_pitch_deg: -8.0,
                limb_swing: 3.5,
                limb_swing_amount: 0.9,
                attack_anim: 0.4,
                age_ticks: 42.0,
                aggressive: false,
                ..AnimInput::REST
            })
            .expect("zombie resolves");
        let mesh = models.get("zombie").expect("zombie mesh");
        let mut checked = 0;
        for (_, armour) in set.iter() {
            for (_, wearer_index) in armour.attach(&mesh.skeleton) {
                let m = instance.part_transforms[wearer_index];
                assert!(
                    m.determinant() > 0.0,
                    "armour part matrix determinant must be positive, was {}",
                    m.determinant()
                );
                // And the composed clip transform must then inherit the
                // camera's sign, which is what actually decides facing.
                assert_eq!(
                    (camera.view_projection() * m).determinant().signum(),
                    view_proj.signum()
                );
                checked += 1;
            }
        }
        assert!(checked >= 8, "only {checked} armour parts checked");
    }

    /// Layer resolution: two coplanar layers for leather (base + overlay), one
    /// for a plain material, none across slots, none for the head-slot items
    /// vanilla draws through some other layer.
    #[test]
    fn armour_layer_resolution_follows_the_item_and_its_slot() {
        assert_eq!(armour_layers(ArmourSlot::Chest, "leather_chestplate").len(), 2);
        assert_eq!(armour_layers(ArmourSlot::Legs, "leather_leggings").len(), 2);
        assert_eq!(armour_layers(ArmourSlot::Head, "diamond_helmet").len(), 1);
        assert_eq!(armour_layers(ArmourSlot::Head, "turtle_helmet").len(), 1);
        // A helmet forced into the boots slot draws nothing, as
        // `shouldRender`'s slot equality demands.
        assert!(armour_layers(ArmourSlot::Feet, "diamond_helmet").is_empty());
        // Not armour at all.
        assert!(armour_layers(ArmourSlot::Head, "carved_pumpkin").is_empty());
        assert!(armour_layers(ArmourSlot::Chest, "elytra").is_empty());
        assert!(armour_layers(ArmourSlot::Chest, "wolf_armor").is_empty());
        assert!(armour_layers(ArmourSlot::Head, "stone").is_empty());
    }

    /// Only leather's base layer is tinted, and it is tinted to vanilla's
    /// `color_when_undyed`. White for everything else — a tint of `[0,0,0]`
    /// would be black armour and a tint applied to the overlay would recolour
    /// the buckles.
    #[test]
    fn only_leathers_base_layer_carries_a_tint() {
        let leather = armour_layers(ArmourSlot::Chest, "leather_chestplate");
        assert_eq!(
            armour_layer_tint(&leather[0]),
            lodestone_assets::equipment::UNDYED_LEATHER_RGB
        );
        assert_eq!(armour_layer_tint(&leather[1]), [255, 255, 255]);
        let diamond = armour_layers(ArmourSlot::Head, "diamond_helmet");
        assert_eq!(armour_layer_tint(&diamond[0]), [255, 255, 255]);
    }

    /// A real `minecraft:dyed_color` reaches the base leather layer
    /// unchanged (mod the alpha byte `ARGB.opaque` strips), while the
    /// non-dyeable overlay layer ignores it exactly as
    /// `getColorForLayer`'s `else -> -1` branch does — two competing
    /// hypotheses (dye applied vs. dye ignored) landing on different layers
    /// of the *same* item, so a broken layer/dye pairing cannot pass by
    /// accident.
    #[test]
    fn a_real_dye_reaches_the_base_layer_but_not_the_overlay() {
        let leather = armour_layers(ArmourSlot::Chest, "leather_chestplate");
        // Bright cyan (`0x00FFFF`), chosen only for being nowhere near
        // `UNDYED_LEATHER_RGB` (`0xA06540`) so a fallback-to-undyed bug is
        // unmistakable.
        let dye = Some(0x0000_FFFF_u32);
        assert_eq!(armour_layer_tint_with_dye(&leather[0], dye), [0x00, 0xFF, 0xFF]);
        // The overlay has no `dyeable` block: `getColorForLayer` never reads
        // `dyeColor` for it.
        assert_eq!(armour_layer_tint_with_dye(&leather[1], dye), [255, 255, 255]);
    }

    /// `dyed_color: None` (component absent) falls back to
    /// `Dyeable.colorWhenUndyed`, matching the zero-argument
    /// [`armour_layer_tint`] this delegates to.
    #[test]
    fn absent_dye_falls_back_to_color_when_undyed() {
        let leather = armour_layers(ArmourSlot::Chest, "leather_chestplate");
        assert_eq!(
            armour_layer_tint_with_dye(&leather[0], None),
            lodestone_assets::equipment::UNDYED_LEATHER_RGB
        );
        assert_eq!(
            armour_layer_tint_with_dye(&leather[0], None),
            armour_layer_tint(&leather[0])
        );
    }

    /// The vanilla quirk pinned in [`armour_layer_tint_with_dye`]'s own
    /// doc: dyeing leather pure black is indistinguishable from not dyeing
    /// it at all, because `DyedItemColor.getOrDefault`'s `ARGB.opaque` only
    /// touches the alpha byte, so a black dye's RGB portion is still `0` and
    /// `dyeColor != 0` reads false. This is `EquipmentLayerRenderer`'s own
    /// behaviour (`EquipmentLayerRenderer.java:117`), not a port bug — a
    /// "fix" that special-cases black would diverge from the game it ports.
    #[test]
    fn dyed_color_zero_reads_as_undyed() {
        let leather = armour_layers(ArmourSlot::Chest, "leather_chestplate");
        assert_eq!(
            armour_layer_tint_with_dye(&leather[0], Some(0x0000_0000)),
            lodestone_assets::equipment::UNDYED_LEATHER_RGB
        );
    }

    /// A non-dyeable layer ignores `dyed_color` even when one is present —
    /// `getColorForLayer`'s `else` branch never reads `dyeColor` at all, so
    /// a diamond helmet dyed (nonsensically) any colour still draws white.
    #[test]
    fn non_dyeable_layers_ignore_a_present_dye() {
        let diamond = armour_layers(ArmourSlot::Head, "diamond_helmet");
        assert_eq!(
            armour_layer_tint_with_dye(&diamond[0], Some(0x00FF_0000)),
            [255, 255, 255]
        );
    }

    /// The two vanilla anchor values, hand-derived from the real timeline
    /// keyframes (`Timelines.java:79`) rather than from this implementation,
    /// so agreement is evidence rather than a tautology:
    ///
    /// * noon (6000) falls inside the `[730, 11270)` plateau segment, both of
    ///   whose keyframes are `1.0` — constant `1.0` regardless of where in the
    ///   segment 6000 lands.
    /// * midnight (18000) falls inside the `[13140, 22860)` plateau segment,
    ///   both of whose keyframes are `0.24` — constant `0.24` likewise.
    ///
    /// Both are covered far more thoroughly, tick-by-tick against a real JVM,
    /// by `tests/sky_light_factor_timeline.rs`; these two stay as a fast
    /// same-crate smoke check.
    #[test]
    fn sky_darken_hits_vanillas_noon_and_midnight_anchors() {
        assert!((sky_darken_for_time_of_day(6_000) - 1.0).abs() < 1e-5);
        assert!((sky_darken_for_time_of_day(18_000) - 0.24).abs() < 1e-5);
    }

    /// A large world age must reduce into the day, not drift: `time_of_day`
    /// keeps counting past 24000 for the life of a world, and a curve that read
    /// it raw would eventually saturate at one end and stop darkening at all —
    /// a bug that only appears on a world that has been running for a while,
    /// i.e. never in a test and always for the player.
    #[test]
    fn sky_darken_reduces_a_large_world_age_into_the_day() {
        assert_eq!(
            sky_darken_for_time_of_day(18_000),
            sky_darken_for_time_of_day(18_000 + 24_000 * 500)
        );
        assert_eq!(
            sky_darken_for_time_of_day(6_000),
            sky_darken_for_time_of_day(6_000 - 24_000 * 500)
        );
    }

    /// The curve must stay inside vanilla's `[0.24, 1.0]` across a whole day and
    /// must actually *vary* — a constant 1.0 is the shipped bug, and a value
    /// that ever reaches 0.0 would collide with the shader's "not wired"
    /// sentinel and silently mean full daylight at exactly the darkest moment.
    #[test]
    fn sky_darken_stays_in_vanillas_range_and_is_not_constant() {
        let samples: Vec<f32> = (0..24_000)
            .step_by(50)
            .map(sky_darken_for_time_of_day)
            .collect();
        let lo = samples.iter().copied().fold(f32::INFINITY, f32::min);
        let hi = samples.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(lo >= 0.24 - 1e-5, "dipped to {lo}, below vanilla's 0.24 floor");
        assert!(hi <= 1.0 + 1e-5, "rose to {hi}, above 1.0");
        assert!(lo > 0.0, "0.0 is the shader's 'unset' sentinel and must be unreachable");
        assert!(hi - lo > 0.5, "the curve barely moves ({lo}..{hi}) — that is the defect");
    }

    #[test]
    fn maps_known_entity_types_to_models() {
        assert_eq!(model_for_type(EntityType::Pig).unwrap().name, "pig");
        assert_eq!(model_for_type(EntityType::Cow).unwrap().name, "cow");
        assert_eq!(model_for_type(EntityType::Chicken).unwrap().name, "chicken");
        assert_eq!(model_for_type(EntityType::Sheep).unwrap().name, "sheep");
        assert_eq!(model_for_type(EntityType::Zombie).unwrap().name, "zombie");
        assert_eq!(model_for_type(EntityType::Skeleton).unwrap().name, "skeleton");
        assert_eq!(model_for_type(EntityType::Creeper).unwrap().name, "creeper");
        assert_eq!(model_for_type(EntityType::Spider).unwrap().name, "spider");
        // The two surviving aliases: a type path that is not a corpus name.
        assert_eq!(model_for_type(EntityType::Player).unwrap().name, "player_wide");
        assert_eq!(model_for_type(EntityType::Bogged).unwrap().name, "skeleton");
    }

    /// The reported defect: a drowned rendered as an ordinary zombie. Its mesh
    /// and its sheet both exist in the corpus; a stale alias in this module was
    /// routing it to the zombie's. Every mob here is one that alias table used
    /// to swallow, so each assertion is a distinct wrong-mesh substitution.
    #[test]
    fn mob_variants_resolve_to_their_own_model_not_a_base_mob() {
        for (entity_type, wrong) in [
            (EntityType::Drowned, "zombie"),
            (EntityType::Husk, "zombie"),
            (EntityType::ZombieVillager, "zombie"),
            (EntityType::Stray, "skeleton"),
            (EntityType::WitherSkeleton, "skeleton"),
            (EntityType::CaveSpider, "spider"),
            (EntityType::Mooshroom, "cow"),
        ] {
            let ty = entity_type.path();
            let model =
                model_for_type(entity_type).unwrap_or_else(|| panic!("{ty} has a corpus model"));
            assert_eq!(
                model.name, ty,
                "{ty} resolved to {} — a variant is being drawn as its base mob",
                model.name
            );
            assert_ne!(model.name, wrong);
            // ...and it must not merely resolve: the *sheet* has to differ too,
            // or the mob is still a zombie with a zombie skin under a new name.
            assert_ne!(
                entity_texture_candidates(ty),
                entity_texture_candidates(wrong),
                "{ty} shares {wrong}'s texture candidates"
            );
        }
    }

    /// The island this agent closed: `model_for_type` returned `None` for six
    /// of the seven `minecraft:*_minecart` registry types (every one except
    /// the plain `minecart`), so a chest/furnace/tnt/hopper minecart streamed
    /// correctly and drew nothing — `resolve_animated` silently skips any
    /// `type_path` with no baked model. All four now share the plain cart's
    /// `"minecart"` rig, matching vanilla's `AbstractMinecartRenderer` reusing
    /// one `MinecartModel` for every subclass.
    ///
    /// All six subclasses, including `spawner_minecart` and
    /// `command_block_minecart`. Those two used to be a negative control here,
    /// asserted to resolve `None` on the grounds that this repo's own server
    /// never spawns them — a claim about the *server* standing in for a claim
    /// about the *client*, which meets them on anyone else's. The control that
    /// replaces it is the one that was always the real risk: a *non*-minecart
    /// type must not pick up the rig by name.
    #[test]
    fn minecart_subclasses_share_the_plain_carts_frame() {
        for entity_type in [
            EntityType::Minecart,
            EntityType::ChestMinecart,
            EntityType::FurnaceMinecart,
            EntityType::TntMinecart,
            EntityType::HopperMinecart,
            EntityType::SpawnerMinecart,
            EntityType::CommandBlockMinecart,
        ] {
            let ty = entity_type.path();
            let model = model_for_type(entity_type)
                .unwrap_or_else(|| panic!("{ty} must resolve to the minecart corpus rig"));
            assert_eq!(
                model.name, "minecart",
                "{ty} resolved to {} instead of the shared minecart rig",
                model.name
            );
        }
        // The control the roster above needs: the alias is an enum match, so a
        // type whose *path* merely ends in `minecart` must not reach the rig,
        // and a real mob must not either. `minecart` is not a suffix rule and
        // this is what says so.
        for absent in ["boat_minecart", "minecart_of_holding", "pig", "chest"] {
            assert!(
                canonical_model_name(absent) != Some("minecart"),
                "`{absent}` resolved to the minecart rig — the alias is an enum match on \
                 six real registry types, not a name rule"
            );
        }
    }

    /// The round trip issue #523's third pass asks for: `u8` (the wire's
    /// registry id) → [`EntityType`] → [`model_for_type`], swept for all 158
    /// generated variants and checked against the independent `&str`-keyed
    /// path ([`canonical_model_name`], reached via
    /// [`EntityType::from_name`] rather than [`model_for_type`]) rather than
    /// against itself — the two implementations cannot silently disagree
    /// without this test naming which entity they disagreed on. Mismatches
    /// are collected, not asserted inside the loop, so a regression reports
    /// every disagreeing type instead of only the first.
    #[test]
    fn model_for_type_agrees_with_the_string_path_for_every_generated_entity_type() {
        let mut checked = 0usize;
        let mut mismatches = Vec::new();
        for entity_type in EntityType::all() {
            checked += 1;
            // `u8 -> EntityType`: the decode seam's only fallible step.
            let via_wire_id = EntityType::from_registry_id(entity_type.registry_id());
            if via_wire_id != Some(entity_type) {
                mismatches.push(format!(
                    "{}: registry id {} did not round-trip (got {via_wire_id:?})",
                    entity_type.path(),
                    entity_type.registry_id()
                ));
                continue;
            }
            let via_type = model_for_type(entity_type).map(|e| e.name);
            let via_str = canonical_model_name(entity_type.path());
            if via_type != via_str {
                mismatches.push(format!(
                    "{}: model_for_type={via_type:?} canonical_model_name(str)={via_str:?}",
                    entity_type.path()
                ));
            }
        }
        assert_eq!(
            checked,
            EntityType::COUNT as usize,
            "swept {checked} of {} generated variants — roster too small to be a real gate",
            EntityType::COUNT
        );
        assert!(
            mismatches.is_empty(),
            "{} of {checked} entity types disagree between the `EntityType` path and the \
             `&str` path:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }

    /// The sweep above could pass by accident if every code path happened to
    /// agree on some entity whose registry id and alphabetical rank coincide
    /// (registration order is not alphabetical, but nothing rules out a lone
    /// coincidence). This picks a **specific** entity where the two orders
    /// provably disagree and checks the round trip on exactly that one, so a
    /// hypothetical bug that resolves by alphabetical position instead of by
    /// registry id has a concrete input it cannot pass.
    #[test]
    fn model_for_type_round_trips_on_an_entity_whose_registry_id_and_alphabetical_rank_differ() {
        let mut alphabetical: Vec<EntityType> = EntityType::all().collect();
        alphabetical.sort_by_key(|e| e.path());
        let (alpha_rank, entity_type) = alphabetical
            .iter()
            .copied()
            .enumerate()
            .find(|&(alpha_rank, entity_type)| alpha_rank as u8 != entity_type.registry_id())
            .expect(
                "registration order and alphabetical order must differ somewhere among 158 \
                 entries — if this ever fires, the roster has no discriminating input left",
            );
        assert_ne!(
            alpha_rank as u8,
            entity_type.registry_id(),
            "chose a coincidentally-aligned entity, which cannot discriminate"
        );

        let via_wire_id =
            EntityType::from_registry_id(entity_type.registry_id()).expect("id round trip");
        assert_eq!(via_wire_id, entity_type);
        assert_eq!(
            model_for_type(via_wire_id).map(|e| e.name),
            canonical_model_name(entity_type.path()),
            "{} (registry id {}, alphabetical rank {alpha_rank}) disagreed",
            entity_type.path(),
            entity_type.registry_id()
        );
    }

    /// The suffix rules must not shadow the corpus's own names.
    ///
    /// `chest_boat` and `chest_raft` are corpus entries that also satisfy the
    /// `_boat`/`_raft` suffix tests, so a resolver that consults the suffixes
    /// *before* the corpus resolves the literal `"chest_boat"` to the plain
    /// `boat` rig — a silent wrong-mesh substitution for any caller that
    /// passes a corpus name straight through, which the `player_wide`/
    /// `player_slim` path already does.
    ///
    /// Moved from `tests/boat_model_resolution.rs` (issue #523's third pass):
    /// none of `"boat"`/`"chest_boat"`/`"raft"`/`"chest_raft"` is a real
    /// `minecraft:entity_type` registry entry, so there is no `EntityType`
    /// value to hand `model_for_type` any more — this is squarely the
    /// surviving `&str`/corpus-name boundary, `canonical_model_name`, which
    /// is private to this module and so cannot be reached from an external
    /// integration test.
    #[test]
    fn a_literal_corpus_rig_name_still_resolves_to_itself() {
        for name in ["boat", "chest_boat", "raft", "chest_raft"] {
            assert_eq!(
                canonical_model_name(name),
                Some(name),
                "a literal corpus rig name must resolve to itself, not through the \
                 boat suffix rules"
            );
        }
    }

    /// The negative control: the suffix rules must not hand a boat rig to
    /// something that merely shares a word.
    ///
    /// `chest_minecart` has no ported rig (the corpus has `minecart` only),
    /// and a resolver matching on `contains("boat")`/`contains("chest")`
    /// rather than on the suffix would be caught here. Without this arm,
    /// "return `chest_boat` for anything with `chest` in it" would satisfy
    /// every assertion above. `"chest"`/`"boater"`/`"raft_of_ducks"` are not
    /// real registry paths at all, so — like the test above — this exercises
    /// `canonical_model_name` directly rather than `model_for_type`.
    ///
    /// Moved from `tests/boat_model_resolution.rs` for the same reason as
    /// [`a_literal_corpus_rig_name_still_resolves_to_itself`].
    #[test]
    fn a_non_boat_type_gets_no_boat_rig() {
        for name in ["chest_minecart", "chest", "boater", "raft_of_ducks", "pig"] {
            let resolved = canonical_model_name(name);
            assert!(
                !matches!(resolved, Some("boat" | "chest_boat" | "raft" | "chest_raft")),
                "`{name}` is not a boat, but resolved to {resolved:?}"
            );
        }
    }

    /// Control for issue #523: `canonical_model_name` and `EntityModelSet::get`
    /// were switched from an O(90) linear `&str` scan to a `OnceLock`-cached
    /// `HashSet`/`HashMap` index. This re-derives the *old* linear scan from
    /// scratch, independently of both functions under test, and checks it
    /// against the new implementation for every one of the 158 generated
    /// entity-type paths plus the non-registry pseudo-types
    /// (`player_wide`/`player_slim`, the four boat-family aliases) that
    /// `canonical_model_name` also has to resolve — the "world-species" gate
    /// the issue asks for, so a roster of only-already-corpus-named types
    /// cannot pass by never exercising the alias table.
    #[test]
    fn canonical_model_name_and_get_agree_with_an_independent_linear_scan() {
        // The alias arms are restated here rather than shared, which is the whole
        // point: this function is the pre-index implementation, re-derived. That
        // costs one line whenever a *genuine* alias lands — a real one has to be
        // mirrored here, and the mirror going red is the reminder — but it is the
        // only way the comparison is between two constructions rather than one.
        // What it is a control for is the **lookup structure** (linear scan vs
        // `OnceLock` index), not the alias set.
        fn old_canonical_model_name(type_path: &str) -> Option<&'static str> {
            match type_path {
                "player" | "mannequin" => return Some("player_wide"),
                "bogged" => return Some("skeleton"),
                "breeze_wind_charge" => return Some("wind_charge"),
                "chest_minecart"
                | "furnace_minecart"
                | "tnt_minecart"
                | "hopper_minecart"
                | "spawner_minecart"
                | "command_block_minecart" => {
                    return Some("minecart");
                }
                _ => {}
            }
            entity_models()
                .into_iter()
                .map(|e| e.name)
                .find(|n| *n == type_path)
                .or_else(|| boat_model_name(type_path))
        }

        let set = EntityModelSet::load();
        let mut checked = 0usize;
        let mut mismatches = Vec::new();
        let paths: Vec<&str> = (0..lodestone_data::entity_types::TYPE_COUNT as i32)
            .filter_map(lodestone_data::entity_types::entity_type_name)
            .map(|name| name.strip_prefix("minecraft:").unwrap())
            .collect();
        // Sanity: the roster really is the full 158, not an accidentally-empty
        // iterator that would make this test vacuous.
        assert_eq!(paths.len(), lodestone_data::entity_types::TYPE_COUNT as usize);

        for &type_path in paths
            .iter()
            .chain(["player_wide", "player_slim", "oak_boat", "oak_chest_boat", "bamboo_raft", "bamboo_chest_raft"].iter())
        {
            checked += 1;
            let old = old_canonical_model_name(type_path);
            let new = canonical_model_name(type_path);
            if old != new {
                mismatches.push(format!("{type_path}: old={old:?} new={new:?}"));
                continue;
            }
            // And the second scan `EntityModelSet::get` replaced, keyed by
            // whatever name `canonical_model_name` resolved to.
            if let Some(name) = new {
                let via_index = set.get(name);
                let via_scan = set.models.iter().find(|(n, _)| *n == name).map(|(_, m)| m);
                if !std::ptr::eq(
                    via_index.map_or(std::ptr::null(), |m| m as *const _),
                    via_scan.map_or(std::ptr::null(), |m| m as *const _),
                ) {
                    mismatches.push(format!("{name}: EntityModelSet::get index/scan disagree"));
                }
            }
        }
        assert!(checked >= 158 + 6, "roster too small to be a real gate: {checked}");
        assert!(
            mismatches.is_empty(),
            "{} of {checked} paths disagree between the old linear scan and the new \
             indexed lookup:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }

    #[test]
    fn unknown_entity_type_has_no_model() {
        // Types the corpus genuinely has no mesh for — the renderer skips them
        // rather than substituting something mob-shaped.
        //
        // `arrow` used to be the headline entry here (issue #380): the physics was
        // modelled in `lodestone-entity`, no rig existed, and this assert was the
        // written record of that gap. It is kept as its **positive** form rather
        // than deleted, so the gap closing is visible in the diff of the test that
        // recorded it — and so a corpus edit that silently dropped the rig fails
        // here rather than only in an `#[ignore]`d pixel gate.
        //
        // **`experience_orb` is a different case from the other two, and it is
        // *not* a pinned absence waiting to be inverted.** An orb now draws — see
        // `experience_orb_mesh`/`experience_orb_matrix` above — as a camera-facing
        // sprite, which is not a cuboid part hierarchy and so is not a corpus
        // entry. Adding one would make `EntityModelSet::resolve` hand the mob pass
        // a rig for an entity that has none, which is a worse failure than the
        // nothing that used to draw. The precedent is `ITEM_ENTITY_TYPE_PATH`: a
        // dropped item reaches the render path and is deliberately absent here for
        // exactly the same reason. So this assertion is load-bearing *after* the
        // orb landed, not before it.
        assert!(model_for_type(EntityType::ExperienceOrb).is_none());
        assert!(model_for_type(EntityType::Tnt).is_none());
        // An empty/garbage path is not something `model_for_type` can even be
        // asked any more — there is no `EntityType` value for it — so the
        // equivalent negative belongs to the surviving `&str` boundary,
        // `canonical_model_name`, which every non-registry type path (still)
        // routes through.
        assert!(canonical_model_name("").is_none());
    }

    /// The other side of [`unknown_entity_type_has_no_model`]: the three
    /// projectiles issue #380 was about now resolve, and resolve to their **own**
    /// rigs.
    ///
    /// `arrow` and `spectral_arrow` deliberately *share* a builder
    /// (`ArrowRenderer` bakes one `ModelLayers.ARROW` for both), so equal geometry
    /// is correct there and the sheet is the only thing that must differ — the
    /// same drowned-vs-zombie shape as `variant_mobs_point_at_their_own_sheet`.
    /// `trident` is a genuine sibling with its own mesh, so its geometry must
    /// differ too.
    #[test]
    fn projectiles_resolve_to_their_own_rigs_and_sheets() {
        for entity_type in [EntityType::Arrow, EntityType::SpectralArrow, EntityType::Trident] {
            let ty = entity_type.path();
            let model =
                model_for_type(entity_type).unwrap_or_else(|| panic!("{ty} must have a corpus model"));
            assert_eq!(model.name, ty);
            assert_eq!(
                entity_texture_candidates(ty).len(),
                1,
                "{ty} should have exactly one sheet (no `_temperate` legacy fallback)"
            );
        }
        assert_eq!(
            entity_texture_candidates("arrow"),
            ["assets/minecraft/textures/entity/projectiles/arrow.png"]
        );
        assert_eq!(
            entity_texture_candidates("spectral_arrow"),
            ["assets/minecraft/textures/entity/projectiles/arrow_spectral.png"]
        );
        assert_eq!(
            entity_texture_candidates("trident"),
            ["assets/minecraft/textures/entity/trident/trident.png"]
        );
        // Same rig, different sheet for the two arrows; a different rig entirely
        // for the trident.
        let set = EntityModelSet::load();
        let arrow = set.get("arrow").expect("arrow mesh");
        let spectral = set.get("spectral_arrow").expect("spectral_arrow mesh");
        let trident = set.get("trident").expect("trident mesh");
        assert_eq!(arrow.vertices.len(), spectral.vertices.len());
        assert_ne!(
            arrow.vertices.len(),
            trident.vertices.len(),
            "trident must not be sharing the arrow rig"
        );
    }

    /// Every projectile in the corpus must be on the projectile placement, and
    /// **no mob may be**. The switch is one `match`; getting an entry wrong in
    /// either direction is silent — a mob on the projectile path loses its
    /// 1.501-block lift, an arrow on the mob path gains one.
    #[test]
    fn exactly_the_projectile_models_take_the_projectile_placement() {
        let mut projectiles = Vec::new();
        for entry in entity_models() {
            if projectile_pitch_offset_deg(entry.name).is_some() {
                projectiles.push(entry.name);
            }
        }
        // Corpus order, not alphabetical — this is the sweep's own iteration
        // order, so a rig moving in the corpus is visible here too.
        assert_eq!(
            projectiles,
            ["llama_spit", "arrow", "spectral_arrow", "trident"]
        );
        // A spot-check of the negative direction that names real mobs rather than
        // relying on the sweep above: these are the two families whose renderer is
        // most often assumed to be an `EntityRenderer`.
        for mob in ["pig", "player_wide", "zombie", "boat", "end_crystal"] {
            assert!(
                projectile_pitch_offset_deg(mob).is_none(),
                "{mob} must stay on the LivingEntityRenderer placement"
            );
        }
    }

    /// The placement itself, against hand-derived values rather than against
    /// [`projectile_model_matrix`]'s own output.
    ///
    /// The three things that would each be individually plausible and wrong:
    /// a `MODEL_FEET_OFFSET` lift, a mirror, and pitch about `X` instead of `Z`.
    #[test]
    fn projectile_placement_has_no_lift_no_mirror_and_pitches_about_z() {
        let pos = Vec3::new(3.0, 64.0, -7.0);
        let m = projectile_model_matrix(pos, 0.0, 0.0, 1.0);
        // (a) No lift: the model origin lands exactly on the reported position.
        let origin = m.transform_point3(Vec3::ZERO);
        assert!(
            (origin - pos).length() < 1e-5,
            "projectile origin {origin} is not the entity position {pos} — a \
             MODEL_FEET_OFFSET lift has crept in"
        );
        // (b) No mirror: determinant of the linear part is positive. The mob
        // matrix's `scale(-1,-1,1)` is +1 too (two flips cancel), so this is not
        // the discriminator for the flip — `arrow_pixels` is. It does catch a
        // single-axis mirror.
        let det = glam::Mat3::from_mat4(m).determinant();
        assert!(det > 0.0, "determinant {det} — geometry is mirrored");

        // (c) Pitch is about Z. At yaw 0 the shaft (model +X) must point +Z; at
        // pitch +45° it must rise. Hand-derived: Ry(-90) maps +X to +Z, and Rz(45)
        // first sends +X to (cos45, sin45, 0), so the tip ends at
        // (0, sin45, cos45) — i.e. equal parts up and forward, with **zero** x.
        let tip = |pitch: f32| {
            projectile_model_matrix(Vec3::ZERO, 0.0, pitch, 1.0)
                .transform_point3(Vec3::new(1.0, 0.0, 0.0))
        };
        let level = tip(0.0);
        assert!(
            (level - Vec3::new(0.0, 0.0, 1.0)).length() < 1e-5,
            "at yaw 0 / pitch 0 the shaft points {level}, not +Z"
        );
        let up = tip(45.0);
        let root_half = std::f32::consts::FRAC_1_SQRT_2;
        assert!(
            (up - Vec3::new(0.0, root_half, root_half)).length() < 1e-5,
            "at pitch 45 the shaft points {up}, not (0, √½, √½) — a rotation about \
             X instead of Z spins the arrow about its own axis and leaves this at +Z"
        );

        // (d) Yaw agrees with the *projectile* convention, which is not the mob
        // one. `Projectile.shoot` sets yRot = atan2(mx, mz), so the shaft must
        // point along (sin yaw, 0, cos yaw) — note the **+** sin, where a mob's
        // facing is (-sin yaw, 0, cos yaw).
        for yaw in [0.0f32, 37.0, 90.0, 180.0, -125.0] {
            let dir = projectile_model_matrix(Vec3::ZERO, yaw, 0.0, 1.0)
                .transform_point3(Vec3::new(1.0, 0.0, 0.0));
            let want = Vec3::new(
                yaw.to_radians().sin(),
                0.0,
                yaw.to_radians().cos(),
            );
            assert!(
                (dir - want).length() < 1e-5,
                "yaw {yaw}: shaft points {dir}, want {want}"
            );
        }

        // (e) The trident's +90° offset is what puts its own long axis (model -Y)
        // where the arrow's +X is: both must point the same way for the same
        // reported rotation.
        let arrow_dir = projectile_model_matrix(Vec3::ZERO, 20.0, 15.0, 1.0)
            .transform_point3(Vec3::new(1.0, 0.0, 0.0));
        let trident_dir = projectile_model_matrix(
            Vec3::ZERO,
            20.0,
            15.0 + projectile_pitch_offset_deg("trident").expect("trident is a projectile"),
            1.0,
        )
        .transform_point3(Vec3::new(0.0, -1.0, 0.0));
        assert!(
            (arrow_dir.normalize() - trident_dir.normalize()).length() < 1e-5,
            "trident tip {trident_dir} does not point where the arrow tip {arrow_dir} does"
        );
    }

    /// The whole point of the separate placement, stated as a delta a reviewer can
    /// check by eye: a projectile and a mob at the *same* reported position put
    /// their **model origin** [`MODEL_FEET_OFFSET`] apart in Y, and the arrow's
    /// tip in the opposite direction along X.
    ///
    /// # The sign is the other way round from the obvious guess
    ///
    /// Issue #380's investigation note — and this test's own first draft — said
    /// reusing the mob matrix would draw an arrow "1.5 blocks **low**". It draws
    /// it 1.5 blocks **high**, and the difference is the mirror, not the lift:
    /// `entity_model_matrix` is `T(feet) · Ry · S(-1,-1,1) · T(0, -1.501, 0)`, so
    /// the lift is applied *before* the Y negation and comes back out as
    /// `feet + 1.501`. That is exactly right for a mob — model space is Y-down and
    /// the model origin is a humanoid's shoulder line, ~1.5 blocks up — and
    /// exactly wrong for a rig authored the other way up. The first draft asserted
    /// `feet - 1.501` and failed at `65.501`; the control's premise was false in
    /// the safe-looking direction, which is why it is spelled out here rather than
    /// quietly corrected.
    #[test]
    fn reusing_the_mob_matrix_would_lift_an_arrow_and_reverse_it() {
        let pos = Vec3::new(0.0, 64.0, 0.0);
        let projectile = projectile_model_matrix(pos, 0.0, 0.0, 1.0).transform_point3(Vec3::ZERO);
        let mob = entity_model_matrix(pos, 0.0, 1.0).transform_point3(Vec3::ZERO);
        assert!(
            (projectile.y - pos.y).abs() < 1e-5,
            "the projectile placement moved the model origin off the reported \
             position: {} vs {}",
            projectile.y,
            pos.y
        );
        assert!(
            (mob.y - (pos.y + MODEL_FEET_OFFSET)).abs() < 1e-5,
            "mob model origin at {} — expected feet + {MODEL_FEET_OFFSET}. If this \
             fires, the control for this test is wrong, not the code under test",
            mob.y
        );
        assert!(
            mob.y - projectile.y > 1.5,
            "the two placements differ by only {} blocks in Y",
            mob.y - projectile.y
        );

        // The second half of the damage, which the Y offset alone would hide: the
        // two placements send the arrow's tip (model `+X`) different ways.
        //
        // Hand-derived. The projectile linear part is `Ry(yaw - 90)`, which sends
        // `+X` to `(sin yaw, 0, cos yaw)` — the motion direction. The mob linear
        // part is `Ry(180 - yaw) · S(-1, -1, 1)`, which sends it to
        // `(cos yaw, 0, sin yaw)`. Those are **reflections of each other across the
        // `x = z` diagonal**, not a fixed rotation apart — so they happen to agree
        // at `yaw = 45°` and are exactly opposed at `135°`. Asserting "the two
        // point opposite ways" at an arbitrary yaw is therefore a control whose
        // premise is false a quarter of the time; assert the relation instead, and
        // then name the yaw where it is worst.
        for yaw in [0.0f32, 90.0, 135.0, -20.0] {
            let (s, c) = (yaw.to_radians().sin(), yaw.to_radians().cos());
            let tip = Vec3::new(1.0, 0.0, 0.0);
            let good = projectile_model_matrix(Vec3::ZERO, yaw, 0.0, 1.0).transform_point3(tip);
            let m = entity_model_matrix(Vec3::ZERO, yaw, 1.0);
            let bad = m.transform_point3(tip) - m.transform_point3(Vec3::ZERO);
            assert!(
                (good - Vec3::new(s, 0.0, c)).length() < 1e-5,
                "yaw {yaw}: projectile tip {good}, want (sin, 0, cos)"
            );
            assert!(
                (bad - Vec3::new(c, 0.0, s)).length() < 1e-5,
                "yaw {yaw}: mob-placed tip {bad}, want (cos, 0, sin)"
            );
        }
        // The worst case, spelled out: at 135° the mob placement flies the arrow
        // exactly backwards.
        let good = projectile_model_matrix(Vec3::ZERO, 135.0, 0.0, 1.0)
            .transform_point3(Vec3::new(1.0, 0.0, 0.0));
        let m = entity_model_matrix(Vec3::ZERO, 135.0, 1.0);
        let bad =
            m.transform_point3(Vec3::new(1.0, 0.0, 0.0)) - m.transform_point3(Vec3::ZERO);
        assert!(
            good.normalize().dot(bad.normalize()) < -0.99,
            "at yaw 135 the placements should be opposed: {good} vs {bad}"
        );
    }

    #[test]
    fn every_drawable_model_has_a_texture_candidate() {
        // Now that the drawable set *is* the corpus, sweep the whole corpus:
        // every baked model gets uploaded with a sheet by the shell, so a model
        // with no candidate is a mob that draws as a flat placeholder colour.
        let mut checked = 0;
        for entry in entity_models() {
            let candidates = entity_texture_candidates(entry.name);
            assert!(
                !candidates.is_empty(),
                "model {:?} has no texture candidate",
                entry.name
            );
            for path in candidates {
                assert!(
                    path.starts_with("assets/minecraft/textures/entity/") && path.ends_with(".png"),
                    "candidate {path:?} for {:?} is not an entity sheet path",
                    entry.name
                );
            }
            checked += 1;
        }
        assert!(checked > 60, "only {checked} models swept");
        // The temperature-variant mobs keep their pre-26.2 sheet as a fallback,
        // so one binary works against both pack layouts.
        assert_eq!(
            entity_texture_candidates("pig"),
            [
                "assets/minecraft/textures/entity/pig/pig_temperate.png",
                "assets/minecraft/textures/entity/pig/pig.png",
            ]
        );
        // A name that is not a model resolves to nothing rather than a wrong sheet.
        // This was `"arrow"` until issue #380 landed the `ArrowRenderer` rig; the
        // assertion is kept (with a name that really is not a corpus entry) rather
        // than deleted, because "an unknown name yields no sheet" is the property
        // that stops a typo in the corpus from silently drawing a mob under some
        // other mob's skin. `arrow`'s own sheet is asserted positively in
        // `projectiles_resolve_to_their_own_rigs_and_sheets`.
        // `experience_orb` stays empty **after** the orb started drawing, and that
        // is not an oversight: this function answers "which sheet does a *corpus
        // rig* wear", and the orb has no rig. Its sheet is
        // [`EXPERIENCE_ORB_TEXTURE`], bound by the orb pass's own group 1 the same
        // way the mob-fire strip is — neither goes through this table.
        assert!(entity_texture_candidates("experience_orb").is_empty());
        assert!(entity_texture_candidates("").is_empty());
    }

    /// [`special_item_hover_lift`] must measure **all eight corners** of the rig's
    /// box through the display transform, not just `local_min`.
    ///
    /// The wrong hypothesis — transform `local_min` alone — agrees exactly whenever
    /// the transform has no rotation, which is true of most `ground` transforms. So
    /// the discriminating input is a transform that *does* rotate: a 90° turn about
    /// `x` sends the box's lowest posed point to the image of a **different**
    /// corner, and the two answers then differ by the box's own depth.
    ///
    /// Both hypotheses are computed here from the same box, and the test fails if
    /// they coincide at the chosen input.
    #[test]
    fn the_hover_lift_measures_the_whole_box_not_just_its_lowest_corner() {
        use lodestone_assets::DisplayTransform;
        // A tall, shallow box, so a rotation about `x` visibly changes which corner
        // is lowest: y spans 0.75 and z spans 0.25.
        let local_min = Vec3::new(-0.5, 0.0, -0.125);
        let local_max = Vec3::new(0.5, 0.75, 0.125);

        // No rotation, so the two hypotheses must agree. The expected value is
        // **not** `ITEM_MIN_HOVER_HEIGHT` alone, and that is the whole reason to
        // state it: `display_matrix` ends with vanilla's own
        // `translate(-0.5, -0.5, -0.5)` (`ItemTransform.apply`, taken even by
        // `NO_TRANSFORM`), which centres a `[0,1]³` model. A box whose bottom is at
        // local `y = 0` therefore poses at `y = -0.5`, and the lift is
        // `0.5 + ITEM_MIN_HOVER_HEIGHT`. Predicting the round `0.0625` here is the
        // mistake, and it fails in the direction that looks like a code bug.
        let flat = DisplayTransform::default();
        let lift = special_item_hover_lift(local_min, local_max, &flat);
        let expected = 0.5 + ITEM_MIN_HOVER_HEIGHT;
        assert!(
            (lift - expected).abs() < 1.0e-5,
            "an unrotated box whose bottom is at local y=0 poses at y=-0.5 through \
             the display centring, so the lift is {expected}; got {lift}"
        );

        // A 90° turn about `x`. `DisplayTransform`'s rotation is in degrees.
        let turned = DisplayTransform {
            rotation: [90.0, 0.0, 0.0],
            ..DisplayTransform::default()
        };
        let got = special_item_hover_lift(local_min, local_max, &turned);
        // The wrong hypothesis, evaluated at this input: `local_min` alone.
        let naive = -display_matrix(&turned).transform_point3(local_min).y + ITEM_MIN_HOVER_HEIGHT;
        assert!(
            (got - naive).abs() > 1.0e-3,
            "at this input the whole-box lift ({got}) and the lowest-corner lift \
             ({naive}) coincide, so this test measures nothing"
        );
        // And the answer is the real one: the lowest posed corner must land exactly
        // `ITEM_MIN_HOVER_HEIGHT` above zero once the lift is applied.
        let pose = display_matrix(&turned);
        let mut lowest = f32::INFINITY;
        for i in 0..8u8 {
            let corner = Vec3::new(
                if i & 1 == 0 { local_min.x } else { local_max.x },
                if i & 2 == 0 { local_min.y } else { local_max.y },
                if i & 4 == 0 { local_min.z } else { local_max.z },
            );
            lowest = lowest.min(pose.transform_point3(corner).y);
        }
        assert!(
            (lowest + got - ITEM_MIN_HOVER_HEIGHT).abs() < 1.0e-5,
            "lifted bottom lands at {}, expected {ITEM_MIN_HOVER_HEIGHT}",
            lowest + got
        );
    }

    /// A framed item sits just outside the attachment block's wall face, on the
    /// side the frame faces, and is drawn at vanilla's `0.5` rather than the
    /// framed *map*'s full block.
    ///
    /// # The magnitudes, not the signs
    ///
    /// A sign-only assertion ("it lifts toward `+z`") is satisfied by both
    /// candidate readings of this chain, because both put a south-facing frame's
    /// item somewhere in `+z`. The two disagree about *how far*, and that is the
    /// whole bug:
    ///
    /// | reading | offset from the attachment block centre |
    /// |---|---|
    /// | lift along the frame's own `+z`, which points **into** the wall | `-0.4375 · facing` — correct |
    /// | lift along the facing, i.e. the signs added | `+0.4375 · facing` — through the block |
    ///
    /// So each arm predicts the correct value *and* names the wrong one, per
    /// `CLAUDE.md`'s magnitude rule.
    ///
    /// The scale carries the same treatment: `prepare_framed_maps` draws its
    /// picture at `1.0`, and copying that number is the plausible mistake — it
    /// draws, it faces the right way, and it is twice the size of the frame
    /// around it.
    #[test]
    fn a_framed_item_sits_just_in_front_of_its_frame_and_is_drawn_half_size() {
        use lodestone_assets::DisplayTransform;
        let identity = DisplayTransform::default();
        let anchor = Vec3::new(4.0, 65.0, -9.0);
        let block_centre = anchor + Vec3::splat(0.5);
        // A block-entity rig lives in the block's own `[0,1]³` corner-origin space
        // (see `block_entity_placement_matrix`'s pivot of `(0.5, 0, 0.5)`), and
        // `display_matrix`'s trailing `translate(-0.5, -0.5, -0.5)` centres exactly
        // that space. So the point that maps to the pose origin is the rig's
        // **centre**, not its corner — probing `Vec3::ZERO` measures a corner and
        // reports a spurious offset along every axis the rotation touches.
        let centre_of = |m: Mat4| m.transform_point3(Vec3::splat(0.5));
        const CONTENT_LIFT: f32 = 0.4375;

        // Yaw 0 is south (`+z`) in vanilla.
        let pose = framed_item_matrix(anchor, 0.0, 0.0, 0, false, &identity);
        let centre = centre_of(pose);
        let south_expected = block_centre - Vec3::new(0.0, 0.0, CONTENT_LIFT);
        assert!(
            (centre - south_expected).length() < 1.0e-5,
            "a south-facing frame's item must sit at {south_expected}, got {centre}"
        );
        let north = centre_of(framed_item_matrix(anchor, 180.0, 0.0, 0, false, &identity));
        assert!(
            (north - (block_centre + Vec3::new(0.0, 0.0, CONTENT_LIFT))).length() < 1.0e-5,
            "a north-facing frame's item must mirror about the attachment block centre, got {north}"
        );
        // West and east are the arms that separate the frame's real `Direction`
        // from a bare `Ry(yaw)`: those two agree at yaw 0 and 180 and disagree in
        // sign at 90 and 270, so a corpus that only ever probes a north/south wall
        // cannot see the difference at all.
        let west = centre_of(framed_item_matrix(anchor, 90.0, 0.0, 0, false, &identity));
        assert!(
            (west - (block_centre + Vec3::new(CONTENT_LIFT, 0.0, 0.0))).length() < 1.0e-5,
            "yaw 90 is west, so the item must move to +x from the attachment centre; got {west}"
        );
        let east = centre_of(framed_item_matrix(anchor, 270.0, 0.0, 0, false, &identity));
        assert!(
            (east - (block_centre - Vec3::new(CONTENT_LIFT, 0.0, 0.0))).length() < 1.0e-5,
            "yaw 270 is east, so the item must move to -x from the attachment centre; got {east}"
        );

        // The scale: the rig's full `[0,1]` width must come out half a block.
        let left = pose.transform_point3(Vec3::new(0.0, 0.5, 0.5));
        let right = pose.transform_point3(Vec3::new(1.0, 0.5, 0.5));
        let width = (right - left).length();
        assert!(
            (width - 0.5).abs() < 1.0e-5,
            "a unit-wide rig must draw 0.5 blocks wide, got {width}"
        );
        assert!(
            (width - 1.0).abs() > 0.1,
            "the framed-map scale of 1.0 would also pass the facing assertions above"
        );

        // Pitch 90 is a **ceiling** frame, not a floor one: `ItemFrame.setDirection`
        // writes `xRot = -90 * direction.getAxisDirection().getStep()`, and
        // `Direction.DOWN`'s step is `-1`. So a pitch-90 frame faces down and its
        // item hangs *below* the entity. Reading it the other way round is the
        // mistake this arm exists to name, and it is invisible on a wall frame.
        let ceiling = centre_of(framed_item_matrix(anchor, 0.0, 90.0, 0, false, &identity));
        assert!(
            (ceiling - (block_centre + Vec3::new(0.0, CONTENT_LIFT, 0.0))).length() < 1.0e-5,
            "a pitch-90 frame faces DOWN, so its item must sit above the attachment centre; got {ceiling}"
        );
        let floor = centre_of(framed_item_matrix(anchor, 0.0, -90.0, 0, false, &identity));
        assert!(
            (floor - (block_centre - Vec3::new(0.0, CONTENT_LIFT, 0.0))).length() < 1.0e-5,
            "a pitch -90 frame faces UP, so its item must sit below the attachment centre; got {floor}"
        );
        assert!(
            (ceiling.z - block_centre.z).abs() < 1.0e-4,
            "a pitched frame must not also move along z, got z={}",
            ceiling.z
        );
    }

    /// The frame's **body** covers the wall face of the block it hangs in, and its
    /// back plate is against that wall rather than facing the room.
    ///
    /// The discriminating quantity is the back plate's `z`. `template_item_frame`'s
    /// back element spans `z = 15.5..16` in model units — local `0.96875..1.0` —
    /// so under [`item_frame_body_matrix`] it must land in the 1/32 of a block
    /// **behind** the block's centre plane, on the wall side. Dropping the
    /// `180 - yaw` mirrors it to the front, which still looks like a frame from a
    /// distance and hides the item behind its own backing.
    #[test]
    fn the_item_frame_body_puts_its_back_plate_against_the_wall() {
        // The actual spawn packet carries this attachment block position.
        let anchor = Vec3::new(4.0, 65.0, -9.0);
        let centre = anchor + Vec3::splat(0.5);
        let pose = item_frame_body_matrix(anchor, 0.0, 0.0);

        // Local (0.5, 0.5, 1.0) is the middle of the back plate's outer face.
        let back = pose.transform_point3(Vec3::new(0.5, 0.5, 1.0));
        assert!(
            (back - (centre - Vec3::new(0.0, 0.0, 0.5))).length() < 1.0e-5,
            "the back plate's outer face must land on the wall side of the cell \
             ({}), got {back}",
            centre - Vec3::new(0.0, 0.0, 0.5)
        );
        // And the opposite face is the one the room sees.
        let front = pose.transform_point3(Vec3::new(0.5, 0.5, 0.0));
        assert!(
            front.z > back.z,
            "the frame is drawn back-to-front: back={back}, front={front}"
        );

        // The body is a full block across, unrotated in its own plane: a unit-wide
        // model must measure one block, which is what separates it from the item's
        // own half scale.
        let left = pose.transform_point3(Vec3::new(0.0, 0.5, 1.0));
        let right = pose.transform_point3(Vec3::new(1.0, 0.5, 1.0));
        assert!(
            ((right - left).length() - 1.0).abs() < 1.0e-5,
            "the frame body draws at 1:1, got {}",
            (right - left).length()
        );
    }

    /// An item frame's add-entity packet carries its attachment `BlockPos`, not
    /// its offset entity centre. The renderer therefore begins at that block's
    /// centre after its dispatch/render-offset pair cancels.
    #[test]
    fn item_frame_space_centres_the_integer_packet_anchor() {
        let anchor = Vec3::new(4.0, 65.0, -9.0);
        let expected = anchor + Vec3::splat(0.5);
        for (yaw, pitch) in [
            (0.0_f32, 0.0_f32),
            (90.0, 0.0),
            (180.0, 0.0),
            (270.0, 0.0),
            (0.0, -90.0),
            (0.0, 90.0),
        ] {
            let origin = item_frame_space(anchor, yaw, pitch).transform_point3(Vec3::ZERO);
            assert!(
                (origin - expected).length() < 1.0e-5,
                "yaw {yaw}, pitch {pitch}: packet anchor {anchor} must centre frame space at {expected}, got {origin}"
            );
        }
    }

    /// The back of the one-block frame body is flush with the attachment block's
    /// wall face for every possible `Direction`.
    #[test]
    fn item_frame_body_back_plate_lands_on_the_packet_anchors_wall_face() {
        let anchor = Vec3::new(4.0, 65.0, -9.0);
        for (yaw, pitch) in [
            (0.0_f32, 0.0_f32),
            (90.0, 0.0),
            (180.0, 0.0),
            (270.0, 0.0),
            (0.0, -90.0),
            (0.0, 90.0),
        ] {
            let facing = item_frame_facing_step(yaw, pitch);
            let back = item_frame_body_matrix(anchor, yaw, pitch)
                .transform_point3(Vec3::new(0.5, 0.5, 1.0));
            let expected = anchor + Vec3::splat(0.5) - facing * 0.5;
            assert!(
                (back - expected).length() < 1.0e-5,
                "yaw {yaw}, pitch {pitch}: body back must lie on attachment wall {expected}, got {back}"
            );
        }
    }

    #[test]
    fn item_frame_culling_box_matches_wall_offset_dimensions_and_renderer_inflate() {
        let anchor = Vec3::new(1965.0, 73.0, 3806.0);
        let (map_min, map_max) = item_frame_culling_aabb(anchor, 90.0, 0.0, true);
        assert!((map_min - Vec3::new(1965.4375, 72.5, 3805.5)).length() < 1.0e-5);
        assert!((map_max - Vec3::new(1966.5, 74.5, 3807.5)).length() < 1.0e-5);

        let (plain_min, plain_max) = item_frame_culling_aabb(anchor, 90.0, 0.0, false);
        assert!((plain_min - Vec3::new(1965.4375, 72.625, 3805.625)).length() < 1.0e-5);
        assert!((plain_max - Vec3::new(1966.5, 74.375, 3807.375)).length() < 1.0e-5);
    }

    /// [`item_frame_facing_step`] is the frame's real `Direction`, and the four
    /// horizontal yaws are the inputs that prove it.
    ///
    /// The wrong hypothesis is a plain `Ry(yaw)` applied to `+z` — the expression
    /// the framed-*map* path used to lift by. It agrees with the truth at yaw `0`
    /// and `180` and is the exact opposite at `90` and `270`, so any gate probing
    /// only a north or south wall passes under both.
    #[test]
    fn the_item_frame_facing_step_is_the_frames_own_direction() {
        for (yaw, expected) in [
            (0.0_f32, Vec3::new(0.0, 0.0, 1.0)),
            (90.0, Vec3::new(-1.0, 0.0, 0.0)),
            (180.0, Vec3::new(0.0, 0.0, -1.0)),
            (270.0, Vec3::new(1.0, 0.0, 0.0)),
        ] {
            let got = item_frame_facing_step(yaw, 0.0);
            assert!(
                (got - expected).length() < 1.0e-5,
                "yaw {yaw}: expected {expected}, got {got}"
            );
        }
        // `Direction.DOWN` has `getAxisDirection().getStep() == -1`, so its
        // `xRot` is `+90` — a pitch-90 frame faces down.
        assert!(
            (item_frame_facing_step(0.0, 90.0) - Vec3::new(0.0, -1.0, 0.0)).length() < 1.0e-5,
            "pitch 90 must face DOWN, got {}",
            item_frame_facing_step(0.0, 90.0)
        );
        assert!(
            (item_frame_facing_step(0.0, -90.0) - Vec3::new(0.0, 1.0, 0.0)).length() < 1.0e-5,
            "pitch -90 must face UP, got {}",
            item_frame_facing_step(0.0, -90.0)
        );
    }

    /// The eight-step in-frame rotation turns the item about the frame's own
    /// normal, and a full eight steps is the identity.
    ///
    /// A quarter turn is the discriminating input: two steps of `45°` must move a
    /// point off the frame's centre line by the full radius, which a `rotation`
    /// that never reaches the matrix (the state before it was decoded) cannot do.
    #[test]
    fn the_framed_item_rotation_turns_about_the_frames_normal() {
        use lodestone_assets::DisplayTransform;
        let identity = DisplayTransform::default();
        let feet = Vec3::new(4.0, 65.0, -9.0);
        // A point one unit up the rig's own +y, so a rotation about z moves it.
        let probe = Vec3::new(0.5, 1.5, 0.5);
        let at = |rotation: u8| {
            framed_item_matrix(feet, 0.0, 0.0, rotation, false, &identity).transform_point3(probe)
        };

        let up = at(0);
        let quarter = at(2);
        let half = at(4);
        // Two steps = 90°: what was straight up is now straight sideways.
        assert!(
            (quarter.y - up.y).abs() > 0.4,
            "two rotation steps must move the probe off vertical: {up} -> {quarter}"
        );
        assert!(
            (half.y - up.y).abs() > 0.9,
            "four rotation steps must invert it: {up} -> {half}"
        );
        // Eight steps is a full turn, and `% 8` makes 8 and 0 the same pose.
        assert!(
            (at(8) - up).length() < 1.0e-5,
            "rotation 8 must equal rotation 0, got {} vs {up}",
            at(8)
        );
    }

    /// An invisible frame lifts its contents further, because there is no body to
    /// hold them clear of the wall.
    ///
    /// `0.5` against `0.4375` — a 1/16 difference, which is exactly the sort of
    /// magnitude a direction-only assertion cannot see.
    #[test]
    fn an_invisible_frame_lifts_its_contents_the_extra_sixteenth() {
        assert!((item_frame_content_lift(false) - 0.4375).abs() < 1.0e-6);
        assert!((item_frame_content_lift(true) - 0.5).abs() < 1.0e-6);
        // The consequence in world space: an invisible frame's item reaches the
        // attachment block wall; the visible item stays one sixteenth outside it.
        use lodestone_assets::DisplayTransform;
        let identity = DisplayTransform::default();
        let feet = Vec3::new(4.0, 65.0, -9.0);
        let centre_of = |m: Mat4| m.transform_point3(Vec3::splat(0.5));
        let visible = centre_of(framed_item_matrix(feet, 0.0, 0.0, 0, false, &identity));
        let invisible = centre_of(framed_item_matrix(feet, 0.0, 0.0, 0, true, &identity));
        assert!(
            ((visible.z - invisible.z) - 0.0625).abs() < 1.0e-5,
            "the two lifts must differ by exactly 1/16, got {} vs {}",
            visible.z,
            invisible.z
        );
    }

    /// The orb sprite cell is a **bucketed** lookup, and the only inputs that can
    /// observe that are ones straddling a threshold.
    ///
    /// The wrong hypothesis this discriminates against is a *linear* map from value
    /// to cell — the shape a reader would reach for, and the shape a single-value
    /// gate cannot rule out. Both hypotheses are evaluated at every input below and
    /// the test fails if they ever agree there, so the inputs cannot silently stop
    /// discriminating.
    #[test]
    fn orb_icon_is_bucketed_and_constant_inside_a_bucket() {
        // The linear reading someone would write instead: eleven cells spread
        // evenly over the top denomination.
        let linear = |value: i32| -> u32 {
            ((value.max(0) as u32) * (EXPERIENCE_ORB_ICON_COUNT - 1) / 2477)
                .min(EXPERIENCE_ORB_ICON_COUNT - 1)
        };
        // `(value, expected cell, must discriminate)`. Every pair is either side of
        // a threshold, so a version that shifted one boundary by one fails.
        //
        // The three `false` rows are the ladder's **endpoints**, where the two
        // hypotheses provably agree and no choice of input can separate them: any
        // monotone map from value to cell sends the bottom of the range to cell 0
        // and the top to cell 10. They are kept as correctness assertions (a
        // transcription that dropped the `>= 2477` arm still fails) and excluded
        // from the discrimination requirement rather than quietly satisfying it —
        // which is what the coincidence check below exists to force.
        let cases: [(i32, u32, bool); 14] = [
            (0, 0, false),
            (2, 0, false),
            (3, 1, true),
            (6, 1, true),
            (7, 2, true),
            (16, 2, true),
            (17, 3, true),
            (36, 3, true),
            (37, 4, true),
            (73, 5, true),
            (149, 6, true),
            (307, 7, true),
            (617, 8, true),
            (2477, 10, false),
        ];
        let mut mismatches = Vec::new();
        let mut coincidences = Vec::new();
        for (value, expected, discriminating) in cases {
            let got = experience_orb_icon(value);
            if got != expected {
                mismatches.push(format!("value {value}: expected cell {expected}, got {got}"));
            }
            // The corollary that makes this a test rather than a re-run of the
            // code: an input where the bucketed and the linear answer coincide
            // measures nothing.
            if discriminating && linear(value) == expected {
                coincidences.push(format!(
                    "value {value} cannot discriminate — the linear hypothesis also says {expected}"
                ));
            }
        }
        // And the reciprocal, so the eleven `true` rows cannot all quietly become
        // endpoint-like if someone rewrites `linear`: at least ten of them must
        // really separate the two hypotheses.
        let separating = cases
            .iter()
            .filter(|(value, expected, discriminating)| *discriminating && linear(*value) != *expected)
            .count();
        assert!(
            separating >= 10,
            "only {separating} inputs separate the bucketed and linear hypotheses"
        );
        assert!(mismatches.is_empty(), "{mismatches:#?}");
        assert!(coincidences.is_empty(), "{coincidences:#?}");
        // And the property a per-value table could satisfy while still being
        // linear: the cell must be *constant* across a whole bucket. 7..=16 is ten
        // consecutive values that all draw cell 2.
        let inside: Vec<u32> = (7..=16).map(experience_orb_icon).collect();
        assert_eq!(inside, vec![2; 10], "cell 2's bucket is not flat: {inside:?}");
    }

    /// Two cells of the sheet must address two *different* 16-pixel squares of the
    /// 64-pixel sheet, and the row must advance every four cells.
    ///
    /// The failure this rules out is the one a "does it draw?" check cannot see: a
    /// mesh that always samples cell 0 draws a perfectly plausible orb for every
    /// value in the game.
    #[test]
    fn orb_cells_tile_the_sheet_by_row_and_column() {
        // Cell 0 is the top-left 16×16 square: u and v both 0..0.25.
        assert_eq!(
            experience_orb_cell_uvs(0),
            [[0.0, 0.25], [0.25, 0.25], [0.25, 0.0], [0.0, 0.0]]
        );
        // Cell 3 is the last column of row 0 — same v, u shifted three cells.
        assert_eq!(experience_orb_cell_uvs(3)[0], [0.75, 0.25]);
        // Cell 4 wraps to row 1: u back to 0, v advanced one cell.
        assert_eq!(experience_orb_cell_uvs(4)[0], [0.0, 0.5]);
        // Cell 10 is row 2, column 2 — the highest cell `experience_orb_icon` returns.
        assert_eq!(experience_orb_cell_uvs(10)[0], [0.5, 0.75]);
        // Every cell must fit inside the sheet, or the sampler clamps and two
        // different values draw the same edge texels.
        for icon in 0..EXPERIENCE_ORB_ICON_COUNT {
            for [u, v] in experience_orb_cell_uvs(icon) {
                assert!((0.0..=1.0).contains(&u), "cell {icon} u {u} off-sheet");
                assert!((0.0..=1.0).contains(&v), "cell {icon} v {v} off-sheet");
            }
        }
    }

    /// The orb quad sits **above** its own origin, and its vertical span is
    /// vanilla's after the scale and the lift.
    ///
    /// Predicted from the record's own constants rather than eyeballed: local
    /// `y ∈ [-0.25, 0.75]`, scaled by `0.3` and lifted `0.1`, is
    /// `[0.025, 0.325]`. The wrong hypothesis — a quad centred on its origin, which
    /// is what "billboard" suggests — would span `[-0.05, 0.25]` and bury the
    /// bottom sixth of every orb in the floor.
    #[test]
    fn orb_quad_sits_above_the_ground_after_the_scale_and_lift() {
        let feet = Vec3::new(4.0, 65.0, -9.0);
        // Identity orientation: a camera looking down -Z with no roll, which is
        // what `camera_orientation` returns for the default view. The vertical
        // extent is then the local one, scaled and lifted.
        let pose = experience_orb_matrix(feet, Mat4::IDENTITY);
        let bottom = pose.transform_point3(Vec3::new(0.0, -0.25, 0.0));
        let top = pose.transform_point3(Vec3::new(0.0, 0.75, 0.0));
        assert!(
            (bottom.y - (65.0 + 0.025)).abs() < 1.0e-5,
            "bottom at {}, expected {}",
            bottom.y,
            65.0 + 0.025
        );
        assert!(
            (top.y - (65.0 + 0.325)).abs() < 1.0e-5,
            "top at {}, expected {}",
            top.y,
            65.0 + 0.325
        );
        // The centred hypothesis, evaluated at this input and required to differ.
        assert!(
            (bottom.y - (65.0 - 0.05)).abs() > 1.0e-3,
            "a centred quad would also pass this"
        );
        // The orb is a fifth of a block wide at 0.3 scale: `x ∈ [-0.15, 0.15]`.
        let left = pose.transform_point3(Vec3::new(-0.5, 0.0, 0.0));
        assert!((left.x - (4.0 - 0.15)).abs() < 1.0e-5, "left at {}", left.x);
    }

    /// The `+7` block-light boost touches the block nibble only.
    ///
    /// A version that boosted the packed byte as a whole (`packed + 7`) agrees with
    /// this one on any byte whose block nibble is below 9 — so the discriminating
    /// input is a **saturating** one, where the correct answer clamps the nibble at
    /// 15 and the wrong one carries into the sky nibble.
    #[test]
    fn orb_light_boosts_only_the_block_nibble() {
        // Pitch black: block 0 -> 7, sky untouched.
        assert_eq!(experience_orb_light(0x00), 0x07);
        // Sky 15, block 0: the boost must not touch the sky nibble.
        assert_eq!(experience_orb_light(0xF0), 0xF7);
        // The discriminating case. Block 10 saturates at 15; a whole-byte `+7`
        // would give 0x51 — a *lower* sky level and a block level of 1.
        assert_eq!(experience_orb_light(0x4A), 0x4F);
        assert_ne!(experience_orb_light(0x4A), 0x4A_u8.wrapping_add(7));
    }

    /// Vanilla's orb colour pins green at full and modulates red far harder than
    /// blue. The wrong hypothesis is a symmetric hue cycle (equal amplitudes),
    /// which is what a transcription that dropped the `0.1` would produce.
    #[test]
    fn orb_tint_pins_green_and_modulates_red_ten_times_harder_than_blue() {
        // Sweep a whole `phase` period — `age/2`, so 4π ticks.
        let mut greens = Vec::new();
        let mut reds = Vec::new();
        let mut blues = Vec::new();
        for tick in 0..64 {
            let [r, g, b] = experience_orb_tint(tick as f32 / 4.0);
            greens.push(g);
            reds.push(r);
            blues.push(b);
        }
        assert!(greens.iter().all(|g| *g == 255), "green must never modulate");
        let red_span = reds.iter().max().copied().unwrap_or(0) - reds.iter().min().copied().unwrap_or(0);
        let blue_span =
            blues.iter().max().copied().unwrap_or(0) - blues.iter().min().copied().unwrap_or(0);
        // `0.5` vs `0.1` amplitude over the same `sin` range: 255 vs 51.
        assert!(red_span > 240, "red span {red_span}, expected the full 0.5 swing");
        assert!(
            (40..=60).contains(&blue_span),
            "blue span {blue_span}, expected ~51 from the 0.1 amplitude"
        );
        // The symmetric-amplitude hypothesis, evaluated here and required to fail.
        assert!(
            blue_span * 4 < red_span,
            "equal amplitudes would pass every assertion above"
        );
    }

    /// The other half of the drowned defect: even with its own mesh, a drowned
    /// wearing `zombie.png` still reads as an ordinary zombie. The path is
    /// derived from the corpus entry, so this pins the derivation, not a table.
    #[test]
    fn variant_mobs_point_at_their_own_sheet() {
        assert_eq!(
            entity_texture_candidates("drowned"),
            ["assets/minecraft/textures/entity/zombie/drowned.png"]
        );
        assert_eq!(
            entity_texture_candidates("husk"),
            ["assets/minecraft/textures/entity/zombie/husk.png"]
        );
        assert_eq!(
            entity_texture_candidates("stray"),
            ["assets/minecraft/textures/entity/skeleton/stray.png"]
        );
    }

    /// **The variant-resolver gate.** A wire variant must select the *breed's* own
    /// sheet, not the model's default.
    ///
    /// # The discriminating requirement
    ///
    /// "Returns `Some`" is satisfied by a resolver that hands back
    /// `default_path()`, which is exactly the behaviour this replaces — so every
    /// assertion below is stated as a **difference from the default**, and the nine
    /// breeds are required to be nine *distinct* sheets so no selector can be a
    /// constant function.
    ///
    /// # Where the expected values come from
    ///
    /// `WolfVariants.register` (26.2, `.cache/mc/26.2/src`), which is both halves of
    /// the answer and neither is guessable from the other: it builds the wild sheet
    /// as `"entity/wolf/" + fileName`, and it registers `fileName` against a
    /// **registry key** — `register(context, ASHEN, "wolf_ashen", …)`, with
    /// `ASHEN = createKey("ashen")`. So the wire's `minecraft:ashen` holder maps to
    /// the stem `wolf_ashen`, and `PALE` maps to the bare `wolf` rather than to
    /// `wolf_pale`, which is the one entry a uniform `"wolf_" + key` rule would get
    /// wrong. That asymmetry is asserted explicitly below.
    #[test]
    fn a_wire_variant_selects_the_breeds_own_sheet_not_the_models_default() {
        use lodestone_model::EntityVariant as Wire;

        let keyed = |path: &str| Wire::Keyed(format!("minecraft:{path}").parse().unwrap());
        let default = entity_models()
            .into_iter()
            .find(|e| e.name == "wolf")
            .expect("wolf is in the corpus")
            .texture
            .default_path();
        assert_eq!(default, "entity/wolf/wolf", "the default is the pale sheet");

        // (registry key, stem) straight off `WolfVariants.bootstrap`.
        let vanilla: [(&str, &str); 9] = [
            ("pale", "wolf"),
            ("spotted", "wolf_spotted"),
            ("snowy", "wolf_snowy"),
            ("black", "wolf_black"),
            ("ashen", "wolf_ashen"),
            ("rusty", "wolf_rusty"),
            ("woods", "wolf_woods"),
            ("chestnut", "wolf_chestnut"),
            ("striped", "wolf_striped"),
        ];

        let mut wrong = Vec::new();
        let mut seen: Vec<&'static str> = Vec::new();
        for (key, stem) in vanilla {
            let want = format!("entity/wolf/{stem}");
            match entity_variant_sheet("wolf", &keyed(key)) {
                Some(got) => {
                    if got != want {
                        wrong.push(format!("{key}: want {want:?}, got {got:?}"));
                    }
                    seen.push(got);
                }
                None => wrong.push(format!("{key}: resolved to None")),
            }
        }
        // Eight of the nine must differ from the default; `pale` legitimately *is*
        // the default, which is why this is a count and not a blanket `!=`.
        let non_default = seen.iter().filter(|s| **s != default).count();
        if non_default != 8 {
            wrong.push(format!(
                "only {non_default} of 9 breeds resolved away from the default sheet \
                 — a resolver returning `default_path()` would score 0"
            ));
        }
        seen.sort_unstable();
        seen.dedup();
        if seen.len() != 9 {
            wrong.push(format!(
                "the nine breeds collapsed to {} distinct sheets, so the selector is \
                 not reading the coat",
                seen.len()
            ));
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// The climate axis, on the same wire shape. `pig`/`cow`/`chicken` all carry it,
    /// and all three currently draw `_temperate` for every animal in the world.
    #[test]
    fn a_climate_variant_selects_the_cold_and_warm_sheets() {
        use lodestone_model::EntityVariant as Wire;

        let keyed = |path: &str| Wire::Keyed(format!("minecraft:{path}").parse().unwrap());
        let mut wrong = Vec::new();
        for model in ["pig", "cow", "chicken"] {
            for climate in ["temperate", "cold", "warm"] {
                let want = format!("entity/{model}/{model}_{climate}");
                match entity_variant_sheet(model, &keyed(climate)) {
                    Some(got) if got == want => {}
                    other => wrong.push(format!("{model}/{climate}: want {want:?}, got {other:?}")),
                }
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// **The controls.** Everything the resolver must decline, so "it returns a
    /// sheet" is demonstrably not unconditional.
    ///
    /// A foreign namespace is the one worth stating: a data pack's `mypack:ashen` is
    /// a different holder with no vanilla sheet, and a path-only comparison would
    /// hand it `wolf_ashen`.
    #[test]
    fn control_the_resolver_declines_what_it_cannot_map() {
        use lodestone_model::EntityVariant as Wire;

        let keyed = |id: &str| Wire::Keyed(id.parse().unwrap());
        let mut wrong = Vec::new();
        for (what, model, variant) in [
            (
                "a model with no variant axis",
                "zombie",
                keyed("minecraft:ashen"),
            ),
            ("an unknown breed key", "wolf", keyed("minecraft:nonesuch")),
            ("a foreign namespace", "wolf", keyed("mypack:ashen")),
            (
                "a wrong-axis variant (sheep dye on a wolf)",
                "wolf",
                Wire::Dyed {
                    color: 4,
                    sheared: false,
                },
            ),
            (
                "a model not in the corpus at all",
                "nonesuch",
                keyed("minecraft:ashen"),
            ),
        ] {
            if let Some(got) = entity_variant_sheet(model, &variant) {
                wrong.push(format!("{what}: expected None, got {got:?}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// **The tame gap, pinned as a fact rather than left as a comment.**
    ///
    /// [`entity_variant_sheet`] (the plain, 2-argument entry point every
    /// production caller still uses) has no way to *receive* a tame bit, so
    /// it must always pin `WolfState::Wild` — not because the wire carries
    /// nothing (it does, see [`entity_variant_sheet`]'s own doc), but because
    /// nothing upstream of its callers folds that wire field into anything
    /// this function's signature can see. This asserts that deliberate
    /// pinning; if it starts failing, `entity_variant_sheet` gained a way to
    /// see the bit without gaining a parameter for it, which would be a
    /// second, undocumented path — fix the signature instead of this
    /// assertion.
    ///
    /// The corpus already knows the tame sheet, and the assertion names it, so the
    /// gate cannot pass by the tame path merely being unimplemented in the corpus.
    /// 26.2 also ships a `_baby` axis (`WolfVariants.register` builds six
    /// identifiers, not three) which `WolfState` does not model at all.
    #[test]
    fn a_tamed_wolf_still_resolves_to_its_wild_sheet_through_the_plain_entry_point() {
        use lodestone_assets::entity::{EntityVariant as Axis, WolfCoat, WolfState};
        use lodestone_model::EntityVariant as Wire;

        let entry = entity_models()
            .into_iter()
            .find(|e| e.name == "wolf")
            .expect("wolf is in the corpus");
        let wild = entry.texture.resolve(Axis::Wolf {
            coat: WolfCoat::Ashen,
            state: WolfState::Wild,
        });
        let tame = entry.texture.resolve(Axis::Wolf {
            coat: WolfCoat::Ashen,
            state: WolfState::Tame,
        });
        assert_eq!(
            (wild, tame),
            ("entity/wolf/wolf_ashen", "entity/wolf/wolf_ashen_tame"),
            "the corpus must model both states, or the claim below is vacuous"
        );

        let got = entity_variant_sheet(
            "wolf",
            &Wire::Keyed("minecraft:ashen".parse().unwrap()),
        );
        assert_eq!(
            got,
            Some(wild),
            "the plain entry point has no tame parameter, so it must resolve wild"
        );
        assert_ne!(
            got,
            Some(tame),
            "if this now fails, `entity_variant_sheet` gained a tame source of its \
             own rather than `entity_variant_sheet_for` gaining a caller — thread \
             the ECS component through instead of changing this function's pin"
        );
    }

    /// The other half of the tame gap: [`entity_variant_sheet_for`] — the
    /// function that *can* see a tame bit — actually uses it. This is the
    /// positive proof the render-side fix works; what remains (an ECS
    /// component and a shell call site, both outside this crate) is named in
    /// [`entity_variant_sheet`]'s own doc comment.
    #[test]
    fn entity_variant_sheet_for_resolves_the_tame_sheet_when_told_the_wolf_is_tamed() {
        use lodestone_model::EntityVariant as Wire;

        let key = Wire::Keyed("minecraft:ashen".parse().unwrap());
        let wild = entity_variant_sheet_for("wolf", &key, false);
        let tame = entity_variant_sheet_for("wolf", &key, true);
        assert_eq!(wild, Some("entity/wolf/wolf_ashen"));
        assert_eq!(tame, Some("entity/wolf/wolf_ashen_tame"));
        assert_ne!(wild, tame, "the tame parameter must actually change the sheet");
        // A non-wolf model ignores the parameter entirely, matching
        // `entity_variant_sheet`'s existing per-model table (only `"wolf"`
        // has a `WolfState` axis at all).
        let pig_key = Wire::Keyed("minecraft:cold".parse().unwrap());
        assert_eq!(
            entity_variant_sheet_for("pig", &pig_key, false),
            entity_variant_sheet_for("pig", &pig_key, true),
        );
    }

    #[test]
    fn mesh_has_four_verts_and_six_indices_per_quad() {
        let mesh = pig_mesh();
        assert!(mesh.quad_count() > 0, "pig must produce geometry");
        assert_eq!(mesh.vertices.len(), mesh.quad_count() * 4);
        assert_eq!(mesh.indices.len(), mesh.quad_count() * 6);
        // Matches the underlying bake exactly (one quad per baked quad).
        let baked =
            lodestone_assets::entity::bake_entity(&lodestone_assets::entity_models::pig_model());
        assert_eq!(mesh.quad_count(), baked.len());
    }

    #[test]
    fn mesh_indices_are_all_in_range() {
        let mesh = pig_mesh();
        let n = mesh.vertices.len() as u32;
        assert!(mesh.indices.iter().all(|&i| i < n));
    }

    #[test]
    fn model_matrix_stands_the_mob_upright_at_its_feet() {
        // A humanoid: head cube top is model y = -8/16 = -0.5, feet reach
        // y = 24/16 = 1.5 (Y-down). After placement, feet ≈ world feet, head above.
        let feet = Vec3::new(10.0, 64.0, -20.0);
        let m = entity_model_matrix(feet, 0.0, 1.0);

        let model_feet = m.transform_point3(Vec3::new(0.0, 1.5, 0.0));
        let model_head = m.transform_point3(Vec3::new(0.0, -0.5, 0.0));

        // Feet land on the ground plane (within a couple of cm of the offset).
        assert!(
            (model_feet.y - feet.y).abs() < 0.05,
            "feet should sit at the entity position, got {model_feet:?}",
        );
        // Head is clearly above the feet: upright, not upside-down.
        assert!(
            model_head.y > model_feet.y + 1.5,
            "head must be above feet (upright), head={model_head:?} feet={model_feet:?}",
        );
        // Horizontal position stays at the feet column.
        assert!((model_feet.x - feet.x).abs() < 1e-4);
        assert!((model_feet.z - feet.z).abs() < 1e-4);
    }

    #[test]
    fn model_matrix_preserves_handedness() {
        // scale(-1,-1,1) has det +1, so combined with rotation/translation the
        // transform must preserve winding (positive determinant).
        let m = entity_model_matrix(Vec3::new(1.0, 2.0, 3.0), 37.0, 1.0);
        assert!(
            m.determinant() > 0.0,
            "det must stay positive so back-face culling remains valid, got {}",
            m.determinant(),
        );
    }

    #[test]
    fn yaw_rotates_about_the_vertical_axis() {
        // A point offset in +X (model) with feet at origin: under a 90° body-yaw
        // change it must swing in the horizontal plane while its height is
        // unchanged (rotation is about Y only).
        let feet = Vec3::ZERO;
        let probe = Vec3::new(1.0, 0.5, 0.0);
        let a = entity_model_matrix(feet, 0.0, 1.0).transform_point3(probe);
        let b = entity_model_matrix(feet, 90.0, 1.0).transform_point3(probe);
        assert!(
            (a.y - b.y).abs() < 1e-5,
            "yaw must not change height: {a:?} vs {b:?}",
        );
        let horizontal = ((a.x - b.x).powi(2) + (a.z - b.z).powi(2)).sqrt();
        assert!(
            horizontal > 0.5,
            "a 90° yaw change must move the point horizontally: {a:?} vs {b:?}",
        );
    }

    #[test]
    fn scale_shrinks_the_model_about_the_feet() {
        let feet = Vec3::new(0.0, 0.0, 0.0);
        let full = entity_model_matrix(feet, 0.0, 1.0).transform_point3(Vec3::new(0.0, -0.5, 0.0));
        let baby = entity_model_matrix(feet, 0.0, 0.5).transform_point3(Vec3::new(0.0, -0.5, 0.0));
        // Feet stay near the ground for both; the head of the scaled mob is lower.
        assert!(baby.y < full.y, "scaled-down mob's head must be lower");
        assert!(baby.y > feet.y, "scaled mob still stands above its feet");
    }

    /// A zombie's resting arms stick out ~0.75 blocks in front of it, so its
    /// culling box has to be drawn around the mob *as posed*, not around a mob
    /// standing to attention. `EntityMesh::from_named_model` gets that by
    /// choosing the arm rig before taking the local bounds; if it did not, the
    /// error would be invisible until a zombie clipped out at the screen edge.
    #[test]
    fn a_zombies_local_bounds_include_its_outstretched_arms() {
        let plain = EntityMesh::from_model(&lodestone_assets::entity_models::zombie_model());
        let zombie = EntityMesh::from_named_model(
            "zombie",
            &lodestone_assets::entity_models::zombie_model(),
        );
        assert_eq!(
            humanoid_arms_for("zombie"),
            crate::entity_anim::HumanoidArms::Zombie
        );
        // Every model that calls `animateZombieArms`, so the set is not "zombie
        // plus whatever was remembered". `zombified_piglin` was the one missing.
        for name in ["husk", "drowned", "zombie_villager", "zombified_piglin"] {
            assert_eq!(
                humanoid_arms_for(name),
                crate::entity_anim::HumanoidArms::Zombie,
                "{name}'s model calls AnimationUtils.animateZombieArms"
            );
        }
        // `GiantMobRenderer` uses a bare `HumanoidModel`, so a giant's arms hang.
        assert_eq!(
            humanoid_arms_for("giant"),
            crate::entity_anim::HumanoidArms::Swinging
        );
        // Model -Z is the mob's facing, so the arms extend the *minimum* Z.
        // The arm cube ends 10 texels (0.625 blocks) down from its pivot, so at
        // -80° it reaches ~0.63 blocks forward against an arms-down torso whose
        // frontmost point is the 0.28-block hat overlay.
        assert!(
            zombie.local_min.z < plain.local_min.z - 0.3,
            "the zombie's bounds reach {} forward against an arms-down {} — the rig was not \
             applied before the AABB was taken",
            zombie.local_min.z,
            plain.local_min.z
        );

        // And the bound must actually hold for every posed vertex.
        let feet = Vec3::new(5.0, 70.0, 5.0);
        let inst = EntityInstance::new("zombie", &zombie, feet, 37.0, 1.0, &AnimInput::REST);
        for (part, range) in zombie.parts.iter().enumerate() {
            let m = inst.part_transforms[part];
            let lo = range.vertex_start as usize;
            let hi = lo + range.vertex_count as usize;
            for v in &zombie.vertices[lo..hi] {
                let w = m.transform_point3(Vec3::from(v.position));
                assert!(
                    w.cmpge(inst.aabb_min - Vec3::splat(1e-2)).all()
                        && w.cmple(inst.aabb_max + Vec3::splat(1e-2)).all(),
                    "vertex {w:?} escaped AABB [{:?}, {:?}]",
                    inst.aabb_min,
                    inst.aabb_max,
                );
            }
        }
    }

    #[test]
    fn instance_world_aabb_contains_the_transformed_mesh() {
        let mesh = pig_mesh();
        let feet = Vec3::new(5.0, 70.0, 5.0);
        let inst = EntityInstance::new("pig", &mesh, feet, 45.0, 1.0, &AnimInput::REST);
        // Vertices are part-local, so a vertex only lands in the world once it
        // has been through *its own part's* matrix — the same matrix the GPU
        // draws it with. Using `inst.transform` alone would collapse every part
        // onto the model origin and the AABB check would be meaningless.
        for (part, range) in mesh.parts.iter().enumerate() {
            let m = inst.part_transforms[part];
            let lo = range.vertex_start as usize;
            let hi = lo + range.vertex_count as usize;
            for v in &mesh.vertices[lo..hi] {
                let w = m.transform_point3(Vec3::from(v.position));
                assert!(
                    w.cmpge(inst.aabb_min - Vec3::splat(1e-3)).all()
                        && w.cmple(inst.aabb_max + Vec3::splat(1e-3)).all(),
                    "vertex {w:?} escaped AABB [{:?}, {:?}]",
                    inst.aabb_min,
                    inst.aabb_max,
                );
            }
        }
    }

    fn frustum_looking_down_pos_z() -> Frustum {
        use crate::camera::Camera;
        Camera {
            position: Vec3::new(0.0, 64.0, 0.0),
            yaw: 0.0, // faces +Z
            pitch: 0.0,
            fov_y_degrees: 70.0,
            aspect: 1.0,
            near: 0.05,
            far: 128.0,
        }
        .frustum()
    }

    #[test]
    fn frustum_culls_entities_behind_the_camera() {
        let mesh = pig_mesh();
        let frustum = frustum_looking_down_pos_z();
        let in_front = EntityInstance::new(
            "pig",
            &mesh,
            Vec3::new(0.0, 63.0, 20.0),
            0.0,
            1.0,
            &AnimInput::REST,
        );
        let behind = EntityInstance::new(
            "pig",
            &mesh,
            Vec3::new(0.0, 63.0, -20.0),
            0.0,
            1.0,
            &AnimInput::REST,
        );

        let frame = plan_entities(&[in_front, behind], &frustum);
        assert_eq!(frame.stats.drawn, 1, "only the front entity draws");
        assert_eq!(frame.stats.culled_frustum, 1, "the one behind is culled");
        assert!(frame.stats.is_meaningful());
        assert_eq!(frame.instance_count(), 1);
    }

    #[test]
    fn plan_groups_instances_by_model_type() {
        let pig = pig_mesh();
        let cow = EntityMesh::from_model(&lodestone_assets::entity_models::cow_model());
        let frustum = frustum_looking_down_pos_z();
        let at = |model, m: &EntityMesh, z: f32| {
            EntityInstance::new(
                model,
                &m,
                Vec3::new(0.0, 63.0, z),
                0.0,
                1.0,
                &AnimInput::REST,
            )
        };
        let instances = [
            at("pig", &pig, 10.0),
            at("cow", &cow, 12.0),
            at("pig", &pig, 14.0),
            // one behind the camera to force a cull so the frame is meaningful.
            at("pig", &pig, -30.0),
        ];
        let frame = plan_entities(&instances, &frustum);
        assert_eq!(frame.stats.drawn, 3);
        assert_eq!(frame.stats.culled_frustum, 1);
        assert!(frame.stats.is_meaningful());

        let pig_batch = frame.batches.iter().find(|b| b.model == "pig").unwrap();
        let cow_batch = frame.batches.iter().find(|b| b.model == "cow").unwrap();
        assert_eq!(
            pig_batch.transforms.len(),
            2,
            "two visible pigs batch together"
        );
        assert_eq!(cow_batch.transforms.len(), 1);
    }

    #[test]
    fn model_set_resolves_types_and_skips_unknowns() {
        let set = EntityModelSet::load();
        assert!(!set.is_empty());
        assert_eq!(set.len(), entity_models().len());

        let feet = Vec3::new(0.0, 63.0, 10.0);
        let pig = set
            .resolve("pig", feet, 0.0, 1.0, &AnimInput::REST)
            .expect("pig resolves");
        assert_eq!(pig.model, "pig");
        assert_eq!(
            set.resolve("cave_spider", feet, 0.0, 1.0, &AnimInput::REST)
                .unwrap()
                .model,
            "cave_spider"
        );
        // Unknown type resolves to nothing (renderer skips it).
        assert!(
            set.resolve("experience_orb", feet, 0.0, 1.0, &AnimInput::REST)
                .is_none()
        );
        // The resolved instance's model is present in the set for upload.
        assert!(set.get(pig.model).is_some());
    }

    #[test]
    fn plan_seam_resolves_culls_and_skips_modelless_in_one_call() {
        let set = EntityModelSet::load();
        let frustum = frustum_looking_down_pos_z();
        // A mix mirroring a live scene: two drawable pigs, one drawable cow, a
        // modelless type that must be dropped (not culled), and one pig behind
        // the camera to force a real cull.
        // The two visible pigs carry *different* light so the batch's `lights`
        // can be checked to stay in step with its `transforms`: a batch that
        // merged or reordered them would still have the right length.
        let spawns = [
            EntitySpawn {
                type_path: "pig",
                feet: Vec3::new(0.0, 63.0, 10.0),
                body_yaw_deg: 0.0,
                scale: 1.0,
                anim: AnimInput::REST,
                light: ENTITY_FULLBRIGHT,
            },
            EntitySpawn {
                type_path: "cow",
                feet: Vec3::new(0.0, 63.0, 12.0),
                body_yaw_deg: 0.0,
                scale: 1.0,
                anim: AnimInput::REST,
                light: 0x0A, // block light 10, no sky: a torch-lit cow indoors
            },
            EntitySpawn {
                type_path: "experience_orb", // no model — dropped, not counted
                feet: Vec3::new(0.0, 63.0, 14.0),
                body_yaw_deg: 0.0,
                scale: 1.0,
                anim: AnimInput::REST,
                light: ENTITY_FULLBRIGHT,
            },
            EntitySpawn {
                type_path: "pig",
                feet: Vec3::new(0.0, 63.0, 16.0),
                body_yaw_deg: 0.0,
                scale: 1.0,
                anim: AnimInput::REST,
                light: 0x00, // pitch dark
            },
            EntitySpawn {
                type_path: "pig",
                feet: Vec3::new(0.0, 63.0, -30.0), // behind camera
                body_yaw_deg: 0.0,
                scale: 1.0,
                anim: AnimInput::REST,
                light: ENTITY_FULLBRIGHT,
            },
        ];

        let frame = set.plan(spawns, &frustum);

        // The modelless dragon is dropped before culling, so `total` counts only
        // the four entities that had a model, and exactly one of those culled.
        assert_eq!(frame.stats.total, 4, "modelless types are not counted");
        assert_eq!(frame.stats.drawn, 3);
        assert_eq!(frame.stats.culled_frustum, 1);
        assert!(frame.stats.is_meaningful());
        let pig_batch = frame.batches.iter().find(|b| b.model == "pig").unwrap();
        assert_eq!(pig_batch.transforms.len(), 2, "two visible pigs batch");
        assert!(frame.batches.iter().any(|b| b.model == "cow"));

        // Light must ride through `plan` per instance and stay index-aligned
        // with `transforms` — the culled pig drops out of both, so the surviving
        // pair is the lit one then the dark one, in spawn order.
        assert_eq!(
            pig_batch.lights,
            vec![u32::from(ENTITY_FULLBRIGHT), 0x00],
            "per-entity light must survive resolve + cull in transform order"
        );
        let cow_batch = frame.batches.iter().find(|b| b.model == "cow").unwrap();
        assert_eq!(cow_batch.lights, vec![0x0A]);
        for batch in &frame.batches {
            assert_eq!(
                batch.lights.len(),
                batch.transforms.len(),
                "one light per instance, or the instance buffer would misalign"
            );
        }

        // The one-call seam is exactly manual resolve + plan_entities: same frame.
        let manual: Vec<EntityInstance> = spawns
            .iter()
            .filter_map(|s| {
                set.resolve(s.type_path, s.feet, s.body_yaw_deg, s.scale, &s.anim)
                    .map(|i| i.with_light(s.light))
            })
            .collect();
        let manual_frame = plan_entities(&manual, &frustum);
        assert_eq!(frame.batches, manual_frame.batches);
        assert_eq!(frame.instance_count(), manual_frame.instance_count());
    }

    // ---- dropped items ---------------------------------------------------

    /// A unit cube's six outward-wound faces, in `mesh_item_quads`' vertex
    /// order, as a stand-in for a baked block item's geometry.
    fn cube_face(dir: Direction) -> [Vec3; 4] {
        let n = match dir {
            Direction::East => Vec3::X,
            Direction::West => -Vec3::X,
            Direction::Up => Vec3::Y,
            Direction::Down => -Vec3::Y,
            Direction::South => Vec3::Z,
            Direction::North => -Vec3::Z,
        };
        let u = if n.x.abs() < 0.5 { Vec3::X } else { Vec3::Y };
        let v = n.cross(u);
        let centre = Vec3::splat(0.5) + n * 0.5;
        [
            centre - u * 0.5 - v * 0.5,
            centre + u * 0.5 - v * 0.5,
            centre + u * 0.5 + v * 0.5,
            centre - u * 0.5 + v * 0.5,
        ]
    }

    fn cube_quad(dir: Direction) -> BakedQuad {
        let p = cube_face(dir);
        BakedQuad {
            positions: [p[0].into(), p[1].into(), p[2].into(), p[3].into()],
            uvs: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            direction: dir,
            cullface: None,
            tint_index: None,
            shade: true,
            layer: 0,
            anim: 0,
            sprite: 0,
        }
    }

    fn unit_cube_quads() -> Vec<BakedQuad> {
        [
            Direction::East,
            Direction::West,
            Direction::Up,
            Direction::Down,
            Direction::South,
            Direction::North,
        ]
        .into_iter()
        .map(cube_quad)
        .collect()
    }

    /// The signed screen area of a quad's first triangle after `m` — the sign
    /// `FrontFace::Ccw` + `cull_mode: Back` acts on.
    fn screen_area(m: Mat4, q: [Vec3; 4]) -> f32 {
        let p: Vec<Vec3> = q.iter().map(|v| m.project_point3(*v)).collect();
        let a = p[1] - p[0];
        let b = p[2] - p[0];
        a.x * b.y - a.y * b.x
    }

    #[test]
    fn the_bob_never_dips_below_the_entity_position() {
        // `sin(..) * 0.1 + 0.1` is bounded to 0.0..=0.2, so a dropped item
        // hovers and never sinks into the block it landed on.
        for tick in 0..400 {
            let age = tick as f32 * 0.5;
            let bob = item_bob_height(age, 1.234);
            assert!(
                (0.0..=2.0 * ITEM_BOB_AMPLITUDE + 1e-6).contains(&bob),
                "bob {bob} at age {age} escaped 0..=0.2"
            );
        }
    }

    #[test]
    fn the_bob_and_the_spin_have_vanillas_periods() {
        // Bob: sin(age/10 + offs) has period 20*PI ticks. Spin: age/20 + offs
        // radians, so a full turn is 40*PI ticks. Asserting the *ratio* as well
        // catches a swapped pair of divisors, which equal-period tests do not.
        let offs = 0.0;
        let bob_period = std::f32::consts::TAU * ITEM_BOB_TICKS_PER_RADIAN;
        assert!(
            (item_bob_height(0.0, offs) - item_bob_height(bob_period, offs)).abs() < 1e-4,
            "the bob must repeat after {bob_period} ticks"
        );
        let spin_period = std::f32::consts::TAU * ITEM_SPIN_TICKS_PER_RADIAN;
        assert!(
            (item_spin_radians(spin_period, offs) - item_spin_radians(0.0, offs)
                - std::f32::consts::TAU)
                .abs()
                < 1e-4,
            "the spin must complete exactly one turn after {spin_period} ticks"
        );
        assert!(
            (spin_period / bob_period - 2.0).abs() < 1e-4,
            "vanilla bobs twice per revolution"
        );
    }

    #[test]
    fn two_entities_do_not_bob_in_lockstep() {
        // The whole point of a per-entity phase: a pile of drops must not
        // pulse as one object.
        let offsets: Vec<f32> = (1..=8).map(item_bob_offset).collect();
        for (i, a) in offsets.iter().enumerate() {
            assert!(
                (0.0..std::f32::consts::TAU).contains(a),
                "phase {a} out of range"
            );
            for b in &offsets[i + 1..] {
                assert!((a - b).abs() > 1e-3, "ids share a phase: {a} vs {b}");
            }
        }
        // ...and it must be stable, or the item jitters instead of spinning.
        assert_eq!(item_bob_offset(7), item_bob_offset(7));
    }

    #[test]
    fn the_hover_lift_puts_the_lowest_point_one_pixel_up() {
        // Vanilla's `minOffsetY = -box.minY + 0.0625`, measured on the GROUND-
        // posed model. Under block/block's ground pose the unit cube is scaled
        // to 0.25 and centred on y = 3/16, so its base sits at 3/16 - 1/8.
        let quads = unit_cube_quads();
        let (min_y, max_y) = posed_item_y_extent(&quads, &BLOCK_ITEM_GROUND);
        assert!((min_y - (3.0 / 16.0 - 0.125)).abs() < 1e-5, "min_y = {min_y}");
        assert!((max_y - (3.0 / 16.0 + 0.125)).abs() < 1e-5, "max_y = {max_y}");

        let lift = item_hover_lift(&quads, &BLOCK_ITEM_GROUND);
        let pose = dropped_item_matrix(Vec3::ZERO, 0.0, 0.0, &BLOCK_ITEM_GROUND, lift);
        let lowest = unit_cube_quads()
            .iter()
            .flat_map(|q| q.positions)
            .map(|p| pose.transform_point3(Vec3::from(p)).y)
            .fold(f32::INFINITY, f32::min);
        // At age 0 with phase 0 the bob is exactly its 0.1 midpoint.
        let expected = ITEM_MIN_HOVER_HEIGHT + item_bob_height(0.0, 0.0);
        assert!(
            (lowest - expected).abs() < 1e-5,
            "the posed model's base must float {expected} above the entity, got {lowest}"
        );
    }

    #[test]
    fn the_spin_is_about_the_entity_position_not_the_model_origin() {
        // The centring inside `display_matrix` is what makes the item rotate on
        // the spot. If it were dropped, the cube would orbit its own corner and
        // swing half a block sideways every revolution.
        let quads = unit_cube_quads();
        let lift = item_hover_lift(&quads, &BLOCK_ITEM_GROUND);
        let feet = Vec3::new(10.0, 64.0, -3.0);
        for age in [0.0f32, 13.0, 27.5, 61.0] {
            let pose = dropped_item_matrix(feet, age, 0.4, &BLOCK_ITEM_GROUND, lift);
            let centre = pose.transform_point3(Vec3::splat(0.5));
            assert!(
                (centre.x - feet.x).abs() < 1e-4 && (centre.z - feet.z).abs() < 1e-4,
                "at age {age} the item centre drifted to {centre} from {feet}"
            );
        }
    }

    #[test]
    fn dropped_item_pose_preserves_winding() {
        // Derive the front-facing sign from the camera rather than asserting
        // "positive" or "negative" — the same discipline `item_render`'s
        // `winding_matches_the_world_camera` uses, and the reason that test
        // cannot be fooled by a misremembered glam/wgpu convention.
        //
        // The trap this pins: the GUI rule is that `gui_ortho * gui_item_pose`
        // matches `view_projection`'s determinant SIGN. Applying that to a
        // *world* pose — which is left-multiplied by that same
        // `view_projection` — inverts it. A world pose must have a POSITIVE
        // determinant, and the composition then inherits the camera's.
        //
        // The camera's own sign is deliberately not written down here. It is a
        // property of the projection (negative under a forward `[0,1]` one,
        // positive under reversed-Z, because mirroring the clip `z` axis flips a
        // 4x4 determinant) and it decides nothing the rasterizer can see, which
        // reads facing from projected `x`/`y` alone. All that is required is
        // that it be non-degenerate, so that the relative claims below have a
        // reference to be relative to.
        let camera = crate::camera::Camera {
            position: Vec3::new(0.5, 0.5, 4.0),
            yaw: 180.0,
            pitch: 0.0,
            ..crate::camera::Camera::default()
        };
        let world = camera.view_projection();
        assert!(
            world.determinant().abs() > 1.0e-6,
            "the reference camera's projection is degenerate ({}), so it cannot \
             supply a front-facing sign",
            world.determinant()
        );
        let front_sign = screen_area(world, cube_face(Direction::South)).signum();
        assert_eq!(
            screen_area(world, cube_face(Direction::North)).signum(),
            -front_sign,
            "the reference camera must disagree about the far face"
        );

        let quads = unit_cube_quads();
        let lift = item_hover_lift(&quads, &BLOCK_ITEM_GROUND);
        // Several ages, so a spin angle cannot be what rescues the sign.
        for age in [0.0f32, 5.0, 17.0, 33.0, 70.0] {
            let pose = dropped_item_matrix(
                Vec3::new(0.5, 0.5, 0.0),
                age,
                0.0,
                &BLOCK_ITEM_GROUND,
                lift,
            );
            assert!(
                pose.determinant() > 0.0,
                "a world-space item pose must not flip handedness; det = {} at age {age}",
                pose.determinant()
            );
            let composed = world * pose;
            assert_eq!(
                composed.determinant().signum(),
                world.determinant().signum(),
                "view_projection * pose must keep the camera's winding at age {age}"
            );
            // And on-screen: whichever cube face currently points at the camera
            // must carry the front-facing sign.
            let towards_camera = if (item_spin_radians(age, 0.0) / std::f32::consts::TAU).fract()
                < 0.25
            {
                Direction::South
            } else {
                continue;
            };
            assert_eq!(
                screen_area(composed, cube_face(towards_camera)).signum(),
                front_sign,
                "the face turned towards the camera must survive back-face culling at age {age}"
            );
        }
    }

    #[test]
    fn vault_spin_degrees_predicts_the_exact_running_total() {
        // Magnitude, not sign: at game_time 37 with partial_tick 0.5 the answer
        // is exactly 375.0 degrees (37.5 ticks * 10 deg/tick), derived from the
        // constant outside the function under test rather than a plausible
        // round number.
        let deg = vault_spin_degrees(37, 0.5);
        assert!(
            (deg - 375.0).abs() < 1e-4,
            "expected 375.0 degrees at (37, 0.5), got {deg}"
        );
        // Two continuous samples one tick apart must differ by exactly
        // VAULT_SPIN_DEGREES_PER_TICK, matching `updateDisplayItemSpin`'s
        // constant per-tick step.
        let a = vault_spin_degrees(100, 0.25);
        let b = vault_spin_degrees(101, 0.25);
        assert!(
            (b - a - VAULT_SPIN_DEGREES_PER_TICK).abs() < 1e-4,
            "one tick later must advance by exactly {VAULT_SPIN_DEGREES_PER_TICK} degrees, got {}",
            b - a
        );
    }

    #[test]
    fn vault_display_item_centres_on_the_blocks_upper_middle() {
        // `T(0.5, 0.4, 0.5)`: the cluster's pivot sits above the block's floor
        // centre, not at its corner — getting this backwards buries the item in
        // the vault's base or floats it a whole block up.
        let block_pos = Vec3::new(4.0, 70.0, -9.0);
        for spin in [0.0f32, 90.0, 217.0] {
            // `display_matrix` recentres the baked item box on its own middle
            // (`T(-0.5,-0.5,-0.5)` innermost — see that function's doc), so
            // `Vec3::splat(0.5)`, not the model-space origin, is the point that
            // survives a rotation unmoved — the same probe
            // `the_spin_is_about_the_entity_position_not_the_model_origin`
            // uses above for exactly this reason.
            let pose = vault_display_item_matrix(
                block_pos,
                spin,
                Vec3::ZERO,
                &DisplayTransform::default(),
            );
            let pivot = pose.transform_point3(Vec3::splat(0.5));
            let expected = block_pos + Vec3::new(0.5, 0.4, 0.5);
            assert!(
                pivot.distance(expected) < 1e-4,
                "pivot at spin {spin} was {pivot}, expected {expected}"
            );
        }
    }

    #[test]
    fn vault_display_item_pose_preserves_winding() {
        // Same discipline as `dropped_item_pose_preserves_winding`: a
        // world-space pose must have a POSITIVE determinant regardless of the
        // spin angle, so it composes correctly with a negative-determinant
        // camera.
        for spin in [0.0f32, 45.0, 133.0, 270.0] {
            let pose = vault_display_item_matrix(
                Vec3::new(1.0, 65.0, 2.0),
                spin,
                Vec3::ZERO,
                &BLOCK_ITEM_GROUND,
            );
            assert!(
                pose.determinant() > 0.0,
                "a world-space vault item pose must not flip handedness; det = {} at spin {spin}",
                pose.determinant()
            );
        }
    }

    #[test]
    fn the_mesh_carries_the_world_light_not_the_gui_full_bright() {
        // The regression this guards: reusing `mesh_item_quads` verbatim nails
        // every vertex to GUI_ITEM_LIGHT, so a drop in a pitch-black cave glows
        // exactly as brightly as one at noon.
        let quads = unit_cube_quads();
        let dark = dropped_item_mesh(
            &quads,
            GuiLight::Side,
            &BLOCK_ITEM_GROUND,
            Vec3::ZERO,
            0.0,
            0.0,
            0x02,
        );
        assert!(!dark.vertices.is_empty(), "the cube must mesh to something");
        assert!(
            dark.vertices.iter().all(|v| v.light == 0x02),
            "every vertex must carry the sampled world light"
        );
        assert_eq!(dark.quad_count(), quads.len());
    }

    #[test]
    fn the_two_ground_transforms_are_selected_by_gui_light() {
        assert_eq!(ground_transform_for(GuiLight::Side), BLOCK_ITEM_GROUND);
        assert_eq!(ground_transform_for(GuiLight::Front), GENERATED_ITEM_GROUND);
        // The flat family is posed twice as large and one pixel lower; a swap
        // would halve every dropped block.
        const {
            assert!(GENERATED_ITEM_GROUND.scale[0] > BLOCK_ITEM_GROUND.scale[0]);
        }
    }

    /// The declared slot must **win**, and the fallback must still fire — the
    /// second half is the control. Without it a `ground_transform` that ignored
    /// its `display` argument entirely would pass on every vanilla item, because
    /// the constants happen to equal what vanilla declares.
    #[test]
    fn a_declared_ground_slot_beats_the_gui_light_fallback() {
        let odd = DisplayTransform {
            rotation: [0.0, 17.0, 0.0],
            translation: [0.0, 9.0, 0.0],
            scale: [0.125, 0.125, 0.125],
        };
        let declared = DisplayTransforms::NONE.with(DisplaySlot::Ground, odd);
        assert_eq!(
            ground_transform(&declared, GuiLight::Side),
            odd,
            "the model's own display.ground must be used, not the constant"
        );

        // Control: a chain that declares nothing falls back, and the two
        // fallbacks are still told apart by gui_light.
        assert_eq!(
            ground_transform(&DisplayTransforms::NONE, GuiLight::Side),
            BLOCK_ITEM_GROUND
        );
        assert_eq!(
            ground_transform(&DisplayTransforms::NONE, GuiLight::Front),
            GENERATED_ITEM_GROUND
        );

        // And an *explicitly declared* identity is honoured rather than being
        // mistaken for "absent" — the trap `DisplayTransforms::get` would fall
        // into here, since `get` cannot tell the two apart.
        let flat = DisplayTransforms::NONE.with(DisplaySlot::Ground, DisplayTransform::default());
        assert_eq!(
            ground_transform(&flat, GuiLight::Side),
            DisplayTransform::default()
        );
    }

    /// A left-hand slot with no left-hand data must mirror onto the right-hand
    /// one, as vanilla's `ItemTransforms.Deserializer` does. `block/block` and
    /// `item/generated` both declare `thirdperson_righthand` and no
    /// `thirdperson_lefthand`, so without this every block in an off hand would
    /// be posed with the identity.
    // ---- held items, and the first-person arm ---------------------------

    fn player_mesh() -> EntityMesh {
        EntityMesh::from_named_model(
            "player_wide",
            &lodestone_assets::entity::player_model(false),
        )
    }

    /// A plausible `thirdperson_righthand`: vanilla's `item/handheld` declares
    /// `rotation [0, -90, 55]`, `translation [0, 4, 0.5]`, `scale [0.85, …]`.
    fn handheld_third_person() -> DisplayTransform {
        DisplayTransform {
            rotation: [0.0, -90.0, 55.0],
            translation: [0.0, 4.0, 0.5],
            scale: [0.85, 0.85, 0.85],
        }
    }

    #[test]
    fn the_held_item_x_offset_mirrors_between_hands_and_nothing_else_does() {
        // The only asymmetry the layer itself introduces is `±offsetX`. Isolate
        // it by handing both arms the *same* identity display transform and the
        // same arm matrix, so any other difference would have to come from this
        // function.
        let flat = DisplayTransform::default();
        let right = held_item_matrix(Mat4::IDENTITY, Arm::Right, false, &flat);
        let left = held_item_matrix(Mat4::IDENTITY, Arm::Left, false, &flat);
        let r = right.transform_point3(Vec3::splat(0.5));
        let l = left.transform_point3(Vec3::splat(0.5));
        // Rx(-90) then Ry(180) sends the +x offset to -x, so the sign is flipped
        // once more than the naive reading — which is exactly why this is
        // measured rather than asserted from the constant.
        assert!((r.y - l.y).abs() < 1e-6, "y must not mirror: {r} vs {l}");
        assert!((r.z - l.z).abs() < 1e-6, "z must not mirror: {r} vs {l}");
        assert!(
            (r.x + l.x).abs() < 1e-6 && r.x.abs() > 1e-3,
            "x must mirror about zero and be non-zero: {r} vs {l}"
        );
    }

    #[test]
    fn a_baby_holds_its_item_closer_in() {
        // The baby triple is smaller on every axis, so the item sits nearer the
        // shoulder. A swapped adult/baby branch is the failure this catches.
        let t = handheld_third_person();
        let adult = held_item_matrix(Mat4::IDENTITY, Arm::Right, false, &t)
            .transform_point3(Vec3::splat(0.5));
        let baby =
            held_item_matrix(Mat4::IDENTITY, Arm::Right, true, &t).transform_point3(Vec3::splat(0.5));
        assert!(
            baby.length() < adult.length(),
            "the baby offset must be nearer the pivot: {baby} vs {adult}"
        );
    }

    #[test]
    fn the_held_item_pose_hangs_off_the_arm_matrix_it_is_given() {
        // The seam that makes this non-island: the caller passes
        // `part_transforms[arm]`, and translating that must translate the item
        // by exactly the same amount.
        let t = handheld_third_person();
        let base = held_item_matrix(Mat4::IDENTITY, Arm::Right, false, &t);
        let shift = Vec3::new(3.0, 64.0, -7.0);
        let moved = held_item_matrix(
            Mat4::from_translation(shift),
            Arm::Right,
            false,
            &t,
        );
        let a = base.transform_point3(Vec3::splat(0.5));
        let b = moved.transform_point3(Vec3::splat(0.5));
        assert!((b - a - shift).length() < 1e-4, "{a} -> {b}, expected +{shift}");
    }

    #[test]
    fn the_held_item_pose_preserves_winding_for_a_real_mob() {
        // Same discipline as `dropped_item_pose_preserves_winding`: the whole
        // chain is a *world* pose left-multiplied by `view_projection`, so its
        // determinant must be POSITIVE and the composition must inherit the
        // camera's negative sign. The GUI rule ("negative") applied here ships an
        // item you see the inside of, which still looks like a sword.
        let camera = crate::camera::Camera {
            position: Vec3::new(0.5, 1.0, 4.0),
            yaw: 180.0,
            pitch: 0.0,
            ..crate::camera::Camera::default()
        };
        let world = camera.view_projection();
        let front_sign = screen_area(world, cube_face(Direction::South)).signum();
        assert_eq!(
            screen_area(world, cube_face(Direction::North)).signum(),
            -front_sign,
            "the reference camera must disagree about the far face"
        );

        let mesh = player_mesh();
        let t = handheld_third_person();
        for yaw in [0.0f32, 37.0, 180.0, 271.0] {
            for (scale, baby) in [(1.0f32, false), (0.5, true)] {
                let inst = EntityInstance::new(
                    "player_wide",
                    &mesh,
                    Vec3::new(0.5, 0.0, 0.0),
                    yaw,
                    scale,
                    &AnimInput::REST,
                );
                for hand in [Arm::Right, Arm::Left] {
                    let arm_transform = inst
                        .hand_transform(hand)
                        .expect("player_wide has both arms");
                    let pose = held_item_matrix(arm_transform, hand, baby, &t);
                    assert!(
                        pose.determinant() > 0.0,
                        "a world-space held-item pose must not flip handedness; det = {} \
                         (yaw {yaw}, scale {scale}, {hand:?})",
                        pose.determinant()
                    );
                    assert_eq!(
                        (world * pose).determinant().signum(),
                        world.determinant().signum(),
                        "view_projection * pose must keep the camera's winding"
                    );
                }
            }
        }
    }

    /// The whole reason [`EntityInstance::hand_transform`] exists rather than
    /// reusing `part_transforms[skeleton.index_of(arm.part_name())]`: for a
    /// skeleton the two must actually differ (by the pivot shift), and the
    /// arm's *own* body-mesh transform (`part_transforms`) must stay exactly
    /// what it was — proof this crate's override never leaks into the
    /// whole-body draw it shares an index with.
    #[test]
    fn a_skeletons_hand_transform_differs_from_its_arms_body_transform() {
        let mesh = EntityMesh::from_named_model(
            "skeleton",
            &lodestone_assets::entity_models::skeleton_model(),
        );
        let inst = EntityInstance::new(
            "skeleton",
            &mesh,
            Vec3::new(0.5, 0.0, 0.0),
            0.0,
            1.0,
            &AnimInput::REST,
        );
        let arm_idx = mesh.skeleton.index_of("right_arm").unwrap();
        let body_mesh_transform = inst.part_transforms[arm_idx];
        let hand_transform = inst.hand_transform(Arm::Right).unwrap();
        assert!(
            (hand_transform.w_axis - body_mesh_transform.w_axis).length() > 1e-4,
            "the pivot shift did not reach the hand transform: {hand_transform:?} vs \
             {body_mesh_transform:?}"
        );
    }

    #[test]
    fn hand_transform_picks_the_slot_the_arm_and_person_name() {
        let third = handheld_third_person();
        let first = DisplayTransform {
            rotation: [0.0, -90.0, 25.0],
            translation: [1.13, 3.2, 1.13],
            scale: [0.68, 0.68, 0.68],
        };
        let d = DisplayTransforms::NONE
            .with(DisplaySlot::ThirdPersonRightHand, third)
            .with(DisplaySlot::FirstPersonRightHand, first);
        assert_eq!(hand_transform(&d, Arm::Right, false), third);
        assert_eq!(hand_transform(&d, Arm::Right, true), first);
        // Both left slots are undeclared, so both fall back to their right-hand
        // partner — vanilla's deserializer rule, and the reason a block in an off
        // hand is not identity-posed.
        assert_eq!(hand_transform(&d, Arm::Left, false), third);
        assert_eq!(hand_transform(&d, Arm::Left, true), first);
        // ...and a model that declares nothing gets NO_TRANSFORM, not a guess.
        assert_eq!(
            hand_transform(&DisplayTransforms::NONE, Arm::Right, false),
            DisplayTransform::default()
        );
    }

    #[test]
    fn the_player_arm_rest_pose_is_a_pure_translation() {
        // What makes `rest_pose()[arm] * Rz(±0.1)` *exact* rather than an
        // approximation of `arm.resetPose(); arm.zRot = ±0.1`: the authored rest
        // rotation is zero and the root above it is the identity, so replacing
        // zRot is the same as post-multiplying Rz. If a future corpus edit gave
        // the player arm a rest rotation, this fails instead of silently drifting.
        let mesh = player_mesh();
        for arm in [Arm::Right, Arm::Left] {
            let i = mesh.skeleton.index_of(arm.part_name()).expect("arm part");
            let rest = mesh.skeleton.rest_pose()[i];
            let expect = Mat4::from_translation(Vec3::new(
                arm.invert() * -5.0 / 16.0,
                2.0 / 16.0,
                0.0,
            ));
            assert!(
                (rest - expect).to_cols_array().iter().all(|v| v.abs() < 1e-6),
                "{arm:?} rest pose must be PartPose::offset(∓5, 2, 0) with no rotation; got {rest}"
            );
        }
    }

    #[test]
    fn the_sleeve_shares_the_arms_matrix_exactly() {
        // `right_sleeve` is `PartPose::ZERO` under `right_arm`, so one uploaded
        // matrix drives both parts. Drawing the sleeve with its own recomputed
        // matrix would be the same number; drawing it with the *body's* would
        // put a floating sleeve mid-screen.
        let mesh = player_mesh();
        let rest = mesh.skeleton.rest_pose();
        for arm in [Arm::Right, Arm::Left] {
            let a = mesh.skeleton.index_of(arm.part_name()).expect("arm");
            let s = mesh
                .skeleton
                .index_of(arm.sleeve_part_name())
                .expect("sleeve");
            assert!(
                (rest[a] - rest[s]).to_cols_array().iter().all(|v| v.abs() < 1e-6),
                "{arm:?} sleeve must share the arm's matrix"
            );
            let parts = first_person_arm_parts(&mesh, arm);
            assert_eq!(parts, vec![a, s], "both parts must be drawn, arm first");
        }
        // A rig with no sleeve yields just the arm, and a rig with no arm at all
        // yields nothing — the control that keeps the `Vec` honest.
        assert_eq!(
            first_person_arm_parts(&pig_mesh(), Arm::Right),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn the_first_person_arm_lands_in_the_bottom_right_of_frame() {
        // Hand-computed from `renderPlayerArm`'s chain with attack = 0 and
        // inverseArmHeight = 0, in camera space (x right, y up, -z forward):
        // the arm cube spans roughly x 0.33..0.91, y -0.99..-0.29, z -1.19..-0.44.
        // The load-bearing claims are the *signs*: right of centre, below the
        // eye, and in front of it. A missing rotation in the chain flips one.
        let mesh = player_mesh();
        let pose =
            first_person_arm_pose(&mesh, Arm::Right, 0.0).expect("player_wide has a right arm");
        // `player_wide`'s right arm cube: from [-3, -2, -2], size [4, 12, 4].
        let corners: Vec<Vec3> = (0..8u32)
            .map(|i| {
                let x = if i & 1 == 0 { -3.0f32 } else { 1.0 };
                let y = if i & 2 == 0 { -2.0f32 } else { 10.0 };
                let z = if i & 4 == 0 { -2.0f32 } else { 2.0 };
                pose.transform_point3(Vec3::new(x, y, z) / 16.0)
            })
            .collect();
        let lo = corners.iter().copied().reduce(Vec3::min).unwrap();
        let hi = corners.iter().copied().reduce(Vec3::max).unwrap();
        assert!(lo.x > 0.2 && hi.x < 1.1, "x span {}..{}", lo.x, hi.x);
        assert!(hi.y < -0.2 && lo.y > -1.2, "y span {}..{}", lo.y, hi.y);
        assert!(hi.z < -0.3 && lo.z > -1.4, "z span {}..{}", lo.z, hi.z);
        // Beyond the near plane, or the arm is clipped away entirely.
        assert!(hi.z < -HAND_NEAR, "the arm must be past the near plane");

        // The left arm is the mirror image about x, to within the zRot sign.
        let left = first_person_arm_pose(&mesh, Arm::Left, 0.0).expect("left arm");
        let lc = left.transform_point3(Vec3::ZERO);
        let rc = pose.transform_point3(Vec3::ZERO);
        assert!((lc.x + rc.x).abs() < 1e-4, "left/right must mirror: {lc} vs {rc}");
        assert!((lc.y - rc.y).abs() < 1e-4 && (lc.z - rc.z).abs() < 1e-4);
    }

    #[test]
    fn first_person_arm_pose_preserves_winding() {
        // The arm is drawn with the HUD projection alone, and a view matrix has
        // determinant +1, so `sign(det(hand_projection))` equals
        // `sign(det(view_projection))`. The arm pose must therefore be
        // orientation-*preserving*, like a world model matrix — not
        // orientation-reversing like the GUI item pose.
        let camera = crate::camera::Camera::default();
        let world = camera.view_projection();
        assert!(
            camera.view_matrix().determinant() > 0.0,
            "a view matrix must have determinant +1; that is why the two signs agree"
        );
        let proj = hand_projection(16.0 / 9.0);
        assert_eq!(
            proj.determinant().signum(),
            world.determinant().signum(),
            "hand_projection must share view_projection's handedness \
             (proj {}, world {})",
            proj.determinant(),
            world.determinant()
        );

        let mesh = player_mesh();
        for arm in [Arm::Right, Arm::Left] {
            // Every phase of the swing, not just rest: a rotation cannot change a
            // determinant's sign, but the chain is edited by hand and a stray
            // reflection (a negated scale, a mirrored translation folded into a
            // rotation) would only show up mid-swing.
            for step in 0..=8 {
                let attack = step as f32 / 8.0;
                let pose = first_person_arm_pose(&mesh, arm, attack).expect("arm");
                assert!(
                    pose.determinant() > 0.0,
                    "{arm:?} arm pose must not flip handedness at attack {attack}; det = {}",
                    pose.determinant()
                );
                assert_eq!(
                    (proj * pose).determinant().signum(),
                    world.determinant().signum(),
                    "hand_projection * arm pose must keep the world's winding at attack {attack}"
                );
            }
        }
    }

    /// The swing must be **additive**: `attack_anim == 0` has to reproduce the
    /// pre-swing chain exactly, or every existing framing assertion above (and the
    /// shell's headless arm gate) is silently measuring a different arm.
    ///
    /// The expected matrix is written out longhand rather than taken from
    /// `first_person_arm_chain` itself — comparing the function to itself would
    /// pass for any pair of symmetric mistakes.
    #[test]
    fn arm_chain_at_rest_matches_the_static_chain() {
        for arm in [Arm::Right, Arm::Left] {
            let i = arm.invert();
            let expected = Mat4::from_translation(Vec3::new(i * 0.640_000_05, -0.6, -0.719_999_97))
                * Mat4::from_rotation_y((i * 45.0).to_radians())
                * Mat4::from_translation(Vec3::new(i * -1.0, 3.6, 3.5))
                * Mat4::from_rotation_z((i * 120.0).to_radians())
                * Mat4::from_rotation_x(200.0f32.to_radians())
                * Mat4::from_rotation_y((i * -135.0).to_radians())
                * Mat4::from_translation(Vec3::new(i * 5.6, 0.0, 0.0));
            let actual = first_person_arm_chain(arm, 0.0);
            let delta = (expected - actual)
                .to_cols_array()
                .iter()
                .fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(delta < 1e-5, "{arm:?} rest chain drifted by {delta}");
        }
        // The control: something must actually change once the swing is running,
        // or "rest matches" is satisfied by a chain that ignores `attack_anim`.
        let moved = (first_person_arm_chain(Arm::Right, 0.0)
            - first_person_arm_chain(Arm::Right, 0.4))
        .to_cols_array()
        .iter()
        .fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(moved > 0.05, "the swing must move the chain, moved by {moved}");
    }

    /// A drawn bow does not merely select `bow_pulling_2` geometry.  Vanilla's
    /// `ItemInHandRenderer` replaces the ordinary held-item chain with this
    /// BOW transform while the use button is down.
    ///
    /// # Why this gate is written against two *wrong* hypotheses as well
    ///
    /// The version this replaced restated the implementation's own chain as its
    /// expected matrix, so it agreed with the code by construction and stayed
    /// green for as long as the bow was invisible in live play. Both real
    /// divergences it could not see are asserted here as named alternatives that
    /// the measurement must land *away* from:
    ///
    /// * **no leading `applyItemArmTransform`** — the chain starting at the
    ///   BOW-specific translation, which is what shipped;
    /// * **the shake folded into the leading translation's `y`** — displacing
    ///   along camera-space Y instead of the rotated local Y.
    ///
    /// `inverse_arm_height` is deliberately `0.35` rather than `0.0`: at zero the
    /// dip term vanishes and the correct chain coincides with one that never
    /// threaded the equip height through at all, so zero cannot discriminate.
    /// `held_ticks` is `20.0` (full charge, `power == 1.0`) because the shake is
    /// largest there and a zero shake would collapse the second hypothesis into
    /// the first.
    #[test]
    fn charged_bow_pose_matches_vanillas_item_in_hand_transform() {
        let arm = Arm::Right;
        let i = arm.invert();
        let held_ticks = 20.0f32;
        let inverse_arm_height = 0.35f32;
        let power = 1.0f32;
        assert_eq!(
            first_person_bow_power(held_ticks),
            power,
            "full bow charge is 20 elapsed ticks"
        );
        let shake = lodestone_physics::mth::sin(f64::from((held_ticks - 0.1) * 1.3))
            * (power - 0.1)
            * 0.004;
        assert!(
            shake.abs() > 1e-4,
            "the fixture must charge far enough for a shake to exist, got {shake}"
        );

        // `submitArmWithItem` applies `applyItemArmTransform` before the switch
        // for every animation whose `hasCustomArmTransform()` is false, and
        // `ItemUseAnimation.BOW`'s is false.
        let arm_transform =
            Mat4::from_translation(Vec3::new(i * 0.56, -0.52 + inverse_arm_height * -0.6, -0.72));
        // Everything from `case BOW:` onward, in source order.
        let bow_case = Mat4::from_translation(Vec3::new(i * -0.278_568_2, 0.183_443_87, 0.157_315_31))
            * Mat4::from_rotation_x((-13.935f32).to_radians())
            * Mat4::from_rotation_y((i * 35.3).to_radians())
            * Mat4::from_rotation_z((i * -9.785).to_radians())
            * Mat4::from_translation(Vec3::new(0.0, shake, 0.0))
            * Mat4::from_translation(Vec3::new(0.0, 0.0, power * 0.04))
            * Mat4::from_scale(Vec3::new(1.0, 1.0, 1.0 + power * 0.2))
            * Mat4::from_rotation_y((i * -45.0).to_radians());
        // `ItemTransform.NO_TRANSFORM` still centres the model cube.
        let centre = Mat4::from_translation(Vec3::splat(-0.5));

        let expected = arm_transform * bow_case * centre;
        let without_arm_transform = bow_case * centre;
        let shake_in_leading_translation = arm_transform
            * Mat4::from_translation(Vec3::new(
                i * -0.278_568_2,
                0.183_443_87 + shake,
                0.157_315_31,
            ))
            * Mat4::from_rotation_x((-13.935f32).to_radians())
            * Mat4::from_rotation_y((i * 35.3).to_radians())
            * Mat4::from_rotation_z((i * -9.785).to_radians())
            * Mat4::from_translation(Vec3::new(0.0, 0.0, power * 0.04))
            * Mat4::from_scale(Vec3::new(1.0, 1.0, 1.0 + power * 0.2))
            * Mat4::from_rotation_y((i * -45.0).to_radians())
            * centre;

        let actual = first_person_bow_matrix(
            arm,
            held_ticks,
            inverse_arm_height,
            &DisplayTransform::default(),
        );
        let spread = |a: Mat4, b: Mat4| {
            (a - b)
                .to_cols_array()
                .iter()
                .fold(0.0f32, |max, value| max.max(value.abs()))
        };

        let delta = spread(expected, actual);
        assert!(
            delta < 1e-5,
            "a charging bow must use ItemInHandRenderer's BOW pose, drifted by {delta}"
        );
        // Both alternatives are stated as distances rather than as a bare
        // "differs", so a chain that drifted toward either one fails with the
        // number rather than silently satisfying an inequality.
        let missing_arm = spread(without_arm_transform, actual);
        assert!(
            missing_arm > 0.5,
            "omitting applyItemArmTransform must move the pose by the arm offset, \
             but the two chains sit {missing_arm} apart"
        );
        let misplaced_shake = spread(shake_in_leading_translation, actual);
        assert!(
            misplaced_shake > 1e-4,
            "the shake belongs after the rotations, but both placements agree to \
             {misplaced_shake}"
        );
    }

    /// The equip dip must actually reach the bow pose.
    ///
    /// The gate above pins one `inverse_arm_height`; this one pins the *response*,
    /// because a chain that accepted the parameter and dropped it would still
    /// satisfy a single-point comparison written against the same value. Vanilla's
    /// coefficient is `-0.6` per unit, applied to `y` before any rotation, so a
    /// full swap lowers the whole chain by exactly that.
    #[test]
    fn bow_pose_dips_with_the_equip_height() {
        let rested = first_person_bow_chain(Arm::Right, 20.0, 0.0);
        let swapping = first_person_bow_chain(Arm::Right, 20.0, 1.0);
        let drop = swapping.w_axis - rested.w_axis;
        assert!(
            (drop.y - FIRST_PERSON_ITEM_EQUIP_DIP).abs() < 1e-6,
            "a full swap must lower the bow by {FIRST_PERSON_ITEM_EQUIP_DIP}, got {}",
            drop.y
        );
        assert!(
            drop.x.abs() < 1e-6 && drop.z.abs() < 1e-6,
            "the dip is vertical only, got {drop:?}"
        );
    }

    /// The five swing scalars against hand-evaluated vanilla values.
    ///
    /// `a = 0.25` is chosen because `sqrt(0.25) = 0.5` **exactly**, so every
    /// expected number below is a closed form off the unit circle rather than
    /// something read back out of this code:
    ///
    /// ```text
    /// xs = -0.3 · sin(0.5π)    = -0.3 · 1          = -0.3
    /// ys =  0.4 · sin(1.0π)    =  0.4 · 0          =  0.0
    /// zs = -0.4 · sin(0.25π)   = -0.4 · √2/2       = -0.28284271
    /// yr =        sin(0.5π)    =  1                =  1.0
    /// zr =        sin(0.0625π) =  sin(11.25°)      =  0.19509032
    /// ```
    ///
    /// This is where the `sqrt` shaping is actually pinned. A linear ramp gives
    /// `xs = -0.3·sin(0.25π) = -0.212`, `yr = 0.707` instead of `1.0` — the arm
    /// still swings, just wrongly, which is exactly the failure the matrix-level
    /// and pixel-level gates cannot distinguish.
    ///
    /// `ys == 0` here is not a weak assertion, it is the `2π` term crossing zero
    /// a quarter of the way in; a `π` typo would give `0.4` and fail loudly.
    #[test]
    fn arm_swing_terms_match_hand_evaluated_vanilla() {
        let t = ArmSwingTerms::new(0.25);
        assert!((t.x_position - -0.3).abs() < 1e-6, "xs {}", t.x_position);
        assert!(t.y_position.abs() < 1e-6, "ys {}", t.y_position);
        assert!(
            (t.z_position - -0.282_842_71).abs() < 1e-6,
            "zs {}",
            t.z_position
        );
        assert!((t.y_rotation - 1.0).abs() < 1e-6, "yr {}", t.y_rotation);
        assert!(
            (t.z_rotation - 0.195_090_32).abs() < 1e-6,
            "zr {}",
            t.z_rotation
        );

        // At a = 1.0 the arm is back at rest in x and y (both `sin` arguments are
        // whole multiples of π) — the property that makes the wrapped
        // `attack_anim_lerp` in `lodestone_entity::pose` land the arm at rest
        // rather than mid-arc.
        let end = ArmSwingTerms::new(1.0);
        assert!(end.x_position.abs() < 1e-6, "xs at end {}", end.x_position);
        assert!(end.y_position.abs() < 1e-6, "ys at end {}", end.y_position);
        assert!(end.y_rotation.abs() < 1e-6, "yr at end {}", end.y_rotation);

        // Every term is zero at rest, which is what `arm_chain_at_rest_matches_
        // the_static_chain` depends on.
        let rest = ArmSwingTerms::new(0.0);
        for (name, v) in [
            ("xs", rest.x_position),
            ("ys", rest.y_position),
            ("zs", rest.z_position),
            ("yr", rest.y_rotation),
            ("zr", rest.z_rotation),
        ] {
            assert_eq!(v, 0.0, "{name} must be 0 at rest");
        }

        // Out of range clamps rather than extrapolating.
        assert_eq!(ArmSwingTerms::new(-1.0).y_rotation, rest.y_rotation);
        assert_eq!(ArmSwingTerms::new(4.0).y_rotation, end.y_rotation);
    }

    // ---- the deferred third-person body: `EntityInstance::part_transforms`,
    // not `first_person_arm_pose` -- see that function's doc comment for why
    // sharing a code path would silently give one of the two the other's pose.

    fn player_slim_mesh() -> EntityMesh {
        EntityMesh::from_named_model("player_slim", &lodestone_assets::entity::player_model(true))
    }

    #[test]
    fn player_model_name_selects_wide_or_slim() {
        assert_eq!(player_model_name(false), "player_wide");
        assert_eq!(player_model_name(true), "player_slim");
        // Both names must be real corpus entries in their own right (not just
        // `canonical_model_name`'s hidden alias target), since a caller with
        // real skin data passes this straight through as a `type_path` — and
        // neither name is a `minecraft:entity_type` registry entry at all
        // (there is no `EntityType::PlayerWide`), so this goes through the
        // surviving `&str` boundary, `canonical_model_name`, not
        // `model_for_type`.
        assert_eq!(canonical_model_name("player_wide"), Some("player_wide"));
        assert_eq!(canonical_model_name("player_slim"), Some("player_slim"));
    }

    /// Vanilla draws two layers per limb: the base skin cube, and a slightly
    /// `grow`n overlay (`hat`/`jacket`/`right_sleeve`/`left_sleeve`/
    /// `right_pants`/`left_pants`) parented to it at `PartPose::ZERO`.
    /// Omitting the overlay looks like a missing-skin-layer bug, not a missing
    /// feature, so this pins that every overlay part is (a) present in the
    /// baked mesh and (b) posed *exactly* onto its base part by the animated
    /// third-person chain -- not just at rest, where a `ZERO`-pose child would
    /// trivially agree with its parent even if the composition were wrong.
    #[test]
    fn outer_layer_parts_follow_their_base_part_exactly() {
        for (name, mesh) in [("player_wide", player_mesh()), ("player_slim", player_slim_mesh())] {
            let anim = AnimInput {
                head_yaw_deg: 25.0,
                head_pitch_deg: -15.0,
                limb_swing: 3.7,
                limb_swing_amount: 1.0,
                attack_anim: 0.0,
                age_ticks: 40.0,
                aggressive: false,
                ..AnimInput::REST
            };
            let instance =
                EntityInstance::new(name, &mesh, Vec3::new(1.0, 0.0, 2.0), 37.0, 1.0, &anim);
            let pairs = [
                ("head", "hat"),
                ("body", "jacket"),
                ("right_arm", "right_sleeve"),
                ("left_arm", "left_sleeve"),
                ("right_leg", "right_pants"),
                ("left_leg", "left_pants"),
            ];
            for (base, overlay) in pairs {
                let bi = mesh.skeleton.index_of(base).unwrap_or_else(|| panic!("{name}.{base}"));
                let oi =
                    mesh.skeleton.index_of(overlay).unwrap_or_else(|| panic!("{name}.{overlay}"));
                let b = instance.part_transforms[bi].to_cols_array();
                let o = instance.part_transforms[oi].to_cols_array();
                for i in 0..16 {
                    assert!(
                        (b[i] - o[i]).abs() < 1e-5,
                        "{name}: {overlay} must be posed exactly onto {base} (a PartPose::ZERO \
                         child), element {i} differs: {} vs {}",
                        b[i],
                        o[i]
                    );
                }
            }
        }
    }

    /// The whole-body third-person chain is
    /// `entity_model_matrix(feet, yaw, scale) * Skeleton::pose(anim)[part]`
    /// (see [`EntityInstance::new`]) -- the *same* `scale(-1,-1,1)`-carrying
    /// placement matrix the module doc already proves has determinant `+1` for
    /// any rigid part chain, just exercised over every part of a real player
    /// mesh (including the outer-layer overlays) instead of asserted once in
    /// prose. A negative determinant here would mean a player rendered
    /// inside-out the moment a third-person camera exists to look at one.
    #[test]
    fn third_person_body_part_transforms_preserve_winding() {
        for (name, mesh) in [("player_wide", player_mesh()), ("player_slim", player_slim_mesh())] {
            for yaw in [0.0, 47.0, 90.0, 181.0, 300.0] {
                let anim = AnimInput {
                    limb_swing: yaw * 0.1,
                    limb_swing_amount: 1.0,
                    ..AnimInput::REST
                };
                let instance =
                    EntityInstance::new(name, &mesh, Vec3::new(3.0, 5.0, -2.0), yaw, 1.0, &anim);
                assert!(
                    !instance.part_transforms.is_empty(),
                    "{name}: expected a non-empty part chain"
                );
                for (i, part) in instance.part_transforms.iter().enumerate() {
                    assert!(
                        part.determinant() > 0.0,
                        "{name} part {i} at yaw {yaw}: determinant must be positive, was {}",
                        part.determinant()
                    );
                }
            }
        }
    }

    #[test]
    fn hand_projection_survives_a_degenerate_aspect() {
        assert!(hand_projection(0.0).to_cols_array().iter().all(|v| v.is_finite()));
        assert!(hand_projection(f32::NAN).to_cols_array().iter().all(|v| v.is_finite()));
    }

    #[test]
    fn a_missing_left_hand_slot_falls_back_to_the_right_hand_one() {
        let right = DisplayTransform {
            rotation: [75.0, 45.0, 0.0],
            translation: [0.0, 2.5, 0.0],
            scale: [0.375, 0.375, 0.375],
        };
        let d = DisplayTransforms::NONE.with(DisplaySlot::ThirdPersonRightHand, right);
        assert_eq!(d.get(DisplaySlot::ThirdPersonLeftHand), right);
        assert_eq!(
            d.declared(DisplaySlot::ThirdPersonLeftHand),
            None,
            "the fallback must not pretend the slot was declared"
        );
        // A slot with no fallback rule still reads as the identity.
        assert_eq!(
            d.get(DisplaySlot::Ground),
            DisplayTransform::default(),
            "an undeclared non-hand slot is vanilla's NO_TRANSFORM"
        );
    }
}
