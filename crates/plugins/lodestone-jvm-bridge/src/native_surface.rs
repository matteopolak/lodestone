//! The deliberately tiny native registration contract for an isolated shim.
//!
//! This is not a Bukkit or Paper API. An operator-built shim may opt into this
//! one class while its loader is being prepared. The generated registration
//! list is intentionally narrow: extending it requires a concrete
//! host capability and a separately predicted test, not an inventory of
//! speculative compatibility methods.

use std::fmt;

use jni::objects::{JClass, JObject};
use jni::{Env, jni_sig, jni_str};

use crate::adapter;
use crate::runtime::{JvmError, JvmRuntime};

/// Binary name an operator-built isolated shim must use for this native seam.
pub const ISOLATED_SHIM_CLASS: &str = "lodestone.bridge.IsolatedPaperShim";

/// A native member in the generated isolated server-state surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeMethodSpec {
    /// Java method name.
    pub name: &'static str,
    /// Exact JNI descriptor, including argument and return types.
    pub descriptor: &'static str,
}

const ISOLATED_SHIM_METHODS: [NativeMethodSpec; 3] = [
    NativeMethodSpec {
        name: "blockStateId",
        descriptor: "(III)I",
    },
    NativeMethodSpec {
        name: "serverTickCount",
        descriptor: "()J",
    },
    NativeMethodSpec {
        name: "setBlockStateId",
        descriptor: "(IIII)I",
    },
];

/// One non-interchangeable phase of native registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeRegistrationStep {
    /// Check that the isolated class declares the exact static native member.
    Validate(NativeMethodSpec),
    /// Attach its Rust function pointer only after validation succeeds.
    Register(NativeMethodSpec),
}

const ISOLATED_SHIM_REGISTRATION: [NativeRegistrationStep; 6] = [
    NativeRegistrationStep::Validate(ISOLATED_SHIM_METHODS[0]),
    NativeRegistrationStep::Validate(ISOLATED_SHIM_METHODS[1]),
    NativeRegistrationStep::Validate(ISOLATED_SHIM_METHODS[2]),
    NativeRegistrationStep::Register(ISOLATED_SHIM_METHODS[0]),
    NativeRegistrationStep::Register(ISOLATED_SHIM_METHODS[1]),
    NativeRegistrationStep::Register(ISOLATED_SHIM_METHODS[2]),
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
                method_id(env, class, *method)
                    .map_err(|error| NativeSurfaceError::MissingMethod {
                        class: ISOLATED_SHIM_CLASS,
                        method: *method,
                        detail: error.to_string(),
                    })?;
            }
            NativeRegistrationStep::Register(method) => {
                register_method(env, class, *method)
                    .map_err(|error| NativeSurfaceError::Registration {
                        class: ISOLATED_SHIM_CLASS,
                        detail: error.to_string(),
                    })?;
            }
        }
    }
    Ok(())
}

fn method_id(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method: NativeMethodSpec,
) -> jni::errors::Result<jni::objects::JStaticMethodID> {
    match (method.name, method.descriptor) {
        ("blockStateId", "(III)I") => {
            env.get_static_method_id(class, jni_str!("blockStateId"), jni_sig!("(III)I"))
        }
        ("serverTickCount", "()J") => {
            env.get_static_method_id(class, jni_str!("serverTickCount"), jni_sig!("()J"))
        }
        ("setBlockStateId", "(IIII)I") => {
            env.get_static_method_id(class, jni_str!("setBlockStateId"), jni_sig!("(IIII)I"))
        }
        _ => unreachable!("the isolated native surface has only generated method specs"),
    }
}

