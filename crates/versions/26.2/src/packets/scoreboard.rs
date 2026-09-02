//! Clientbound scoreboard, team and boss-bar packets for protocol 776.
//!
//! These are all conditional-field or multi-mode packets whose body shape
//! depends on a leading discriminator byte, so the decoders are hand-written
//! against the 26.2 wire format (behavioural reference only):
//!
//! * `set_objective` — a leading `method` byte (0 add / 1 remove / 2 change);
//!   the display name, render type and number format are present only for add
//!   and change.
//! * `set_display_objective` — a display-slot id plus an objective name where
//!   the empty string means "clear".
//! * `set_score` / `reset_score` — 1.20.3+ split score removal into its own
//!   packet rather than overloading `set_score` with an action byte.
//! * `set_player_team` — five modes (create / remove / update / add-members /
//!   remove-members); the parameter block is present only for create and
//!   update, and the member list only for create, add and remove.
//! * `boss_event` — a UUID then an operation enum selecting one of six bodies.
//!
//! Every decoder consumes exactly the bytes the wire carries; the adapter
//! asserts an empty buffer afterwards, so a wrong conditional branch (which
//! consumes the wrong number of bytes) is caught immediately.

use lodestone_core::{Ctx, Decode, Error, Reader, Result, read_network_nbt};
use lodestone_model::Text;
use uuid::Uuid;

/// Vanilla `readUtf()` default cap (32767 UTF-16 units).
const MAX_STRING: usize = 32_767;
/// Defensive upper bound on a team member list, to avoid a hostile VarInt
/// count triggering a huge allocation.
const MAX_MEMBERS: i32 = 1 << 16;

/// Optional per-score/objective number format (`NumberFormatTypes`).
///
/// The wire is `optional(registry-dispatched)`: a present flag, then a VarInt
/// registry id (`0` blank, `1` styled, `2` fixed) and the type's payload.
#[derive(Debug, Clone, PartialEq)]
pub enum NumberFormat {
    /// Render nothing in place of the number.
    Blank,
    /// Render the number using a style (carried as its component NBT).
    Styled(Text),
    /// Always render this fixed text instead of the number.
    Fixed(Text),
}

fn read_optional_number_format(r: &mut Reader<'_>) -> Result<Option<NumberFormat>> {
    if !r.bool()? {
        return Ok(None);
    }
    let type_id = r.var_i32()?;
    Ok(Some(match type_id {
        0 => NumberFormat::Blank,
        1 => NumberFormat::Styled(Text::from_nbt(&read_network_nbt(r)?)),
        2 => NumberFormat::Fixed(Text::from_nbt(&read_network_nbt(r)?)),
        other => {
            return Err(Error::Custom(format!(
                "unknown number format type id {other}"
            )));
        }
    }))
}

fn read_component(r: &mut Reader<'_>) -> Result<Text> {
    Ok(Text::from_nbt(&read_network_nbt(r)?))
}

fn read_optional_component(r: &mut Reader<'_>) -> Result<Option<Text>> {
    if r.bool()? {
        Ok(Some(read_component(r)?))
    } else {
        Ok(None)
    }
}

/// `set_objective` (create / remove / change).
#[derive(Debug, Clone, PartialEq)]
pub struct SetObjective {
    /// Internal objective name.
    pub name: String,
    /// Method: `0` add, `1` remove, `2` change.
    pub method: u8,
    /// Display name (present for add/change only).
    pub display_name: Option<Text>,
    /// Render type id: `0` integer, `1` hearts (present for add/change only).
    pub render_type: Option<i32>,
    /// Default number format (present for add/change only).
    pub number_format: Option<NumberFormat>,
}

impl Decode for SetObjective {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        let name = r.string(MAX_STRING)?;
        let method = r.u8()?;
        if method == 0 || method == 2 {
            let display_name = Some(read_component(r)?);
            let render_type = Some(r.var_i32()?);
            let number_format = read_optional_number_format(r)?;
            Ok(Self {
                name,
                method,
                display_name,
                render_type,
                number_format,
            })
        } else {
            Ok(Self {
                name,
                method,
                display_name: None,
                render_type: None,
                number_format: None,
            })
        }
    }
}

