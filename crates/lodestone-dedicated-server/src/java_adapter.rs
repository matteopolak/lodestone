//! Operator-configured experimental Java adapter over the public server read API.

use std::time::Duration;

use lodestone_jvm_bridge::adapter::{AdapterEvent, AdapterHost};
use lodestone_jvm_bridge::paper::{PaperBootstrapConfig, PaperBootstrapPlan};
use lodestone_jvm_bridge::runtime::JvmConfig;
use lodestone_server::IntegratedServer;

#[derive(Debug)]
pub(crate) struct JavaAdapter {
    host: AdapterHost,
    paper_plan: Option<PaperBootstrapPlan>,
    last_dispatched: Option<u64>,
}

#[derive(Debug)]
struct JavaAdapterConfig {
    class: String,
    classpath: std::ffi::OsString,
    deadline: Duration,
    paper: Option<PaperBootstrapConfig>,
}

impl JavaAdapter {
    pub(crate) fn from_environment() -> Result<Option<Self>, String> {
        let class = std::env::var_os("LODESTONE_JAVA_ADAPTER_CLASS");
        let classpath = std::env::var_os("LODESTONE_JAVA_CLASSPATH");
        let deadline = std::env::var_os("LODESTONE_JAVA_DEADLINE_MS");
        let paper_jar = std::env::var_os("LODESTONE_PAPER_JAR");
        let plugins_directory = std::env::var_os("LODESTONE_PAPER_PLUGIN_DIRECTORY");
        let shim_path = std::env::var_os("LODESTONE_PAPER_SHIM_PATH");
        Self::from_values(class, classpath, deadline, paper_jar, plugins_directory, shim_path)
    }

    fn from_values(
        class: Option<std::ffi::OsString>,
        classpath: Option<std::ffi::OsString>,
        deadline: Option<std::ffi::OsString>,
        paper_jar: Option<std::ffi::OsString>,
        plugins_directory: Option<std::ffi::OsString>,
        shim_path: Option<std::ffi::OsString>,
    ) -> Result<Option<Self>, String> {
        let configuration = JavaAdapterConfig::from_values(
            class,
            classpath,
            deadline,
            paper_jar,
            plugins_directory,
            shim_path,
        )?;
        let Some(configuration) = configuration else {
            return Ok(None);
        };
        let paths: Vec<_> = std::env::split_paths(&configuration.classpath).collect();
        if paths.iter().any(|path| path.as_os_str().is_empty()) {
            return Err("LODESTONE_JAVA_CLASSPATH contains an empty entry".to_owned());
        }
        let config = paths.into_iter().fold(JvmConfig::new(), |config, path| config.with_classpath(path));
        let paper_plan = configuration.paper
            .map(PaperBootstrapConfig::discover)
            .transpose()
            .map_err(|error| format!("invalid Paper bootstrap configuration: {error}"))?;
        Self::start(config, &configuration.class, configuration.deadline, paper_plan).map(Some)
    }

    pub(crate) fn start(
        config: JvmConfig,
        class: &str,
        deadline: Duration,
        paper_plan: Option<PaperBootstrapPlan>,
    ) -> Result<Self, String> {
        let host = if let Some(plan) = paper_plan.clone() {
            AdapterHost::start_with_setup(config, class, deadline, move |runtime, env| {
                plan.load_lifecycle_entries_in_runtime(runtime, env)
                    .map(|_| ())
                    .map_err(|error| format!("could not load configured Paper lifecycle entries: {error}"))
            })
        } else {
            AdapterHost::start(config, class, deadline)
        }.map_err(|error| error.to_string())?;
        if let Some(plan) = &paper_plan {
            tracing::info!(
                adapter = class,
                paper_jar = %plan.paper_jar().display(),
                plugins = plan.plugins().len(),
                "starting experimental Java adapter with validated Paper bootstrap inputs"
            );
        } else {
            tracing::info!(adapter = class, "starting experimental Java adapter; Paper plugin loading is not enabled");
        }
        Ok(Self { host, paper_plan, last_dispatched: None })
    }

