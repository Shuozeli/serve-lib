use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ServeError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    #[default]
    Off,
    Tls,
    Mtls,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TlsPolicy {
    pub mode: TlsMode,
    pub server_cert: Option<PathBuf>,
    pub server_key: Option<PathBuf>,
    pub client_ca: Option<PathBuf>,
}

impl Default for TlsPolicy {
    fn default() -> Self {
        Self {
            mode: TlsMode::Off,
            server_cert: None,
            server_key: None,
            client_ca: None,
        }
    }
}

impl TlsPolicy {
    pub fn off() -> Self {
        Self::default()
    }

    pub fn validate(&self) -> Result<(), ServeError> {
        match self.mode {
            TlsMode::Off => Ok(()),
            TlsMode::Tls => {
                require_absolute("server_cert", self.server_cert.as_ref())?;
                require_absolute("server_key", self.server_key.as_ref())
            }
            TlsMode::Mtls => {
                require_absolute("server_cert", self.server_cert.as_ref())?;
                require_absolute("server_key", self.server_key.as_ref())?;
                require_absolute("client_ca", self.client_ca.as_ref())
            }
        }
    }

    pub fn scheme(&self) -> &'static str {
        match self.mode {
            TlsMode::Off => "http",
            TlsMode::Tls | TlsMode::Mtls => "https",
        }
    }

    pub fn is_runtime_supported(&self) -> bool {
        true
    }
}

fn require_absolute(name: &str, path: Option<&PathBuf>) -> Result<(), ServeError> {
    let path =
        path.ok_or_else(|| ServeError::InvalidConfig(format!("{name} is required for TLS")))?;
    if !path.is_absolute() {
        return Err(ServeError::InvalidConfig(format!(
            "{name} must be an absolute path: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_off_policy_without_paths() {
        // Arrange
        let policy = TlsPolicy::off();

        // Act
        let result = policy.validate();

        // Assert
        assert!(result.is_ok());
        assert!(policy.is_runtime_supported());
    }

    #[test]
    fn mtls_requires_absolute_paths() {
        // Arrange
        let policy = TlsPolicy {
            mode: TlsMode::Mtls,
            server_cert: Some("/tmp/server.crt".into()),
            server_key: Some("/tmp/server.key".into()),
            client_ca: Some("client-ca.crt".into()),
        };

        // Act
        let error = policy.validate().unwrap_err();

        // Assert
        assert_eq!(error.code(), crate::ErrorCode::InvalidConfig);
    }
}
