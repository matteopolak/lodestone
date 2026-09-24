use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::Duration;

use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JClass, JString};
use jni::sys::jint;
use jni::vm::{InitArgsBuilder, JavaVM};
use jni::{Env, EnvUnowned, JNIVersion, JValue, NativeMethod, jni_sig, jni_str};
use lodestone_jvm_bridge::{PortServicer, WorldPort, channel};

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
            other => Err(format!(
                "unknown scenario {other:?}; expected success, unregistered, dropped, timeout, or panic"
            )),
        }
    }

    const fn registers_native(self) -> bool {
        !matches!(self, Self::Unregistered)
    }

    const fn plugin_mode(self) -> jint {
        if matches!(self, Self::Panicked) { 1 } else { 0 }
    }
}

#[derive(Clone, Copy, Debug)]
struct Request {
    x: i32,
    y: i32,
    z: i32,
    callback_thread: ThreadId,
}

#[derive(Clone, Copy, Debug)]
struct Response {
    value: i32,
    service_thread: ThreadId,
}

#[derive(Clone, Copy, Debug)]
struct ThreadEvidence {
    callback: ThreadId,
    service: ThreadId,
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

extern "system" fn native_score<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    x: jint,
    y: jint,
    z: jint,
) -> jint {
    unowned_env
        .with_env(|_env| -> Result<jint, CallbackError> {
            let request = Request {
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
            if x == PANIC_INPUT {
                panic!("deliberate callback panic after service response");
            }
            Ok(response.value)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

fn service_request(request: Request) -> Response {
    Response {
        value: predicted_value(request.x, request.y, request.z),
        service_thread: thread::current().id(),
    }
}

fn spawn_active_servicer(
    servicer: PortServicer<Request, Response>,
    done: Arc<AtomicBool>,
) -> Result<JoinHandle<Option<ThreadEvidence>>, String> {
    thread::Builder::new()
        .name("world-port-servicer".to_owned())
        .spawn(move || {
            while !done.load(Ordering::Acquire) {
                let mut evidence = None;
                let served = servicer.service_pending(|request| {
                    let response = service_request(request);
                    evidence = Some(ThreadEvidence {
                        callback: request.callback_thread,
                        service: response.service_thread,
                    });
                    response
                });
                if served {
                    return evidence;
                }
                thread::yield_now();
            }
            None
        })
        .map_err(|error| format!("could not start service thread: {error}"))
}

fn spawn_silent_servicer(
    servicer: PortServicer<Request, Response>,
    done: Arc<AtomicBool>,
) -> Result<JoinHandle<Option<ThreadEvidence>>, String> {
    thread::Builder::new()
        .name("silent-world-port-servicer".to_owned())
        .spawn(move || {
            while !done.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(1));
            }
            drop(servicer);
            None
        })
        .map_err(|error| format!("could not start silent service thread: {error}"))
}

fn invoke_plugin(env: &mut Env<'_>, scenario: Scenario) -> jni::errors::Result<String> {
    let class = env.find_class(jni_str!("org/example/InvocationPlugin"))?;
    if scenario.registers_native() {
        // SAFETY: `native_score` is a static JNI function whose arguments and
        // return value exactly match `(III)I`. The class parameter is `JClass`
        // because the Java declaration is static.
        let method = unsafe {
            NativeMethod::from_raw_parts(
                jni_str!("nativeScore"),
                jni_str!("(III)I"),
                native_score as *mut c_void,
            )
        };
        // SAFETY: the Java declaration is `static native int
        // nativeScore(int, int, int)`, matching the descriptor and function
        // pointer justified above.
        unsafe { env.register_native_methods(&class, &[method])? };
    }

    let returned = env.call_static_method(
        &class,
        jni_str!("runAndReport"),
        jni_sig!("(I)Ljava/lang/String;"),
        &[JValue::Int(scenario.plugin_mode())],
    )?;
    let object = returned.into_object()?;
    let string = env.cast_local::<JString>(object)?;
    string.try_to_string(env)
}

fn validate_output(scenario: Scenario, output: &str) -> Result<(), String> {
    let accepted = match scenario {
        Scenario::Success => output == "RESULT:422",
        Scenario::Unregistered => output.starts_with("ERROR:UnsatisfiedLinkError:"),
        Scenario::Dropped => {
            output
                == "ERROR:RuntimeException:Rust error: world port failure: the world servicer is no longer running"
        }
        Scenario::TimedOut => {
            output
                == "ERROR:RuntimeException:Rust error: world port failure: the world servicer did not answer within 150ms"
        }
        Scenario::Panicked => {
            output
                == "ERROR:RuntimeException:Rust panic: deliberate callback panic after service response"
        }
    };
    if accepted {
        Ok(())
    } else {
        Err(format!("{scenario:?} produced unexpected output: {output}"))
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let scenario = Scenario::parse(
        &args
            .next()
            .ok_or_else(|| "missing scenario argument".to_owned())?,
    )?;
    let classes = args
        .next()
        .ok_or_else(|| "missing compiled Java classes directory".to_owned())?;
    if args.next().is_some() {
        return Err("too many arguments".to_owned());
    }

    let (port, servicer) = channel::<Request, Response>(REQUEST_DEADLINE);
    let done = Arc::new(AtomicBool::new(false));
    let service = match scenario {
        Scenario::Success | Scenario::Panicked => {
            let handle = spawn_active_servicer(servicer, Arc::clone(&done))?;
            Some(handle)
        }
        Scenario::TimedOut => {
            let handle = spawn_silent_servicer(servicer, Arc::clone(&done))?;
            Some(handle)
        }
        Scenario::Dropped | Scenario::Unregistered => {
            drop(servicer);
            None
        }
    };

    if scenario.registers_native() {
        CALLBACK_PORT
            .set(port)
            .map_err(|_| "callback port was installed twice in one process".to_owned())?;
    } else {
        drop(port);
    }

    let args = InitArgsBuilder::new()
        .version(JNIVersion::V1_8)
        .option(format!("-Djava.class.path={classes}"))
        .build()
        .map_err(|error| format!("could not build JVM arguments: {error}"))?;
    // JNI supports only one live VM per process. Each runner arm starts this
    // executable afresh, and this is the only creation call in the binary.
    let vm = JavaVM::new(args).map_err(|error| format!("could not create JVM: {error}"))?;

    let invocation = thread::Builder::new()
        .name("jni-invocation".to_owned())
        .spawn(move || {
            vm.attach_current_thread(|env| invoke_plugin(env, scenario))
                .map_err(|error| format!("JNI invocation failed: {error}"))
        })
        .map_err(|error| format!("could not start invocation thread: {error}"))?;

    let output = invocation
        .join()
        .map_err(|_| "invocation thread panicked outside the JNI callback guard".to_owned())??;
    done.store(true, Ordering::Release);

    let evidence = service
        .map(|handle| {
            handle
                .join()
                .map_err(|_| "service thread panicked".to_owned())
        })
        .transpose()?
        .flatten();

    validate_output(scenario, &output)?;
    println!("scenario={scenario:?} {output}");
    if let Some(evidence) = evidence {
        if evidence.callback == evidence.service {
            return Err("thread evidence shows callback and service on one thread".to_owned());
        }
        println!(
            "callback_thread={:?} service_thread={:?} distinct=PASS",
            evidence.callback, evidence.service
        );
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("SPIKE FAILURE: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_transform_has_an_independently_predicted_value() {
        assert_eq!(predicted_value(11, 7, -3), 422);
    }

    #[test]
    fn every_process_arm_requires_its_distinguishing_observation() {
        assert!(validate_output(Scenario::Success, "RESULT:422").is_ok());
        assert!(validate_output(
            Scenario::Unregistered,
            "ERROR:UnsatisfiedLinkError:'int org.example.InvocationPlugin.nativeScore(int, int, int)'",
        )
        .is_ok());
        assert!(validate_output(
            Scenario::Dropped,
            "ERROR:RuntimeException:Rust error: world port failure: the world servicer is no longer running",
        )
        .is_ok());
        assert!(validate_output(
            Scenario::TimedOut,
            "ERROR:RuntimeException:Rust error: world port failure: the world servicer did not answer within 150ms",
        )
        .is_ok());
        assert!(validate_output(
            Scenario::Panicked,
            "ERROR:RuntimeException:Rust panic: deliberate callback panic after service response",
        )
        .is_ok());
        assert!(validate_output(Scenario::Success, "RESULT:11").is_err());
    }
}
