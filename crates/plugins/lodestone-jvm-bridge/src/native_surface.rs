//! The deliberately tiny native registration contract for an isolated shim.
//!
//! This is not a Bukkit or Paper API. An operator-built shim may opt into this
//! one class while its loader is being prepared. The generated registration
//! list is intentionally one method wide: extending it requires a concrete
//! host capability and a separately predicted test, not an inventory of
//! speculative compatibility methods.

use std::fmt;

use jni::objects::{JClass, JObject};
use jni::{Env, jni_sig, jni_str};

use crate::adapter;
use crate::runtime::{JvmError, JvmRuntime};

/// Binary name an operator-built isolated shim must use for this native seam.
pub const ISOLATED_SHIM_CLASS: &str = "lodestone.bridge.IsolatedPaperShim";

/// The only native member in the generated first surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeMethodSpec {
    /// Java method name.
    pub name: &'static str,
    /// Exact JNI descriptor, including argument and return types.
    pub descriptor: &'static str,
}

const ISOLATED_SHIM_METHODS: [NativeMethodSpec; 1] = [NativeMethodSpec {
    name: "blockStateId",
    descriptor: "(III)I",
}];

/// One non-interchangeable phase of native registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeRegistrationStep {
    /// Check that the isolated class declares the exact static native member.
    Validate(NativeMethodSpec),
    /// Attach its Rust function pointer only after validation succeeds.
    Register(NativeMethodSpec),
}

const ISOLATED_SHIM_REGISTRATION: [NativeRegistrationStep; 2] = [
    NativeRegistrationStep::Validate(ISOLATED_SHIM_METHODS[0]),
    NativeRegistrationStep::Register(ISOLATED_SHIM_METHODS[0]),
];

/// The source-of-truth registration list for [`ISOLATED_SHIM_CLASS`].
///
/// It is data rather than a scattered pair of string literals so hermetic
/// tests can pin the class, member, descriptor, and registration order without
/// starting a JVM or requiring a JDK.
pub const fn isolated_shim_methods() -> &'static [NativeMethodSpec] {
    &ISOLATED_SHIM_METHODS
}

/// Generated validation and registration sequence for the isolated shim.
pub const fn isolated_shim_registration_steps() -> &'static [NativeRegistrationStep] {
    &ISOLATED_SHIM_REGISTRATION
}

/// A bounded failure while validating or registering the isolated native shim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeSurfaceError {
    /// The shim did not declare the exact static native method the bridge owns.
    MissingMethod {
        class: &'static str,
        method: NativeMethodSpec,
        detail: String,
    },
    /// JNI rejected registration after the declaration had been validated.
    Registration {
        class: &'static str,
        detail: String,
    },
    /// The isolated loader could not resolve the operator-built shim class.
    ClassLoad {
        class: &'static str,
        detail: String,
    },
}

impl fmt::Display for NativeSurfaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingMethod { class, method, detail } => write!(
                formatter,
                "isolated shim {class} must declare static native {}{}: {detail}",
                method.name, method.descriptor,
            ),
            Self::Registration { class, detail } => {
                write!(formatter, "could not register isolated shim natives on {class}: {detail}")
            }
            Self::ClassLoad { class, detail } => {
                write!(formatter, "could not load isolated shim {class}: {detail}")
            }
        }
    }
}

impl std::error::Error for NativeSurfaceError {}

/// Validates declarations before installing native pointers on one class.
///
/// The validation phase is intentionally distinct from registration. A missing
/// declaration cannot leave a partly registered list, and its exact name and
/// descriptor survive into the host's typed error.
pub(crate) fn validate_and_register(
    env: &mut Env<'_>,
    class: &JClass<'_>,
) -> Result<(), NativeSurfaceError> {
    for step in isolated_shim_registration_steps() {
        match step {
            NativeRegistrationStep::Validate(method) => {
                // `jni` represents identifiers as compile-time modified-UTF8
                // literals. The generated spec above remains the checked
                // source of truth; this branch is its one JNI encoding.
                env.get_static_method_id(class, jni_str!("blockStateId"), jni_sig!("(III)I"))
                    .map_err(|error| NativeSurfaceError::MissingMethod {
                        class: ISOLATED_SHIM_CLASS,
                        method: *method,
                        detail: error.to_string(),
                    })?;
            }
            NativeRegistrationStep::Register(method) => {
                adapter::register_block_query(env, class, method.name, method.descriptor)
                    .map_err(|error| NativeSurfaceError::Registration {
                        class: ISOLATED_SHIM_CLASS,
                        detail: error.to_string(),
                    })?;
            }
        }
    }
    Ok(())
}

