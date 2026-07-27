//! `sounds.json` — the sound-event registry.
//!
//! `assets/<ns>/sounds.json` maps sound *events* (e.g. `entity.creeper.death`)
//! to a weighted list of sound entries. Each entry is either a **file** (an
//! `.ogg` under `assets/<ns>/sounds/<path>.ogg`) or a reference to another
//! **event** (`type: "event"`), which resolves at play time by delegating to
//! that event's own weighted selection and multiplying volume/pitch through.
//!
//! Actual audio decoding/playback is out of scope; this module provides the
//! registry, the weighted selection, and the chain resolution. The `.ogg` files
//! themselves live in the **external asset index** (`asset-index-*.json`), not
//! inside `client.jar` — see the crate docs for how they are addressed.
//!
//! # Weighted selection
//!
//! Selection matches vanilla: draw `roll` in `[0, total_weight)` and walk the
//! entries subtracting each weight until the running total goes negative. A
//! subtlety faithfully reproduced here: a `type: event` entry contributes the
//! **referenced event's total weight** to the parent sum (not its own declared
//! `weight`), because vanilla's delegating `Weighted` reports the target's
//! weight.
//!
//! # Cycle safety
//!
//! Vanilla resolves `type: event` lazily with no static cycle guard, so a
//! cyclic pack would recurse until the stack overflows. This module instead
//! bounds resolution with a visited-set and a depth cap, returning
//! [`SoundError::ReferenceCycle`] rather than panicking. In vanilla 26.2 all 61
//! event references are depth-1 and acyclic, but custom packs are not trusted.

use std::collections::HashMap;

use serde_json::Value;

use crate::ResourceLocation;
use crate::error::SoundError;

/// Whether a sound entry names a file or references another event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundKind {
    /// An `.ogg` file under `sounds/<path>.ogg`.
    File,
    /// A reference to another sound event.
    Event,
}

/// One weighted entry within a sound event.
#[derive(Debug, Clone, PartialEq)]
pub struct SoundEntry {
    /// The file location (for [`SoundKind::File`]) or referenced event id
    /// (for [`SoundKind::Event`]).
    pub name: ResourceLocation,
    /// Playback volume multiplier (> 0).
    pub volume: f32,
    /// Playback pitch multiplier (> 0).
    pub pitch: f32,
    /// Relative selection weight (> 0).
    pub weight: u32,
    /// File or event reference.
    pub kind: SoundKind,
    /// Whether the sound is streamed rather than fully buffered.
    pub stream: bool,
    /// Whether the sound is preloaded at pack load.
    pub preload: bool,
    /// Linear attenuation distance in blocks.
    pub attenuation_distance: i32,
}

/// A single sound event: a weighted list of entries plus metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct SoundEvent {
    /// The weighted entries.
    pub sounds: Vec<SoundEntry>,
    /// When merging packs, `true` resets the event instead of appending.
    pub replace: bool,
    /// Optional subtitle translation key.
    pub subtitle: Option<String>,
}

/// The resolved outcome of selecting and following a sound event: a concrete
/// file plus the accumulated playback parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSound {
    /// The chosen `.ogg` file location.
    pub file: ResourceLocation,
    /// Effective volume (product of the chain).
    pub volume: f32,
    /// Effective pitch (product of the chain).
    pub pitch: f32,
    /// Whether the sound streams.
    pub stream: bool,
    /// Attenuation distance of the resolved file entry.
    pub attenuation_distance: i32,
}

impl ResolvedSound {
    /// The full in-pack path of the resolved file:
    /// `assets/<ns>/sounds/<path>.ogg`.
    pub fn file_path(&self) -> String {
        format!(
            "assets/{}/sounds/{}.ogg",
            self.file.namespace(),
            self.file.path()
        )
    }
}

/// A parsed `sounds.json` registry, keyed by event name (the JSON key).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SoundRegistry {
    events: HashMap<String, SoundEvent>,
}

const MAX_CHAIN_DEPTH: usize = 64;