/// `set_display_objective`: which objective renders in a display slot.
#[derive(Debug, Clone, PartialEq)]
pub struct SetDisplayObjective {
    /// Display slot id: `0` list, `1` sidebar, `2` below-name, `3..=18`
    /// per-team-colour sidebars (black..white).
    pub slot: i32,
    /// Objective name, or `None` when the wire sent the empty string (clear).
    pub objective: Option<String>,
}

impl Decode for SetDisplayObjective {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        let slot = r.var_i32()?;
        let name = r.string(MAX_STRING)?;
        let objective = if name.is_empty() { None } else { Some(name) };
        Ok(Self { slot, objective })
    }
}

/// `set_score`: set one holder's score under an objective.
#[derive(Debug, Clone, PartialEq)]
pub struct SetScore {
    /// Score holder (player name or entity UUID string).
    pub owner: String,
    /// Objective name.
    pub objective: String,
    /// Score value.
    pub score: i32,
    /// Optional custom display for the holder.
    pub display: Option<Text>,
    /// Optional per-score number format.
    pub number_format: Option<NumberFormat>,
}

impl Decode for SetScore {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        let owner = r.string(MAX_STRING)?;
        let objective = r.string(MAX_STRING)?;
        let score = r.var_i32()?;
        let display = read_optional_component(r)?;
        let number_format = read_optional_number_format(r)?;
        Ok(Self {
            owner,
            objective,
            score,
            display,
            number_format,
        })
    }
}

/// `reset_score`: clear a holder's score (1.20.3+ dedicated packet).
#[derive(Debug, Clone, PartialEq)]
pub struct ResetScore {
    /// Score holder.
    pub owner: String,
    /// Objective name, or `None` to clear the holder from every objective.
    pub objective: Option<String>,
}

impl Decode for ResetScore {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        let owner = r.string(MAX_STRING)?;
        let objective = if r.bool()? {
            Some(r.string(MAX_STRING)?)
        } else {
            None
        };
        Ok(Self { owner, objective })
    }
}

/// The formatting/behaviour block carried by team create and update.
#[derive(Debug, Clone, PartialEq)]
pub struct TeamParameters {
    /// Team display name.
    pub display_name: Text,
    /// Prefix prepended to member names.
    pub prefix: Text,
    /// Suffix appended to member names.
    pub suffix: Text,
    /// Name-tag visibility id (`0` always, `1` never, `2` hide-for-other-teams,
    /// `3` hide-for-own-team).
    pub name_tag_visibility: i32,
    /// Collision-rule id (same ordering as visibility, with push semantics).
    pub collision_rule: i32,
    /// Colour id (`ChatFormatting`, `0..=15` for the sixteen colours), or
    /// `None` when the optional colour was absent.
    pub color: Option<i32>,
    /// Whether members can damage each other (options bit 0).
    pub friendly_fire: bool,
    /// Whether members see invisible teammates (options bit 1).
    pub see_friendly_invisibles: bool,
}

fn read_team_parameters(r: &mut Reader<'_>) -> Result<TeamParameters> {
    let display_name = read_component(r)?;
    let prefix = read_component(r)?;
    let suffix = read_component(r)?;
    let name_tag_visibility = r.var_i32()?;
    let collision_rule = r.var_i32()?;
    let color = if r.bool()? { Some(r.var_i32()?) } else { None };
    let options = r.u8()?;
    Ok(TeamParameters {
        display_name,
        prefix,
        suffix,
        name_tag_visibility,
        collision_rule,
        color,
        friendly_fire: options & 1 != 0,
        see_friendly_invisibles: options & 2 != 0,
    })
}

