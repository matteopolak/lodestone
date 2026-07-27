use std::{
    fmt::{self, Display, Formatter},
    str::FromStr,
};

use thiserror::Error;

const DEFAULT_NAMESPACE: &str = "minecraft";

/// A namespaced identifier such as `minecraft:stone`.
///
/// Identifiers are the canonical way to refer to registry-backed game concepts
/// in this model. Numeric registry IDs are intentionally excluded because they
/// belong to protocol adapters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Identifier {
    namespace: String,
    path: String,
}

impl Identifier {
    /// Creates an identifier from namespace and path components.
    ///
    /// Namespace characters must be lowercase ASCII letters, digits, `_`, `.`,
    /// or `-`. Path characters may additionally include `/`.
    ///
    /// # Errors
    ///
    /// Returns [`ParseIdentifierError`] if either component is empty or contains
    /// a character outside the allowed Minecraft identifier character set.
    pub fn new(
        namespace: impl Into<String>,
        path: impl Into<String>,
    ) -> Result<Self, ParseIdentifierError> {
        let namespace = namespace.into();
        let path = path.into();

        validate_namespace(&namespace)?;
        validate_path(&path)?;

        Ok(Self { namespace, path })
    }

    /// Returns the identifier namespace.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Returns the identifier path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl Display for Identifier {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.namespace, self.path)
    }
}

impl FromStr for Identifier {
    type Err = ParseIdentifierError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            return Err(ParseIdentifierError::Empty);
        }

        let (namespace, path) = match value.split_once(':') {
            Some((namespace, path)) => {
                if path.contains(':') {
                    return Err(ParseIdentifierError::TooManySeparators);
                }
                (namespace, path)
            }
            None => (DEFAULT_NAMESPACE, value),
        };

        Self::new(namespace, path)
    }
}

/// Error returned when parsing a namespaced identifier fails.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseIdentifierError {
    /// The complete identifier string was empty.
    #[error("identifier is empty")]
    Empty,
    /// The namespace component was empty.
    #[error("identifier namespace is empty")]
    EmptyNamespace,
    /// The path component was empty.
    #[error("identifier path is empty")]
    EmptyPath,
    /// More than one namespace separator was present.
    #[error("identifier contains more than one ':' separator")]
    TooManySeparators,
    /// The namespace contains an invalid character.
    #[error("identifier namespace contains invalid character {0:?}")]
    InvalidNamespaceChar(char),
    /// The path contains an invalid character.
    #[error("identifier path contains invalid character {0:?}")]
    InvalidPathChar(char),
}

fn validate_namespace(namespace: &str) -> Result<(), ParseIdentifierError> {
    if namespace.is_empty() {
        return Err(ParseIdentifierError::EmptyNamespace);
    }

    for character in namespace.chars() {
        if !matches!(character, 'a'..='z' | '0'..='9' | '_' | '.' | '-') {
            return Err(ParseIdentifierError::InvalidNamespaceChar(character));
        }
    }

    Ok(())
}

fn validate_path(path: &str) -> Result<(), ParseIdentifierError> {
    if path.is_empty() {
        return Err(ParseIdentifierError::EmptyPath);
    }

    for character in path.chars() {
        if !matches!(character, 'a'..='z' | '0'..='9' | '_' | '.' | '-' | '/') {
            return Err(ParseIdentifierError::InvalidPathChar(character));
        }
    }

    Ok(())
}

/// A canonical registry key.
pub type ResourceKey = Identifier;

/// A canonical dimension identifier.
pub type DimensionId = ResourceKey;

/// A canonical dimension identifier.
pub type Dimension = DimensionId;
