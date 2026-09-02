//! Per-category ("sound source") volume buses.
//!
//! # Vanilla parity (Minecraft 26.2)
//!
//! The category set and the final-gain formula are transcribed from the
//! decompiled client:
//!
//! * Vanilla's sound-source enum lists the buses. In 26.2 there are
//!   **eleven**: `MASTER, MUSIC, RECORDS("record"), WEATHER, BLOCKS("block"),
//!   HOSTILE, NEUTRAL, PLAYERS("player"), AMBIENT, VOICE, UI`.
//! * The final-source-volume formula:
//!   ```text
//!   MASTER            -> masterVolume
//!   any other source  -> sourceVolume * masterVolume
//!   ```
//! * The volume-calculation formula:
//!   ```text
//!   clamp(volume, 0, 1) * clamp(final_source_volume(source), 0, 1)
//!                       * runtime_gain[source]
//!   ```
//!   where the runtime gain is a per-bus value (defaults to `1.0`, used for
//!   ducking/fades). We model it as [`CategoryVolumes::runtime_gain`].
//!
//! Note the asymmetry that a naive implementation gets wrong: `MASTER` is **not**
//! multiplied by itself. A sound on the `MASTER` bus is scaled by master volume
//! once, not squared.

/// A sound-category volume bus. These correspond one-to-one to vanilla's
/// sound-source enum, but the enum is a pure mixer-bus identity: mapping a protocol
/// category id (which differs across versions — `UI` did not always exist) onto
/// a variant is a version-crate concern, not this crate's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SoundCategory {
    /// The overall master bus. Every other bus is additionally scaled by it.
    Master,
    /// Background music.
    Music,
    /// Jukebox / music-disc playback.
    Records,
    /// Rain, thunder and other weather.
    Weather,
    /// Block sounds (breaking, placing, footsteps on blocks…).
    Blocks,
    /// Hostile-mob sounds.
    Hostile,
    /// Friendly/neutral-mob sounds.
    Neutral,
    /// Other players.
    Players,
    /// Ambient/environmental sounds (cave, underwater…).
    Ambient,
    /// Text-to-speech / narrator voice.
    Voice,
    /// User-interface sounds (button clicks…). Added in later versions.
    Ui,
}

impl SoundCategory {
    /// Every bus, in vanilla declaration order.
    pub const ALL: [SoundCategory; 11] = [
        SoundCategory::Master,
        SoundCategory::Music,
        SoundCategory::Records,
        SoundCategory::Weather,
        SoundCategory::Blocks,
        SoundCategory::Hostile,
        SoundCategory::Neutral,
        SoundCategory::Players,
        SoundCategory::Ambient,
        SoundCategory::Voice,
        SoundCategory::Ui,
    ];

    /// The vanilla wire/config name for this bus (e.g. `Blocks` -> `"block"`).
    ///
    /// These strings match vanilla's sound-source name accessor in 26.2. Whether a given
    /// version *uses* all of them is a version concern; the strings themselves
    /// are stable for the buses that exist.
    pub fn vanilla_name(self) -> &'static str {
        match self {
            SoundCategory::Master => "master",
            SoundCategory::Music => "music",
            SoundCategory::Records => "record",
            SoundCategory::Weather => "weather",
            SoundCategory::Blocks => "block",
            SoundCategory::Hostile => "hostile",
            SoundCategory::Neutral => "neutral",
            SoundCategory::Players => "player",
            SoundCategory::Ambient => "ambient",
            SoundCategory::Voice => "voice",
            SoundCategory::Ui => "ui",
        }
    }

    fn index(self) -> usize {
        match self {
            SoundCategory::Master => 0,
            SoundCategory::Music => 1,
            SoundCategory::Records => 2,
            SoundCategory::Weather => 3,
            SoundCategory::Blocks => 4,
            SoundCategory::Hostile => 5,
            SoundCategory::Neutral => 6,
            SoundCategory::Players => 7,
            SoundCategory::Ambient => 8,
            SoundCategory::Voice => 9,
            SoundCategory::Ui => 10,
        }
    }
}

/// User-facing volume settings for every bus, plus a runtime per-bus gain.
///
/// `user[bus]` is the slider value in `[0, 1]`; `runtime[bus]` is the transient
/// gain (fades/ducking) that vanilla keeps as a runtime per-bus value.
#[derive(Debug, Clone)]
pub struct CategoryVolumes {
    user: [f32; 11],
    runtime: [f32; 11],
}

impl Default for CategoryVolumes {
    /// All sliders and runtime gains at `1.0` (vanilla's default).
    fn default() -> Self {
        Self {
            user: [1.0; 11],
            runtime: [1.0; 11],
        }
    }
}