/// `set_player_team` (create / remove / update / add-members / remove-members).
#[derive(Debug, Clone, PartialEq)]
pub struct SetPlayerTeam {
    /// Internal team name.
    pub name: String,
    /// Method: `0` create, `1` remove, `2` update, `3` add-members,
    /// `4` remove-members.
    pub method: u8,
    /// Parameter block (present for create and update only).
    pub parameters: Option<TeamParameters>,
    /// Affected member holders (present for create, add and remove only).
    pub players: Vec<String>,
}

impl Decode for SetPlayerTeam {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        let name = r.string(MAX_STRING)?;
        let method = r.u8()?;
        let parameters = if method == 0 || method == 2 {
            Some(read_team_parameters(r)?)
        } else {
            None
        };
        let players = if method == 0 || method == 3 || method == 4 {
            let count = r.var_i32()?;
            if count < 0 {
                return Err(Error::NegativeLength(count));
            }
            if count > MAX_MEMBERS {
                return Err(Error::LimitExceeded {
                    limit: MAX_MEMBERS as usize,
                    actual: count as usize,
                });
            }
            let mut players = Vec::new();
            for _ in 0..count {
                players.push(r.string(MAX_STRING)?);
            }
            players
        } else {
            Vec::new()
        };
        Ok(Self {
            name,
            method,
            parameters,
            players,
        })
    }
}

/// A boss-bar operation, selected by the leading operation enum.
#[derive(Debug, Clone, PartialEq)]
pub enum BossOp {
    /// Add a new bar.
    Add {
        /// Title component.
        title: Text,
        /// Progress `0.0..=1.0`.
        progress: f32,
        /// Colour id (`0` pink … `6` white).
        color: i32,
        /// Overlay id (`0` progress, `1..=4` notched 6/10/12/20).
        overlay: i32,
        /// Darken the sky.
        darken: bool,
        /// Play boss music.
        music: bool,
        /// Create world fog.
        fog: bool,
    },
    /// Remove the bar.
    Remove,
    /// Update progress only.
    UpdateProgress(f32),
    /// Update title only.
    UpdateName(Text),
    /// Update colour/overlay only.
    UpdateStyle {
        /// Colour id.
        color: i32,
        /// Overlay id.
        overlay: i32,
    },
    /// Update the flag byte only.
    UpdateProperties {
        /// Darken the sky.
        darken: bool,
        /// Play boss music.
        music: bool,
        /// Create world fog.
        fog: bool,
    },
}

/// `boss_event`, keyed by the bar's UUID.
#[derive(Debug, Clone, PartialEq)]
pub struct BossEvent {
    /// Boss-bar id.
    pub id: Uuid,
    /// The operation to apply.
    pub op: BossOp,
}

impl Decode for BossEvent {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        let id = r.uuid()?;
        let op_type = r.var_i32()?;
        let op = match op_type {
            0 => {
                let title = read_component(r)?;
                let progress = r.f32()?;
                let color = r.var_i32()?;
                let overlay = r.var_i32()?;
                let flags = r.u8()?;
                BossOp::Add {
                    title,
                    progress,
                    color,
                    overlay,
                    darken: flags & 1 != 0,
                    music: flags & 2 != 0,
                    fog: flags & 4 != 0,
                }
            }
            1 => BossOp::Remove,
            2 => BossOp::UpdateProgress(r.f32()?),
            3 => BossOp::UpdateName(read_component(r)?),
            4 => BossOp::UpdateStyle {
                color: r.var_i32()?,
                overlay: r.var_i32()?,
            },
            5 => {
                let flags = r.u8()?;
                BossOp::UpdateProperties {
                    darken: flags & 1 != 0,
                    music: flags & 2 != 0,
                    fog: flags & 4 != 0,
                }
            }
            other => {
                return Err(Error::Custom(format!(
                    "unknown boss_event operation {other}"
                )));
            }
        };
        Ok(Self { id, op })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_core::Writer;

    const CTX: Ctx = Ctx { version: 776 };

