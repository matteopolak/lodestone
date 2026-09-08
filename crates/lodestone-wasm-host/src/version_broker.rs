//! Version-locked data exposed by the privileged WASM broker by value.
//!
//! This module deliberately contains no protocol types, registry handles, ECS
//! borrows, sockets, or callbacks. A version-specific embedding supplies a
//! descriptor and small copied records; the WASM ABI can then expose those
//! records without making the host depend on a concrete protocol family.

use std::fmt;

/// The unstable ABI identifier for the version broker import.
pub const ABI: &str = "lodestone:version-broker@0.1";

/// The exact family/protocol/ABI identity required by a WASM broker consumer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionBrokerDescriptor {
    family: String,
    protocol: i32,
    abi: String,
}

impl VersionBrokerDescriptor {
    /// Creates a descriptor from owned manifest or host configuration values.
    #[must_use]
    pub fn new(family: impl Into<String>, protocol: i32, abi: impl Into<String>) -> Self {
        Self {
            family: family.into(),
            protocol,
            abi: abi.into(),
        }
    }

    /// The required version family.
    #[must_use]
    pub fn family(&self) -> &str {
        &self.family
    }

    /// The required negotiated protocol.
    #[must_use]
    pub const fn protocol(&self) -> i32 {
        self.protocol
    }

    /// The required version-broker ABI.
    #[must_use]
    pub fn abi(&self) -> &str {
        &self.abi
    }

    /// Checks an exact host/plugin identity match.
    pub fn validate_against(&self, actual: &Self) -> Result<(), VersionBrokerCompatibilityError> {
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

/// One bounded, copied record returned by a version broker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionBrokerRecord {
    key: String,
    value: String,
}

impl VersionBrokerRecord {
    /// Creates a copied key/value record for a broker response.
    #[must_use]
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }

    /// The copied registry/protocol key.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The copied value, with no retained host reference.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// Host-provided source for the broker import.
pub trait VersionBroker: Send + Sync {
    /// The exact identity of the version-specific data source.
    fn descriptor(&self) -> VersionBrokerDescriptor;

    /// Returns one copied record for `key`, if this source knows it.
    fn lookup(&self, key: &str) -> Option<VersionBrokerRecord>;
}

/// A requested broker identity did not match the selected host source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionBrokerCompatibilityError {
    required: VersionBrokerDescriptor,
    actual: VersionBrokerDescriptor,
}

impl VersionBrokerCompatibilityError {
    /// The identity declared by the plugin.
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
    fn matching_broker_descriptor_is_accepted() {
        let descriptor = VersionBrokerDescriptor::new("v26-2", 776, ABI);
        assert_eq!(descriptor.validate_against(&descriptor), Ok(()));
    }

    #[test]
    fn mismatch_names_required_and_actual_family_protocol_and_abi() {
        let required = VersionBrokerDescriptor::new("v26-2", 776, ABI);
        let actual = VersionBrokerDescriptor::new("v1-21-11", 774, "lodestone:version-broker@0.2");
        let error = required
            .validate_against(&actual)
            .expect_err("a different broker identity must be refused");

        let message = error.to_string();
        assert!(
            message.contains("requires family=v26-2 protocol=776 ABI=lodestone:version-broker@0.1")
        );
        assert!(message.contains(
            "host provides family=v1-21-11 protocol=774 ABI=lodestone:version-broker@0.2"
        ));
    }

    #[test]
    fn broker_records_are_owned_copies() {
        let record = VersionBrokerRecord::new("block-state", "minecraft:stone");
        assert_eq!(record.key(), "block-state");
        assert_eq!(record.value(), "minecraft:stone");
    }
}
