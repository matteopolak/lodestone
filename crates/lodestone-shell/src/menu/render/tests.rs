//! Tests for the frame model and its geometry, kept beside the production
//! renderer so pixel and data-path assertions share the same helpers.

use super::*;
use super::account_screen::{ACCOUNTS_BUTTON_W, ACCOUNTS_FOOTER_SPACING, ACCOUNTS_HEAD_ICON, AccountsBlock, accounts_block, accounts_button_slot, accounts_failed_frame, accounts_idle_frame};
use super::draw::{
    Quads, TOOLTIP_BG, TOOLTIP_LINE_H, TOOLTIP_MOUSE_OFFSET, TOOLTIP_PAD, restyle_wrapped,
    tooltip_content_origin, wrap_bounded, wrap_measured,
};
use super::renderer::{FLOATS_PER_VERTEX, SPRITE_FLOATS_PER_VERTEX};
use super::screens::credits_frame;
use super::server_list::{SERVER_ENTRY_SPACING, SERVER_ICON_DARKEN, SERVER_JOIN_SPRITES, SERVER_LIST_REF_CANVAS, SERVER_LIST_ROW_W, SERVER_MOVE_DOWN_SPRITES, SERVER_MOVE_UP_SPRITES, ServerListBlock};
use super::title_pause::pause_menu_grid_with;
use super::world_list::{WorldSelectBlock, world_select_block};
use crate::menu::nav::{MenuKey, MenuNav};
use crate::menu::servers::ServerEntry;
use crate::menu::status::{ServerStatus, StatusCache, unavailable_probe};
use crate::menu::{Screen, SessionKind, UiState};

mod title_command;
mod server_render_tests;
mod frame_geometry;
mod accounts;
mod screen_frames;
mod widget_sprites;
mod world_select;
mod settings;
mod loading;
mod packs;
mod layered_screens;

/// Vertex stride in the emitted buffer.
const STRIDE: usize = FLOATS_PER_VERTEX;

/// A death message with no style, click or hover — for a gate that only
/// cares about the screen this text lands on, not its interactivity.
fn plain_death_message(text: &str) -> Vec<lodestone_model::text::InteractiveTextSpan> {
    vec![lodestone_model::text::InteractiveTextSpan {
        text: text.to_string(),
        style: lodestone_model::TextStyle::default(),
        click: None,
        hover: None,
        insertion: None,
    }]
}

/// A nav with a temporary (never-loaded) list path, so no test reads the
/// developer's real `servers.json`.
fn test_nav(tag: &str) -> MenuNav {
    test_nav_path(tag, true)
}