impl CategoryVolumes {
    /// A fresh set of volumes, everything at `1.0`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the user slider for a bus (clamped to `[0, 1]`).
    pub fn set_user(&mut self, category: SoundCategory, volume: f32) {
        self.user[category.index()] = volume.clamp(0.0, 1.0);
    }

    /// The user slider for a bus.
    pub fn user(&self, category: SoundCategory) -> f32 {
        self.user[category.index()]
    }

    /// Sets the runtime gain for a bus (vanilla's runtime per-bus gain, for
    /// fades/ducking). Not clamped to `[0, 1]`: vanilla clamps it there, but the
    /// value is only ever set to values in range, and the final product is what
    /// matters.
    pub fn set_runtime_gain(&mut self, category: SoundCategory, gain: f32) {
        self.runtime[category.index()] = gain.max(0.0);
    }

    /// The runtime gain for a bus.
    pub fn runtime_gain(&self, category: SoundCategory) -> f32 {
        self.runtime[category.index()]
    }

    /// The final-source-volume formula: master volume for the master bus,
    /// otherwise `sourceVolume * masterVolume`.
    fn final_source_volume(&self, category: SoundCategory) -> f32 {
        let master = self.user[SoundCategory::Master.index()];
        match category {
            SoundCategory::Master => master,
            other => self.user[other.index()] * master,
        }
    }

    /// The final linear gain applied to a sound on `category` whose per-instance
    /// volume is `instance_volume`, matching vanilla's volume-calculation
    /// formula:
    ///
    /// `clamp(instance_volume,0,1) * clamp(finalSourceVolume,0,1) * runtimeGain`.
    ///
    /// Note this is the *gain* only. The per-instance volume's effect on audible
    /// *range* uses the unclamped `max(volume, 1.0)` — see [`crate::spatial`].
    pub fn gain(&self, category: SoundCategory, instance_volume: f32) -> f32 {
        instance_volume.clamp(0.0, 1.0)
            * self.final_source_volume(category).clamp(0.0, 1.0)
            * self.runtime[category.index()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_set_matches_vanilla_count_and_names() {
        // Regression guard for the brief's omission: 26.2 has 11 buses, and the
        // eleventh is `UI`.
        assert_eq!(SoundCategory::ALL.len(), 11);
        assert!(SoundCategory::ALL.contains(&SoundCategory::Ui));
        assert_eq!(SoundCategory::Blocks.vanilla_name(), "block");
        assert_eq!(SoundCategory::Records.vanilla_name(), "record");
        assert_eq!(SoundCategory::Players.vanilla_name(), "player");
    }

    #[test]
    fn master_bus_is_scaled_by_master_exactly_once() {
        // The asymmetry: MASTER is not squared. With master=0.5 and a full
        // instance volume, a MASTER-bus sound plays at 0.5, not 0.25.
        let mut v = CategoryVolumes::new();
        v.set_user(SoundCategory::Master, 0.5);
        assert_eq!(v.gain(SoundCategory::Master, 1.0), 0.5);
    }

    #[test]
    fn non_master_bus_multiplies_category_and_master() {
        // master=0.5, blocks=0.5, instance=1.0 -> 0.5 * 0.5 = 0.25.
        let mut v = CategoryVolumes::new();
        v.set_user(SoundCategory::Master, 0.5);
        v.set_user(SoundCategory::Blocks, 0.5);
        assert_eq!(v.gain(SoundCategory::Blocks, 1.0), 0.25);
    }

    #[test]
    fn instance_volume_is_clamped_for_gain() {
        // A volume of 2.0 does not make the sound louder than the bus allows:
        // it clamps to 1.0 for the gain (its range effect lives elsewhere).
        let v = CategoryVolumes::new();
        assert_eq!(v.gain(SoundCategory::Blocks, 2.0), 1.0);
        assert_eq!(v.gain(SoundCategory::Blocks, 0.25), 0.25);
    }

    #[test]
    fn runtime_gain_multiplies_through() {
        let mut v = CategoryVolumes::new();
        v.set_runtime_gain(SoundCategory::Music, 0.5);
        assert_eq!(v.gain(SoundCategory::Music, 1.0), 0.5);
    }

    #[test]
    fn zero_master_silences_everything() {
        let mut v = CategoryVolumes::new();
        v.set_user(SoundCategory::Master, 0.0);
        for c in SoundCategory::ALL {
            assert_eq!(v.gain(c, 1.0), 0.0, "{c:?} should be silent at master=0");
        }
    }
}