/// Loads, validates, and registers the shim in one fresh isolated loader.
///
/// The caller must invoke this before it loads the bootstrap or plugin entry
/// through that same loader. Registration does not initialize the shim; field
/// reads are deliberately absent because `getstatic` would initialize its
/// declaring class and break the lifecycle's non-initializing contract.
pub(crate) fn install_in_loader<'local>(
    runtime: &JvmRuntime,
    env: &mut Env<'local>,
    loader: &JObject<'local>,
) -> Result<(), NativeSurfaceError> {
    let class = runtime
        .load_class_from_loader(env, loader, ISOLATED_SHIM_CLASS)
        .map_err(class_load_error)?;
    validate_and_register(env, &class)
}

fn class_load_error(error: JvmError) -> NativeSurfaceError {
    NativeSurfaceError::ClassLoad {
        class: ISOLATED_SHIM_CLASS,
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    use crate::runtime::JvmConfig;

    #[test]
    fn generated_surface_pins_the_only_supported_member_and_descriptor() {
        assert_eq!(ISOLATED_SHIM_CLASS, "lodestone.bridge.IsolatedPaperShim");
        assert_eq!(
            isolated_shim_methods(),
            &[NativeMethodSpec { name: "blockStateId", descriptor: "(III)I" }],
        );
        let method = isolated_shim_methods()[0];
        assert_eq!(
            isolated_shim_registration_steps(),
            &[
                NativeRegistrationStep::Validate(method),
                NativeRegistrationStep::Register(method),
            ],
            "a registration must never precede declaration validation",
        );
    }

    #[test]
    fn generated_surface_has_no_field_contract_that_would_initialize_a_shim() {
        // `getstatic` is an initialization trigger. Keeping fields out of this
        // registration list protects the lifecycle-load guarantee until there
        // is a concrete, separately designed live-state strategy.
        assert!(isolated_shim_methods().iter().all(|method| method.name != "ABI_VERSION"));
    }

    #[test]
    #[ignore = "requires JAVA_HOME pointing to a JDK with javac and libjvm"]
    fn live_registration_accepts_the_generated_native_declaration() {
        let jdk = std::env::var_os("JAVA_HOME").expect("JAVA_HOME is required");
        let fixture = std::env::temp_dir().join(format!(
            "lodestone-native-surface-{}",
            std::process::id(),
        ));
        fs::create_dir_all(&fixture).expect("fixture directory");
        let source_root = fixture.join("lodestone/bridge");
        fs::create_dir_all(&source_root).expect("source directory");
        let source = source_root.join("IsolatedPaperShim.java");
        fs::write(
            &source,
            "package lodestone.bridge; public final class IsolatedPaperShim { \
             public static native int blockStateId(int x, int y, int z); }",
        )
        .expect("shim source");
        let output = Command::new(std::path::PathBuf::from(jdk).join("bin/javac"))
            .arg("-d")
            .arg(&fixture)
            .arg(&source)
            .output()
            .expect("javac");
        assert!(
            output.status.success(),
            "javac: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let runtime = JvmRuntime::start(&JvmConfig::new()).expect("start JVM");
        runtime
            .with_attached_thread(|env| {
                let config = JvmConfig::new().with_classpath(&fixture);
                runtime
                    .with_isolated_loader(env, &config, |env, loader| {
                        install_in_loader(&runtime, env, loader)
                            .expect("register generated native declaration");
                        Ok(())
                    })
                    .expect("isolated loader");
                Ok(())
            })
            .expect("attach JVM thread");
        fs::remove_dir_all(fixture).expect("remove fixture directory");
    }
}