impl SoundRegistry {
    /// Parses a `sounds.json` document.
    pub fn parse(bytes: &[u8]) -> Result<Self, SoundError> {
        let root: Value =
            serde_json::from_slice(bytes).map_err(|e| SoundError::Json(e.to_string()))?;
        let obj = root
            .as_object()
            .ok_or_else(|| SoundError::Json("root must be an object".into()))?;
        let mut events = HashMap::with_capacity(obj.len());
        for (name, body) in obj {
            events.insert(name.clone(), parse_event(body)?);
        }
        Ok(Self { events })
    }

    /// Looks up an event by name.
    pub fn event(&self, name: &str) -> Option<&SoundEvent> {
        self.events.get(name)
    }

    /// The number of registered events.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Iterates the registered event names.
    pub fn event_names(&self) -> impl Iterator<Item = &str> {
        self.events.keys().map(String::as_str)
    }

    /// Merges another registry into this one, honoring per-event `replace`:
    /// `replace: true` resets the event's entry list, otherwise entries append.
    /// This is vanilla's resource-pack stacking merge for sounds.
    pub fn merge_from(&mut self, other: &SoundRegistry) {
        for (name, ev) in &other.events {
            match self.events.get_mut(name) {
                Some(existing) if !ev.replace => {
                    existing.sounds.extend(ev.sounds.iter().cloned());
                    if ev.subtitle.is_some() {
                        existing.subtitle = ev.subtitle.clone();
                    }
                }
                _ => {
                    self.events.insert(name.clone(), ev.clone());
                }
            }
        }
    }

    /// The total selection weight of an event, following `type: event`
    /// references (which contribute their target's total weight).
    pub fn total_weight(&self, name: &str) -> Result<u64, SoundError> {
        self.total_weight_inner(name, &mut Vec::new())
    }

    fn total_weight_inner(&self, name: &str, stack: &mut Vec<String>) -> Result<u64, SoundError> {
        if stack.len() > MAX_CHAIN_DEPTH || stack.iter().any(|s| s == name) {
            return Err(SoundError::ReferenceCycle {
                id: name.to_string(),
            });
        }
        let Some(ev) = self.events.get(name) else {
            return Ok(0);
        };
        stack.push(name.to_string());
        let mut sum = 0u64;
        for entry in &ev.sounds {
            sum += self.entry_weight(entry, stack)?;
        }
        stack.pop();
        Ok(sum)
    }

    fn entry_weight(&self, entry: &SoundEntry, stack: &mut Vec<String>) -> Result<u64, SoundError> {
        match entry.kind {
            SoundKind::File => Ok(entry.weight as u64),
            SoundKind::Event => self.total_weight_inner(&event_key(&entry.name), stack),
        }
    }

    /// Selects a sound from an event, following `type: event` chains.
    ///
    /// `roll` is called once per weighted selection with the level's total
    /// weight as its argument and must return a value in `[0, total)`. Returns
    /// `Ok(None)` when the event is absent or has zero total weight (vanilla's
    /// "empty sound"). Returns [`SoundError::ReferenceCycle`] on a cyclic or
    /// over-deep reference chain.
    pub fn resolve(
        &self,
        name: &str,
        roll: &mut impl FnMut(u32) -> u32,
    ) -> Result<Option<ResolvedSound>, SoundError> {
        self.resolve_inner(name, roll, 1.0, 1.0, false, &mut Vec::new())
    }

    fn resolve_inner(
        &self,
        name: &str,
        roll: &mut impl FnMut(u32) -> u32,
        vol_acc: f32,
        pitch_acc: f32,
        stream_acc: bool,
        stack: &mut Vec<String>,
    ) -> Result<Option<ResolvedSound>, SoundError> {
        if stack.len() > MAX_CHAIN_DEPTH || stack.iter().any(|s| s == name) {
            return Err(SoundError::ReferenceCycle {
                id: name.to_string(),
            });
        }
        let Some(ev) = self.events.get(name) else {
            return Ok(None);
        };
        stack.push(name.to_string());

        // Compute per-entry weights (event entries use their target's total).
        let mut weights = Vec::with_capacity(ev.sounds.len());
        let mut total = 0u64;
        for entry in &ev.sounds {
            let w = self.entry_weight(entry, stack)?;
            weights.push(w);
            total += w;
        }
        if total == 0 {
            stack.pop();
            return Ok(None);
        }

        let capped = total.min(u32::MAX as u64) as u32;
        let mut index = roll(capped) as u64;
        let mut chosen = ev.sounds.len() - 1;
        for (i, w) in weights.iter().enumerate() {
            if index < *w {
                chosen = i;
                break;
            }
            index -= *w;
        }
        let entry = &ev.sounds[chosen];

        let result = match entry.kind {
            SoundKind::File => Some(ResolvedSound {
                file: entry.name.clone(),
                volume: vol_acc * entry.volume,
                pitch: pitch_acc * entry.pitch,
                stream: stream_acc || entry.stream,
                attenuation_distance: entry.attenuation_distance,
            }),
            SoundKind::Event => self.resolve_inner(
                &event_key(&entry.name),
                roll,
                vol_acc * entry.volume,
                pitch_acc * entry.pitch,
                stream_acc || entry.stream,
                stack,
            )?,
        };
        stack.pop();
        Ok(result)
    }
}

