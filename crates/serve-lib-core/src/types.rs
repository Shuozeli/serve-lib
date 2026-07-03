use std::fmt;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{BindTarget, DurationSpec, NormalizedRoute};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MountId(Uuid);

impl MountId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for MountId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for MountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ListenerKey {
    pub bind_addr: IpAddr,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteMount {
    pub id: MountId,
    pub listener: ListenerKey,
    pub route: NormalizedRoute,
    pub local_root: PathBuf,
    pub index_file: String,
    pub spa: bool,
    pub render: RenderConfig,
    pub readonly: bool,
    pub expires_at: Option<SystemTime>,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub local_path: PathBuf,
    pub route: NormalizedRoute,
    pub bind: BindTarget,
    pub port: u16,
    pub timeout: Option<DurationSpec>,
    pub index_file: String,
    pub spa: bool,
    pub render: RenderConfig,
    pub readonly: bool,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterResponse {
    pub mount: RouteMount,
    pub display_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeregisterRequest {
    pub bind: Option<BindTarget>,
    pub port: u16,
    pub route: NormalizedRoute,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeregisterResponse {
    pub removed: RouteMount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RenderConfig {
    pub markdown: bool,
    pub code_highlight: bool,
}