/// The shared body. `owned` seeds a one-account `profiles.json` beside the
/// server list **before** construction, because `AccountsNav` reads the roster
/// once in its constructor.
///
/// Seeded by default: every frame test in this file is about what a player who
/// can play sees, and an empty roster would make `frame_for` return the
/// ownership gate for all of them.
fn test_nav_path(tag: &str, owned: bool) -> MenuNav {
    let path = std::env::temp_dir().join(format!(
        "lodestone-render-{}-{tag}/servers.json",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    if owned {
        let mut meta = lodestone_auth::AccountsMetadata::default();
        let id = uuid::Uuid::new_v4();
        meta.upsert(lodestone_auth::AccountProfile {
            profile_id: id,
            username: "OwnerAccount".to_owned(),
            skin_url: None,
            last_used: 1,
        });
        meta.selected = Some(id);
        meta.save_to(&path.parent().unwrap().join("profiles.json"))
            .expect("the temp roster must be writable");
    }
    MenuNav::with_path(path)
}

fn add_server(nav: &mut MenuNav, ui: &mut UiState, name: &str, addr: &str) {
    let back = ui.screen();
    if back != Screen::ServerList {
        *ui = UiState::new();
        ui.open_server_list();
    }
    nav.key(ui, MenuKey::Char('a'));
    for c in name.chars() {
        nav.key(ui, MenuKey::Char(c));
    }
    nav.key(ui, MenuKey::Tab);
    for c in addr.chars() {
        nav.key(ui, MenuKey::Char(c));
    }
    nav.key(ui, MenuKey::Enter);
}

/// A nav sitting on the multiplayer screen with `servers` saved, reached the
/// way a player reaches it.
fn list_nav(tag: &str, servers: &[(&str, &str)]) -> (MenuNav, UiState) {
    let mut nav = test_nav(tag);
    let mut ui = UiState::new();
    ui.open_server_list();
    for (name, address) in servers {
        add_server(&mut nav, &mut ui, name, address);
    }
    assert_eq!(ui.screen(), Screen::ServerList, "premise: the list is up");
    assert_eq!(nav.list().len(), servers.len());
    (nav, ui)
}

/// The bounding box of every colour-stream vertex drawn in exactly `want`, in
/// logical pixels, or `None` if that colour never appeared.
///
/// Keyed on the **colour** rather than on a rect, because the thing under test
/// here is *where* a mark landed: a rect-shaped detector would need to know the
/// answer first. Reports a box, never a count, per `CLAUDE.md`.
fn colour_bounds(colour: &[f32], w: f32, h: f32, want: [f32; 4]) -> Option<(f32, f32, f32, f32)> {
    let (mut x0, mut y0) = (f32::MAX, f32::MAX);
    let (mut x1, mut y1) = (f32::MIN, f32::MIN);
    let mut seen = false;
    for v in colour.chunks_exact(STRIDE) {
        if (2..6).any(|c| (v[c] - want[c - 2]).abs() > 1e-4) {
            continue;
        }
        seen = true;
        let px = (v[0] + 1.0) * 0.5 * w;
        let py = (1.0 - v[1]) * 0.5 * h;
        x0 = x0.min(px);
        y0 = y0.min(py);
        x1 = x1.max(px);
        y1 = y1.max(py);
    }
    seen.then_some((x0, y0, x1 - x0, y1 - y0))
}

/// As [`colour_bounds`], restricted to vertices whose logical-pixel `y` falls
/// in `y_range`.
///
/// Vanilla's own `FULL` chunk-status colour is opaque white — the same
/// [`LABEL`] colour every plain-text label in this file draws with, and the
/// jar-less debug font (no [`crate::hud::VanillaFont`] attached, which is
/// every headless test's path) emits one 1x1 px white [`Quads::rect`] per lit
/// glyph pixel. So a bare [`colour_bounds`] for white unions a Full grid
/// cell with whatever label text happens to be on screen — not a bug in the
/// draw, just two unrelated things sharing vanilla's own colour. This keys on
/// **position** instead, the same discriminator `CLAUDE.md` asks for, rather
/// than trying to invent a colour vanilla does not use.
fn colour_bounds_in_band(
    colour: &[f32],
    w: f32,
    h: f32,
    want: [f32; 4],
    y_range: (f32, f32),
) -> Option<(f32, f32, f32, f32)> {
    let (mut x0, mut y0) = (f32::MAX, f32::MAX);
    let (mut x1, mut y1) = (f32::MIN, f32::MIN);
    let mut seen = false;
    for v in colour.chunks_exact(STRIDE) {
        if (2..6).any(|c| (v[c] - want[c - 2]).abs() > 1e-4) {
            continue;
        }
        let py = (1.0 - v[1]) * 0.5 * h;
        if py < y_range.0 - 0.5 || py > y_range.1 + 0.5 {
            continue;
        }
        seen = true;
        let px = (v[0] + 1.0) * 0.5 * w;
        x0 = x0.min(px);
        y0 = y0.min(py);
        x1 = x1.max(px);
        y1 = y1.max(py);
    }
    seen.then_some((x0, y0, x1 - x0, y1 - y0))
}

/// Asserts `got` — a [`colour_bounds`] box — equals `want` within the NDC
/// round-trip's epsilon. A tolerance, not `assert_eq!`: the measurement
/// round-trips through NDC and back, so 555.0 comes out 555.00003.
fn assert_box(got: Option<(f32, f32, f32, f32)>, want: (f32, f32, f32, f32), what: &str) {
    let g = got.unwrap_or_else(|| panic!("{what}: nothing drew, expected {want:?}"));
    let near = (g.0 - want.0).abs() < 0.01
        && (g.1 - want.1).abs() < 0.01
        && (g.2 - want.2).abs() < 0.01
        && (g.3 - want.3).abs() < 0.01;
    assert!(near, "{what}: got {g:?}, expected {want:?}");
}

/// A cache of `Ok` statuses, one per entry in `nav`'s list, each spec
/// `(host, players, sample, online)` seeded so its row resolves to
/// `ServerState::Successful` — the state vanilla shows the "who's online"
/// tooltip for.
fn ok_statuses(
    nav: &MenuNav,
    specs: &[(&str, &str, &[&str], Option<u32>)],
) -> StatusCache {
    // The probe outlives the caller's `specs` (`Probe` is a `'static` `dyn`), so
    // the specs are copied into owned data the closure can hold without borrowing
    // the test's locals.
    let owned: Vec<(String, String, Vec<String>, Option<u32>)> = specs
        .iter()
        .map(|(host, players, sample, online)| {
            (
                host.to_string(),
                players.to_string(),
                sample.iter().map(|s| s.to_string()).collect(),
                *online,
            )
        })
        .collect();
    let mut statuses = StatusCache::with_probe(std::sync::Arc::new(move |e: &ServerEntry| {
        let (_, players, sample, online) = owned
            .iter()
            .find(|(host, ..)| host == &e.host)
            .expect("the probe only ever sees an entry the test added");
        Ok(ServerStatus {
            motd: "hi".into(),
            motd_spans: Vec::new(),
            players: players.clone(),
            online: *online,
            sample: sample.clone(),
            version: "26.2".into(),
            protocol: Some(crate::menu::status::STATUS_PROTOCOL),
            favicon_png: None,
            latency_ms: Some(5),
        })
    }));
    let entries = nav.list().entries().to_vec();
    statuses.refresh(&entries);
    let deadline = crate::platform::Instant::now() + std::time::Duration::from_secs(5);
    while statuses.pump() == 0 && crate::platform::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(statuses.len(), entries.len(), "every entry must drain");
    statuses
}

/// A synthetic pack carrying the `server_list/*` sprites plus the button set,
/// so sprite *identity* can be asserted with no jar — `button_pack`'s trick.
fn server_list_pack() -> lodestone_assets::ResourceManager {
    use crate::menu::status::{PING_SPRITES, PINGING_SPRITES};
    use lodestone_assets::{MemorySource, ResourceSource};
    let mut src = MemorySource::default();
    for (id, border) in [
        ("widget/button", 3u32),
        ("widget/button_highlighted", 3),
        ("widget/button_disabled", 1),
    ] {
        src.insert(
            format!("assets/minecraft/textures/gui/sprites/{id}.png"),
            solid_rgba_png(200, 20, [10, 20, 30, 255]),
        );
        src.insert(
            format!("assets/minecraft/textures/gui/sprites/{id}.png.mcmeta"),
            format!(
                r#"{{"gui":{{"scaling":{{"type":"nine_slice","width":200,"height":20,"border":{border}}}}}}}"#
            )
            .into_bytes(),
        );
    }
    // Every status sprite at vanilla's own 10×8, and the three 32×32 overlays.
    for id in PING_SPRITES.iter().chain(PINGING_SPRITES.iter()).chain([
        &crate::menu::status::INCOMPATIBLE_SPRITE,
        &crate::menu::status::UNREACHABLE_SPRITE,
    ]) {
        src.insert(
            format!("assets/minecraft/textures/gui/sprites/{id}.png"),
            solid_rgba_png(10, 8, [40, 90, 200, 255]),
        );
    }
    for (a, b) in [
        SERVER_JOIN_SPRITES,
        SERVER_MOVE_UP_SPRITES,
        SERVER_MOVE_DOWN_SPRITES,
    ] {
        for id in [a, b] {
            src.insert(
                format!("assets/minecraft/textures/gui/sprites/{id}.png"),
                solid_rgba_png(32, 32, [200, 40, 90, 255]),
            );
        }
    }
    // The favicon fallback is a **loose** texture, so it arrives through the
    // extras list rather than the sprite glob — the same path the logo takes.
    src.insert(
        crate::resources::UNKNOWN_SERVER_TEXTURE.1,
        solid_rgba_png(32, 32, [70, 70, 70, 255]),
    );
    lodestone_assets::ResourceManager::new(vec![Box::new(src) as Box<dyn ResourceSource>])
}

/// The atlas the two sprite gates below sample against.
fn server_list_atlas() -> GuiAtlas {
    GuiAtlas::build_with_extras(
        &server_list_pack(),
        &[crate::resources::UNKNOWN_SERVER_TEXTURE],
    )
    .expect("synthetic atlas builds")
}

/// Whether any whole **quad** on the sprite stream samples inside `id`'s atlas
/// region.
///
/// `all_uvs_within`'s companion, and needed because the hover overlay blits
/// **three** sprites into the same 32×32 rect: "every UV is inside join" is
/// false by construction there, while "some quad is inside join_highlighted and
/// none is inside join" is exactly the question.
///
/// A *quad* rather than a vertex, and that is not fussiness: the packer may
/// place two sprites edge to edge, and a vertex exactly on the shared edge is
/// inside both regions to within any epsilon. A whole quad can only be inside
/// one of two equal-sized regions.
fn any_quad_within(sprite: &[f32], min: [f32; 2], max: [f32; 2]) -> bool {
    sprite
        .chunks_exact(SPRITE_FLOATS_PER_VERTEX * 6)
        .any(|quad| all_uvs_within(quad, min, max))
}

/// Every sprite-stream UV whose **destination** falls inside `rect`.
///
/// The pair of questions together — where it landed and which region it
/// sampled — is what makes a per-widget assertion possible on a stream that
/// carries every sprite on the screen at once.
fn uvs_in_dest(sprite: &[f32], w: f32, h: f32, rect: (f32, f32, f32, f32)) -> Vec<[f32; 2]> {
    let (rx, ry, rw, rh) = rect;
    sprite
        .chunks_exact(SPRITE_FLOATS_PER_VERTEX)
        .filter(|v| {
            let px = (v[0] + 1.0) * 0.5 * w;
            let py = (1.0 - v[1]) * 0.5 * h;
            px >= rx - 0.01 && px <= rx + rw + 0.01 && py >= ry - 0.01 && py <= ry + rh + 0.01
        })
        .map(|v| [v[2], v[3]])
        .collect()
}

/// What reached one rectangle of the colour stream: how many vertices, and
/// **where**.
///
/// A box rather than a fraction, per `CLAUDE.md`: a gate that reports only a
/// count cannot tell a shifted widget from a missing one, and both of the
/// control-premise failures recorded there were diagnosed by printing a
/// bounding box instead of a percentage.
#[derive(Debug)]
struct BandCoverage {
    count: usize,
    /// `(x0, y0, x1, y1)` in logical pixels, or `None` when nothing reached.
    bounds: Option<(f32, f32, f32, f32)>,
}

/// Colour-stream vertices inside `band`, in logical pixels — the inverse of
/// `Quads::rect`'s `(2x/w - 1, 1 - 2y/h)`.
///
/// **Strict on y, inclusive on x**, and the asymmetry is the whole reason
/// this reads a *band* rather than the field rect. `CLAUDE.md`'s rule is to
/// ask what else already paints here; the answer is the field's own chrome:
///
/// - its background fill and its focus outline's left/right edges sit at the
///   field's outer `x`, which is `BORDER_INSET` outside the band's — so the
///   horizontal test can be inclusive and still exclude them, which keeps the
///   caret's own left edge (exactly at `text_x`) counted;
/// - its outline's **bottom** edge, though, lands *inside* the band's
///   vertical extent while spanning the full field width. Only a strict `y`
///   keeps it out, and an inclusive one would report a bounding box the width
///   of the whole field whatever the value was — a control that fires while
///   measuring something unrelated.
fn band_coverage(
    colour: &[f32],
    w: f32,
    h: f32,
    band: (f32, f32, f32, f32),
) -> BandCoverage {
    let (bx, by, bw, bh) = band;
    let mut count = 0;
    let (mut x0, mut y0) = (f32::MAX, f32::MAX);
    let (mut x1, mut y1) = (f32::MIN, f32::MIN);
    for v in colour.chunks_exact(STRIDE) {
        let px = (v[0] + 1.0) * 0.5 * w;
        let py = (1.0 - v[1]) * 0.5 * h;
        if px >= bx - 0.01 && px <= bx + bw + 0.01 && py > by && py < by + bh {
            count += 1;
            x0 = x0.min(px);
            y0 = y0.min(py);
            x1 = x1.max(px);
            y1 = y1.max(py);
        }
    }
    BandCoverage {
        count,
        bounds: (count > 0).then_some((x0, y0, x1, y1)),
    }
}

fn frame_with(rows: Vec<MenuRow>, selected: usize) -> MenuFrame<'static> {
    MenuFrame {
        title: "LODESTONE",
        subtitle: "",
        rows,
        selected,
        footer: vec![],
        message: None,
        gui_scale: 0,
        backdrop: MenuBackdrop::Panorama,
        ..Default::default()
    }
}

fn button(label: &str) -> MenuRow {
    MenuRow {
        label: label.to_string(),
        enabled: true,
        ..Default::default()
    }
}

/// Fraction of sample points inside the pixel rect `(x, y, w, h)` that any
/// emitted quad covers with a colour other than the background.
///
/// This is the coverage measurement the repo's rules call for: it asks
/// *where* pixels landed, not how many vertices came out, so a layout bug
/// that draws everything off-screen fails it.
fn coverage(verts: &[f32], w: f32, h: f32, rect: (f32, f32, f32, f32)) -> f32 {
    let (rx, ry, rw, rh) = rect;
    const N: usize = 24;
    let mut hit = 0usize;
    for iy in 0..N {
        for ix in 0..N {
            let px = rx + rw * (ix as f32 + 0.5) / N as f32;
            let py = ry + rh * (iy as f32 + 0.5) / N as f32;
            // NDC of this sample.
            let nx = 2.0 * px / w - 1.0;
            let ny = 1.0 - 2.0 * py / h;
            if covered(verts, nx, ny) {
                hit += 1;
            }
        }
    }
    hit as f32 / (N * N) as f32
}

/// Whether any emitted quad other than the full-screen background covers
/// NDC point `(nx, ny)`. Quads are axis-aligned pairs of triangles, so the
/// first and fifth vertex of each six give the corners.
fn covered(verts: &[f32], nx: f32, ny: f32) -> bool {
    verts
        .chunks_exact(STRIDE * 6)
        .skip(1) // vertex 0..6 is the background clear rect
        .any(|q| {
            let (x0, y0) = (q[0], q[1]);
            let (x1, y1) = (q[STRIDE * 4], q[STRIDE * 4 + 1]);
            let (lo_x, hi_x) = (x0.min(x1), x0.max(x1));
            let (lo_y, hi_y) = (y0.min(y1), y0.max(y1));
            nx >= lo_x && nx <= hi_x && ny >= lo_y && ny <= hi_y
        })
}

/// The colour of the *last* (i.e. topmost-painted) quad covering NDC
/// point `(nx, ny)`, or `None` if only the background is there.
///
/// Unlike `covered`, which only answers "is anything here", this can
/// tell a row's own fill (`ROW_BG`/`ROW_SEL`) apart from a border drawn
/// on top of it in a different colour — necessary because the fill
/// quad already covers every pixel the border does, so presence alone
/// cannot distinguish "outlined" from "an ordinary row". Quads are
/// pushed in paint order, so the last one in the buffer that covers the
/// point is the one actually visible there.
fn colour_at(verts: &[f32], nx: f32, ny: f32) -> Option<[f32; 4]> {
    verts
        .chunks_exact(STRIDE * 6)
        .skip(1) // vertex 0..6 is the background clear rect
        .filter(|q| {
            let (x0, y0) = (q[0], q[1]);
            let (x1, y1) = (q[STRIDE * 4], q[STRIDE * 4 + 1]);
            let (lo_x, hi_x) = (x0.min(x1), x0.max(x1));
            let (lo_y, hi_y) = (y0.min(y1), y0.max(y1));
            nx >= lo_x && nx <= hi_x && ny >= lo_y && ny <= hi_y
        })
        .last()
        .map(|q| [q[2], q[3], q[4], q[5]])
}

/// Fraction of sample points inside `(x, y, w, h)` whose topmost quad is
/// (approximately) `colour` — see `colour_at`. Where `coverage`'s
/// colour-blind "is anything here" cannot separate a highlight border
/// from the row fill it is painted over, this can.
fn coverage_of(
    verts: &[f32],
    w: f32,
    h: f32,
    rect: (f32, f32, f32, f32),
    colour: [f32; 4],
) -> f32 {
    let (rx, ry, rw, rh) = rect;
    const N: usize = 24;
    let mut hit = 0usize;
    for iy in 0..N {
        for ix in 0..N {
            let px = rx + rw * (ix as f32 + 0.5) / N as f32;
            let py = ry + rh * (iy as f32 + 0.5) / N as f32;
            let nx = 2.0 * px / w - 1.0;
            let ny = 1.0 - 2.0 * py / h;
            let matches = colour_at(verts, nx, ny)
                .is_some_and(|c| c.iter().zip(colour).all(|(a, b)| (a - b).abs() < 0.01));
            if matches {
                hit += 1;
            }
        }
    }
    hit as f32 / (N * N) as f32
}

// -- the account screen -----------------------------------------------------

/// A nav whose `profiles.json` holds `names`, most-recently-used **first**
/// (the order `AccountsNav::ordered` sorts into, so `names[0]` is row 0).
/// Written beside a temp `servers.json`, which is where `MenuNav::with_path`
/// looks for it.
fn accounts_nav(tag: &str, names: &[&str]) -> MenuNav {
    let path = std::env::temp_dir().join(format!(
        "lodestone-render-accounts-{}-{tag}/servers.json",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    let mut meta = lodestone_auth::metadata::AccountsMetadata::default();
    for (i, name) in names.iter().enumerate() {
        meta.upsert(lodestone_auth::metadata::AccountProfile {
            profile_id: uuid::Uuid::new_v4(),
            username: (*name).to_string(),
            skin_url: None,
            last_used: (names.len() - i) as u64,
        });
    }
    meta.save_to(&path.parent().unwrap().join("profiles.json"))
        .expect("the temp profiles file must be writable");
    MenuNav::with_path(path)
}

/// A 64×64 skin sheet whose **face** and **hat** rects are each one flat colour,
/// so a mosaic built from it is predictable without reproducing the box filter
/// here. Everything outside those two 8×8 rects is a third colour that must
/// never reach the avatar.
fn skin_sheet(face: [u8; 3], hat_alpha: u8, hat: [u8; 3]) -> lodestone_assets::Image {
    let mut rgba = vec![0u8; 64 * 64 * 4];
    for y in 0..64usize {
        for x in 0..64usize {
            let i = (y * 64 + x) * 4;
            // The "must never be sampled" filler: a colour that is neither the
            // face nor the hat, so a wrong crop rect is visible rather than
            // plausible.
            let (c, a) = if (8..16).contains(&x) && (8..16).contains(&y) {
                (face, 255)
            } else if (40..48).contains(&x) && (8..16).contains(&y) {
                (hat, hat_alpha)
            } else {
                ([0x00, 0xFF, 0x00], 255)
            };
            rgba[i] = c[0];
            rgba[i + 1] = c[1];
            rgba[i + 2] = c[2];
            rgba[i + 3] = a;
        }
    }
    lodestone_assets::Image {
        width: 64,
        height: 64,
        rgba,
    }
}

/// An accounts nav holding `n` generated accounts (so `n + 1` logical rows) with the
/// list parked at `scroll` **pixels**.
///
/// The offset is set through `AccountsNav::scroll_by`, i.e. the real wheel path, so a
/// test cannot park the list somewhere the wheel could never reach — and the notch
/// count is derived from the rate rather than being restated, so this helper does not
/// quietly encode a second opinion about how far a notch goes.
fn accounts_nav_scrolled(tag: &str, n: usize, scroll: f32) -> MenuNav {
    let names: Vec<String> = (0..n).map(|i| format!("p{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let nav = accounts_nav(tag, &refs);
    let accounts = nav.accounts();
    let rate = crate::menu::render::accounts_list_spec(n + 1, 0.0)
        .model(crate::config::MIN_SCALED_HEIGHT as f32)
        .expect("the band must exist at the minimum canvas")
        .scroll_rate();
    accounts.scroll_by(-(scroll / rate), crate::config::MIN_SCALED_HEIGHT as f32);
    assert_eq!(
        accounts.scroll(),
        scroll,
        "the helper failed to park the list at {scroll} px"
    );
    nav
}

/// The canvas vanilla's own default window resolves to (854×480 at GUI
/// scale 1 is vanilla's canonical GUI size), so the expected rects below are
/// the numbers a vanilla screenshot at that size would show.
const V_W: f32 = 854.0;
/// See [`V_W`].
const V_H: f32 = 480.0;

/// A synthetic pack carrying just the three `widget/button*` sprites, each a
/// different size so its atlas region is identifiable, and each with a
/// **different nine-slice border** in its `.mcmeta` — 3 / 3 / 1, exactly the
/// real 26.2 pack's values, which is what lets a test tell "border read from
/// the pack" apart from "border hardcoded to 3".
#[cfg(test)]
fn button_pack() -> lodestone_assets::ResourceManager {
    use lodestone_assets::{MemorySource, ResourceSource};
    let mut src = MemorySource::default();
    for (id, border) in [
        ("widget/button", 3u32),
        ("widget/button_highlighted", 3),
        ("widget/button_disabled", 1),
    ] {
        src.insert(
            format!("assets/minecraft/textures/gui/sprites/{id}.png"),
            solid_rgba_png(200, 20, [10, 20, 30, 255]),
        );
        src.insert(
            format!("assets/minecraft/textures/gui/sprites/{id}.png.mcmeta"),
            format!(
                r#"{{"gui":{{"scaling":{{"type":"nine_slice","width":200,"height":20,"border":{border}}}}}}}"#
            )
            .into_bytes(),
        );
    }
    // A 15×15 icon, so the icon-button path has something to draw too.
    src.insert(
        "assets/minecraft/textures/gui/sprites/icon/language.png",
        solid_rgba_png(15, 15, [90, 200, 90, 255]),
    );
    lodestone_assets::ResourceManager::new(vec![Box::new(src) as Box<dyn ResourceSource>])
}

/// The atlas rect of a sprite id, in normalised UVs — the ground truth a
/// "which sprite was sampled" assertion compares against.
fn sprite_uv_bounds(atlas: &GuiAtlas, id: &str) -> ([f32; 2], [f32; 2]) {
    let loc: lodestone_assets::ResourceLocation =
        format!("minecraft:gui/sprites/{id}").parse().expect("location");
    let s = atlas.atlas().sprite(&loc).expect("sprite placed");
    let (aw, ah) = (atlas.atlas().width as f32, atlas.atlas().height as f32);
    (
        [s.x as f32 / aw, s.y as f32 / ah],
        [
            (s.x + s.width) as f32 / aw,
            (s.y + s.height) as f32 / ah,
        ],
    )
}

/// Whether every sprite-stream vertex's UV lies inside `(min, max)`.
fn all_uvs_within(sprite: &[f32], min: [f32; 2], max: [f32; 2]) -> bool {
    !sprite.is_empty()
        && sprite.chunks_exact(SPRITE_FLOATS_PER_VERTEX).all(|v| {
            v[2] >= min[0] - 1e-6
                && v[2] <= max[0] + 1e-6
                && v[3] >= min[1] - 1e-6
                && v[3] <= max[1] + 1e-6
        })
}

/// The **destination** bounding box of every sprite-stream vertex, back in
/// logical pixels — the inverse of `Quads::rect`'s
/// `(2x/w - 1, 1 - 2y/h)`.
///
/// This is what turns "a sprite was drawn" into "a sprite was drawn *there*",
/// and it reports a box rather than a fraction so a failure says where
/// (`CLAUDE.md`: a gate that reports only a percentage cannot tell a shifted
/// widget from a missing one). `GuiAtlas::geometry`'s quads "tile the target
/// exactly, with no gaps or overlap", so for an integral rect this *is* the
/// rect — but the round trip through NDC and back costs a few `f32` ulps
/// (`327` can come back as `326.99997`), so callers compare within a hundredth
/// of a pixel rather than with `assert_eq!`. Two orders of magnitude below the
/// one pixel a real layout error moves something by.
fn sprite_dest_bounds(sprite: &[f32], w: f32, h: f32) -> (f32, f32, f32, f32) {
    assert!(!sprite.is_empty(), "no sprite quads to measure");
    let (mut x0, mut y0) = (f32::MAX, f32::MAX);
    let (mut x1, mut y1) = (f32::MIN, f32::MIN);
    for v in sprite.chunks_exact(SPRITE_FLOATS_PER_VERTEX) {
        let px = (v[0] + 1.0) * 0.5 * w;
        let py = (1.0 - v[1]) * 0.5 * h;
        x0 = x0.min(px);
        y0 = y0.min(py);
        x1 = x1.max(px);
        y1 = y1.max(py);
    }
    (x0, y0, x1 - x0, y1 - y0)
}

/// Whether **any** emitted quad's UV *centre* lies strictly inside
/// `(min, max)`.
///
/// Centres, not vertices: the atlas packs sprites edge to edge, so a
/// neighbouring sprite's quad has vertices exactly *on* this region's
/// boundary. The first version of the icon test tested vertices and its
/// negative control failed — correctly — because a button-background quad
/// shares an edge with the icon's region.
fn any_quad_centre_in(sprite: &[f32], min: [f32; 2], max: [f32; 2]) -> bool {
    sprite
        .chunks_exact(SPRITE_FLOATS_PER_VERTEX * 6)
        .any(|q| {
            let (u0, v0) = (q[2], q[3]);
            let (u1, v1) = (q[SPRITE_FLOATS_PER_VERTEX * 4 + 2], q[SPRITE_FLOATS_PER_VERTEX * 4 + 3]);
            let (cu, cv) = ((u0 + u1) * 0.5, (v0 + v1) * 0.5);
            cu > min[0] && cu < max[0] && cv > min[1] && cv < max[1]
        })
}

/// Floats the flat-fill fallback would contribute for `frame`'s slotted rows
/// (one quad each, plus a 4-quad outline for the selected one) — the term the
/// real-pack gate subtracts to say "no button fell back".
fn geometry_button_fill_floats(frame: &MenuFrame<'_>, _w: f32, _h: f32) -> usize {
    let slotted = frame.rows.iter().filter(|r| r.slot.is_some()).count();
    let selected = frame
        .rows
        .get(frame.selected)
        .is_some_and(|r| r.slot.is_some()) as usize;
    (slotted + selected * 4) * STRIDE * 6
}

/// A real single-colour PNG of arbitrary dimensions. `solid_png` below is
/// square-only and is what the favicon tests want; the button pack needs
/// 200×20.
fn solid_rgba_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("write header");
        let data: Vec<u8> = (0..w * h).flat_map(|_| rgba).collect();
        writer.write_image_data(&data).expect("write image");
    }
    out
}


// -- world select --------------------------------------------

/// A nav and a `UiState` sitting on the world-select screen, reached the way
/// a player reaches it: by activating the title screen's Singleplayer button.
///
/// That is the anti-island premise for this whole screen — if the button no
/// longer opens it, every test below fails at this assertion rather than
/// quietly testing a screen nothing can reach.
fn world_select_nav(tag: &str) -> (MenuNav, UiState) {
    let mut nav = test_nav(tag);
    let mut ui = UiState::new();
    assert_eq!(
        nav.main_button(),
        crate::menu::nav::MainButton::Singleplayer,
        "premise: Singleplayer is the initially selected title-screen button"
    );
    let action = nav.key(&mut ui, MenuKey::Enter);
    assert_eq!(action, crate::menu::nav::MenuAction::None);
    assert_eq!(
        ui.screen(),
        Screen::WorldSelect,
        "the title screen's Singleplayer button must open the world list"
    );
    (nav, ui)
}

/// The same, with `names` planted in this nav's own (temp) saves root first —
/// so the list the screen enumerates is non-empty.
///
/// The worlds are written with the **production** codec
/// (`lodestone_anvil::level_dat`), because `crate::saves`' enumeration reads them
/// with it: a hand-rolled fixture here would be testing the fixture. `LastPlayed`
/// counts *down* with the index so `names[0]` sorts to row 0 under
/// `WorldSummary::cmp_for_list` (last played descending) — otherwise the row
/// order would depend on the write order, which is not what the screen sorts by.
fn world_select_nav_with_worlds(tag: &str, names: &[&str]) -> (MenuNav, UiState) {
    let nav = test_nav(tag);
    for (i, name) in names.iter().enumerate() {
        let dir = nav.saves_root().join(name);
        std::fs::create_dir_all(&dir).expect("create world dir");
        let mut level = lodestone_anvil::level_dat::LevelDat::for_new_world(
            name,
            &lodestone_anvil::level_dat::Spawn::default(),
            0,
        );
        level
            .set_last_played(1_700_000_000_000 - i as i64 * 1000)
            .expect("sets LastPlayed");
        lodestone_anvil::level_dat::write_to_file(
            &level,
            &lodestone_anvil::level_dat::path_in(&dir),
        )
        .expect("write level.dat");
    }
    let mut nav = nav;
    let mut ui = UiState::new();
    let action = nav.key(&mut ui, MenuKey::Enter);
    assert_eq!(action, crate::menu::nav::MenuAction::None);
    assert_eq!(ui.screen(), Screen::WorldSelect);
    assert_eq!(
        nav.world_select().shown_len(),
        names.len(),
        "premise: opening the screen enumerated every planted world"
    );
    (nav, ui)
}

fn world_select_frame(nav: &MenuNav, ui: &UiState) -> MenuFrame<'static> {
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    frame_for(ui, nav, &statuses, &mut fav).expect("the world list owns its frame")
}

/// Which visible row on the current settings page is the nav button for `page`.
fn settings_nav_row(nav: &MenuNav, page: crate::menu::options::SettingsPage) -> Option<usize> {
    use crate::menu::options;
    nav.settings()
        .visible()
        .iter()
        .position(|c| matches!(c.cell, options::Cell::Nav { page: Some(p), .. } if p == page))
}

/// Open the settings tree on `page` the way a player does — by **clicking** nav
/// buttons found in the row list `app` hit-tests, never by writing a page into a
/// field.
///
/// An anti-island premise: if the button that reaches a page stops opening it, the
/// setup fails instead of the assertion. Two levels deep, because two pages are not
/// on the root grid — Mouse hangs off Controls and Online off the root's own header
/// button — and hardcoding a parent table would be the thing this is avoiding. The
/// walk restarts from a fresh screen per candidate parent rather than backing out,
/// so a page reached down the wrong branch cannot leave a nav stack behind.
fn settings_nav_on(page: crate::menu::options::SettingsPage) -> (MenuNav, crate::menu::UiState) {
    use crate::menu::options;
    let fresh = || {
        let nav = test_nav("settings-band-chrome");
        let mut ui = crate::menu::UiState::new();
        ui.open_settings();
        (nav, ui)
    };
    let (mut nav, mut ui) = fresh();
    if let Some(row) = settings_nav_row(&nav, page) {
        nav.click(&mut ui, row);
        assert_eq!(nav.settings().page(), page, "premise: {page:?} is up");
        return (nav, ui);
    }
    let parents: Vec<options::SettingsPage> = nav
        .settings()
        .visible()
        .iter()
        .filter_map(|c| match c.cell {
            options::Cell::Nav { page: Some(p), .. } => Some(p),
            _ => None,
        })
        .collect();
    for parent in parents {
        let (mut n, mut u) = fresh();
        let Some(row) = settings_nav_row(&n, parent) else {
            continue;
        };
        n.click(&mut u, row);
        if let Some(row) = settings_nav_row(&n, page) {
            n.click(&mut u, row);
            if n.settings().page() == page {
                return (n, u);
            }
        }
    }
    panic!("premise: no nav button within two clicks of the root opens {page:?}");
}

/// A real single-colour PNG, encoded here so the favicon test's input is a
/// genuine PNG stream (IHDR/IDAT/IEND with zlib and CRCs) rather than
/// something only our own decoder would accept.
fn solid_png(side: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, side, side);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("write header");
        let data: Vec<u8> = (0..side * side).flat_map(|_| rgba).collect();
        writer.write_image_data(&data).expect("write image");
    }
    out
}