    pub(crate) fn requires_paper_bootstrap(&self) -> bool {
        self.paper_plan.is_some()
    }

    pub(crate) fn poll(&mut self, server: &IntegratedServer) -> Result<Option<u64>, String> {
        let event = self.host.poll().map_err(|error| error.to_string())?;
        match event {
            Some(AdapterEvent::Ready) => {
                if let Some(plan) = &self.paper_plan {
                    tracing::info!(
                        paper_jar = %plan.paper_jar().display(),
                        plugins = plan.plugins().len(),
                        "configured Paper bootstrap and plugin entry classes loaded; Paper is not initialized and plugins are not instantiated or enabled"
                    );
                } else {
                    tracing::info!("experimental Java adapter ready");
                }
            }
            Some(AdapterEvent::TickCompleted(tick)) => tracing::debug!(tick, "Java adapter callback completed"),
            None => {}
        }
        self.host.service_pending(64, |query| {
            server.resident_block_state_id(query.x, query.y, query.z)
                .map(|state| state.raw())
                .ok_or_else(|| format!("primary-world block unavailable at {},{},{}", query.x, query.y, query.z))
        });
        if self.host.is_idle() {
            if let Some(tick) = server.server_tick_count() {
                if self.last_dispatched != Some(tick) {
                    self.host.dispatch_tick(tick).map_err(|error| error.to_string())?;
                    self.last_dispatched = Some(tick);
                }
            }
        }
        Ok(match event {
            Some(AdapterEvent::TickCompleted(tick)) => Some(tick),
            _ => None,
        })
    }
}

