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
use jni::objects::{JClass, JObject};
use jni::{Env, JNIVersion, JValue, jni_sig, jni_str};

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

    /// Loads one operator-supplied class through a fresh, isolated URL loader.
    ///
    /// The loader's parent is the platform loader, rather than the system
    /// loader. Consequently it cannot silently resolve an application class
    /// before consulting the ordered operator paths. A host may put its shim
    /// directory before an operator jar, so bytecode in that jar resolves the
    /// shim without changing either input jar. This only establishes loading;
    /// the caller still owns native registration and the supported surface.
    ///
    /// `binary_name` uses Java's dotted form. The returned class retains its
    /// defining loader for as long as the local class reference is live.
    pub fn load_isolated_class<'local>(
        &self,
        env: &mut Env<'local>,
        config: &JvmConfig,
        binary_name: &str,
    ) -> Result<JClass<'local>, JvmError> {
        self.with_isolated_loader(env, config, |env, loader| {
            self.load_class_from_loader(env, loader, binary_name)
        })
    }

    /// Runs one operation with a fresh isolated URL loader.
    ///
    /// The callback can load more than one class through this loader. That is
    /// needed when an operator shim has native members: registration must name
    /// the same class definition that the following lifecycle load will use,
    /// rather than a same-named definition from another fresh loader.
    pub fn with_isolated_loader<'local, T>(
        &self,
        env: &mut Env<'local>,
        config: &JvmConfig,
        operation: impl FnOnce(&mut Env<'local>, &JObject<'local>) -> Result<T, JvmError>,
    ) -> Result<T, JvmError> {
        let parent = env.call_static_method(
            jni_str!("java/lang/ClassLoader"),
            jni_str!("getPlatformClassLoader"),
            jni_sig!("()Ljava/lang/ClassLoader;"),
            &[],
        )?.l()?;
        self.with_isolated_loader_with_parent(env, config, &parent, operation)
    }

    /// Runs one operation with a fresh isolated URL loader below `parent`.
    ///
    /// A server-owned host uses this to give every plugin a fresh child loader
    /// while retaining one shared definition of the server API and bridge shim
    /// in the parent.  Supplying the parent explicitly is important for Java
    /// type identity: placing the server jar in every plugin's URL list would
    /// create separate definitions of the same API class.
    pub fn with_isolated_loader_with_parent<'local, T>(
        &self,
        env: &mut Env<'local>,
        config: &JvmConfig,
        parent: &JObject<'local>,
        operation: impl FnOnce(&mut Env<'local>, &JObject<'local>) -> Result<T, JvmError>,
    ) -> Result<T, JvmError> {
        if config.classpath.is_empty() {
            return Err(JvmError::new(
                "isolated class loading requires at least one operator path",
            ));
        }
        let path_count = i32::try_from(config.classpath.len())
            .map_err(|_| JvmError::new("too many operator paths for one class loader"))?;
        let url_class = env.find_class(jni_str!("java/net/URL"))?;
        let urls = env.new_object_array(path_count, &url_class, JObject::null())?;
        for (index, path) in config.classpath.iter().enumerate() {
            if !path.exists() {
                return Err(JvmError::new(format!(
                    "operator path does not exist: {}",
                    path.display()
                )));
            }
            let path = path.to_str().ok_or_else(|| {
                JvmError::new(format!("operator path is not valid UTF-8: {}", path.display()))
            })?;
            let path = env.new_string(path)?;
            let file = env.new_object(
                jni_str!("java/io/File"),
                jni_sig!("(Ljava/lang/String;)V"),
                &[JValue::Object(&path)],
            )?;
            let uri = env.call_method(
                &file,
                jni_str!("toURI"),
                jni_sig!("()Ljava/net/URI;"),
                &[],
            )?.l()?;
            let url = env.call_method(
                &uri,
                jni_str!("toURL"),
                jni_sig!("()Ljava/net/URL;"),
                &[],
            )?.l()?;
            urls.set_element(env, index, &url)?;
        }
        let loader = env.new_object(
            jni_str!("java/net/URLClassLoader"),
            jni_sig!("([Ljava/net/URL;Ljava/lang/ClassLoader;)V"),
            &[JValue::Object(&urls), JValue::Object(&parent)],
        )?;
        operation(env, &loader)
    }

    /// Loads one class with a loader created by [`Self::with_isolated_loader`].
    ///
    /// This uses `ClassLoader.loadClass`, which resolves but does not initialize
    /// the requested class.
    pub fn load_class_from_loader<'local>(
        &self,
        env: &mut Env<'local>,
        loader: &JObject<'local>,
        binary_name: &str,
    ) -> Result<JClass<'local>, JvmError> {
        let name = env.new_string(binary_name)?;
        let class = env.call_method(
            loader,
            jni_str!("loadClass"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/Class;"),
            &[JValue::Object(&name)],
        )?.l()?;
        env.cast_local::<JClass>(class).map_err(JvmError::from)
    }
}

/// Error returned while constructing JVM arguments, starting the VM, or
/// running a scoped JNI callback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JvmError {
    message: String,
}

impl JvmError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
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