/// A pack fixture for the two gates below: `name`, its description, and either a
/// flat single-pixel `pack.png` or none at all.
///
/// One flat colour rather than a picture, because `head_mosaic`'s box filter
/// averages each cell — so a uniform source makes every mosaic cell exactly
/// [`PACK_FIXTURE_ICON`], and `coverage_of` can then ask whether the *icon* landed
/// rather than whether anything did.
fn pack_fixture(name: &str, description: &str, icon: bool) -> crate::resources::DiscoveredPack {
    crate::resources::DiscoveredPack {
        id: format!("file/{name}"),
        title: name.to_string(),
        description: description.to_string(),
        pack_format: 64,
        icon: icon.then(|| lodestone_assets::Image {
            width: 1,
            height: 1,
            rgba: vec![0x20, 0xC0, 0x40, 0xFF],
        }),
        path: std::path::PathBuf::from("/nonexistent").join(name),
        kind: crate::resources::PackKind::Directory,
    }
}

/// [`pack_fixture`]'s flat `pack.png` (`0x20C040`) as a mosaic cell.
const PACK_FIXTURE_ICON: [f32; 4] = [32.0 / 255.0, 192.0 / 255.0, 64.0 / 255.0, 1.0];

// -- composition order: a selection fill must not bury a fallback icon --------
//
// The reported symptom: selecting a server whose icon is the default one turned
// the 32 px thumbnail solid black, and so did focusing a resource pack, while a
// server with a real favicon and a pack with a real `pack.png` were both fine.
// One cause, in `MenuGeometry`: the renderer drew *every* sprite between the
// backdrop and the rest of the colour stream, so a row's opaque black selection
// fill landed on top of the two fallback icons — which are sprites — no matter
// when it was emitted, while a real icon is a `FaviconMosaic` of flat quads on the
// colour stream and stayed in emission order.

