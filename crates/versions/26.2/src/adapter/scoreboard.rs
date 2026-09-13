//! Scoreboard/team/boss-bar/title packets. Split out of the former
//! monolithic `adapter.rs`.
use super::*;

impl V770Adapter {
    /// Clientbound play-state packets in the scoreboard domain, split out of the
    /// former monolithic `handle_play` (see `adapter::mod` for the coordinator).
    pub(super) fn handle_play_scoreboard(&self, packet_id: i32, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        if packet_id == play::clientbound::SET_ACTION_BAR_TEXT {
            // The action bar carries a single trusted text component and always
            // renders as an overlay, so it maps to a `GameInfo` chat event.
            let mut reader = Reader::new(payload);
            let component = read_network_nbt(&mut reader)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            reader
                .ensure_empty()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            return Ok(vec![Directive::Emit(ClientEvent::Chat {
                text: Text::from_nbt(&component),
                kind: ChatKind::GameInfo,
                sender: None,
                ack: None,
            })]);
        }
        if packet_id == play::clientbound::SET_TITLE_TEXT {
            let mut reader = Reader::new(payload);
            let component = read_network_nbt(&mut reader).map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::TitleText {
                text: Text::from_nbt(&component),
            })]);
        }
        if packet_id == play::clientbound::SET_SUBTITLE_TEXT {
            let mut reader = Reader::new(payload);
            let component = read_network_nbt(&mut reader).map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::SubtitleText {
                text: Text::from_nbt(&component),
            })]);
        }
        if packet_id == play::clientbound::CLEAR_TITLES {
            let mut reader = Reader::new(payload);
            let reset_times = reader.bool().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::TitlesCleared {
                reset_times,
            })]);
        }
        if packet_id == play::clientbound::SET_TITLES_ANIMATION {
            // All three fields are raw `int`s (`readInt`), not VarInts.
            let mut reader = Reader::new(payload);
            let fade_in = reader.i32().map_err(dec_err)?;
            let stay = reader.i32().map_err(dec_err)?;
            let fade_out = reader.i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::TitlesAnimation {
                fade_in,
                stay,
                fade_out,
            })]);
        }
        if packet_id == play::clientbound::SET_OBJECTIVE {
            // Conditional body: the display-name/render-type/number-format tail
            // is present only for add(0)/change(2), absent for remove(1). A wrong
            // branch leaves trailing bytes, which ensure_empty rejects.
            let obj: SetObjective = decode_play(payload)?;
            let render_type = match obj.render_type {
                Some(id) => Some(map_render_type(id)?),
                None => None,
            };
            return Ok(vec![Directive::Emit(ClientEvent::ObjectiveUpdate {
                name: obj.name,
                mode: map_objective_mode(obj.method)?,
                display_name: obj.display_name,
                render_type,
                number_format: obj.number_format.map(map_number_format),
            })]);
        }
        if packet_id == play::clientbound::SET_DISPLAY_OBJECTIVE {
            let display: SetDisplayObjective = decode_play(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::DisplayObjective {
                slot: map_display_slot(display.slot)?,
                objective: display.objective,
            })]);
        }
        if packet_id == play::clientbound::SET_SCORE {
            let score: SetScore = decode_play(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::ScoreUpdate {
                holder: score.owner,
                objective: score.objective,
                value: score.score,
                display: score.display,
                number_format: score.number_format.map(map_number_format),
            })]);
        }
        if packet_id == play::clientbound::RESET_SCORE {
            let reset: ResetScore = decode_play(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::ScoreReset {
                holder: reset.owner,
                objective: reset.objective,
            })]);
        }
        if packet_id == play::clientbound::SET_PLAYER_TEAM {
            // Multi-mode: parameters present for create(0)/update(2); member list
            // present for create(0)/add(3)/remove(4). Zero trailing bytes proves
            // the mode byte selected the right combination.
            let team: SetPlayerTeam = decode_play(payload)?;
            let name = team.name.clone();
            return Ok(vec![Directive::Emit(ClientEvent::TeamUpdate {
                name,
                action: map_team_action(team)?,
            })]);
        }
        if packet_id == play::clientbound::BOSS_EVENT {
            // Op-tagged union keyed by UUID; each op has a distinct body.
            let boss: BossEvent = decode_play(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::BossBarUpdate {
                id: boss.id,
                action: map_boss_action(boss.op)?,
            })]);
        }
        Ok(Vec::new())
    }
}