    /// A network-NBT `{"text": <s>}` compound, matching `read_network_nbt`
    /// framing (root tag id, no root name).
    fn nbt_text(s: &str) -> Vec<u8> {
        let mut out = vec![0x0A]; // TAG_Compound (root)
        out.push(0x08); // TAG_String
        out.extend_from_slice(&4u16.to_be_bytes());
        out.extend_from_slice(b"text");
        out.extend_from_slice(&(s.len() as u16).to_be_bytes());
        out.extend_from_slice(s.as_bytes());
        out.push(0x00); // TAG_End
        out
    }

    fn decode<T: Decode>(bytes: &[u8]) -> T {
        let mut r = Reader::new(bytes);
        let value = T::decode(&mut r, CTX).expect("decode");
        r.ensure_empty().expect("zero trailing bytes");
        value
    }

    #[test]
    fn objective_add_carries_display_and_render_type() {
        let mut w = Writer::default();
        w.string("obj");
        w.u8(0); // add
        w.bytes(&nbt_text("Sidebar Title"));
        w.var_i32(1); // hearts
        w.bool(false); // no number format
        let p: SetObjective = decode(&w.into_vec());
        assert_eq!(p.name, "obj");
        assert_eq!(p.method, 0);
        assert_eq!(p.display_name.unwrap().to_plain_string(), "Sidebar Title");
        assert_eq!(p.render_type, Some(1));
        assert_eq!(p.number_format, None);
    }

    #[test]
    fn objective_remove_has_no_trailing_fields() {
        let mut w = Writer::default();
        w.string("obj");
        w.u8(1); // remove — nothing follows
        let p: SetObjective = decode(&w.into_vec());
        assert_eq!(p.method, 1);
        assert_eq!(p.display_name, None);
        assert_eq!(p.render_type, None);
    }

    #[test]
    fn objective_change_with_fixed_number_format() {
        let mut w = Writer::default();
        w.string("obj");
        w.u8(2); // change
        w.bytes(&nbt_text("Title"));
        w.var_i32(0); // integer
        w.bool(true); // number format present
        w.var_i32(2); // fixed
        w.bytes(&nbt_text("99"));
        let p: SetObjective = decode(&w.into_vec());
        assert_eq!(p.method, 2);
        assert_eq!(
            p.number_format,
            Some(NumberFormat::Fixed(Text::literal("99")))
        );
    }

    #[test]
    fn display_objective_slot_and_clear() {
        let mut w = Writer::default();
        w.var_i32(1); // sidebar
        w.string("obj");
        let p: SetDisplayObjective = decode(&w.into_vec());
        assert_eq!(p.slot, 1);
        assert_eq!(p.objective.as_deref(), Some("obj"));

        let mut w = Writer::default();
        w.var_i32(4); // team colour sidebar (dark_blue)
        w.string(""); // empty ⇒ clear
        let p: SetDisplayObjective = decode(&w.into_vec());
        assert_eq!(p.slot, 4);
        assert_eq!(p.objective, None);
    }

    #[test]
    fn score_with_display_and_without_format() {
        let mut w = Writer::default();
        w.string("Alice");
        w.string("obj");
        w.var_i32(42);
        w.bool(true); // display present
        w.bytes(&nbt_text("Ally"));
        w.bool(false); // no number format
        let p: SetScore = decode(&w.into_vec());
        assert_eq!(p.owner, "Alice");
        assert_eq!(p.objective, "obj");
        assert_eq!(p.score, 42);
        assert_eq!(p.display.unwrap().to_plain_string(), "Ally");
        assert_eq!(p.number_format, None);
    }

    #[test]
    fn reset_score_nullable_objective() {
        let mut w = Writer::default();
        w.string("Alice");
        w.bool(true);
        w.string("obj");
        let p: ResetScore = decode(&w.into_vec());
        assert_eq!(p.owner, "Alice");
        assert_eq!(p.objective.as_deref(), Some("obj"));

        let mut w = Writer::default();
        w.string("Alice");
        w.bool(false); // no objective
        let p: ResetScore = decode(&w.into_vec());
        assert_eq!(p.objective, None);
    }

