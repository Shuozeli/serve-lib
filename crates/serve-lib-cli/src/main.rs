use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use clap::{ArgAction, Parser, Subcommand};
use serve_lib_core::{
    BindTarget, DeregisterRequest, DurationSpec, EventLogDatabasePath, LocalConfig,
    RegisterOverride, RegisterRequest, TlsMode, TlsPolicy,
};
use serve_lib_daemon::{
    run_control_server, ControlClient, ControlRequest, ControlResponse, DaemonRuntime,
    RuntimeOptions,
};

const DEFAULT_CONTROL_ADDR: &str = "127.0.0.1:7878";

#[derive(Debug, Parser)]
#[command(name = "serve-lib")]
#[command(about = "Daemon-backed local file serving for private networks")]
struct Cli {
    #[arg(long, default_value = DEFAULT_CONTROL_ADDR, global = true)]
    control: SocketAddr,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    Daemon {
        #[command(subcommand)]
        command: DaemonCommands,
    },
    Register {
        local_path: PathBuf,

        #[arg(long)]
        route: String,

        #[arg(long)]
        port: Option<u16>,

        #[arg(long)]
        bind: Option<String>,

        #[arg(long)]
        timeout: Option<String>,

        #[arg(long)]
        index: Option<String>,

        #[arg(long, action = ArgAction::SetTrue)]
        spa: bool,

        #[arg(long)]
        profile: Option<String>,

        #[arg(long)]
        name: Option<String>,

        #[arg(long, default_value = "off")]
        tls_mode: String,

        #[arg(long)]
        server_cert: Option<PathBuf>,

        #[arg(long)]
        server_key: Option<PathBuf>,

        #[arg(long)]
        client_ca: Option<PathBuf>,
    },
    Deregister {
        #[arg(long)]
        route: String,

        #[arg(long, default_value = "8088")]
        port: u16,

        #[arg(long)]
        bind: Option<String>,
    },
    List,
    Events,
}

#[derive(Debug, Subcommand)]
enum DaemonCommands {
    Run,
    Start,
    Stop,
    Status,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Daemon { command } => run_daemon_command(cli.control, command),
        Commands::Register {
            local_path,
            route,
            port,
            bind,
            timeout,
            index,
            spa,
            profile,
            name,
            tls_mode,
            server_cert,
            server_key,
            client_ca,
        } => {
            let config = load_config()?;
            let effective = config.effective_register_defaults(
                profile.as_deref(),
                RegisterOverride {
                    bind: bind.map(|value| value.parse::<BindTarget>()).transpose()?,
                    port,
                    timeout: timeout
                        .map(|value| value.parse::<DurationSpec>())
                        .transpose()?,
                    index_file: index,
                    spa: spa.then_some(true),
                    ..RegisterOverride::default()
                },
            )?;
            let request = RegisterRequest {
                local_path,
                route: route.parse()?,
                bind: effective.bind,
                port: effective.port,
                timeout: effective.timeout,
                index_file: effective.index_file,
                spa: effective.spa,
                render: effective.render,
                readonly: true,
                display_name: name,
            };
            let tls_policy = TlsPolicy {
                mode: parse_tls_mode(&tls_mode)?,
                server_cert,
                server_key,
                client_ca,
            };
            let client = ControlClient::new(cli.control);
            print_response(client.post(
                "/register",
                &ControlRequest::Register {
                    request,
                    tls_policy,
                },
            )?)
        }
        Commands::Deregister { route, port, bind } => {
            let request = DeregisterRequest {
                bind: bind.map(|value| value.parse::<BindTarget>()).transpose()?,
                port,
                route: route.parse()?,
            };
            let client = ControlClient::new(cli.control);
            print_response(client.post("/deregister", &ControlRequest::Deregister { request })?)
        }
        Commands::List => {
            let client = ControlClient::new(cli.control);
            print_response(client.get("/list")?)
        }
        Commands::Events => {
            let client = ControlClient::new(cli.control);
            print_response(client.get("/events")?)
        }
    }
}

