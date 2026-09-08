//! The deliberately small, version-locked WASM broker surface.
//!
//! A native plugin can depend on a concrete protocol crate and wrap its
//! adapter. A WASM guest cannot receive that adapter, a registry handle, or a
//! world guard. This module is the host-side contract for the useful middle:
//! an embedding supplies a version-specific source, and the guest receives
//! only owned descriptor and key/value records.

use std::fmt;

/// ABI identifier for the privileged version broker import.
pub const ABI: &str = "lodestone:version-broker@0.1";

/// Exact identity of the version-specific source selected by an embedding.
///
/// Family, negotiated protocol, and broker ABI all participate in equality.
/// Keeping the descriptor owned makes it suitable for manifest data while
/// ensuring no string borrowed from an adapter crosses into a guest store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionBrokerDescriptor {
    family: String,
    protocol: i32,
    abi: String,
}

impl VersionBrokerDescriptor {
    /// Construct a required or selected broker identity.
    #[must_use]
    pub fn new(
        family: impl Into<String>,
        protocol: i32,
        abi: impl Into<String>,
    ) -> Self {
        Self {
            family: family.into(),
            protocol,
            abi: abi.into(),
        }
    }

    /// The version-family label.
    #[must_use]
    pub fn family(&self) -> &str {
        &self.family
    }

    /// The negotiated wire protocol.
    #[must_use]
    pub const fn protocol(&self) -> i32 {
        self.protocol
    }

    /// The broker interface ABI.
    #[must_use]
    pub fn abi(&self) -> &str {
        &self.abi
    }

    /// Require an exact identity match, preserving both sides on failure.
    pub fn validate_against(
        &self,
        actual: &Self,
    ) -> Result<(), VersionBrokerCompatibilityError> {
        if self == actual {
            Ok(())
        } else {
            Err(VersionBrokerCompatibilityError {
                required: self.clone(),
                actual: actual.clone(),
            })
        }
    }
}

/// One copied value returned by a broker lookup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionBrokerRecord {
    key: String,
    value: String,
}

impl VersionBrokerRecord {
    /// Construct an owned broker record.
    #[must_use]
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }

    /// The canonical key returned by the source.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The copied value.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// A version-specific source that is safe to expose through the WASM boundary.
///
/// Implementations must keep their key vocabulary finite and document it. The
/// trait intentionally returns an owned record: a provider may use an adapter,
/// registry, or other internal cache, but it cannot hand any of those objects
/// or a borrowed reference to guest code.
pub trait VersionBroker: Send + Sync {
    /// Identity of the selected source.
    fn descriptor(&self) -> VersionBrokerDescriptor;

    /// Resolve one provider-defined, copied key.
    fn lookup(&self, key: &str) -> Option<VersionBrokerRecord>;
}

/// A manifest's required broker identity did not match the selected source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionBrokerCompatibilityError {
    required: VersionBrokerDescriptor,
    actual: VersionBrokerDescriptor,
}

impl VersionBrokerCompatibilityError {
    /// The identity required by the plugin.
    #[must_use]
    pub fn required(&self) -> &VersionBrokerDescriptor {
        &self.required
    }

    /// The identity selected by the host.
    #[must_use]
    pub fn actual(&self) -> &VersionBrokerDescriptor {
        &self.actual
    }
}

impl fmt::Display for VersionBrokerCompatibilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "WASM version broker requires family={} protocol={} ABI={}, but host provides family={} protocol={} ABI={}",
            self.required.family,
            self.required.protocol,
            self.required.abi,
            self.actual.family,
            self.actual.protocol,
            self.actual.abi,
        )
    }
}

impl std::error::Error for VersionBrokerCompatibilityError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_identity_matches() {
        let descriptor = VersionBrokerDescriptor::new("v26-2", 776, ABI);
        assert!(descriptor.validate_against(&descriptor).is_ok());
    }

    #[test]
    fn mismatch_preserves_required_and_actual_identity() {
        let required = VersionBrokerDescriptor::new("v26-2", 776, ABI);
        let actual = VersionBrokerDescriptor::new("v1-21-11", 774, "lodestone:version-broker@0.2");
        let error = required
            .validate_against(&actual)
            .expect_err("a different source must not be accepted");

        assert_eq!(error.required(), &required);
        assert_eq!(error.actual(), &actual);
        let text = error.to_string();
        assert!(text.contains("requires family=v26-2 protocol=776"));
        assert!(text.contains("host provides family=v1-21-11 protocol=774"));
    }

    #[test]
    fn records_are_owned_values() {
        let record = VersionBrokerRecord::new("block-name/1", "minecraft:stone");
        assert_eq!(record.key(), "block-name/1");
        assert_eq!(record.value(), "minecraft:stone");
    }
}
