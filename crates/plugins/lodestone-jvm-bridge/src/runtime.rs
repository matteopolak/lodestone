//! Opt-in JVM ownership and scoped thread attachment.
//!
//! This module is available only with the `jvm` feature. Starting a JVM is an
//! explicit host action; constructing a [`JvmConfig`] does not load or start
//! one. The callback passed to [`JvmRuntime::with_attached_thread`] receives a
//! scoped JNI environment and no world handle or ECS guard, so a host can keep
//! Java invocation separate from the short [`crate::port`] request service.

use std::fmt;
use std::path::{Path, PathBuf};

use jni::vm::{InitArgsBuilder, JavaVM, JvmError as JvmArgsError};
use jni::{Env, JNIVersion};

/// Configuration used when a host explicitly starts its one JVM.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JvmConfig {
    classpath: Vec<PathBuf>,
    options: Vec<String>,
}

impl JvmConfig {
    /// Creates an empty configuration. No JVM work happens here.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one directory or jar to the JVM classpath.
    #[must_use]
    pub fn with_classpath(mut self, path: impl AsRef<Path>) -> Self {
        self.classpath.push(path.as_ref().to_owned());
        self
    }

    /// Adds one JVM option, such as a system property.
    #[must_use]
    pub fn with_option(mut self, option: impl Into<String>) -> Self {
        self.options.push(option.into());
        self
    }
}

/// A JVM explicitly started by a host using the `jvm` feature.
#[derive(Debug)]
pub struct JvmRuntime {
    vm: JavaVM,
}

impl JvmRuntime {
    /// Starts one JVM with the supplied classpath and options.
    ///
    /// This is the only startup operation in the production bridge. It is
    /// intentionally not called from constructors, plugin discovery, or tick
    /// paths, preserving the default-off and explicit-start boundary.
    pub fn start(config: &JvmConfig) -> Result<Self, JvmError> {
        let classpath = classpath_option(&config.classpath)?;
        let mut args = InitArgsBuilder::new().version(JNIVersion::V1_8);
        if let Some(classpath) = classpath.as_deref() {
            args = args.option(classpath);
        }
        for option in &config.options {
            args = args.option(option);
        }
        let args = args.build().map_err(JvmError::from)?;
        JavaVM::new(args).map(|vm| Self { vm }).map_err(JvmError::from)
    }

    /// Attaches the current thread for the callback's scope, then detaches it
    /// when the callback returns if it was not already attached.
    ///
    /// The callback receives only a scoped JNI environment. It does not hold
    /// an ECS guard; world access must use [`crate::port::WorldPort`] and the
    /// tick-side [`crate::port::service_with_world`] boundary instead.
    pub fn with_attached_thread<F, T>(&self, callback: F) -> Result<T, JvmError>
    where
        F: for<'local> FnOnce(&mut Env<'local>) -> jni::errors::Result<T>,
    {
        self.vm
            .attach_current_thread_for_scope(callback)
            .map_err(JvmError::from)
    }
}

/// Error returned while constructing JVM arguments, starting the VM, or
/// running a scoped JNI callback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JvmError {
    message: String,
}

impl JvmError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for JvmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for JvmError {}

impl From<JvmArgsError> for JvmError {
    fn from(error: JvmArgsError) -> Self {
        Self::new(format!("could not build JVM arguments: {error}"))
    }
}

impl From<jni::errors::StartJvmError> for JvmError {
    fn from(error: jni::errors::StartJvmError) -> Self {
        Self::new(format!("could not start JVM: {error}"))
    }
}

impl From<jni::errors::Error> for JvmError {
    fn from(error: jni::errors::Error) -> Self {
        Self::new(format!("JNI callback failed: {error}"))
    }
}

fn classpath_option(paths: &[PathBuf]) -> Result<Option<String>, JvmError> {
    if paths.is_empty() {
        return Ok(None);
    }
    let joined = std::env::join_paths(paths)
        .map_err(|error| JvmError::new(format!("could not build JVM classpath: {error}")))?;
    Ok(Some(format!("-Djava.class.path={}", joined.to_string_lossy())))
}

#[cfg(test)]
mod tests {
    use super::classpath_option;
    use std::path::PathBuf;

    #[test]
    fn empty_config_has_no_implicit_classpath_option() {
        assert_eq!(classpath_option(&[]).expect("empty classpath"), None);
    }

    #[test]
    fn classpath_option_preserves_each_entry() {
        let paths = [PathBuf::from("/tmp/plugin.jar"), PathBuf::from("/tmp/classes")];
        let option = classpath_option(&paths)
            .expect("classpath")
            .expect("non-empty classpath");
        // `join_paths` uses the platform's path-list separator, not the path
        // separator. Check the stable prefix and both entries without baking
        // a Unix-only delimiter into this hermetic test.
        assert!(option.starts_with("-Djava.class.path="));
        assert!(option.contains(paths[0].to_str().expect("UTF-8 fixture")));
        assert!(option.contains(paths[1].to_str().expect("UTF-8 fixture")));
    }
}