fn run_daemon_command(
    control: SocketAddr,
    command: DaemonCommands,
) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        DaemonCommands::Run => {
            let runtime = Arc::new(DaemonRuntime::new(runtime_options_from_config()?)?);
            println!("serve-lib daemon listening on control API {control}");
            run_control_server(runtime, control)?;
            Ok(())
        }
        DaemonCommands::Start => {
            let exe = std::env::current_exe()?;
            Command::new(exe)
                .arg("--control")
                .arg(control.to_string())
                .arg("daemon")
                .arg("run")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            println!("started serve-lib daemon on {control}");
            Ok(())
        }
        DaemonCommands::Stop => {
            let client = ControlClient::new(control);
            print_response(client.post("/shutdown", &ControlRequest::Shutdown)?)
        }
        DaemonCommands::Status => {
            let client = ControlClient::new(control);
            print_response(client.get("/status")?)
        }
    }
}

fn load_config() -> Result<LocalConfig, Box<dyn std::error::Error>> {
    let Some(path) = config_path() else {
        return Ok(LocalConfig::default());
    };
    if !path.exists() {
        return Ok(LocalConfig::default());
    }
    let input = std::fs::read_to_string(&path)?;
    let config = LocalConfig::from_toml_str(&input)?;
    config.validate()?;
    Ok(config)
}

fn runtime_options_from_config() -> Result<RuntimeOptions, Box<dyn std::error::Error>> {
    let config = load_config()?;
    let mut options = RuntimeOptions {
        cleanup_retention: config.event_log.retention.as_duration(),
        cleanup_interval: config.event_log.cleanup_interval.as_duration(),
        ..RuntimeOptions::default()
    };
    options.event_log_path = match config.event_log.database_path {
        EventLogDatabasePath::Default => default_event_log_path(),
        EventLogDatabasePath::Path(path) => Some(path),
    };
    if let Some(path) = &options.event_log_path {
        ensure_parent_dir(path)?;
    }
    Ok(options)
}

fn config_path() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("SERVE_LIB_CONFIG") {
        return Some(PathBuf::from(value));
    }
    default_config_path()
}

fn default_config_path() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("serve-lib")
                .join("config.toml")
        })
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .map(|dir| dir.join("serve-lib").join("config.toml"))
    }
}

fn default_event_log_path() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("serve-lib")
                .join("events.sqlite")
        })
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            })
            .map(|dir| dir.join("serve-lib").join("events.sqlite"))
    }
}

fn ensure_parent_dir(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn parse_tls_mode(value: &str) -> Result<TlsMode, Box<dyn std::error::Error>> {
    match value {
        "off" => Ok(TlsMode::Off),
        "tls" => Ok(TlsMode::Tls),
        "mtls" => Ok(TlsMode::Mtls),
        other => Err(format!("unknown TLS mode: {other}").into()),
    }
}

fn print_response(response: ControlResponse) -> Result<(), Box<dyn std::error::Error>> {
    match response {
        ControlResponse::Register { response } => {
            println!("registered {}", response.mount.route);
            if let Some(url) = response.display_url {
                println!("{url}");
            }
        }
        ControlResponse::Deregister { route } => {
            println!("deregistered {route}");
        }
        ControlResponse::List { mounts } => {
            if mounts.is_empty() {
                println!("no active mounts");
            } else {
                println!("ROUTE\tLOCAL PATH\tBIND\tPORT");
                for mount in mounts {
                    println!(
                        "{}\t{}\t{}\t{}",
                        mount.route,
                        mount.local_root.display(),
                        mount.bind_addr,
                        mount.port
                    );
                }
            }
        }
        ControlResponse::Status { status } => {
            println!("mounts: {}", status.mounts);
            println!("listeners: {}", status.listeners);
            println!("tls_runtime: {}", status.tls_runtime);
        }
        ControlResponse::Events { events } => {
            if events.is_empty() {
                println!("no events");
            } else {
                println!("ID\tKIND\tSTATUS\tROUTE\tREQUEST\tMESSAGE");
                for event in events {
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        event.id,
                        event.kind,
                        event
                            .status
                            .map(|status| status.to_string())
                            .unwrap_or_default(),
                        event.route.unwrap_or_default(),
                        event.request_path.unwrap_or_default(),
                        event.message.unwrap_or_default()
                    );
                }
            }
        }
        ControlResponse::Shutdown => println!("shutdown requested"),
        ControlResponse::Error { code, message } => {
            return Err(format!("{code}: {message}").into());
        }
    }
    Ok(())
}
