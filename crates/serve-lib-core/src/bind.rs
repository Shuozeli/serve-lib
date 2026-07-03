use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::process::Command;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ServeError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindTarget {
    Tailscale,
    Private,
    Any,
    Loopback,
    Ip(IpAddr),
    InterfaceName(String),
}

impl Serialize for BindTarget {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            BindTarget::Tailscale => serializer.serialize_str("tailscale"),
            BindTarget::Private => serializer.serialize_str("private"),
            BindTarget::Any => serializer.serialize_str("0.0.0.0"),
            BindTarget::Loopback => serializer.serialize_str("loopback"),
            BindTarget::Ip(ip) => serializer.serialize_str(&ip.to_string()),
            BindTarget::InterfaceName(name) => serializer.serialize_str(name),
        }
    }
}

impl<'de> Deserialize<'de> for BindTarget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

impl FromStr for BindTarget {
    type Err = ServeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        if value.is_empty() {
            return Err(ServeError::InvalidBindTarget(
                "bind target cannot be empty".to_string(),
            ));
        }

        match value {
            "tailscale" => Ok(BindTarget::Tailscale),
            "private" => Ok(BindTarget::Private),
            "any" | "0.0.0.0" | "::" => Ok(BindTarget::Any),
            "loopback" | "localhost" | "127.0.0.1" | "::1" => Ok(BindTarget::Loopback),
            other => match other.parse::<IpAddr>() {
                Ok(ip) => Ok(BindTarget::Ip(ip)),
                Err(_) if is_valid_interface_name(other) => {
                    Ok(BindTarget::InterfaceName(other.to_string()))
                }
                Err(_) => Err(ServeError::InvalidBindTarget(other.to_string())),
            },
        }
    }
}

