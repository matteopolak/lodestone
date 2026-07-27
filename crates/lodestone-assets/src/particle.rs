//! Particle definitions (`assets/<ns>/particles/*.json`).
//!
//! Each file lists the sprite textures a particle type animates through, as a
//! `textures` array of resource locations. The prefix `textures/particle/` and
//! the `.png` extension are applied when resolving each sprite, matching
//! vanilla's particle atlas. A missing `textures` key yields an empty list
//! (some code-defined particle types carry no sprites).

use serde_json::Value;

use crate::ResourceLocation;
use crate::error::ParticleError;

/// A parsed particle definition: the ordered list of sprite textures.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParticleDefinition {
    /// The sprite textures, in declaration order.
    pub textures: Vec<ResourceLocation>,
}

impl ParticleDefinition {
    /// Parses a particle definition document. A missing `textures` key is
    /// tolerated and produces an empty list, matching vanilla.
    pub fn parse(bytes: &[u8]) -> Result<Self, ParticleError> {
        let root: Value =
            serde_json::from_slice(bytes).map_err(|e| ParticleError::Json(e.to_string()))?;
        let textures = match root.get("textures") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(arr)) => {
                let mut out = Vec::with_capacity(arr.len());
                for t in arr {
                    let s = t.as_str().ok_or_else(|| {
                        ParticleError::Json("particle texture must be a string".into())
                    })?;
                    out.push(ResourceLocation::parse(s)?);
                }
                out
            }
            Some(_) => {
                return Err(ParticleError::Json("`textures` must be an array".into()));
            }
        };
        Ok(Self { textures })
    }

    /// The full in-pack paths of the sprite textures:
    /// `assets/<ns>/textures/particle/<path>.png`.
    pub fn texture_paths(&self) -> Vec<String> {
        self.textures
            .iter()
            .map(|t| {
                format!(
                    "assets/{}/textures/particle/{}.png",
                    t.namespace(),
                    t.path()
                )
            })
            .collect()
    }
}