impl JavaAdapterConfig {
    fn from_values(
        class: Option<std::ffi::OsString>,
        classpath: Option<std::ffi::OsString>,
        deadline: Option<std::ffi::OsString>,
        paper_jar: Option<std::ffi::OsString>,
        plugins_directory: Option<std::ffi::OsString>,
        shim_path: Option<std::ffi::OsString>,
    ) -> Result<Option<Self>, String> {
        if [
            class.as_ref(),
            classpath.as_ref(),
            deadline.as_ref(),
            paper_jar.as_ref(),
            plugins_directory.as_ref(),
            shim_path.as_ref(),
        ].iter().all(Option::is_none) {
            return Ok(None);
        }
        let class = class.and_then(|value| value.into_string().ok())
            .filter(|value| !value.is_empty())
            .ok_or("LODESTONE_JAVA_ADAPTER_CLASS must name an explicit adapter class")?;
        let classpath = classpath.filter(|value| !value.is_empty())
            .ok_or("LODESTONE_JAVA_CLASSPATH must name adapter jars or class directories")?;
        let millis = match deadline {
            Some(value) => value.to_str().and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .ok_or("LODESTONE_JAVA_DEADLINE_MS must be a positive integer")?,
            None => 5000,
        };
        let paper = match (paper_jar, plugins_directory, shim_path) {
            (None, None, None) => None,
            (Some(paper_jar), Some(plugins_directory), shim_path) => {
                if paper_jar.is_empty() {
                    return Err("LODESTONE_PAPER_JAR must name an operator-supplied Paper server jar".to_owned());
                }
                if plugins_directory.is_empty() {
                    return Err("LODESTONE_PAPER_PLUGIN_DIRECTORY must name an operator-supplied plugin directory".to_owned());
                }
                let mut config = PaperBootstrapConfig::new(paper_jar, plugins_directory);
                if let Some(shim_path) = shim_path {
                    if shim_path.is_empty() {
                        return Err("LODESTONE_PAPER_SHIM_PATH must name a shim directory or jar when set".to_owned());
                    }
                    config = config.with_shim_path(shim_path).with_isolated_native_shim();
                }
                Some(config)
            }
            (None, _, _) => return Err(
                "LODESTONE_PAPER_JAR must name an operator-supplied Paper server jar when Paper configuration is present".to_owned()
            ),
            (Some(_), None, _) => return Err(
                "LODESTONE_PAPER_PLUGIN_DIRECTORY must name an operator-supplied plugin directory when LODESTONE_PAPER_JAR is set".to_owned()
            ),
        };
        Ok(Some(Self { class, classpath, deadline: Duration::from_millis(millis), paper }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_java_config_does_not_start_a_worker() {
        assert!(JavaAdapterConfig::from_values(None, None, None, None, None, None).unwrap().is_none());
    }

    #[test]
    fn partial_and_invalid_java_config_fail_before_startup() {
        assert!(JavaAdapterConfig::from_values(Some("a.B".into()), None, None, None, None, None).is_err());
        assert!(JavaAdapterConfig::from_values(None, Some("classes".into()), None, None, None, None).is_err());
        for value in ["0", "-1", "bad"] {
            assert!(JavaAdapterConfig::from_values(Some("a.B".into()), Some("classes".into()),
                Some(value.into()), None, None, None).is_err());
        }
    }

    #[test]
    fn paper_configuration_requires_jar_and_plugin_directory_before_startup() {
        let class = Some("a.B".into());
        let classpath = Some("classes".into());
        let missing_jar = JavaAdapterConfig::from_values(
            class.clone(), classpath.clone(), None, None, Some("plugins".into()), None,
        ).unwrap_err();
        assert!(missing_jar.contains("LODESTONE_PAPER_JAR"));
        let missing_plugins = JavaAdapterConfig::from_values(
            class.clone(), classpath.clone(), None, Some("paper.jar".into()), None, None,
        ).unwrap_err();
        assert!(missing_plugins.contains("LODESTONE_PAPER_PLUGIN_DIRECTORY"));
        let shim_only = JavaAdapterConfig::from_values(
            class, classpath, None, None, None, Some("shims".into()),
        ).unwrap_err();
        assert!(shim_only.contains("LODESTONE_PAPER_JAR"));
    }

    #[test]
    fn paper_configuration_keeps_the_optional_shim_out_of_the_adapter_classpath() {
        let config = JavaAdapterConfig::from_values(
            Some("a.B".into()),
            Some("adapter-classes".into()),
            None,
            Some("paper.jar".into()),
            Some("plugins".into()),
            Some("shims".into()),
        ).unwrap().expect("configured adapter");
        assert_eq!(config.classpath, std::ffi::OsString::from("adapter-classes"));
        assert!(config.paper.is_some());
    }

    #[test]
    fn empty_paper_values_are_rejected_before_filesystem_discovery() {
        let error = JavaAdapterConfig::from_values(
            Some("a.B".into()), Some("classes".into()), None,
            Some("".into()), Some("plugins".into()), None,
        ).unwrap_err();
        assert!(error.contains("LODESTONE_PAPER_JAR"));
        let error = JavaAdapterConfig::from_values(
            Some("a.B".into()), Some("classes".into()), None,
            Some("paper.jar".into()), Some("plugins".into()), Some("".into()),
        ).unwrap_err();
        assert!(error.contains("LODESTONE_PAPER_SHIM_PATH"));
    }

    #[test]
    fn invalid_paper_input_is_named_before_a_worker_starts() {
        let fixture = tempfile::tempdir().expect("fixture directory");
        let plugins = fixture.path().join("plugins");
        std::fs::create_dir(&plugins).expect("plugins directory");
        let error = JavaAdapter::from_values(
            Some("a.B".into()),
            Some("adapter-classes".into()),
            None,
            Some(fixture.path().join("missing-paper.jar").into_os_string()),
            Some(plugins.into_os_string()),
            None,
        ).expect_err("missing Paper jar must not start a worker");
        assert!(error.contains("invalid Paper bootstrap configuration"), "{error}");
        assert!(error.contains("Paper server jar"), "{error}");
        assert!(error.len() < 4096, "Paper configuration error must stay bounded");
    }
}