/// One quad from either stream, reduced to what a paint-order question needs.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Paint {
    /// A flat colour quad's RGBA.
    Colour([f32; 4]),
    /// A sprite quad's mean UV, which is what says *which* atlas region it
    /// sampled and therefore which sprite it is.
    Sprite([f32; 2]),
}

/// Whether a quad's destination bounding box contains `at`, in logical pixels.
///
/// **Containment of a point, not vertices-inside-a-rect.** `band_coverage` in this
/// file counts *vertices* inside a rect, so a quad that **encloses** the rect
/// contributes none and reads as zero coverage — a canvas-wide fill measured
/// straight through that probe and it still reported 0. A row's selection fill
/// encloses the 32 px icon exactly, so it is precisely the species that probe
/// cannot see, and sampling one point inside the icon is a detector that can.
fn quad_covers(quad: &[f32], stride: usize, w: f32, h: f32, at: (f32, f32)) -> bool {
    let (mut x0, mut y0) = (f32::MAX, f32::MAX);
    let (mut x1, mut y1) = (f32::MIN, f32::MIN);
    for v in quad.chunks_exact(stride) {
        let px = (v[0] + 1.0) * 0.5 * w;
        let py = (1.0 - v[1]) * 0.5 * h;
        x0 = x0.min(px);
        y0 = y0.min(py);
        x1 = x1.max(px);
        y1 = y1.max(py);
    }
    at.0 >= x0 - 0.01 && at.0 <= x1 + 0.01 && at.1 >= y0 - 0.01 && at.1 <= y1 + 0.01
}

