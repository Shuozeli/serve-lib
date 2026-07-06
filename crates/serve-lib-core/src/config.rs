use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{BindTarget, DurationSpec, RenderConfig, ServeError};

pub const DEFAULT_PORT: u16 = 8088;
const SECS_PER_WEEK: u64 = 7 * 24 * 60 * 60;
const SECS_PER_HOUR: u64 = 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct LocalConfig {
    pub defaults: DefaultConfig,
    pub event_log: EventLogConfig,
    pub render: RenderConfigToml,
    pub profiles: Vec<ProfileConfig>,
}

impl LocalConfig {
    pub fn from_toml_str(input: &str) -> Result<Self, ServeError> {
        toml::from_str(input).map_err(|err| ServeError::InvalidConfig(err.to_string()))
    }

    pub fn validate(&self) -> Result<(), ServeError> {
        self.defaults.validate()?;
        self.event_log.validate()?;

        let mut names = std::collections::BTreeSet::new();
        for profile in &self.profiles {
            profile.validate()?;
            if !names.insert(profile.name.as_str()) {
                return Err(ServeError::InvalidConfig(format!(
                    "duplicate profile name: {}",
                    profile.name
                )));
            }
        }

        Ok(())
    }

    pub fn profile(&self, name: &str) -> Result<&ProfileConfig, ServeError> {
        self.profiles
            .iter()
            .find(|profile| profile.name == name)
            .ok_or_else(|| ServeError::InvalidConfig(format!("unknown profile: {name}")))
    }