    #[test]
    fn team_create_with_parameters_and_members() {
        let mut w = Writer::default();
        w.string("red");
        w.u8(0); // create
        // parameters
        w.bytes(&nbt_text("Red Team"));
        w.bytes(&nbt_text("[R] "));
        w.bytes(&nbt_text(""));
        w.var_i32(2); // name tag: hide for other teams
        w.var_i32(1); // collision: never
        w.bool(true); // colour present
        w.var_i32(14); // red
        w.u8(0b10); // options: friendly fire off, see invisibles on
        // members
        w.var_i32(2);
        w.string("Alice");
        w.string("Bob");
        let p: SetPlayerTeam = decode(&w.into_vec());
        assert_eq!(p.name, "red");
        assert_eq!(p.method, 0);
        let params = p.parameters.expect("parameters");
        assert_eq!(params.prefix.to_plain_string(), "[R] ");
        assert_eq!(params.name_tag_visibility, 2);
        assert_eq!(params.collision_rule, 1);
        assert_eq!(params.color, Some(14));
        assert!(!params.friendly_fire);
        assert!(params.see_friendly_invisibles);
        assert_eq!(p.players, vec!["Alice".to_string(), "Bob".to_string()]);
    }

    #[test]
    fn team_remove_and_member_ops() {
        // remove: no parameters, no member list
        let mut w = Writer::default();
        w.string("red");
        w.u8(1);
        let p: SetPlayerTeam = decode(&w.into_vec());
        assert_eq!(p.method, 1);
        assert_eq!(p.parameters, None);
        assert!(p.players.is_empty());

        // add-members: no parameters, but a member list
        let mut w = Writer::default();
        w.string("red");
        w.u8(3);
        w.var_i32(1);
        w.string("Carol");
        let p: SetPlayerTeam = decode(&w.into_vec());
        assert_eq!(p.method, 3);
        assert_eq!(p.parameters, None);
        assert_eq!(p.players, vec!["Carol".to_string()]);
    }

    #[test]
    fn boss_add_then_style_progress_remove() {
        let bar = Uuid::from_u128(0x1234_5678_9abc_def0_1122_3344_5566_7788);

        let mut w = Writer::default();
        w.uuid(bar);
        w.var_i32(0); // add
        w.bytes(&nbt_text("Ender Dragon"));
        w.f32(0.5);
        w.var_i32(2); // red
        w.var_i32(1); // notched 6
        w.u8(0b101); // darken + fog
        let p: BossEvent = decode(&w.into_vec());
        assert_eq!(p.id, bar);
        match p.op {
            BossOp::Add {
                title,
                progress,
                color,
                overlay,
                darken,
                music,
                fog,
            } => {
                assert_eq!(title.to_plain_string(), "Ender Dragon");
                assert!((progress - 0.5).abs() < f32::EPSILON);
                assert_eq!(color, 2);
                assert_eq!(overlay, 1);
                assert!(darken && !music && fog);
            }
            other => panic!("expected Add, got {other:?}"),
        }

        let mut w = Writer::default();
        w.uuid(bar);
        w.var_i32(2); // update progress
        w.f32(0.25);
        let p: BossEvent = decode(&w.into_vec());
        assert_eq!(p.op, BossOp::UpdateProgress(0.25));

        let mut w = Writer::default();
        w.uuid(bar);
        w.var_i32(4); // update style
        w.var_i32(5); // purple
        w.var_i32(0); // progress overlay
        let p: BossEvent = decode(&w.into_vec());
        assert_eq!(
            p.op,
            BossOp::UpdateStyle {
                color: 5,
                overlay: 0
            }
        );

        let mut w = Writer::default();
        w.uuid(bar);
        w.var_i32(1); // remove — no body
        let p: BossEvent = decode(&w.into_vec());
        assert_eq!(p.op, BossOp::Remove);
    }
}
