//! Contracts for native plugins that deliberately cross the stable version seam.
//!
//! The ordinary plugin API is version-free. A native plugin that needs a
//! concrete protocol or registry adapter must opt into this module and carry a
//! [`VersionDescriptor`] alongside its adapter. The host compares that
//! descriptor with the selected adapter before loading the plugin, so a
//! version-specific plugin cannot silently run against a different wire shape.
//!
//! This module describes identity only. It does not expose protocol codecs,
//! registry handles, ECS borrows, sockets, or any other privileged resource.
//! Those resources remain the responsibility of the version-specific adapter
//! integration that consumes a validated descriptor.

use std::fmt;

/// Identity of the unstable native version surface a plugin was built for.
///
/// All three fields participate in compatibility. `family` distinguishes the
/// era's type and registry rules, `protocol` distinguishes wire revisions a
/// family may support, and `abi` identifies the native privileged surface
/// itself. Keeping the descriptor as compile-time string data makes the native
/// opt-in explicit and avoids an implicit "best effort" fallback.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct VersionDescriptor {
    family: &'static str,
    protocol: i32,
    abi: &'static str,
}

impl VersionDescriptor {
    /// Creates an exact version requirement or selected-host identity.
    ///
    /// Callers should use the family and ABI constants published by the
    /// concrete adapter they compile against rather than deriving either from
    /// a folder name or a nearby protocol number.
    #[must_use]
    pub const fn new(family: &'static str, protocol: i32, abi: &'static str) -> Self {
        Self {
            family,
            protocol,
            abi,
        }
    }

    /// Returns the version family label.
    #[must_use]
    pub const fn family(self) -> &'static str {
        self.family
    }

    /// Returns the negotiated wire protocol.
    #[must_use]
    pub const fn protocol(self) -> i32 {
        self.protocol
    }

    /// Returns the privileged native ABI identifier.
    #[must_use]
    pub const fn abi(self) -> &'static str {
        self.abi
    }

    /// Checks this required descriptor against the selected host descriptor.
    ///
    /// Equality is intentionally exact. A mismatch is an error rather than a
    /// warning or fallback because a version-specific plugin can otherwise
    /// decode a valid packet with the wrong registry or wire assumptions.
    pub fn validate(self, actual: Self) -> Result<(), VersionCompatibilityError> {
        if self == actual {
            Ok(())
        } else {
            Err(VersionCompatibilityError {
                required: self,
                actual,
            })
        }
    }
}

/// Opt-in marker for a native plugin that carries a version-specific surface.
///
/// Implementors publish the exact descriptor they were built against. The
/// trait has no ECS or application dependency, so a host can inspect the
/// requirement before it hands the plugin to its own registration API.
pub trait VersionLockedPlugin {
    /// Returns the family, protocol, and privileged ABI required by this
    /// plugin.
    fn version_descriptor(&self) -> VersionDescriptor;
}

/// Validates and registers a plugin that implements [`VersionLockedPlugin`].
///
/// The plugin is moved into the callback only after its declared descriptor
/// matches the selected host descriptor. This is the convenient form for a
/// host whose registration API takes ownership of a plugin value.
pub fn register_version_locked_plugin<P, R>(
    plugin: P,
    actual: VersionDescriptor,
    register: impl FnOnce(P) -> R,
) -> Result<R, VersionCompatibilityError>
where
    P: VersionLockedPlugin,
{
    plugin.version_descriptor().validate(actual)?;
    Ok(register(plugin))
}

/// A native plugin's required descriptor did not match the selected host.
///
/// The complete required and actual descriptors are retained so diagnostics
/// always name the family, protocol, and ABI on both sides of the failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct VersionCompatibilityError {
    required: VersionDescriptor,
    actual: VersionDescriptor,
}

impl VersionCompatibilityError {
    /// Returns the descriptor requested by the plugin.
    #[must_use]
    pub const fn required(self) -> VersionDescriptor {
        self.required
    }