    pub fn effective_register_defaults(
        &self,
        profile_name: Option<&str>,
        overrides: RegisterOverride,
    ) -> Result<EffectiveRegisterDefaults, ServeError> {
        self.validate()?;
        let profile = profile_name.map(|name| self.profile(name)).transpose()?;

        let bind = overrides
            .bind
            .or_else(|| profile.and_then(|profile| profile.bind.clone()))
            .or_else(|| self.defaults.bind.clone())
            .unwrap_or(BindTarget::Loopback);

        let port = overrides
            .port
            .or_else(|| profile.and_then(|profile| profile.port))
            .or(self.defaults.port)
            .unwrap_or(DEFAULT_PORT);

        let timeout = overrides
            .timeout
            .or_else(|| profile.and_then(|profile| profile.timeout))
            .or(self.defaults.timeout);

        let index_file = overrides
            .index_file
            .or_else(|| profile.and_then(|profile| profile.index.clone()))
            .or_else(|| self.defaults.index.clone())
            .unwrap_or_else(|| "index.html".to_string());

        validate_index_file_name(&index_file)?;

        let spa = overrides
            .spa
            .or_else(|| profile.and_then(|profile| profile.spa))
            .or(self.defaults.spa)
            .unwrap_or(false);

        let render = RenderConfig {
            markdown: overrides
                .render_markdown
                .or_else(|| profile.and_then(|profile| profile.render.markdown))
                .or(self.render.markdown)
                .unwrap_or(false),
            code_highlight: overrides
                .render_code_highlight
                .or_else(|| profile.and_then(|profile| profile.render.code_highlight))
                .or(self.render.code_highlight)
                .unwrap_or(false),
        };

        Ok(EffectiveRegisterDefaults {
            bind,
            port,
            timeout,
            index_file,
            spa,
            render,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct DefaultConfig {
    pub bind: Option<BindTarget>,
    pub port: Option<u16>,
    pub timeout: Option<DurationSpec>,
    pub index: Option<String>,
    pub spa: Option<bool>,
}

impl DefaultConfig {
    fn validate(&self) -> Result<(), ServeError> {
        if let Some(port) = self.port {
            validate_port(port)?;
        }
        if let Some(index) = &self.index {
            validate_index_file_name(index)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EventLogConfig {
    pub database_path: EventLogDatabasePath,
    pub retention: DurationSpec,
    pub cleanup_interval: DurationSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct RenderConfigToml {
    pub markdown: Option<bool>,
    pub code_highlight: Option<bool>,
}

impl Default for EventLogConfig {
    fn default() -> Self {
        Self {
            database_path: EventLogDatabasePath::Default,
            retention: DurationSpec::from_seconds(SECS_PER_WEEK).expect("valid retention"),
            cleanup_interval: DurationSpec::from_seconds(SECS_PER_HOUR)
                .expect("valid cleanup interval"),
        }
    }
}

impl EventLogConfig {
    fn validate(&self) -> Result<(), ServeError> {
        match &self.database_path {
            EventLogDatabasePath::Default => {}
            EventLogDatabasePath::Path(path) if path.is_absolute() => {}
            EventLogDatabasePath::Path(path) => {
                return Err(ServeError::InvalidConfig(format!(
                    "event log database path must be absolute: {}",
                    path.display()
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum EventLogDatabasePath {
    #[default]
    Default,
    Path(PathBuf),
}

impl Serialize for EventLogDatabasePath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            EventLogDatabasePath::Default => serializer.serialize_str("default"),
            EventLogDatabasePath::Path(path) => serializer.serialize_str(&path.to_string_lossy()),
        }
    }
}

impl<'de> Deserialize<'de> for EventLogDatabasePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value == "default" {
            Ok(EventLogDatabasePath::Default)
        } else {
            Ok(EventLogDatabasePath::Path(PathBuf::from(value)))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ProfileConfig {
    pub name: String,
    pub bind: Option<BindTarget>,
    pub port: Option<u16>,
    pub timeout: Option<DurationSpec>,
    pub index: Option<String>,
    pub spa: Option<bool>,
    pub render: RenderConfigToml,
}

impl ProfileConfig {
    fn validate(&self) -> Result<(), ServeError> {
        if self.name.trim().is_empty() {
            return Err(ServeError::InvalidConfig(
                "profile name cannot be empty".to_string(),
            ));
        }
        if let Some(port) = self.port {
            validate_port(port)?;
        }
        if let Some(index) = &self.index {
            validate_index_file_name(index)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RegisterOverride {
    pub bind: Option<BindTarget>,
    pub port: Option<u16>,
    pub timeout: Option<DurationSpec>,
    pub index_file: Option<String>,
    pub spa: Option<bool>,
    pub render_markdown: Option<bool>,
    pub render_code_highlight: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveRegisterDefaults {
    pub bind: BindTarget,
    pub port: u16,
    pub timeout: Option<DurationSpec>,
    pub index_file: String,
    pub spa: bool,
    pub render: RenderConfig,
}

fn validate_port(port: u16) -> Result<(), ServeError> {
    if port == 0 {
        return Err(ServeError::InvalidConfig(
            "port must be in 1..=65535".to_string(),
        ));
    }
    Ok(())
}

fn validate_index_file_name(value: &str) -> Result<(), ServeError> {
    if value.trim().is_empty() {
        return Err(ServeError::InvalidConfig(
            "index file name cannot be empty".to_string(),
        ));
    }
    if value.contains('/') || value.contains('\\') || value == "." || value == ".." {
        return Err(ServeError::InvalidConfig(format!(
            "index file name must not be a path: {value}"
        )));
    }
    Ok(())
}

impl FromStr for LocalConfig {
    type Err = ServeError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        LocalConfig::from_toml_str(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_event_log_defaults() {
        let config = LocalConfig::from_toml_str(
            r#"
            [defaults]
            bind = "tailscale"
            port = 8088

            [event_log]
            retention = "3d"
            cleanup_interval = "30m"
            "#,
        )
        .unwrap();

        config.validate().unwrap();
        assert_eq!(
            config.event_log.retention,
            DurationSpec::from_seconds(3 * 24 * 60 * 60).unwrap()
        );
    }

    #[test]
    fn merges_defaults_profile_and_overrides() {
        let config = LocalConfig::from_toml_str(
            r#"
            [defaults]
            bind = "loopback"
            port = 8088
            timeout = "2h"
            index = "index.html"
            spa = false

            [render]
            markdown = true
            code_highlight = false

            [[profiles]]
            name = "tailscale"
            bind = "tailscale"
            port = 9090
            spa = true
            render = { code_highlight = true }
            "#,
        )
        .unwrap();

        let effective = config
            .effective_register_defaults(
                Some("tailscale"),
                RegisterOverride {
                    port: Some(7070),
                    ..RegisterOverride::default()
                },
            )
            .unwrap();

        assert_eq!(effective.bind, BindTarget::Tailscale);
        assert_eq!(effective.port, 7070);
        assert_eq!(effective.timeout.unwrap().as_seconds(), 7200);
        assert_eq!(effective.index_file, "index.html");
        assert!(effective.spa);
        assert!(effective.render.markdown);
        assert!(effective.render.code_highlight);
    }

    #[test]
    fn rejects_duplicate_profiles() {
        let config = LocalConfig::from_toml_str(
            r#"
            [[profiles]]
            name = "same"

            [[profiles]]
            name = "same"
            "#,
        )
        .unwrap();

        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_relative_event_log_database_path() {
        let config = LocalConfig::from_toml_str(
            r#"
            [event_log]
            database_path = "relative/events.sqlite"
            "#,
        )
        .unwrap();

        assert!(config.validate().is_err());
    }
}
