//! Operator-configured experimental Java adapter over the public server read API.

use std::time::Duration;

use lodestone_jvm_bridge::adapter::{AdapterEvent, AdapterHost};
use lodestone_jvm_bridge::runtime::JvmConfig;
use lodestone_server::IntegratedServer;

#[derive(Debug)]
pub(crate) struct JavaAdapter {
    host: AdapterHost,
    last_dispatched: Option<u64>,
}

impl JavaAdapter {
    pub(crate) fn from_environment() -> Result<Option<Self>, String> {
        let class = std::env::var_os("LODESTONE_JAVA_ADAPTER_CLASS");
        let classpath = std::env::var_os("LODESTONE_JAVA_CLASSPATH");
        let deadline = std::env::var_os("LODESTONE_JAVA_DEADLINE_MS");
        Self::from_values(class, classpath, deadline)
    }

    fn from_values(
        class: Option<std::ffi::OsString>,
        classpath: Option<std::ffi::OsString>,
        deadline: Option<std::ffi::OsString>,
    ) -> Result<Option<Self>, String> {
        if class.is_none() && classpath.is_none() && deadline.is_none() {
            return Ok(None);
        }
        let class = class.and_then(|value| value.into_string().ok())
            .filter(|value| !value.is_empty())
            .ok_or("LODESTONE_JAVA_ADAPTER_CLASS must name an explicit adapter class")?;
        let classpath = classpath.filter(|value| !value.is_empty())
            .ok_or("LODESTONE_JAVA_CLASSPATH must name adapter jars or class directories")?;
        let paths: Vec<_> = std::env::split_paths(&classpath).collect();
        if paths.iter().any(|path| path.as_os_str().is_empty()) {
            return Err("LODESTONE_JAVA_CLASSPATH contains an empty entry".to_owned());
        }
        let millis = match deadline {
            Some(value) => value.to_str().and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .ok_or("LODESTONE_JAVA_DEADLINE_MS must be a positive integer")?,
            None => 5000,
        };
        let config = paths.into_iter().fold(JvmConfig::new(), |config, path| config.with_classpath(path));
        Self::start(config, &class, Duration::from_millis(millis)).map(Some)
    }

    pub(crate) fn start(config: JvmConfig, class: &str, deadline: Duration) -> Result<Self, String> {
        let host = AdapterHost::start(config, class, deadline)
            .map_err(|error| error.to_string())?;
        tracing::info!(adapter = class, "starting experimental Java adapter; Paper plugin loading is not enabled");
        Ok(Self { host, last_dispatched: None })
    }

    pub(crate) fn poll(&mut self, server: &IntegratedServer) -> Result<Option<u64>, String> {
        let event = self.host.poll().map_err(|error| error.to_string())?;
        match event {
            Some(AdapterEvent::Ready) => tracing::info!("experimental Java adapter ready"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_java_config_does_not_start_a_worker() {
        assert!(JavaAdapter::from_values(None, None, None).unwrap().is_none());
    }

    #[test]
    fn partial_and_invalid_java_config_fail_before_startup() {
        assert!(JavaAdapter::from_values(Some("a.B".into()), None, None).is_err());
        assert!(JavaAdapter::from_values(None, Some("classes".into()), None).is_err());
        for value in ["0", "-1", "bad"] {
            assert!(JavaAdapter::from_values(Some("a.B".into()), Some("classes".into()),
                Some(value.into())).is_err());
        }
    }
}