/// Colour quads covering `at` within `from..to` **floats** of the colour stream.
///
/// Both streams are always a whole number of six-vertex quads long, and a
/// `SpriteCut`'s `colour_floats` is a snapshot of that length, so the slice is
/// quad-aligned; the assertion says so rather than trusting it.
fn colour_paints(
    geo: &MenuGeometry,
    w: f32,
    h: f32,
    at: (f32, f32),
    from: usize,
    to: usize,
) -> Vec<Paint> {
    let to = to.min(geo.colour.len());
    if to <= from {
        return Vec::new();
    }
    assert_eq!(from % (STRIDE * 6), 0, "colour cut {from} is mid-quad");
    geo.colour[from..to]
        .chunks_exact(STRIDE * 6)
        .filter(|q| quad_covers(q, STRIDE, w, h, at))
        .map(|q| Paint::Colour([q[2], q[3], q[4], q[5]]))
        .collect()
}

/// [`colour_paints`] for the sprite stream, keyed on the quad's mean UV.
fn sprite_paints(
    geo: &MenuGeometry,
    w: f32,
    h: f32,
    at: (f32, f32),
    from: usize,
    to: usize,
) -> Vec<Paint> {
    let to = to.min(geo.sprite.len());
    if to <= from {
        return Vec::new();
    }
    assert_eq!(
        from % (SPRITE_FLOATS_PER_VERTEX * 6),
        0,
        "sprite cut {from} is mid-quad"
    );
    geo.sprite[from..to]
        .chunks_exact(SPRITE_FLOATS_PER_VERTEX * 6)
        .filter(|q| quad_covers(q, SPRITE_FLOATS_PER_VERTEX, w, h, at))
        .map(|q| {
            let (mut u, mut v) = (0.0f32, 0.0f32);
            for vert in q.chunks_exact(SPRITE_FLOATS_PER_VERTEX) {
                u += vert[2];
                v += vert[3];
            }
            Paint::Sprite([u / 6.0, v / 6.0])
        })
        .collect()
}