/// The registry key for a referenced event id. Vanilla keys events by
/// `Identifier`; the JSON keys are the bare `minecraft`-namespaced paths, so a
/// `minecraft:` reference maps back to its path and any other namespace keeps
/// its full form.
fn event_key(loc: &ResourceLocation) -> String {
    if loc.namespace() == "minecraft" {
        loc.path().to_string()
    } else {
        loc.to_string()
    }
}

fn parse_event(body: &Value) -> Result<SoundEvent, SoundError> {
    let obj = body
        .as_object()
        .ok_or_else(|| SoundError::Json("sound event must be an object".into()))?;
    let replace = obj.get("replace").and_then(Value::as_bool).unwrap_or(false);
    let subtitle = obj
        .get("subtitle")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut sounds = Vec::new();
    if let Some(arr) = obj.get("sounds") {
        let arr = arr
            .as_array()
            .ok_or_else(|| SoundError::Json("`sounds` must be an array".into()))?;
        for entry in arr {
            sounds.push(parse_entry(entry)?);
        }
    }
    Ok(SoundEvent {
        sounds,
        replace,
        subtitle,
    })
}

fn parse_entry(v: &Value) -> Result<SoundEntry, SoundError> {
    if let Some(s) = v.as_str() {
        return Ok(SoundEntry {
            name: ResourceLocation::parse(s)?,
            volume: 1.0,
            pitch: 1.0,
            weight: 1,
            kind: SoundKind::File,
            stream: false,
            preload: false,
            attenuation_distance: 16,
        });
    }
    let obj = v
        .as_object()
        .ok_or_else(|| SoundError::Json("sound entry must be a string or object".into()))?;
    let name = obj
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| SoundError::Json("sound entry missing `name`".into()))?;
    let kind = match obj.get("type").and_then(Value::as_str) {
        None | Some("file") => SoundKind::File,
        Some("event") => SoundKind::Event,
        Some(other) => return Err(SoundError::UnknownType(other.to_string())),
    };
    let volume = float_field(obj.get("volume"), 1.0, "volume")?;
    let pitch = float_field(obj.get("pitch"), 1.0, "pitch")?;
    let weight = match obj.get("weight") {
        None => 1,
        Some(w) => {
            let n = w
                .as_i64()
                .ok_or_else(|| SoundError::InvalidField("weight must be an integer".into()))?;
            if n <= 0 {
                return Err(SoundError::InvalidField(format!(
                    "weight must be > 0, got {n}"
                )));
            }
            n.min(u32::MAX as i64) as u32
        }
    };
    let stream = obj.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let preload = obj.get("preload").and_then(Value::as_bool).unwrap_or(false);
    let attenuation_distance = obj
        .get("attenuation_distance")
        .and_then(Value::as_i64)
        .unwrap_or(16) as i32;
    Ok(SoundEntry {
        name: ResourceLocation::parse(name)?,
        volume,
        pitch,
        weight,
        kind,
        stream,
        preload,
        attenuation_distance,
    })
}

fn float_field(v: Option<&Value>, default: f32, field: &str) -> Result<f32, SoundError> {
    match v {
        None => Ok(default),
        Some(n) => {
            let f = n
                .as_f64()
                .ok_or_else(|| SoundError::InvalidField(format!("{field} must be a number")))?
                as f32;
            if f <= 0.0 {
                return Err(SoundError::InvalidField(format!(
                    "{field} must be > 0, got {f}"
                )));
            }
            Ok(f)
        }
    }
}
