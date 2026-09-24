//! Particle definitions (`assets/<ns>/particles/*.json`).
//!
//! Each file lists the sprite textures a particle type animates through, as a
//! `textures` array of resource locations. The prefix `textures/particle/` and
//! the `.png` extension are applied when resolving each sprite, matching
//! vanilla's particle atlas. A missing `textures` key yields an empty list
//! (some code-defined particle types carry no sprites).

use std::fmt;

use serde::{
    Deserialize, Serialize,
    de::{MapAccess, Visitor, value::MapAccessDeserializer},
};

use crate::ResourceLocation;
use crate::error::ParticleError;

/// The closed transport shape of a particle definition.
///
/// A missing or explicit `null` `textures` value is retained as `None`: a
/// small number of code-defined particle types have no sprite list, and the
/// public parser historically treats both forms as an empty definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParticleDocumentFields {
    #[serde(default)]
    textures: Option<Vec<String>>,
}

/// Object-only wrapper around the derived fields. Without the wrapper, Serde
/// accepts an empty positional sequence because every field has a default.
#[derive(Debug, Clone, PartialEq)]
struct ParticleDocument(ParticleDocumentFields);

impl Serialize for ParticleDocument {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ParticleDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ParticleVisitor;

        impl<'de> Visitor<'de> for ParticleVisitor {
            type Value = ParticleDocument;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a particle definition object")
            }

            fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                ParticleDocumentFields::deserialize(MapAccessDeserializer::new(map))
                    .map(ParticleDocument)
            }
        }

        deserializer.deserialize_map(ParticleVisitor)
    }
}

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
        let document: ParticleDocument =
            serde_json::from_slice(bytes).map_err(|e| ParticleError::Json(e.to_string()))?;
        let textures = document
            .0
            .textures
            .unwrap_or_default()
            .into_iter()
            .map(|texture| ResourceLocation::parse(&texture))
            .collect::<Result<Vec<_>, _>>()?;
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

#[cfg(test)]
mod tests {
    use super::ParticleDocument;

    #[test]
    fn typed_document_round_trips_and_keeps_the_declared_order() {
        let source = r#"{"textures":["minecraft:effect_1","minecraft:effect_0"]}"#;
        let document: ParticleDocument = serde_json::from_str(source).unwrap();
        let encoded = serde_json::to_string(&document).unwrap();
        let decoded: ParticleDocument = serde_json::from_str(&encoded).unwrap();
        assert_eq!(document, decoded);
    }

    #[test]
    fn the_closed_document_rejects_an_unknown_field() {
        assert!(
            serde_json::from_str::<ParticleDocument>(r#"{"textures":[],"unexpected":true}"#,)
                .is_err()
        );
    }

    #[test]
    fn the_document_requires_an_object_root() {
        assert!(serde_json::from_str::<ParticleDocument>("[]").is_err());
    }
}