/// Everything painted at `at`, in the order `MenuRenderer::draw` replays it —
/// colour up to each [`SpriteCut`], that cut's sprites, then the colour tail.
///
/// Order, not a composited byte: `CLAUDE.md` records that an exact blended byte
/// through `ALPHA_BLENDING` on this backend is not predictable, so the question
/// asked here is which quad went down after which.
fn paint_at(geo: &MenuGeometry, w: f32, h: f32, at: (f32, f32)) -> Vec<Paint> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    for cut in &geo.sprite_cuts {
        out.extend(colour_paints(geo, w, h, at, cursor, cut.colour_floats));
        cursor = cut.colour_floats.max(cursor);
        out.extend(sprite_paints(
            geo,
            w,
            h,
            at,
            cut.sprite_start,
            cut.sprite_end,
        ));
    }
    out.extend(colour_paints(geo, w, h, at, cursor, geo.colour.len()));
    out
}

/// [`paint_at`] under the ordering the renderer used **before** the interleave:
/// the backdrop, then every sprite, then the whole colour tail.
///
/// This is the negative control, and it is a control rather than a description
/// because the gate below runs it and requires it to *fail* the same assertion —
/// `CLAUDE.md`: an assertion of an absence needs a detector observed firing.
fn paint_at_legacy(geo: &MenuGeometry, w: f32, h: f32, at: (f32, f32)) -> Vec<Paint> {
    let mut out = colour_paints(geo, w, h, at, 0, geo.backdrop_floats);
    out.extend(sprite_paints(geo, w, h, at, 0, geo.sprite.len()));
    out.extend(colour_paints(
        geo,
        w,
        h,
        at,
        geo.backdrop_floats,
        geo.colour.len(),
    ));
    out
}