/// Decodes a play packet body and asserts zero trailing bytes, returning the
/// value. Zero trailing bytes is the misparse detector: a wrong conditional
/// branch consuming the wrong byte count is caught here rather than silently
/// corrupting the following packet.
pub(super) fn decode_play<T: Decode>(payload: &[u8]) -> Result<T, AdapterError> {
    let mut reader = Reader::new(payload);
    let value = T::decode(&mut reader, CTX).map_err(|err| AdapterError::Decode(err.to_string()))?;
    reader
        .ensure_empty()
        .map_err(|err| AdapterError::Decode(err.to_string()))?;
    Ok(value)
}

fn map_objective_mode(method: u8) -> Result<ObjectiveMode, AdapterError> {
    Ok(match method {
        0 => ObjectiveMode::Add,
        1 => ObjectiveMode::Remove,
        2 => ObjectiveMode::Change,
        other => {
            return Err(AdapterError::Decode(format!(
                "unknown objective mode {other}"
            )));
        }
    })
}

fn map_render_type(id: i32) -> Result<ObjectiveRenderType, AdapterError> {
    Ok(match id {
        0 => ObjectiveRenderType::Integer,
        1 => ObjectiveRenderType::Hearts,
        other => return Err(AdapterError::Decode(format!("unknown render type {other}"))),
    })
}

/// Lowers the wire number format into the canonical model form. The wire
/// `styled` variant carries a full `Style` (decoded into a `Text`); the model
/// keeps only the colour, so it is extracted, defaulting to white when absent.
fn map_number_format(nf: sb::NumberFormat) -> NumberFormat {
    match nf {
        sb::NumberFormat::Blank => NumberFormat::Blank,
        sb::NumberFormat::Styled(text) => {
            NumberFormat::Styled(text.style.color.unwrap_or(TextColor::White))
        }
        sb::NumberFormat::Fixed(text) => NumberFormat::Fixed(Box::new(text)),
    }
}

fn map_team_color(id: i32) -> Result<TeamColor, AdapterError> {
    Ok(match id {
        0 => TeamColor::Black,
        1 => TeamColor::DarkBlue,
        2 => TeamColor::DarkGreen,
        3 => TeamColor::DarkAqua,
        4 => TeamColor::DarkRed,
        5 => TeamColor::DarkPurple,
        6 => TeamColor::Gold,
        7 => TeamColor::Gray,
        8 => TeamColor::DarkGray,
        9 => TeamColor::Blue,
        10 => TeamColor::Green,
        11 => TeamColor::Aqua,
        12 => TeamColor::Red,
        13 => TeamColor::LightPurple,
        14 => TeamColor::Yellow,
        15 => TeamColor::White,
        other => return Err(AdapterError::Decode(format!("unknown team color {other}"))),
    })
}

fn map_display_slot(id: i32) -> Result<DisplaySlot, AdapterError> {
    Ok(match id {
        0 => DisplaySlot::List,
        1 => DisplaySlot::Sidebar,
        2 => DisplaySlot::BelowName,
        3..=18 => DisplaySlot::TeamSidebar(map_team_color(id - 3)?),
        other => {
            return Err(AdapterError::Decode(format!(
                "unknown display slot {other}"
            )));
        }
    })
}