fn is_valid_interface_name(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindSource {
    ExplicitIp,
    LogicalTarget,
    Interface,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBind {
    pub target: BindTarget,
    pub bind_addr: IpAddr,
    pub display_host: Option<String>,
    pub source: BindSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub stdout: String,
}

pub trait CommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, ServeError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, ServeError> {
        let output = Command::new(program).args(args).output().map_err(|err| {
            ServeError::BindResolutionFailed(format!("failed to run {program}: {err}"))
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(ServeError::BindResolutionFailed(format!(
                "{program} exited with status {}{}",
                output.status,
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                }
            )));
        }

        Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct BindResolverConfig {
    pub private_candidates: Vec<IpAddr>,
    pub interface_addrs: BTreeMap<String, IpAddr>,
}

impl BindResolverConfig {
    pub fn with_private_candidates(mut self, private_candidates: Vec<IpAddr>) -> Self {
        self.private_candidates = private_candidates;
        self
    }

    pub fn with_interface_addr(mut self, interface_name: impl Into<String>, addr: IpAddr) -> Self {
        self.interface_addrs.insert(interface_name.into(), addr);
        self
    }
}

#[derive(Debug, Clone)]
pub struct BindResolver<R> {
    command_runner: R,
    config: BindResolverConfig,
}

impl<R> BindResolver<R>
where
    R: CommandRunner,
{
    pub fn new(command_runner: R) -> Self {
        Self {
            command_runner,
            config: BindResolverConfig::default(),
        }
    }

    pub fn with_config(command_runner: R, config: BindResolverConfig) -> Self {
        Self {
            command_runner,
            config,
        }
    }

    pub fn resolve(&self, target: &BindTarget) -> Result<ResolvedBind, ServeError> {
        match target {
            BindTarget::Tailscale => self.resolve_tailscale(),
            BindTarget::Private => self.resolve_private(),
            BindTarget::Any => Ok(resolved(
                target.clone(),
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                Some(Ipv4Addr::UNSPECIFIED.to_string()),
                BindSource::LogicalTarget,
            )),
            BindTarget::Loopback => Ok(resolved(
                target.clone(),
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                Some(Ipv4Addr::LOCALHOST.to_string()),
                BindSource::LogicalTarget,
            )),
            BindTarget::Ip(ip) => Ok(resolved(
                target.clone(),
                *ip,
                Some(ip.to_string()),
                BindSource::ExplicitIp,
            )),
            BindTarget::InterfaceName(interface_name) => self.resolve_interface(interface_name),
        }
    }

    fn resolve_tailscale(&self) -> Result<ResolvedBind, ServeError> {
        let ip_output = self.command_runner.run("tailscale", &["ip", "-4"])?;
        let bind_addr = parse_first_ip(&ip_output.stdout).ok_or_else(|| {
            ServeError::BindResolutionFailed("tailscale ip -4 did not return an IP".to_string())
        })?;

        let display_host = match self.command_runner.run("tailscale", &["status", "--json"]) {
            Ok(status_output) => tailscale_dns_name(&status_output.stdout)
                .filter(|host| !host.is_empty())
                .or_else(|| Some(bind_addr.to_string())),
            Err(_) => Some(bind_addr.to_string()),
        };

        Ok(resolved(
            BindTarget::Tailscale,
            bind_addr,
            display_host,
            BindSource::LogicalTarget,
        ))
    }

    fn resolve_private(&self) -> Result<ResolvedBind, ServeError> {
        let candidates = self
            .config
            .private_candidates
            .iter()
            .copied()
            .filter(is_private_ipv4)
            .collect::<Vec<_>>();

        match candidates.as_slice() {
            [ip] => Ok(resolved(
                BindTarget::Private,
                *ip,
                Some(ip.to_string()),
                BindSource::LogicalTarget,
            )),
            [] => Err(ServeError::BindResolutionFailed(
                "no RFC1918 private IPv4 address found".to_string(),
            )),
            _ => Err(ServeError::BindResolutionFailed(format!(
                "multiple private IPv4 addresses found: {}",
                candidates
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    fn resolve_interface(&self, interface_name: &str) -> Result<ResolvedBind, ServeError> {
        let bind_addr = self
            .config
            .interface_addrs
            .get(interface_name)
            .copied()
            .ok_or_else(|| {
                ServeError::BindResolutionFailed(format!(
                    "interface address not configured for {interface_name}"
                ))
            })?;

        Ok(resolved(
            BindTarget::InterfaceName(interface_name.to_string()),
            bind_addr,
            Some(bind_addr.to_string()),
            BindSource::Interface,
        ))
    }
}

impl BindResolver<SystemCommandRunner> {
    pub fn system() -> Self {
        Self::new(SystemCommandRunner)
    }
}

fn resolved(
    target: BindTarget,
    bind_addr: IpAddr,
    display_host: Option<String>,
    source: BindSource,
) -> ResolvedBind {
    ResolvedBind {
        target,
        bind_addr,
        display_host,
        source,
    }
}

fn parse_first_ip(value: &str) -> Option<IpAddr> {
    value
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .and_then(|line| line.parse().ok())
}

fn tailscale_dns_name(value: &str) -> Option<String> {
    serde_json::from_str::<Value>(value)
        .ok()
        .and_then(|json| json.pointer("/Self/DNSName")?.as_str().map(str::to_string))
        .map(|host| host.trim_end_matches('.').to_string())
}

fn is_private_ipv4(ip: &IpAddr) -> bool {
    matches!(ip, IpAddr::V4(ip) if ip.is_private())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Debug, Default)]
    struct FakeCommandRunner {
        calls: RefCell<Vec<(String, Vec<String>)>>,
        outputs: BTreeMap<String, Result<CommandOutput, ServeError>>,
    }

    impl FakeCommandRunner {
        fn with_output(mut self, program: &str, args: &[&str], stdout: &str) -> Self {
            self.outputs.insert(
                command_key(program, args),
                Ok(CommandOutput {
                    stdout: stdout.to_string(),
                }),
            );
            self
        }

        fn with_error(mut self, program: &str, args: &[&str], error: ServeError) -> Self {
            self.outputs.insert(command_key(program, args), Err(error));
            self
        }

        fn calls(&self) -> Vec<(String, Vec<String>)> {
            self.calls.borrow().clone()
        }
    }

    impl CommandRunner for FakeCommandRunner {
        fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, ServeError> {
            self.calls.borrow_mut().push((
                program.to_string(),
                args.iter().map(ToString::to_string).collect(),
            ));

            self.outputs
                .get(&command_key(program, args))
                .cloned()
                .unwrap_or_else(|| {
                    Err(ServeError::BindResolutionFailed(format!(
                        "no fake output for {program} {}",
                        args.join(" ")
                    )))
                })
        }
    }

    fn command_key(program: &str, args: &[&str]) -> String {
        format!("{program} {}", args.join(" "))
    }

    #[test]
    fn parses_logical_targets() {
        assert_eq!("tailscale".parse(), Ok(BindTarget::Tailscale));
        assert_eq!("private".parse(), Ok(BindTarget::Private));
        assert_eq!("0.0.0.0".parse(), Ok(BindTarget::Any));
        assert_eq!("localhost".parse(), Ok(BindTarget::Loopback));
    }

    #[test]
    fn parses_ip_and_interface_targets() {
        assert_eq!(
            "192.168.1.24".parse(),
            Ok(BindTarget::Ip("192.168.1.24".parse().unwrap()))
        );
        assert_eq!(
            "en0".parse(),
            Ok(BindTarget::InterfaceName("en0".to_string()))
        );
    }

    #[test]
    fn rejects_empty_target() {
        assert!("".parse::<BindTarget>().is_err());
    }

    #[test]
    fn resolves_explicit_ip_without_commands() {
        // Arrange
        let runner = FakeCommandRunner::default();
        let resolver = BindResolver::new(runner);
        let target = BindTarget::Ip("192.168.1.24".parse().unwrap());

        // Act
        let bind = resolver.resolve(&target).unwrap();

        // Assert
        assert_eq!(bind.bind_addr, "192.168.1.24".parse::<IpAddr>().unwrap());
        assert_eq!(bind.display_host.as_deref(), Some("192.168.1.24"));
        assert_eq!(bind.source, BindSource::ExplicitIp);
        assert!(resolver.command_runner.calls().is_empty());
    }

    #[test]
    fn resolves_loopback_and_any_without_commands() {
        // Arrange
        let runner = FakeCommandRunner::default();
        let resolver = BindResolver::new(runner);

        // Act
        let loopback = resolver.resolve(&BindTarget::Loopback).unwrap();
        let any = resolver.resolve(&BindTarget::Any).unwrap();

        // Assert
        assert_eq!(loopback.bind_addr, "127.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(any.bind_addr, "0.0.0.0".parse::<IpAddr>().unwrap());
        assert!(resolver.command_runner.calls().is_empty());
    }

    #[test]
    fn resolves_tailscale_ip_and_magic_dns() {
        // Arrange
        let runner = FakeCommandRunner::default()
            .with_output("tailscale", &["ip", "-4"], "100.64.0.10\n")
            .with_output(
                "tailscale",
                &["status", "--json"],
                r#"{"Self":{"DNSName":"machine.tailnet.example.ts.net."}}"#,
            );
        let resolver = BindResolver::new(runner);

        // Act
        let bind = resolver.resolve(&BindTarget::Tailscale).unwrap();

        // Assert
        assert_eq!(bind.bind_addr, "100.64.0.10".parse::<IpAddr>().unwrap());
        assert_eq!(
            bind.display_host.as_deref(),
            Some("machine.tailnet.example.ts.net")
        );
        assert_eq!(bind.source, BindSource::LogicalTarget);
        assert_eq!(
            resolver.command_runner.calls(),
            vec![
                (
                    "tailscale".to_string(),
                    vec!["ip".to_string(), "-4".to_string()]
                ),
                (
                    "tailscale".to_string(),
                    vec!["status".to_string(), "--json".to_string()]
                )
            ]
        );
    }

    #[test]
    fn tailscale_status_failure_falls_back_to_ip_display() {
        // Arrange
        let runner = FakeCommandRunner::default()
            .with_output("tailscale", &["ip", "-4"], "100.64.0.10\n")
            .with_error(
                "tailscale",
                &["status", "--json"],
                ServeError::BindResolutionFailed("status unavailable".to_string()),
            );
        let resolver = BindResolver::new(runner);

        // Act
        let bind = resolver.resolve(&BindTarget::Tailscale).unwrap();

        // Assert
        assert_eq!(bind.display_host.as_deref(), Some("100.64.0.10"));
    }

    #[test]
    fn tailscale_ip_failure_returns_bind_resolution_error() {
        // Arrange
        let runner = FakeCommandRunner::default().with_error(
            "tailscale",
            &["ip", "-4"],
            ServeError::BindResolutionFailed("tailscale unavailable".to_string()),
        );
        let resolver = BindResolver::new(runner);

        // Act
        let error = resolver.resolve(&BindTarget::Tailscale).unwrap_err();

        // Assert
        assert_eq!(error.code(), crate::ErrorCode::BindResolutionFailed);
    }

    #[test]
    fn resolves_single_private_candidate() {
        // Arrange
        let config = BindResolverConfig::default().with_private_candidates(vec![
            "127.0.0.1".parse().unwrap(),
            "172.16.10.4".parse().unwrap(),
        ]);
        let resolver = BindResolver::with_config(FakeCommandRunner::default(), config);

        // Act
        let bind = resolver.resolve(&BindTarget::Private).unwrap();

        // Assert
        assert_eq!(bind.bind_addr, "172.16.10.4".parse::<IpAddr>().unwrap());
        assert_eq!(bind.display_host.as_deref(), Some("172.16.10.4"));
        assert!(resolver.command_runner.calls().is_empty());
    }

    #[test]
    fn rejects_missing_private_candidate() {
        // Arrange
        let resolver = BindResolver::new(FakeCommandRunner::default());

        // Act
        let error = resolver.resolve(&BindTarget::Private).unwrap_err();

        // Assert
        assert_eq!(error.code(), crate::ErrorCode::BindResolutionFailed);
    }

    #[test]
    fn rejects_ambiguous_private_candidates() {
        // Arrange
        let config = BindResolverConfig::default().with_private_candidates(vec![
            "10.0.0.4".parse().unwrap(),
            "192.168.1.20".parse().unwrap(),
        ]);
        let resolver = BindResolver::with_config(FakeCommandRunner::default(), config);

        // Act
        let error = resolver.resolve(&BindTarget::Private).unwrap_err();

        // Assert
        assert_eq!(error.code(), crate::ErrorCode::BindResolutionFailed);
        assert!(error.to_string().contains("multiple private IPv4"));
    }

    #[test]
    fn resolves_configured_interface_name() {
        // Arrange
        let config = BindResolverConfig::default()
            .with_interface_addr("en0", "192.168.1.20".parse().unwrap());
        let resolver = BindResolver::with_config(FakeCommandRunner::default(), config);

        // Act
        let bind = resolver
            .resolve(&BindTarget::InterfaceName("en0".to_string()))
            .unwrap();

        // Assert
        assert_eq!(bind.bind_addr, "192.168.1.20".parse::<IpAddr>().unwrap());
        assert_eq!(bind.source, BindSource::Interface);
    }

    #[test]
    fn rejects_missing_interface_name() {
        // Arrange
        let resolver = BindResolver::new(FakeCommandRunner::default());

        // Act
        let error = resolver
            .resolve(&BindTarget::InterfaceName("en0".to_string()))
            .unwrap_err();

        // Assert
        assert_eq!(error.code(), crate::ErrorCode::BindResolutionFailed);
    }
}