/// Whether a [`Paint`] is a sprite quad sampling inside an atlas region.
fn is_sprite(paint: Paint, min: [f32; 2], max: [f32; 2]) -> bool {
    match paint {
        Paint::Sprite(uv) => {
            uv[0] >= min[0] && uv[0] <= max[0] && uv[1] >= min[1] && uv[1] <= max[1]
        }
        Paint::Colour(_) => false,
    }
}

/// [`sprite_uv_bounds`] for a **loose** texture. The two fallback icons live at
/// `textures/misc/`, outside the `gui/sprites/**` glob, so `build_with_extras`
/// stitches them under a synthetic `lodestone:gui/loose/<id>` location — a
/// different key space, which is why this cannot share the sprite helper.
fn loose_uv_bounds(atlas: &GuiAtlas, id: &str) -> ([f32; 2], [f32; 2]) {
    let loc: lodestone_assets::ResourceLocation = format!("lodestone:gui/loose/{id}")
        .parse()
        .expect("location");
    let s = atlas.atlas().sprite(&loc).expect("loose texture placed");
    let (aw, ah) = (atlas.atlas().width as f32, atlas.atlas().height as f32);
    (
        [s.x as f32 / aw, s.y as f32 / ah],
        [(s.x + s.width) as f32 / aw, (s.y + s.height) as f32 / ah],
    )
}

