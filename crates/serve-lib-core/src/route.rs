use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::ServeError;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NormalizedRoute(String);

impl NormalizedRoute {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0 == "/"
    }

    pub fn matches_request_path(&self, request_path: &str) -> Option<RouteMatch> {
        let request = normalize_request_path(request_path).ok()?;
        if self.is_root() {
            return Some(RouteMatch {
                route: self.clone(),
                relative_path: request.trim_start_matches('/').to_string(),
            });
        }

        if request == self.0 {
            return Some(RouteMatch {
                route: self.clone(),
                relative_path: String::new(),
            });
        }

        let prefix = format!("{}/", self.0);
        request.strip_prefix(&prefix).map(|relative| RouteMatch {
            route: self.clone(),
            relative_path: relative.to_string(),
        })
    }
}

impl fmt::Display for NormalizedRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for NormalizedRoute {
    type Err = ServeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        normalize_route(value).map(NormalizedRoute)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteMatch {
    pub route: NormalizedRoute,
    pub relative_path: String,
}

fn normalize_route(value: &str) -> Result<String, ServeError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ServeError::InvalidRoute(
            "route cannot be empty".to_string(),
        ));
    }
    if !value.starts_with('/') {
        return Err(ServeError::InvalidRoute(format!(
            "route must start with '/': {value}"
        )));
    }
    normalize_path_like(value, false)
}

fn normalize_request_path(value: &str) -> Result<String, ServeError> {
    let path = value.split('?').next().unwrap_or(value);
    if !path.starts_with('/') {
        return Err(ServeError::InvalidRoute(format!(
            "request path must start with '/': {value}"
        )));
    }
    normalize_path_like(path, true)
}

fn normalize_path_like(value: &str, allow_percent: bool) -> Result<String, ServeError> {
    if value.as_bytes().contains(&0) {
        return Err(ServeError::InvalidRoute(
            "route contains null byte".to_string(),
        ));
    }

    let mut parts = Vec::new();
    for segment in value.split('/') {
        if segment.is_empty() {
            continue;
        }
        if segment == "." || segment == ".." {
            return Err(ServeError::InvalidRoute(format!(
                "route contains invalid segment: {segment}"
            )));
        }
        if !allow_percent && segment.contains('%') {
            return Err(ServeError::InvalidRoute(
                "route definitions must not contain percent encoding".to_string(),
            ));
        }
        parts.push(segment);
    }

    if parts.is_empty() {
        return Ok("/".to_string());
    }

    Ok(format!("/{}", parts.join("/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_routes() {
        assert_eq!("/".parse::<NormalizedRoute>().unwrap().as_str(), "/");
        assert_eq!(
            "/app//assets/".parse::<NormalizedRoute>().unwrap().as_str(),
            "/app/assets"
        );
    }

    #[test]
    fn rejects_invalid_routes() {
        assert!("app".parse::<NormalizedRoute>().is_err());
        assert!("/../app".parse::<NormalizedRoute>().is_err());
        assert!("/app/%2e%2e".parse::<NormalizedRoute>().is_err());
    }

    #[test]
    fn matches_route_boundaries() {
        let route = "/app".parse::<NormalizedRoute>().unwrap();
        assert_eq!(
            route.matches_request_path("/app").unwrap().relative_path,
            ""
        );
        assert_eq!(
            route
                .matches_request_path("/app/assets/main.js")
                .unwrap()
                .relative_path,
            "assets/main.js"
        );
        assert!(route.matches_request_path("/application").is_none());
    }
}
