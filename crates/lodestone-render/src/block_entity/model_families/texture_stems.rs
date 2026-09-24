/// Every sheet stem across every block-entity family — what the shell's
/// texture loader preloads. Union of [`chest_texture_stems`],
/// [`skull_texture_stems`], [`bell_texture_stems`],
/// [`banner_texture_stems`], [`shield_texture_stems`] and
/// [`shulker_texture_stems`] rather than the shell iterating each list
/// itself, so a new family only has to update this one function to reach the
/// loader (see the module doc's "How to change it" — this is the "entry in
/// the preload list" step, generalised past chest).
///
/// **Does not include a banner's pattern-mask sprites.** Those are a wholly
/// separate resource (the banner-pattern atlas, `lodestone-assets` work not
/// yet done — see `docs/banner-shield-patterns.md`'s "jar ships individual
/// sprite PNGs" section) and a wholly separate draw list
/// ([`BannerLayerDraw`]), not a stem this preload list can name.
#[must_use]
pub fn block_entity_texture_stems() -> Vec<&'static str> {
    let mut stems = chest_texture_stems();
    stems.extend(skull_texture_stems());
    stems.extend(bell_texture_stems());
    stems.extend(banner_texture_stems());
    stems.extend(shield_texture_stems());
    stems.extend(shulker_texture_stems());
    stems.extend(book_texture_stems());
    stems.extend(decorated_pot_texture_stems());
    stems.extend(conduit_texture_stems());
    stems.extend(copper_golem_statue_texture_stems());
    stems
}
use super::*;