    /// Returns the descriptor selected by the host.
    #[must_use]
    pub const fn actual(self) -> VersionDescriptor {
        self.actual
    }
}

impl fmt::Display for VersionCompatibilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "version-locked plugin requires family={} protocol={} ABI={}, but host provides family={} protocol={} ABI={}",
            self.required.family,
            self.required.protocol,
            self.required.abi,
            self.actual.family,
            self.actual.protocol,
            self.actual.abi,
        )
    }
}

impl std::error::Error for VersionCompatibilityError {}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUIRED: VersionDescriptor =
        VersionDescriptor::new("v26-2", 776, "lodestone:native-version@0.1");

    #[test]
    fn matching_descriptor_is_accepted() {
        assert_eq!(REQUIRED.validate(REQUIRED), Ok(()));
    }

    #[test]
    fn family_mismatch_is_loud_and_preserves_both_descriptors() {
        let actual = VersionDescriptor::new("v1-21-11", 774, REQUIRED.abi());
        let error = REQUIRED
            .validate(actual)
            .expect_err("different families must not load");

        assert_eq!(error.required(), REQUIRED);
        assert_eq!(error.actual(), actual);
        let message = error.to_string();
        assert!(
            message.contains("requires family=v26-2 protocol=776 ABI=lodestone:native-version@0.1")
        );
        assert!(message.contains(
            "host provides family=v1-21-11 protocol=774 ABI=lodestone:native-version@0.1"
        ));
    }

    #[test]
    fn protocol_mismatch_is_rejected_even_with_the_same_family_and_abi() {
        let actual = VersionDescriptor::new(REQUIRED.family(), 777, REQUIRED.abi());
        let error = REQUIRED
            .validate(actual)
            .expect_err("different protocols must not load");

        assert_eq!(error.required().protocol(), 776);
        assert_eq!(error.actual().protocol(), 777);
        assert!(error.to_string().contains("protocol=776"));
        assert!(error.to_string().contains("protocol=777"));
    }

    #[test]
    fn abi_mismatch_is_rejected_even_with_the_same_family_and_protocol() {
        let actual = VersionDescriptor::new(
            REQUIRED.family(),
            REQUIRED.protocol(),
            "lodestone:native-version@0.2",
        );
        let error = REQUIRED
            .validate(actual)
            .expect_err("different privileged ABIs must not load");

        assert_eq!(error.required().abi(), "lodestone:native-version@0.1");
        assert_eq!(error.actual().abi(), "lodestone:native-version@0.2");
        assert!(
            error
                .to_string()
                .contains("ABI=lodestone:native-version@0.1")
        );
        assert!(
            error
                .to_string()
                .contains("ABI=lodestone:native-version@0.2")
        );
    }

    #[test]
    fn descriptor_fields_are_const_and_copyable_for_plugin_metadata() {
        const DESCRIPTOR: VersionDescriptor =
            VersionDescriptor::new("v1-8", 47, "lodestone:native-version@0.1");
        assert_eq!(DESCRIPTOR.family(), "v1-8");
        assert_eq!(DESCRIPTOR.protocol(), 47);
        assert_eq!(DESCRIPTOR.abi(), "lodestone:native-version@0.1");
        assert_eq!(DESCRIPTOR, DESCRIPTOR);
    }

    #[test]
    fn version_locked_plugin_declares_the_requirement_before_registration() {
        struct Plugin;

        impl VersionLockedPlugin for Plugin {
            fn version_descriptor(&self) -> VersionDescriptor {
                REQUIRED
            }
        }

        let mut called = false;
        let result = register_version_locked_plugin(Plugin, REQUIRED, |_| {
            called = true;
        })
        .expect("a plugin's declared descriptor should match the host");

        let _ = result;
        assert!(
            called,
            "a matching plugin must reach the registration callback"
        );
    }
}
