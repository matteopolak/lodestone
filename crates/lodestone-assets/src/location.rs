//! Namespaced identifiers ([`ResourceLocation`]).

use crate::error::ResourceLocationError;
use std::fmt;

/// The namespace vanilla assumes when none is given.
pub const DEFAULT_NAMESPACE: &str = "minecraft";

/// A Minecraft namespaced identifier such as `minecraft:block/stone`.
///
/// Parsing defaults the namespace to `minecraft` when it is omitted, matching
/// vanilla behavior. Characters are validated per vanilla rules: the namespace
/// allows `[a-z0-9_.-]` and the path allows `[a-z0-9_.-/]`.
///
/// The type is cheap to clone and hashes on its string content, so it works
/// directly as a [`std::collections::HashMap`] key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResourceLocation {
    namespace: String,
    path: String,
}

fn is_valid_namespace_char(ch: char) -> bool {
    matches!(ch, 'a'..='z' | '0'..='9' | '_' | '.' | '-')
}

fn is_valid_path_char(ch: char) -> bool {
    matches!(ch, 'a'..='z' | '0'..='9' | '_' | '.' | '-' | '/')
}

fn validate(namespace: &str, path: &str, input: &str) -> Result<(), ResourceLocationError> {
    if namespace.is_empty() {
        return Err(ResourceLocationError::Empty { part: "namespace" });
    }
    if path.is_empty() {
        return Err(ResourceLocationError::Empty { part: "path" });
    }
    if let Some(ch) = namespace.chars().find(|c| !is_valid_namespace_char(*c)) {
        return Err(ResourceLocationError::InvalidCharacter {
            part: "namespace",
            ch,
            input: input.to_owned(),
        });
    }
    if let Some(ch) = path.chars().find(|c| !is_valid_path_char(*c)) {
        return Err(ResourceLocationError::InvalidCharacter {
            part: "path",
            ch,
            input: input.to_owned(),
        });
    }
    Ok(())
}

impl ResourceLocation {
    /// Builds a location from an explicit namespace and path, validating both.
    pub fn new(
        namespace: impl Into<String>,
        path: impl Into<String>,
    ) -> Result<Self, ResourceLocationError> {
        let namespace = namespace.into();
        let path = path.into();
        let input = format!("{namespace}:{path}");
        validate(&namespace, &path, &input)?;
        Ok(Self { namespace, path })
    }

    /// Parses a `namespace:path` string, defaulting the namespace to
    /// `minecraft` when the `namespace:` prefix is absent.
    pub fn parse(input: &str) -> Result<Self, ResourceLocationError> {
        let mut parts = input.splitn(2, ':');
        let first = parts.next().unwrap_or("");
        let (namespace, path) = match parts.next() {
            Some(rest) => (first, rest),
            None => (DEFAULT_NAMESPACE, first),
        };
        if path.contains(':') {
            return Err(ResourceLocationError::TooManySeparators {
                input: input.to_owned(),
            });
        }
        validate(namespace, path, input)?;
        Ok(Self {
            namespace: namespace.to_owned(),
            path: path.to_owned(),
        })
    }

    /// The namespace portion (for example `minecraft`).
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// The path portion (for example `block/stone`).
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl fmt::Display for ResourceLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.namespace, self.path)
    }
}

impl std::str::FromStr for ResourceLocation {
    type Err = ResourceLocationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}