fn map_visibility(id: i32) -> Result<Visibility, AdapterError> {
    Ok(match id {
        0 => Visibility::Always,
        1 => Visibility::Never,
        2 => Visibility::HideForOtherTeams,
        3 => Visibility::HideForOwnTeam,
        other => return Err(AdapterError::Decode(format!("unknown visibility {other}"))),
    })
}

fn map_collision_rule(id: i32) -> Result<CollisionRule, AdapterError> {
    Ok(match id {
        0 => CollisionRule::Always,
        1 => CollisionRule::Never,
        2 => CollisionRule::PushOtherTeams,
        3 => CollisionRule::PushOwnTeam,
        other => {
            return Err(AdapterError::Decode(format!(
                "unknown collision rule {other}"
            )));
        }
    })
}

fn map_team_parameters(params: sb::TeamParameters) -> Result<TeamParameters, AdapterError> {
    let color = match params.color {
        Some(id) => Some(map_team_color(id)?),
        None => None,
    };
    Ok(TeamParameters {
        display_name: params.display_name,
        prefix: params.prefix,
        suffix: params.suffix,
        name_tag_visibility: map_visibility(params.name_tag_visibility)?,
        collision_rule: map_collision_rule(params.collision_rule)?,
        color,
        friendly_fire: params.friendly_fire,
        see_friendly_invisibles: params.see_friendly_invisibles,
    })
}

fn map_team_action(team: SetPlayerTeam) -> Result<TeamAction, AdapterError> {
    Ok(match team.method {
        0 => TeamAction::Create {
            params: Box::new(map_team_parameters(team.parameters.ok_or_else(|| {
                AdapterError::Decode("team create without parameters".into())
            })?)?),
            members: team.players,
        },
        1 => TeamAction::Remove,
        2 => TeamAction::Update {
            params: Box::new(map_team_parameters(team.parameters.ok_or_else(|| {
                AdapterError::Decode("team update without parameters".into())
            })?)?),
        },
        3 => TeamAction::AddMembers(team.players),
        4 => TeamAction::RemoveMembers(team.players),
        other => return Err(AdapterError::Decode(format!("unknown team method {other}"))),
    })
}

fn map_boss_color(id: i32) -> Result<BossColor, AdapterError> {
    Ok(match id {
        0 => BossColor::Pink,
        1 => BossColor::Blue,
        2 => BossColor::Red,
        3 => BossColor::Green,
        4 => BossColor::Yellow,
        5 => BossColor::Purple,
        6 => BossColor::White,
        other => return Err(AdapterError::Decode(format!("unknown boss color {other}"))),
    })
}

fn map_boss_overlay(id: i32) -> Result<BossOverlay, AdapterError> {
    Ok(match id {
        0 => BossOverlay::Progress,
        1 => BossOverlay::Notched6,
        2 => BossOverlay::Notched10,
        3 => BossOverlay::Notched12,
        4 => BossOverlay::Notched20,
        other => {
            return Err(AdapterError::Decode(format!(
                "unknown boss overlay {other}"
            )));
        }
    })
}

fn map_boss_action(op: sb::BossOp) -> Result<BossAction, AdapterError> {
    Ok(match op {
        sb::BossOp::Add {
            title,
            progress,
            color,
            overlay,
            darken,
            music,
            fog,
        } => BossAction::Add {
            title: Box::new(title),
            progress,
            color: map_boss_color(color)?,
            overlay: map_boss_overlay(overlay)?,
            darken,
            music,
            fog,
        },
        sb::BossOp::Remove => BossAction::Remove,
        sb::BossOp::UpdateProgress(p) => BossAction::UpdateProgress(p),
        sb::BossOp::UpdateName(name) => BossAction::UpdateName(Box::new(name)),
        sb::BossOp::UpdateStyle { color, overlay } => BossAction::UpdateStyle {
            color: map_boss_color(color)?,
            overlay: map_boss_overlay(overlay)?,
        },
        sb::BossOp::UpdateProperties { darken, music, fog } => {
            BossAction::UpdateFlags { darken, music, fog }
        }
    })
}

