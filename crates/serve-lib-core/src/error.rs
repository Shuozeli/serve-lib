use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidConfig,
    InvalidRequest,
    InvalidBindTarget,
    InvalidDuration,
    InvalidRoute,
    BindResolutionFailed,
    PortUnavailable,
    RouteConflict,
    PathNotFound,
    PathNotReadable,
    MountNotFound,
    EventLogUnavailable,
    DaemonUnavailable,
    Internal,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ServeError {
    #[error("invalid config: {0}")]
    InvalidConfig(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("invalid bind target: {0}")]
    InvalidBindTarget(String),

    #[error("invalid duration: {0}")]
    InvalidDuration(String),

    #[error("invalid route: {0}")]
    InvalidRoute(String),

    #[error("bind resolution failed: {0}")]
    BindResolutionFailed(String),

    #[error("route conflict: {0}")]
    RouteConflict(String),

    #[error("port unavailable: {0}")]
    PortUnavailable(String),

    #[error("path not found: {0}")]
    PathNotFound(String),

    #[error("path is not readable: {0}")]
    PathNotReadable(String),

    #[error("mount not found: {0}")]
    MountNotFound(String),

    #[error("event log unavailable: {0}")]
    EventLogUnavailable(String),

    #[error("daemon is unavailable: {0}")]
    DaemonUnavailable(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl ServeError {
    pub fn code(&self) -> ErrorCode {
        match self {
            ServeError::InvalidConfig(_) => ErrorCode::InvalidConfig,
            ServeError::InvalidRequest(_) => ErrorCode::InvalidRequest,
            ServeError::InvalidBindTarget(_) => ErrorCode::InvalidBindTarget,
            ServeError::InvalidDuration(_) => ErrorCode::InvalidDuration,
            ServeError::InvalidRoute(_) => ErrorCode::InvalidRoute,
            ServeError::BindResolutionFailed(_) => ErrorCode::BindResolutionFailed,
            ServeError::RouteConflict(_) => ErrorCode::RouteConflict,
            ServeError::PortUnavailable(_) => ErrorCode::PortUnavailable,
            ServeError::PathNotFound(_) => ErrorCode::PathNotFound,
            ServeError::PathNotReadable(_) => ErrorCode::PathNotReadable,
            ServeError::MountNotFound(_) => ErrorCode::MountNotFound,
            ServeError::EventLogUnavailable(_) => ErrorCode::EventLogUnavailable,
            ServeError::DaemonUnavailable(_) => ErrorCode::DaemonUnavailable,
            ServeError::Internal(_) => ErrorCode::Internal,
        }
    }
}