fn register_method(
    env: &mut Env<'_>,
    class: &JClass<'_>,
    method: NativeMethodSpec,
) -> jni::errors::Result<()> {
    match (method.name, method.descriptor) {
        ("blockStateId", "(III)I") => {
            adapter::register_block_query(env, class, method.name, method.descriptor)
        }
        ("serverTickCount", "()J") => {
            adapter::register_server_tick_query(env, class, method.name, method.descriptor)
        }
        ("setBlockStateId", "(IIII)I") => {
            adapter::register_block_state_write(env, class, method.name, method.descriptor)
        }
        _ => unreachable!("the isolated native surface has only generated method specs"),
    }
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
    use std::time::{Duration, Instant};

    use jni::JValue;
    use crate::adapter::{AdapterEvent, AdapterHost};
    use crate::runtime::JvmConfig;

    #[test]
    fn generated_surface_pins_each_supported_member_and_descriptor() {
        assert_eq!(ISOLATED_SHIM_CLASS, "lodestone.bridge.IsolatedPaperShim");
        assert_eq!(
            isolated_shim_methods(),
            &[
                NativeMethodSpec { name: "blockStateId", descriptor: "(III)I" },
                NativeMethodSpec { name: "serverTickCount", descriptor: "()J" },
                NativeMethodSpec { name: "setBlockStateId", descriptor: "(IIII)I" },
            ],
        );
        let block_state = isolated_shim_methods()[0];
        let server_tick = isolated_shim_methods()[1];
        let block_write = isolated_shim_methods()[2];
        assert_eq!(
            isolated_shim_registration_steps(),
            &[
                NativeRegistrationStep::Validate(block_state),
                NativeRegistrationStep::Validate(server_tick),
                NativeRegistrationStep::Validate(block_write),
                NativeRegistrationStep::Register(block_state),
                NativeRegistrationStep::Register(server_tick),
                NativeRegistrationStep::Register(block_write),
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
             public static native int blockStateId(int x, int y, int z); \
             public static native long serverTickCount(); \
             public static native int setBlockStateId(int x, int y, int z, int stateId); }",
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

    #[test]
    #[ignore = "requires JAVA_HOME pointing to a JDK with javac and libjvm"]
    fn live_native_surface_reaches_read_and_write_host_producers() {
        let jdk = std::env::var_os("JAVA_HOME").expect("JAVA_HOME is required");
        let fixture = std::env::temp_dir().join(format!(
            "lodestone-native-server-tick-{}",
            std::process::id(),
        ));
        let shim_root = fixture.join("shim");
        let adapter_root = fixture.join("adapter");
        let shim_source_root = shim_root.join("lodestone/bridge");
        let adapter_source_root = adapter_root.join("fixture/adapter");
        fs::create_dir_all(&shim_source_root).expect("shim source directory");
        fs::create_dir_all(&adapter_source_root).expect("adapter source directory");
        let shim_source = shim_source_root.join("IsolatedPaperShim.java");
        fs::write(
            &shim_source,
            "package lodestone.bridge; public final class IsolatedPaperShim { \
             public static native int blockStateId(int x, int y, int z); \
             public static native long serverTickCount(); \
             public static native int setBlockStateId(int x, int y, int z, int stateId); }",
        )
        .expect("shim source");
        let adapter_source = adapter_source_root.join("SurfaceAdapter.java");
        fs::write(
            &adapter_source,
            "package fixture.adapter; public final class SurfaceAdapter { \
             private static native int blockStateId(int x, int y, int z); \
             public static void onTick(long tick) {} }",
        )
        .expect("adapter source");
        for (output, source) in [(&shim_root, &shim_source), (&adapter_root, &adapter_source)] {
            let compile = Command::new(std::path::PathBuf::from(&jdk).join("bin/javac"))
                .arg("-d")
                .arg(output)
                .arg(source)
                .output()
                .expect("javac");
            assert!(
                compile.status.success(),
                "javac: {}",
                String::from_utf8_lossy(&compile.stderr)
            );
        }

        let mut host = AdapterHost::start_with_setup(
            JvmConfig::new().with_classpath(&adapter_root),
            "fixture.adapter.SurfaceAdapter",
            Duration::from_secs(5),
            move |runtime, env, _surface| {
                let shim_config = JvmConfig::new().with_classpath(&shim_root);
                runtime
                    .with_isolated_loader(env, &shim_config, |env, loader| {
                        install_in_loader(runtime, env, loader)
                            .map_err(|error| JvmError::new(error.to_string()))?;
                        let shim = runtime
                            .load_class_from_loader(env, loader, ISOLATED_SHIM_CLASS)?;
                        let tick = env
                            .call_static_method(
                                &shim,
                                jni_str!("serverTickCount"),
                                jni_sig!("()J"),
                                &[],
                            )
                            .and_then(|value| value.j())
                            ?;
                        if tick != 41 {
                            return Err(JvmError::new(format!(
                                "expected server tick 41, got {tick}"
                            )));
                        }
                        let written = env
                            .call_static_method(
                                &shim,
                                jni_str!("setBlockStateId"),
                                jni_sig!("(IIII)I"),
                                &[
                                    JValue::Int(-7),
                                    JValue::Int(72),
                                    JValue::Int(19),
                                    JValue::Int(1234),
                                ],
                            )
                            .and_then(|value| value.i())?;
                        if written != 1234 {
                            return Err(JvmError::new(format!(
                                "expected written state id 1234, got {written}"
                            )));
                        }
                        Ok(())
                    })
                    .map_err(|error| error.to_string())
            },
        )
        .expect("start native surface adapter");
        let limit = Instant::now() + Duration::from_secs(5);
        loop {
            host.service_pending_server_tick(1, || Ok(41));
            host.service_pending_block_writes(1, |write| {
                if write.x == -7 && write.y == 72 && write.z == 19 && write.state_id == 1234 {
                    Ok(())
                } else {
                    Err(format!("unexpected native block write: {write:?}"))
                }
            });
            match host.poll().expect("native surface adapter event") {
                Some(AdapterEvent::Ready) => break,
                Some(AdapterEvent::TickCompleted(tick)) => {
                    panic!("unexpected adapter tick {tick}");
                }
                None => assert!(Instant::now() < limit, "server tick surface did not become ready"),
            }
            std::thread::yield_now();
        }
        fs::remove_dir_all(fixture).expect("remove fixture directory");
    }
}
