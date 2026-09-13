use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::Duration;

use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JClass, JObject, JString};
use jni::sys::{jint, jlong, jobject};
use jni::vm::{InitArgsBuilder, JavaVM};
use jni::{Env, EnvUnowned, JNIVersion, JValue, NativeMethod, jni_sig, jni_str};
use lodestone_ecs::{EcsHandle, hold_write, new_handle};
use lodestone_jvm_bridge::{
    CallbackDepthGuard, ObjectKind, ObjectRef, ObjectRegistry, PortServicer, ResolveError,
    WorldPort, channel, service_with_world,
};

const REQUEST_DEADLINE: Duration = Duration::from_millis(150);
const PANIC_INPUT: i32 = 19;

static CALLBACK_PORT: OnceLock<WorldPort<Request, Response>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    Success,
    Unregistered,
    Dropped,
    TimedOut,
    Panicked,
}

impl Scenario {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "success" => Ok(Self::Success),
            "unregistered" => Ok(Self::Unregistered),
            "dropped" => Ok(Self::Dropped),
            "timeout" => Ok(Self::TimedOut),
            "panic" => Ok(Self::Panicked),
            other => Err(format!("unknown scenario {other:?}")),
        }
    }

    const fn registers_native(self) -> bool {
        !matches!(self, Self::Unregistered)
    }
}

#[derive(Clone, Copy, Debug)]
enum RequestKind {
    BlockName,
    BlockStateId,
    AcquireBlockHandle,
    ResolveBlockHandle(i64),
    ReleaseBlockHandle(i64),
}

#[derive(Clone, Copy, Debug)]
struct Request {
    kind: RequestKind,
    x: i32,
    y: i32,
    z: i32,
    callback_thread: ThreadId,
}

#[derive(Clone, Copy, Debug)]
struct Response {
    value: i32,
    handle: i64,
    position: Option<(i32, i32, i32)>,
    error: Option<ResolveError>,
    service_thread: ThreadId,
}

#[derive(Debug, Default)]
struct ServiceState {
    block_handles: ObjectRegistry<(i32, i32, i32)>,
}

#[derive(Debug)]
struct CallbackError(String);

impl std::fmt::Display for CallbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CallbackError {}

impl From<jni::errors::Error> for CallbackError {
    fn from(error: jni::errors::Error) -> Self {
        Self(format!("JNI callback failure: {error}"))
    }
}

fn predicted_value(x: i32, y: i32, z: i32) -> i32 {
    x * 31 + y * 7 - z * 5 + 17
}

fn request_response(
    kind: RequestKind,
    x: jint,
    y: jint,
    z: jint,
) -> Result<Response, CallbackError> {
    let request = Request {
        kind,
        x,
        y,
        z,
        callback_thread: thread::current().id(),
    };
    let port = CALLBACK_PORT
        .get()
        .ok_or_else(|| CallbackError("callback port was not installed".to_owned()))?;
    let response = port
        .request(request)
        .map_err(|error| CallbackError(format!("world port failure: {error}")))?;
    if request.callback_thread == response.service_thread {
        return Err(CallbackError(
            "callback and service ran on the same Rust thread".to_owned(),
        ));
    }
    Ok(response)
}