/// A synthetic atlas for the Resource Packs screen: the four
/// `transferable_list/*` overlays plus the **loose** `misc/unknown_pack`
/// fallback, which only arrives because it is named as an extra.
fn pack_screen_atlas() -> GuiAtlas {
    use lodestone_assets::{MemorySource, ResourceSource};
    let mut src = MemorySource::default();
    for id in [
        super::draw::PACK_SELECT_SPRITES.0,
        super::draw::PACK_SELECT_SPRITES.1,
        super::draw::PACK_UNSELECT_SPRITES.0,
        super::draw::PACK_UNSELECT_SPRITES.1,
    ] {
        src.insert(
            format!("assets/minecraft/textures/gui/sprites/{id}.png"),
            solid_rgba_png(32, 32, [10, 200, 10, 255]),
        );
    }
    src.insert(
        crate::resources::UNKNOWN_PACK_TEXTURE.1,
        solid_rgba_png(32, 32, [70, 70, 70, 255]),
    );
    let manager =
        lodestone_assets::ResourceManager::new(vec![Box::new(src) as Box<dyn ResourceSource>]);
    GuiAtlas::build_with_extras(&manager, &[crate::resources::UNKNOWN_PACK_TEXTURE])
        .expect("synthetic pack atlas builds")
}

/// Where in a paint order the row fill and the fallback icon landed, at one
/// point.
fn fill_then_icon(
    order: &[Paint],
    fill: [f32; 4],
    icon: ([f32; 2], [f32; 2]),
) -> (Option<usize>, Option<usize>) {
    (
        order.iter().position(|p| *p == Paint::Colour(fill)),
        order.iter().position(|p| is_sprite(*p, icon.0, icon.1)),
    )
}

/// Reads as an assertion helper so the check above collects both arms instead of
/// aborting on the first.
fn missing_titles_empty(wrong: &[String]) -> bool {
    wrong.is_empty()
}

/// Open the settings tree on `page` **from inside a world**, the way a player does
/// — enter a world, press Escape, activate the pause menu's Options button, then
/// click nav rows. Never by writing a page or an `in_world` flag into a field.
///
/// The in-world counterpart of `settings_nav_on`, and the premise it asserts is the
/// one that matters here: `UiState::settings_in_world` must come out `true`, because
/// a helper that quietly produced an out-of-world nav would make every assertion
/// below measure the path that already worked.
fn settings_nav_in_world_on(
    page: crate::menu::options::SettingsPage,
) -> (MenuNav, crate::menu::UiState) {
    use crate::menu::options;
    let fresh = || {
        // Not `test_nav`: this nav is seeded with a **non-default** `gui_scale`, so
        // the gate's `gui_scale` claim is discriminating. At the default `0` the
        // stamped and the unstamped value coincide, and an input where both
        // hypotheses give the same answer is not a test. `MenuNav::with_path`
        // derives `options.json` from the list path's directory, so this stays
        // inside the temp dir and never touches the developer's real options.
        let path = std::env::temp_dir().join(format!(
            "lodestone-render-{}-in-world-settings-chrome/servers.json",
            std::process::id()
        ));
        let dir = path.parent().expect("the temp path has a parent");
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).expect("a temp dir for the seeded options");
        std::fs::write(dir.join("options.json"), r#"{"gui_scale":3}"#)
            .expect("seed a non-default gui_scale");
        let mut nav = MenuNav::with_path(path);
        assert_eq!(
            nav.gui_scale(),
            3,
            "premise: the seeded gui_scale loaded, so the stamp claim can fail"
        );
        let mut ui = crate::menu::UiState::new();
        ui.enter_dev_world();
        ui.on_escape();
        assert!(ui.is_paused(), "premise: Escape from a world pauses it");
        // Walk the pause menu to Options and activate it, rather than calling
        // `open_settings_from_pause` directly: that click is what sets
        // `SettingsNav::in_world`, and the two must be set by the same action or
        // this helper builds a state no player can reach.
        for _ in 0..crate::menu::nav::PAUSE_BUTTONS.len() {
            if nav.pause_button() == crate::menu::nav::PauseButton::Options {
                break;
            }
            nav.key(&mut ui, MenuKey::Down);
        }
        assert_eq!(
            nav.pause_button(),
            crate::menu::nav::PauseButton::Options,
            "premise: the pause menu has an Options button to reach"
        );
        nav.key(&mut ui, MenuKey::Enter);
        assert!(
            ui.is_settings() && ui.settings_in_world(),
            "premise: the pause menu's Options button opens settings *in world*"
        );
        (nav, ui)
    };
    let (mut nav, mut ui) = fresh();
    if nav.settings().page() == page {
        return (nav, ui);
    }
    if let Some(row) = settings_nav_row(&nav, page) {
        nav.click(&mut ui, row);
        assert_eq!(nav.settings().page(), page, "premise: {page:?} is up");
        return (nav, ui);
    }
    let parents: Vec<options::SettingsPage> = nav
        .settings()
        .visible()
        .iter()
        .filter_map(|c| match c.cell {
            options::Cell::Nav { page: Some(p), .. } => Some(p),
            _ => None,
        })
        .collect();
    for parent in parents {
        let (mut n, mut u) = fresh();
        let Some(row) = settings_nav_row(&n, parent) else {
            continue;
        };
        n.click(&mut u, row);
        if let Some(row) = settings_nav_row(&n, page) {
            n.click(&mut u, row);
            if n.settings().page() == page {
                return (n, u);
            }
        }
    }
    panic!("premise: no in-world nav button within two clicks of the root opens {page:?}");
}