extern "system" fn native_block_name<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    x: jint,
    y: jint,
    z: jint,
) -> jobject {
    unowned_env
        .with_env(|env| -> Result<jobject, CallbackError> {
            let response = request_response(RequestKind::BlockName, x, y, z)?;
            let result = env.new_string(format!("RUST:{}", response.value))?;
            if x == PANIC_INPUT {
                panic!("deliberate callback panic after service response");
            }
            Ok(result.into_raw())
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_block_state_id<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    x: jint,
    y: jint,
    z: jint,
) -> jint {
    unowned_env
        .with_env(|_env| -> Result<jint, CallbackError> {
            Ok(request_response(RequestKind::BlockStateId, x, y, z)?.value)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_reentrant_depth<'local>(
    mut unowned_env: EnvUnowned<'local>,
    class: JClass<'local>,
    remaining: jint,
) -> jobject {
    unowned_env
        .with_env(|env| -> Result<jobject, CallbackError> {
            if remaining < 0 {
                return Err(CallbackError(
                    "reentrant callback depth must be nonnegative".to_owned(),
                ));
            }
            let depth = CallbackDepthGuard::enter()
                .map_err(|error| CallbackError(error.to_string()))?;
            if remaining == 0 {
                return Ok(env.new_string(format!("REENTRANT:OK:{}", depth.level()))?.into_raw());
            }
            let argument = JValue::Int(remaining - 1);
            let result = env.call_static_method(
                &class,
                jni_str!("invokeReentrantDepth"),
                jni_sig!("(I)Ljava/lang/String;"),
                &[argument],
            )?;
            Ok(env.cast_local::<JString>(result.into_object()?)?.into_raw())
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_acquire_block_handle<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    x: jint,
    y: jint,
    z: jint,
) -> jlong {
    unowned_env
        .with_env(|_env| -> Result<jlong, CallbackError> {
            Ok(request_response(RequestKind::AcquireBlockHandle, x, y, z)?.handle)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_read_block_handle<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    bits: jlong,
) -> jobject {
    unowned_env
        .with_env(|env| -> Result<jobject, CallbackError> {
            let response = request_response(RequestKind::ResolveBlockHandle(bits), 0, 0, 0)?;
            let result = match (response.error, response.position) {
                (Some(error), _) => format!("STALE-HANDLE:{error}"),
                (None, Some((x, y, z))) => format!("BLOCK:{x},{y},{z}"),
                (None, None) => "INVALID-HANDLE-RESPONSE".to_owned(),
            };
            Ok(env.new_string(result)?.into_raw())
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

extern "system" fn native_release_block_handle<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    bits: jlong,
) -> jint {
    unowned_env
        .with_env(|_env| -> Result<jint, CallbackError> {
            let response = request_response(RequestKind::ReleaseBlockHandle(bits), 0, 0, 0)?;
            if let Some(error) = response.error {
                return Err(CallbackError(format!("handle release failure: {error}")));
            }
            Ok(response.value)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

fn service_request(
    state: &mut ServiceState,
    world_present: bool,
    request: Request,
) -> Response {
    let service_thread = thread::current().id();
    let mut response = Response {
        value: 0,
        handle: 0,
        position: None,
        error: None,
        service_thread,
    };
    match request.kind {
        RequestKind::BlockName | RequestKind::BlockStateId => {
            response.value =
                predicted_value(request.x, request.y, request.z) + i32::from(world_present);
        }
        RequestKind::AcquireBlockHandle => {
            let position = (request.x, request.y, request.z);
            response.handle = state
                .block_handles
                .handle_for(ObjectKind::Block, position)
                .to_bits();
        }
        RequestKind::ResolveBlockHandle(bits) => {
            let handle = ObjectRef::from_bits(bits, ObjectKind::Block);
            match state.block_handles.resolve(handle, ObjectKind::Block) {
                Ok(position) => response.position = Some(*position),
                Err(error) => response.error = Some(error),
            }
        }
        RequestKind::ReleaseBlockHandle(bits) => {
            let handle = ObjectRef::from_bits(bits, ObjectKind::Block);
            match state.block_handles.resolve(handle, ObjectKind::Block) {
                Ok(position) => {
                    let position = *position;
                    response.value = i32::from(state.block_handles.release(&position));
                }
                Err(error) => response.error = Some(error),
            }
        }
    }
    response
}

fn spawn_active_servicer(
    servicer: PortServicer<Request, Response>,
    done: Arc<AtomicBool>,
    world: EcsHandle,
) -> Result<JoinHandle<bool>, String> {
    let seed = hold_write(&world, |world| world.spawn_empty().id());
    let mut state = ServiceState::default();
    thread::Builder::new()
        .name("world-port-servicer".to_owned())
        .spawn(move || {
            let mut world_present = false;
            while !done.load(Ordering::Acquire) {
                let served = service_with_world(&servicer, &world, 1, |world, request| {
                    world_present = world.get_entity(seed).is_ok();
                    service_request(&mut state, world_present, request)
                });
                if served == 0 {
                    thread::yield_now();
                }
            }
            world_present
        })
        .map_err(|error| format!("could not start service thread: {error}"))
}

fn spawn_silent_servicer(
    servicer: PortServicer<Request, Response>,
    done: Arc<AtomicBool>,
) -> Result<JoinHandle<bool>, String> {
    thread::Builder::new()
        .name("silent-world-port-servicer".to_owned())
        .spawn(move || {
            while !done.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(1));
            }
            drop(servicer);
            false
        })
        .map_err(|error| format!("could not start silent service thread: {error}"))
}

fn load_class<'local>(
    env: &mut Env<'local>,
    helper: &JClass<'local>,
    real: &JString<'local>,
    shim: &JString<'local>,
    app: &JString<'local>,
    use_shim: bool,
) -> jni::errors::Result<JObject<'local>> {
    env.call_static_method(
        helper,
        jni_str!("load"),
        jni_sig!("(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Z)Ljava/lang/Class;"),
        &[
            JValue::Object(real),
            JValue::Object(shim),
            JValue::Object(app),
            JValue::Bool(use_shim),
        ],
    )?.l()
}

fn invoke_java(
    env: &mut Env<'_>,
    scenario: Scenario,
    real: &str,
    shim: &str,
    app: &str,
) -> jni::errors::Result<(String, String, String, String, String, String)> {
    let helper = env.find_class(jni_str!("org/example/BridgeLoader"))?;
    let real = env.new_string(real)?;
    let shim = env.new_string(shim)?;
    let app = env.new_string(app)?;
    let _control_level = load_class(env, &helper, &real, &shim, &app, false)?;
    let describe = env.new_string("describe")?;
    let control = env.call_static_method(
        &helper,
        jni_str!("invoke"),
        jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
        &[JValue::Object(&describe)],
    )?;
    let control = env
        .cast_local::<JString>(control.into_object()?)?
        .try_to_string(env)?;

    let shim_level = load_class(env, &helper, &real, &shim, &app, true)?;
    let describe = env.new_string("describe")?;
    let shim = env.call_static_method(
        &helper,
        jni_str!("invoke"),
        jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
        &[JValue::Object(&describe)],
    )?;
    let shim = env
        .cast_local::<JString>(shim.into_object()?)?
        .try_to_string(env)?;
    if scenario.registers_native() {
        let method = unsafe {
            NativeMethod::from_raw_parts(
                jni_str!("nativeBlockName"),
                jni_str!("(III)Ljava/lang/String;"),
                native_block_name as *mut c_void,
            )
        };
        let state_id = unsafe {
            NativeMethod::from_raw_parts(
                jni_str!("nativeBlockStateId"),
                jni_str!("(III)I"),
                native_block_state_id as *mut c_void,
            )
        };
        let acquire_handle = unsafe {
            NativeMethod::from_raw_parts(
                jni_str!("nativeAcquireBlockHandle"),
                jni_str!("(III)J"),
                native_acquire_block_handle as *mut c_void,
            )
        };
        let read_handle = unsafe {
            NativeMethod::from_raw_parts(
                jni_str!("nativeReadBlockHandle"),
                jni_str!("(J)Ljava/lang/String;"),
                native_read_block_handle as *mut c_void,
            )
        };
        let release_handle = unsafe {
            NativeMethod::from_raw_parts(
                jni_str!("nativeReleaseBlockHandle"),
                jni_str!("(J)I"),
                native_release_block_handle as *mut c_void,
            )
        };
        let reentrant_depth = unsafe {
            NativeMethod::from_raw_parts(
                jni_str!("nativeReentrantDepth"),
                jni_str!("(I)Ljava/lang/String;"),
                native_reentrant_depth as *mut c_void,
            )
        };
        let shim_level = env.cast_local::<JClass>(shim_level)?;
        unsafe {
            env.register_native_methods(
                &shim_level,
                &[
                    method,
                    state_id,
                    acquire_handle,
                    read_handle,
                    release_handle,
                    reentrant_depth,
                ],
            )?
        };
    }
    let method = env.new_string(if scenario == Scenario::Panicked {
        "describeNativePanicMessage"
    } else {
        "describeNativeMessage"
    })?;
    let test = env.call_static_method(
        &helper,
        jni_str!("invoke"),
        jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
        &[JValue::Object(&method)],
    )?;
    let test = env
        .cast_local::<JString>(test.into_object()?)?
        .try_to_string(env)?;
    let state_id = if scenario == Scenario::Success {
        let method = env.new_string("describeNativeId")?;
        let state_id = env.call_static_method(
            &helper,
            jni_str!("invoke"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
            &[JValue::Object(&method)],
        )?;
        env.cast_local::<JString>(state_id.into_object()?)?
            .try_to_string(env)?
    } else {
        String::new()
    };
    let handle_lifetime = if scenario == Scenario::Success {
        let method = env.new_string("describeHandleLifetime")?;
        let handle_lifetime = env.call_static_method(
            &helper,
            jni_str!("invoke"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
            &[JValue::Object(&method)],
        )?;
        env.cast_local::<JString>(handle_lifetime.into_object()?)?
            .try_to_string(env)?
    } else {
        String::new()
    };
    let reentrant_control = if scenario == Scenario::Success {
        let method = env.new_string("describeReentrantControls")?;
        let reentrant_control = env.call_static_method(
            &helper,
            jni_str!("invoke"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
            &[JValue::Object(&method)],
        )?;
        env.cast_local::<JString>(reentrant_control.into_object()?)?
            .try_to_string(env)?
    } else {
        String::new()
    };
    Ok((
        control,
        shim,
        test,
        state_id,
        handle_lifetime,
        reentrant_control,
    ))
}

fn validate_output(
    control: &str,
    shim: &str,
    test: &str,
    state_id: &str,
    handle_lifetime: &str,
    reentrant_control: &str,
    world_probe: &str,
    scenario: &str,
) -> bool {
    control == "REAL:11,1,4"
        && shim == "SHIM:11,1,4"
        && match scenario {
            "success" => {
                test == "RUST:346"
                    && state_id == "NATIVE-ID:346"
                    && handle_lifetime
                        == "HANDLE-LIFETIME:live=BLOCK:11,1,4 forged=STALE-HANDLE:the referenced object no longer exists released=1 after=STALE-HANDLE:the referenced object no longer exists"
                    && reentrant_control
                        == "REENTRANT-CONTROL:below=REENTRANT:OK:3 over=RuntimeException:Rust error: reentrant callback depth limit 4 exceeded"
                    && world_probe == "WORLD:present"
            }
            "unregistered" => {
                state_id.is_empty()
                    && handle_lifetime.is_empty()
                    && reentrant_control.is_empty()
                    && test.starts_with("UnsatisfiedLinkError:")
            }
            "dropped" => {
                state_id.is_empty()
                    && handle_lifetime.is_empty()
                    && reentrant_control.is_empty()
                    && test
                        == "RuntimeException:Rust error: world port failure: the world servicer is no longer running"
            }
            "timeout" => {
                state_id.is_empty()
                    && handle_lifetime.is_empty()
                    && reentrant_control.is_empty()
                    && test
                        == "RuntimeException:Rust error: world port failure: the world servicer did not answer within 150ms"
            }
            "panic" => {
                state_id.is_empty()
                    && handle_lifetime.is_empty()
                    && reentrant_control.is_empty()
                    && test
                        == "RuntimeException:Rust panic: deliberate callback panic after service response"
            }
            _ => false,
        }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let scenario_name = args.next().ok_or_else(|| "missing scenario".to_owned())?;
    let scenario = Scenario::parse(&scenario_name)?;
    let harness = args.next().ok_or_else(|| "missing harness directory".to_owned())?;
    let real = args.next().ok_or_else(|| "missing real directory".to_owned())?;
    let shim = args.next().ok_or_else(|| "missing shim directory".to_owned())?;
    let app = args.next().ok_or_else(|| "missing app directory".to_owned())?;
    if args.next().is_some() {
        return Err("too many arguments".to_owned());
    }

    let (port, servicer) = channel::<Request, Response>(REQUEST_DEADLINE);
    let done = Arc::new(AtomicBool::new(false));
    let world = new_handle();
    let service = match scenario {
        Scenario::Success | Scenario::Panicked => {
            Some(spawn_active_servicer(servicer, Arc::clone(&done), world.clone())?)
        }
        Scenario::TimedOut => Some(spawn_silent_servicer(servicer, Arc::clone(&done))?),
        Scenario::Dropped | Scenario::Unregistered => {
            drop(servicer);
            None
        }
    };
    if scenario.registers_native() {
        CALLBACK_PORT
            .set(port)
            .map_err(|_| "callback port installed twice".to_owned())?;
    }

    let vm_args = InitArgsBuilder::new()
        .version(JNIVersion::V1_8)
        .option(format!("-Djava.class.path={harness}"))
        .build()
        .map_err(|error| format!("could not build JVM arguments: {error}"))?;
    let vm = JavaVM::new(vm_args).map_err(|error| format!("could not create JVM: {error}"))?;
    let attached_before = vm.threads_attached();
    let result = vm
        .attach_current_thread_for_scope(|env| invoke_java(env, scenario, &real, &shim, &app))
        .map_err(|error| format!("JNI invocation failed: {error}"))?;
    let attached_after = vm.threads_attached();
    if attached_before != attached_after {
        return Err(format!("scoped attachment leaked: {attached_before} -> {attached_after}"));
    }
    done.store(true, Ordering::Release);
    let world_probe = if let Some(service) = service {
        service
            .join()
            .map_err(|_| "service thread panicked".to_owned())?
    } else {
        false
    };
    hold_write(&world, |_world| {});
    let (control, shim, test, state_id, handle_lifetime, reentrant_control) = result;
    if !validate_output(
        &control,
        &shim,
        &test,
        &state_id,
        &handle_lifetime,
        &reentrant_control,
        &format!("WORLD:{}", if world_probe { "present" } else { "missing" }),
        &scenario_name,
    ) {
        return Err(format!(
            "unexpected output: control={control:?} shim={shim:?} test={test:?} state_id={state_id:?} handle_lifetime={handle_lifetime:?} reentrant_control={reentrant_control:?}"
        ));
    }
    println!(
        "control={control} shim={shim} test={test} state_id={state_id} handle_lifetime={handle_lifetime} reentrant_control={reentrant_control} world=WORLD:{} attachment={attached_before}->{attached_after} detached=PASS",
        if world_probe { "present" } else { "missing" }
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("INTERCEPTED SPIKE FAILURE: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intercepted_output_requires_control_shim_and_native_evidence() {
        assert!(validate_output(
            "REAL:11,1,4",
            "SHIM:11,1,4",
            "RUST:346",
            "NATIVE-ID:346",
            "HANDLE-LIFETIME:live=BLOCK:11,1,4 forged=STALE-HANDLE:the referenced object no longer exists released=1 after=STALE-HANDLE:the referenced object no longer exists",
            "REENTRANT-CONTROL:below=REENTRANT:OK:3 over=RuntimeException:Rust error: reentrant callback depth limit 4 exceeded",
            "WORLD:present",
            "success"
        ));
        assert!(!validate_output(
            "REAL:11,1,4",
            "REAL:11,1,4",
            "RUST:346",
            "NATIVE-ID:346",
            "HANDLE-LIFETIME:live=BLOCK:11,1,4 forged=STALE-HANDLE:the referenced object no longer exists released=1 after=STALE-HANDLE:the referenced object no longer exists",
            "REENTRANT-CONTROL:below=REENTRANT:OK:3 over=RuntimeException:Rust error: reentrant callback depth limit 4 exceeded",
            "WORLD:missing",
            "success"
        ));
        assert!(!validate_output(
            "REAL:11,1,4",
            "SHIM:11,1,4",
            "RUST:346",
            "NATIVE-ID:347",
            "HANDLE-LIFETIME:live=BLOCK:11,1,4 forged=STALE-HANDLE:the referenced object no longer exists released=1 after=STALE-HANDLE:the referenced object no longer exists",
            "REENTRANT-CONTROL:below=REENTRANT:OK:3 over=RuntimeException:Rust error: reentrant callback depth limit 4 exceeded",
            "WORLD:present",
            "success"
        ));
    }

}
