use std::collections::BTreeMap;
use std::fs;
use std::io::BufReader;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig, ServerConnection, StreamOwned};
use serde::{Deserialize, Serialize};
use serve_lib_core::{
    BindResolver, DeregisterRequest, DeregisterResponse, DirectoryEntryKind, EventKind,
    EventLogStore, EventQuery, EventRow, ListenerKey, MountId, RegisterRequest, RegisterResponse,
    Registry, RenderMode, RouteMount, ServeError, ServeEvent, ServeOutcome, StaticFileService,
    SystemCommandRunner, TlsPolicy,
};

#[derive(Debug, Clone)]
pub struct RuntimeOptions {
    pub event_log_path: Option<PathBuf>,
    pub cleanup_retention: Duration,
    pub cleanup_interval: Duration,
    pub timeout_tick: Duration,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            event_log_path: None,
            cleanup_retention: Duration::from_secs(7 * 24 * 60 * 60),
            cleanup_interval: Duration::from_secs(60 * 60),
            timeout_tick: Duration::from_millis(250),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub mounts: usize,
    pub listeners: usize,
    pub tls_runtime: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListMount {
    pub id: String,
    pub bind_addr: IpAddr,
    pub port: u16,
    pub route: String,
    pub local_root: PathBuf,
    pub display_name: Option<String>,
    pub expires_at: Option<SystemTime>,
}

#[derive(Debug)]
struct ListenerHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Debug, Default)]
struct RuntimeState {
    registry: Registry,
    listeners: BTreeMap<ListenerKey, ListenerHandle>,
    tls_policies: BTreeMap<ListenerKey, TlsPolicy>,
}

#[derive(Debug)]
pub struct DaemonRuntime {
    state: Arc<Mutex<RuntimeState>>,
    events: Arc<Mutex<EventLogStore>>,
    shutdown: Arc<AtomicBool>,
    timeout_thread: Mutex<Option<JoinHandle<()>>>,
    cleanup_thread: Mutex<Option<JoinHandle<()>>>,
    options: RuntimeOptions,
}

impl DaemonRuntime {
    pub fn new(options: RuntimeOptions) -> Result<Self, ServeError> {
        let event_store = match &options.event_log_path {
            Some(path) => EventLogStore::open(path)?,
            None => EventLogStore::open_in_memory()?,
        };

        let runtime = Self {
            state: Arc::new(Mutex::new(RuntimeState::default())),
            events: Arc::new(Mutex::new(event_store)),
            shutdown: Arc::new(AtomicBool::new(false)),
            timeout_thread: Mutex::new(None),
            cleanup_thread: Mutex::new(None),
            options,
        };
        runtime.append_event(ServeEvent::lifecycle(
            EventKind::DaemonStarted,
            "daemon runtime started",
        ));
        runtime.start_timeout_scheduler();
        runtime.start_cleanup_worker();
        Ok(runtime)
    }

    pub fn register(
        &self,
        request: RegisterRequest,
        tls_policy: TlsPolicy,
    ) -> Result<RegisterResponse, ServeError> {
        tls_policy.validate()?;

        let resolved = BindResolver::new(SystemCommandRunner).resolve(&request.bind)?;
        let listener = ListenerKey {
            bind_addr: resolved.bind_addr,
            port: request.port,
        };
        let local_root = request
            .local_path
            .canonicalize()
            .map_err(|_| ServeError::PathNotFound(request.local_path.display().to_string()))?;
        let expires_at = request
            .timeout
            .map(|timeout| SystemTime::now() + timeout.as_duration());
        let mount = RouteMount {
            id: MountId::new(),
            listener: listener.clone(),
            route: request.route.clone(),
            local_root,
            index_file: request.index_file,
            spa: request.spa,
            render: request.render,
            readonly: request.readonly,
            expires_at,
            display_name: request.display_name,
        };

        let mut state = self.lock_state()?;
        if let Some(existing) = state.tls_policies.get(&listener) {
            if existing != &tls_policy {
                return Err(ServeError::InvalidConfig(format!(
                    "listener {}:{} already has a different TLS policy",
                    listener.bind_addr, listener.port
                )));
            }
        }

        let started_listener = if state.listeners.contains_key(&listener) {
            false
        } else {
            let handle = start_listener(
                listener.clone(),
                tls_policy.clone(),
                Arc::clone(&self.state),
                Arc::clone(&self.events),
            )?;
            state.listeners.insert(listener.clone(), handle);
            state
                .tls_policies
                .insert(listener.clone(), tls_policy.clone());
            true
        };

        if let Err(error) = state.registry.insert(mount.clone()) {
            if started_listener {
                stop_listener(&mut state, &listener);
            }
            return Err(error);
        }
        drop(state);

        let mut event = ServeEvent::lifecycle(
            EventKind::RouteRegistered,
            format!(
                "registered {} on {}:{}",
                mount.route, listener.bind_addr, listener.port
            ),
        );
        event.listener = Some(listener.clone());
        event.mount_id = Some(mount.id);
        event.route = Some(mount.route.clone());
        self.append_event(event);

        let display_host = resolved
            .display_host
            .unwrap_or_else(|| listener.bind_addr.to_string());
        Ok(RegisterResponse {
            display_url: Some(format!(
                "{}://{}:{}{}",
                tls_policy.scheme(),
                display_host,
                listener.port,
                mount.route
            )),
            mount,
        })
    }

    pub fn deregister(&self, request: DeregisterRequest) -> Result<DeregisterResponse, ServeError> {
        let listener = self.resolve_deregister_listener(&request)?;
        let mut state = self.lock_state()?;
        let removed = state
            .registry
            .remove_by_listener_route(&listener, &request.route)?;
        if state.registry.is_listener_empty(&listener) {
            stop_listener(&mut state, &listener);
        }
        drop(state);

        let mut event = ServeEvent::lifecycle(
            EventKind::RouteDeregistered,
            format!("deregistered {}", removed.route),
        );
        event.listener = Some(listener);
        event.mount_id = Some(removed.id);
        event.route = Some(removed.route.clone());
        self.append_event(event);

        Ok(DeregisterResponse { removed })
    }

    pub fn list(&self) -> Result<Vec<ListMount>, ServeError> {
        let state = self.lock_state()?;
        Ok(state
            .registry
            .mounts()
            .map(|mount| ListMount {
                id: mount.id.to_string(),
                bind_addr: mount.listener.bind_addr,
                port: mount.listener.port,
                route: mount.route.to_string(),
                local_root: mount.local_root.clone(),
                display_name: mount.display_name.clone(),
                expires_at: mount.expires_at,
            })
            .collect())
    }

    pub fn status(&self) -> Result<DaemonStatus, ServeError> {
        let state = self.lock_state()?;
        Ok(DaemonStatus {
            mounts: state.registry.mounts().count(),
            listeners: state.listeners.len(),
            tls_runtime: "rustls".to_string(),
        })
    }

    pub fn events(&self, query: EventQuery) -> Result<Vec<EventRow>, ServeError> {
        self.events
            .lock()
            .map_err(|_| ServeError::EventLogUnavailable("event log lock poisoned".to_string()))?
            .query(&query)
    }

    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Ok(mut state) = self.state.lock() {
            let keys = state.listeners.keys().cloned().collect::<Vec<_>>();
            for listener in keys {
                stop_listener(&mut state, &listener);
            }
        }
        self.append_event(ServeEvent::lifecycle(
            EventKind::DaemonStopped,
            "daemon runtime stopped",
        ));
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    fn resolve_deregister_listener(
        &self,
        request: &DeregisterRequest,
    ) -> Result<ListenerKey, ServeError> {
        if let Some(bind) = &request.bind {
            let resolved = BindResolver::new(SystemCommandRunner).resolve(bind)?;
            return Ok(ListenerKey {
                bind_addr: resolved.bind_addr,
                port: request.port,
            });
        }

        let state = self.lock_state()?;
        let candidates = state
            .registry
            .listener_keys()
            .filter(|listener| listener.port == request.port)
            .filter(|listener| {
                state
                    .registry
                    .match_request(listener, request.route.as_str())
                    .is_some()
            })
            .cloned()
            .collect::<Vec<_>>();
        match candidates.as_slice() {
            [listener] => Ok(listener.clone()),
            [] => Err(ServeError::MountNotFound(format!(
                "port {} route {}",
                request.port, request.route
            ))),
            _ => Err(ServeError::InvalidRequest(
                "bind is required when multiple listeners match port and route".to_string(),
            )),
        }
    }

    fn start_timeout_scheduler(&self) {
        let state = Arc::clone(&self.state);
        let events = Arc::clone(&self.events);
        let shutdown = Arc::clone(&self.shutdown);
        let tick = self.options.timeout_tick;
        let thread = thread::spawn(move || {
            while !shutdown.load(Ordering::SeqCst) {
                thread::sleep(tick);
                expire_routes(&state, &events);
            }
        });
        *self.timeout_thread.lock().expect("timeout lock") = Some(thread);
    }

    fn start_cleanup_worker(&self) {
        let events = Arc::clone(&self.events);
        let shutdown = Arc::clone(&self.shutdown);
        let retention = self.options.cleanup_retention;
        let interval = self.options.cleanup_interval;
        let thread = thread::spawn(move || {
            while !shutdown.load(Ordering::SeqCst) {
                thread::sleep(interval);
                if let Ok(store) = events.lock() {
                    let _ = store.cleanup_older_than(SystemTime::now(), retention);
                }
            }
        });
        *self.cleanup_thread.lock().expect("cleanup lock") = Some(thread);
    }

    fn append_event(&self, event: ServeEvent) {
        if let Ok(store) = self.events.lock() {
            let _ = store.append(&event);
        }
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, RuntimeState>, ServeError> {
        self.state
            .lock()
            .map_err(|_| ServeError::Internal("runtime state lock poisoned".to_string()))
    }
}

impl Drop for DaemonRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn start_listener(
    listener: ListenerKey,
    tls_policy: TlsPolicy,
    state: Arc<Mutex<RuntimeState>>,
    events: Arc<Mutex<EventLogStore>>,
) -> Result<ListenerHandle, ServeError> {
    let socket = TcpListener::bind((listener.bind_addr, listener.port))
        .map_err(|err| ServeError::PortUnavailable(err.to_string()))?;
    socket
        .set_nonblocking(true)
        .map_err(|err| ServeError::PortUnavailable(err.to_string()))?;
    let tls_config = tls_server_config(&tls_policy)?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let listener_for_thread = listener.clone();
    let thread = thread::spawn(move || {
        append_event(
            &events,
            ServeEvent::lifecycle(
                EventKind::ListenerOpened,
                format!(
                    "listener opened {}:{}",
                    listener_for_thread.bind_addr, listener_for_thread.port
                ),
            ),
        );
        while !stop_thread.load(Ordering::SeqCst) {
            match socket.accept() {
                Ok((stream, remote_addr)) => {
                    let state = Arc::clone(&state);
                    let events = Arc::clone(&events);
                    let listener = listener_for_thread.clone();
                    let tls_config = tls_config.clone();
                    thread::spawn(move || {
                        handle_accepted_connection(
                            stream,
                            remote_addr,
                            listener,
                            tls_config,
                            state,
                            events,
                        )
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break,
            }
        }
        append_event(
            &events,
            ServeEvent::lifecycle(
                EventKind::ListenerClosed,
                format!(
                    "listener closed {}:{}",
                    listener_for_thread.bind_addr, listener_for_thread.port
                ),
            ),
        );
    });

    Ok(ListenerHandle {
        stop,
        thread: Some(thread),
    })
}

fn tls_server_config(policy: &TlsPolicy) -> Result<Option<Arc<ServerConfig>>, ServeError> {
    policy.validate()?;
    if policy.mode == serve_lib_core::TlsMode::Off {
        return Ok(None);
    }

    let cert_path = policy
        .server_cert
        .as_ref()
        .ok_or_else(|| ServeError::InvalidConfig("server_cert is required for TLS".to_string()))?;
    let key_path = policy
        .server_key
        .as_ref()
        .ok_or_else(|| ServeError::InvalidConfig("server_key is required for TLS".to_string()))?;
    let certs = load_certs(cert_path)?;
    let key = load_private_key(key_path)?;

    let builder = ServerConfig::builder();
    let config = if policy.mode == serve_lib_core::TlsMode::Mtls {
        let ca_path = policy.client_ca.as_ref().ok_or_else(|| {
            ServeError::InvalidConfig("client_ca is required for mTLS".to_string())
        })?;
        let roots = load_root_store(ca_path)?;
        let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .map_err(|err| ServeError::InvalidConfig(err.to_string()))?;
        builder
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, key)
            .map_err(|err| ServeError::InvalidConfig(err.to_string()))?
    } else {
        builder
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|err| ServeError::InvalidConfig(err.to_string()))?
    };

    Ok(Some(Arc::new(config)))
}

fn load_certs(path: &PathBuf) -> Result<Vec<CertificateDer<'static>>, ServeError> {
    let file = fs::File::open(path)
        .map_err(|err| ServeError::InvalidConfig(format!("failed to open cert: {err}")))?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| ServeError::InvalidConfig(format!("failed to parse cert: {err}")))
}

fn load_private_key(path: &PathBuf) -> Result<PrivateKeyDer<'static>, ServeError> {
    let file = fs::File::open(path)
        .map_err(|err| ServeError::InvalidConfig(format!("failed to open key: {err}")))?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .map_err(|err| ServeError::InvalidConfig(format!("failed to parse key: {err}")))?
        .ok_or_else(|| ServeError::InvalidConfig("no private key found".to_string()))
}

fn load_root_store(path: &PathBuf) -> Result<RootCertStore, ServeError> {
    let mut roots = RootCertStore::empty();
    let certs = load_certs(path)?;
    for cert in certs {
        roots
            .add(cert)
            .map_err(|err| ServeError::InvalidConfig(err.to_string()))?;
    }
    Ok(roots)
}

fn handle_accepted_connection(
    stream: TcpStream,
    remote_addr: SocketAddr,
    listener: ListenerKey,
    tls_config: Option<Arc<ServerConfig>>,
    state: Arc<Mutex<RuntimeState>>,
    events: Arc<Mutex<EventLogStore>>,
) {
    if let Some(tls_config) = tls_config {
        let Ok(connection) = ServerConnection::new(tls_config) else {
            return;
        };
        let stream = StreamOwned::new(connection, stream);
        handle_http_connection(stream, remote_addr, listener, state, events);
    } else {
        handle_http_connection(stream, remote_addr, listener, state, events);
    }
}

fn stop_listener(state: &mut RuntimeState, listener: &ListenerKey) {
    if let Some(mut handle) = state.listeners.remove(listener) {
        handle.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = handle.thread.take() {
            let _ = thread.join();
        }
    }
    state.tls_policies.remove(listener);
}

fn handle_http_connection<S>(
    mut stream: S,
    remote_addr: SocketAddr,
    listener: ListenerKey,
    state: Arc<Mutex<RuntimeState>>,
    events: Arc<Mutex<EventLogStore>>,
) where
    S: Read + Write,
{
    let mut buffer = [0; 8192];
    let Ok(read) = stream.read(&mut buffer) else {
        return;
    };
    if read == 0 {
        return;
    }
    let request = String::from_utf8_lossy(&buffer[..read]);
    let Some(first_line) = request.lines().next() else {
        return;
    };
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/");
    let user_agent = request
        .lines()
        .find_map(|line| line.strip_prefix("User-Agent: "))
        .map(ToString::to_string);

    if method != "GET" && method != "HEAD" {
        write_response(
            &mut stream,
            405,
            "text/plain; charset=utf-8",
            b"method not allowed",
            method == "HEAD",
        );
        append_access_event(
            &events,
            EventKind::HttpAccessDenied,
            method,
            path,
            None,
            None,
            405,
            0,
            remote_addr,
            user_agent,
        );
        return;
    }

    let matched = {
        let Ok(state) = state.lock() else {
            write_response(
                &mut stream,
                500,
                "text/plain; charset=utf-8",
                b"runtime lock poisoned",
                method == "HEAD",
            );
            return;
        };
        state
            .registry
            .match_request(&listener, path)
            .map(|matched| (matched.mount.clone(), matched.relative_path))
    };

    let Some((mount, relative_path)) = matched else {
        write_response(
            &mut stream,
            404,
            "text/plain; charset=utf-8",
            b"not found",
            method == "HEAD",
        );
        append_access_event(
            &events,
            EventKind::HttpNotFound,
            method,
            path,
            None,
            None,
            404,
            0,
            remote_addr,
            user_agent,
        );
        return;
    };

    match StaticFileService::plan(&mount, &relative_path) {
        ServeOutcome::File(file) => match read_file_response(&file) {
            Ok(response) => {
                let len = response.body.len() as u64;
                write_response(
                    &mut stream,
                    200,
                    &response.content_type,
                    &response.body,
                    method == "HEAD",
                );
                append_access_event(
                    &events,
                    EventKind::HttpAccessServed,
                    method,
                    path,
                    Some(&mount),
                    Some(file.path),
                    200,
                    len,
                    remote_addr,
                    user_agent,
                );
            }
            Err(error) => {
                let body = format!("read failed: {error}");
                write_response(
                    &mut stream,
                    500,
                    "text/plain; charset=utf-8",
                    body.as_bytes(),
                    method == "HEAD",
                );
                append_access_event(
                    &events,
                    EventKind::HttpServeError,
                    method,
                    path,
                    Some(&mount),
                    None,
                    500,
                    0,
                    remote_addr,
                    user_agent,
                );
            }
        },
        ServeOutcome::DirectoryListing { entries, .. } => {
            let html = render_directory_listing(path, &entries);
            let len = html.len() as u64;
            write_response(
                &mut stream,
                200,
                "text/html; charset=utf-8",
                html.as_bytes(),
                method == "HEAD",
            );
            append_access_event(
                &events,
                EventKind::HttpAccessServed,
                method,
                path,
                Some(&mount),
                None,
                200,
                len,
                remote_addr,
                user_agent,
            );
        }
        ServeOutcome::NotFound { .. } => {
            write_response(
                &mut stream,
                404,
                "text/plain; charset=utf-8",
                b"not found",
                method == "HEAD",
            );
            append_access_event(
                &events,
                EventKind::HttpNotFound,
                method,
                path,
                Some(&mount),
                None,
                404,
                0,
                remote_addr,
                user_agent,
            );
        }
        ServeOutcome::Forbidden { reason } => {
            write_response(
                &mut stream,
                403,
                "text/plain; charset=utf-8",
                reason.as_bytes(),
                method == "HEAD",
            );
            append_access_event(
                &events,
                EventKind::HttpAccessDenied,
                method,
                path,
                Some(&mount),
                None,
                403,
                0,
                remote_addr,
                user_agent,
            );
        }
    }
}

struct FileResponse {
    content_type: String,
    body: Vec<u8>,
}

fn read_file_response(
    file: &serve_lib_core::ServeFilePlan,
) -> Result<FileResponse, std::io::Error> {
    let body = fs::read(&file.path)?;
    match file.render_mode {
        RenderMode::Raw => Ok(FileResponse {
            content_type: file.content_type.clone(),
            body,
        }),
        RenderMode::Markdown => {
            let source = String::from_utf8_lossy(&body);
            let html = render_markdown_page(&file.path.to_string_lossy(), &source);
            Ok(FileResponse {
                content_type: "text/html; charset=utf-8".to_string(),
                body: html.into_bytes(),
            })
        }
        RenderMode::CodeHighlight => {
            let source = String::from_utf8_lossy(&body);
            let html = render_code_page(&file.path.to_string_lossy(), &source);
            Ok(FileResponse {
                content_type: "text/html; charset=utf-8".to_string(),
                body: html.into_bytes(),
            })
        }
    }
}

fn render_markdown_page(title: &str, source: &str) -> String {
    let mut options = pulldown_cmark::Options::empty();
    options.insert(pulldown_cmark::Options::ENABLE_TABLES);
    options.insert(pulldown_cmark::Options::ENABLE_FOOTNOTES);
    options.insert(pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
    options.insert(pulldown_cmark::Options::ENABLE_TASKLISTS);
    let parser = pulldown_cmark::Parser::new_ext(source, options);
    let mut body = String::new();
    pulldown_cmark::html::push_html(&mut body, parser);
    render_html_shell(title, &body)
}

fn render_code_page(title: &str, source: &str) -> String {
    use syntect::highlighting::ThemeSet;
    use syntect::html::highlighted_html_for_string;
    use syntect::parsing::SyntaxSet;

    let syntax_set = SyntaxSet::load_defaults_newlines();
    let theme_set = ThemeSet::load_defaults();
    let syntax = std::path::Path::new(title)
        .extension()
        .and_then(|extension| extension.to_str())
        .and_then(|extension| syntax_set.find_syntax_by_extension(extension))
        .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
    let Some(theme) = theme_set
        .themes
        .get("base16-ocean.dark")
        .or_else(|| theme_set.themes.values().next())
    else {
        return render_html_shell(title, &format!("<pre>{}</pre>", html_escape(source)));
    };
    let highlighted = highlighted_html_for_string(source, &syntax_set, syntax, theme)
        .unwrap_or_else(|_| format!("<pre>{}</pre>", html_escape(source)));
    render_html_shell(title, &highlighted)
}

fn render_html_shell(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title><style>{}</style></head><body><main>{}</main></body></html>",
        html_escape(title),
        "body{margin:0;background:#f7f7f5;color:#1d1d1f;font:16px/1.55 system-ui,-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif}main{max-width:920px;margin:0 auto;padding:32px}pre{overflow:auto;padding:16px;border-radius:8px;background:#111;color:#f8f8f2}code{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}a{color:#075985}",
        body
    )
}

fn write_response(
    stream: &mut impl Write,
    status: u16,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) {
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    if !head_only {
        let _ = stream.write_all(body);
    }
}

fn render_directory_listing(path: &str, entries: &[serve_lib_core::DirectoryEntry]) -> String {
    let mut html =
        format!("<!doctype html><title>Index of {path}</title><h1>Index of {path}</h1><ul>");
    for entry in entries {
        let suffix = if entry.kind == DirectoryEntryKind::Directory {
            "/"
        } else {
            ""
        };
        html.push_str(&format!(
            "<li><a href=\"{}{}\">{}{}</a></li>",
            html_escape(&entry.name),
            suffix,
            html_escape(&entry.name),
            suffix
        ));
    }
    html.push_str("</ul>");
    html
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[allow(clippy::too_many_arguments)]
fn append_access_event(
    events: &Arc<Mutex<EventLogStore>>,
    kind: EventKind,
    method: &str,
    path: &str,
    mount: Option<&RouteMount>,
    local_path: Option<PathBuf>,
    status: u16,
    bytes_sent: u64,
    remote_addr: SocketAddr,
    user_agent: Option<String>,
) {
    let mut event = ServeEvent::access(kind, method, path);
    if let Some(mount) = mount {
        event.listener = Some(mount.listener.clone());
        event.mount_id = Some(mount.id);
        event.route = Some(mount.route.clone());
    }
    event.local_path = local_path;
    event.status = Some(status);
    event.bytes_sent = Some(bytes_sent);
    event.remote_addr = Some(remote_addr);
    event.user_agent = user_agent;
    append_event(events, event);
}

fn append_event(events: &Arc<Mutex<EventLogStore>>, event: ServeEvent) {
    if let Ok(store) = events.lock() {
        let _ = store.append(&event);
    }
}

fn expire_routes(state: &Arc<Mutex<RuntimeState>>, events: &Arc<Mutex<EventLogStore>>) {
    let now = SystemTime::now();
    let expired = {
        let Ok(state) = state.lock() else {
            return;
        };
        state
            .registry
            .mounts()
            .filter(|mount| mount.expires_at.is_some_and(|expires_at| expires_at <= now))
            .map(|mount| mount.id)
            .collect::<Vec<_>>()
    };

    if expired.is_empty() {
        return;
    }

    let mut state = match state.lock() {
        Ok(state) => state,
        Err(_) => return,
    };
    for mount_id in expired {
        if let Ok(removed) = state.registry.remove_by_id(mount_id) {
            if state.registry.is_listener_empty(&removed.listener) {
                stop_listener(&mut state, &removed.listener);
            }
            let mut event = ServeEvent::lifecycle(
                EventKind::RouteExpired,
                format!("expired {}", removed.route),
            );
            event.listener = Some(removed.listener);
            event.mount_id = Some(removed.id);
            event.route = Some(removed.route);
            append_event(events, event);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Arc;

    use rcgen::{
        BasicConstraints, Certificate, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair,
        KeyUsagePurpose,
    };
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
    use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
    use serve_lib_core::{BindTarget, DurationSpec, NormalizedRoute, TlsMode};
    use tempfile::TempDir;

    use super::*;

    fn request(path: PathBuf, route: &str, port: u16) -> RegisterRequest {
        RegisterRequest {
            local_path: path,
            route: route.parse::<NormalizedRoute>().unwrap(),
            bind: BindTarget::Loopback,
            port,
            timeout: None,
            index_file: "index.html".to_string(),
            spa: false,
            render: Default::default(),
            readonly: true,
            display_name: None,
        }
    }

    fn free_port() -> u16 {
        TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn write_cert_files(temp: &TempDir, name: &str, cert: &str, key: &str) -> (PathBuf, PathBuf) {
        let cert_path = temp.path().join(format!("{name}.crt"));
        let key_path = temp.path().join(format!("{name}.key"));
        fs::write(&cert_path, cert).unwrap();
        fs::write(&key_path, key).unwrap();
        (cert_path, key_path)
    }

    struct TestCa {
        cert: Certificate,
        key_pair: KeyPair,
    }

    impl TestCa {
        fn pem(&self) -> String {
            self.cert.pem()
        }
    }

    fn test_ca() -> TestCa {
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, "serve-lib test ca");
        let key_pair = KeyPair::generate().unwrap();
        let cert = params.self_signed(&key_pair).unwrap();
        TestCa { cert, key_pair }
    }

    fn signed_cert(common_name: &str, ca: &TestCa) -> (String, String) {
        let mut params = CertificateParams::new(vec![common_name.to_string()]).unwrap();
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, common_name);
        let key_pair = KeyPair::generate().unwrap();
        let cert = params.signed_by(&key_pair, &ca.cert, &ca.key_pair).unwrap();
        (cert.pem(), key_pair.serialize_pem())
    }

    fn tls_policy(cert_path: PathBuf, key_path: PathBuf) -> TlsPolicy {
        TlsPolicy {
            mode: TlsMode::Tls,
            server_cert: Some(cert_path),
            server_key: Some(key_path),
            client_ca: None,
        }
    }

    fn mtls_policy(cert_path: PathBuf, key_path: PathBuf, ca_path: PathBuf) -> TlsPolicy {
        TlsPolicy {
            mode: TlsMode::Mtls,
            server_cert: Some(cert_path),
            server_key: Some(key_path),
            client_ca: Some(ca_path),
        }
    }

    fn tls_get(
        port: u16,
        path: &str,
        ca_cert: CertificateDer<'static>,
        client_identity: Option<(CertificateDer<'static>, PrivateKeyDer<'static>)>,
    ) -> String {
        try_tls_get(port, path, ca_cert, client_identity).unwrap()
    }

    fn try_tls_get(
        port: u16,
        path: &str,
        ca_cert: CertificateDer<'static>,
        client_identity: Option<(CertificateDer<'static>, PrivateKeyDer<'static>)>,
    ) -> Result<String, String> {
        let mut roots = RootCertStore::empty();
        roots.add(ca_cert).map_err(|err| err.to_string())?;
        let builder = ClientConfig::builder().with_root_certificates(roots);
        let config = if let Some((cert, key)) = client_identity {
            builder
                .with_client_auth_cert(vec![cert], key)
                .map_err(|err| err.to_string())?
        } else {
            builder.with_no_client_auth()
        };
        let server_name = ServerName::try_from("localhost").map_err(|err| err.to_string())?;
        let connection =
            ClientConnection::new(Arc::new(config), server_name).map_err(|err| err.to_string())?;
        let stream = TcpStream::connect(("127.0.0.1", port)).map_err(|err| err.to_string())?;
        let mut tls = StreamOwned::new(connection, stream);
        tls.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .map_err(|err| err.to_string())?;
        let mut response = Vec::new();
        let mut buffer = [0; 1024];
        loop {
            match tls.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => response.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(error.to_string()),
            }
        }
        String::from_utf8(response).map_err(|err| err.to_string())
    }

    fn cert_der_from_pem(pem: &str) -> CertificateDer<'static> {
        let mut reader = std::io::BufReader::new(pem.as_bytes());
        let cert = rustls_pemfile::certs(&mut reader).next().unwrap().unwrap();
        cert
    }

    fn key_der_from_pem(pem: &str) -> PrivateKeyDer<'static> {
        let mut reader = std::io::BufReader::new(pem.as_bytes());
        rustls_pemfile::private_key(&mut reader).unwrap().unwrap()
    }

    #[test]
    fn registers_two_routes_on_one_listener() {
        // Arrange
        let temp = TempDir::new().unwrap();
        let app = temp.path().join("app");
        let logs = temp.path().join("logs");
        fs::create_dir(&app).unwrap();
        fs::create_dir(&logs).unwrap();
        fs::write(app.join("index.html"), "app").unwrap();
        fs::write(logs.join("out.txt"), "logs").unwrap();
        let runtime = DaemonRuntime::new(RuntimeOptions::default()).unwrap();
        let port = free_port();

        // Act
        runtime
            .register(request(app, "/app", port), TlsPolicy::off())
            .unwrap();
        runtime
            .register(request(logs, "/logs", port), TlsPolicy::off())
            .unwrap();

        // Assert
        assert_eq!(runtime.status().unwrap().listeners, 1);
        assert_eq!(runtime.list().unwrap().len(), 2);
    }

    #[test]
    fn timeout_expires_route() {
        // Arrange
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("index.html"), "app").unwrap();
        let runtime = DaemonRuntime::new(RuntimeOptions {
            timeout_tick: Duration::from_millis(25),
            ..RuntimeOptions::default()
        })
        .unwrap();
        let port = free_port();
        let mut register = request(temp.path().to_path_buf(), "/app", port);
        register.timeout = Some(DurationSpec::from_seconds(1).unwrap());

        // Act
        runtime.register(register, TlsPolicy::off()).unwrap();
        thread::sleep(Duration::from_millis(1300));

        // Assert
        assert!(runtime.list().unwrap().is_empty());
        let events = runtime
            .events(EventQuery {
                kind: Some(EventKind::RouteExpired),
                ..EventQuery::default()
            })
            .unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn http_listener_serves_registered_file() {
        // Arrange
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("hello.txt"), "hello").unwrap();
        let runtime = DaemonRuntime::new(RuntimeOptions::default()).unwrap();
        let port = free_port();
        runtime
            .register(
                request(temp.path().to_path_buf(), "/share", port),
                TlsPolicy::off(),
            )
            .unwrap();
        thread::sleep(Duration::from_millis(100));

        // Act
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .write_all(b"GET /share/hello.txt HTTP/1.1\r\nHost: example\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();

        // Assert
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.ends_with("hello"));
    }

    #[test]
    fn tls_listener_serves_registered_file() {
        // Arrange
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("hello.txt"), "hello-tls").unwrap();
        let ca = test_ca();
        let ca_pem = ca.pem();
        let (server_cert, server_key) = signed_cert("localhost", &ca);
        let (server_cert_path, server_key_path) =
            write_cert_files(&temp, "server", &server_cert, &server_key);
        let runtime = DaemonRuntime::new(RuntimeOptions::default()).unwrap();
        let port = free_port();

        // Act
        runtime
            .register(
                request(temp.path().to_path_buf(), "/secure", port),
                tls_policy(server_cert_path, server_key_path),
            )
            .unwrap();
        thread::sleep(Duration::from_millis(100));
        let response = tls_get(port, "/secure/hello.txt", cert_der_from_pem(&ca_pem), None);

        // Assert
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.ends_with("hello-tls"));
    }

    #[test]
    fn mtls_listener_requires_client_certificate() {
        // Arrange
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("hello.txt"), "hello-mtls").unwrap();
        let ca = test_ca();
        let ca_pem = ca.pem();
        let (server_cert, server_key) = signed_cert("localhost", &ca);
        let (client_cert, client_key) = signed_cert("serve-lib-client", &ca);
        let (server_cert_path, server_key_path) =
            write_cert_files(&temp, "server", &server_cert, &server_key);
        let ca_path = temp.path().join("client-ca.crt");
        fs::write(&ca_path, &ca_pem).unwrap();
        let runtime = DaemonRuntime::new(RuntimeOptions::default()).unwrap();
        let port = free_port();
        runtime
            .register(
                request(temp.path().to_path_buf(), "/secure", port),
                mtls_policy(server_cert_path, server_key_path, ca_path),
            )
            .unwrap();
        thread::sleep(Duration::from_millis(100));

        // Act
        let response = tls_get(
            port,
            "/secure/hello.txt",
            cert_der_from_pem(&ca_pem),
            Some((
                cert_der_from_pem(&client_cert),
                key_der_from_pem(&client_key),
            )),
        );

        // Assert
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.ends_with("hello-mtls"));
    }

    #[test]
    fn mtls_listener_rejects_missing_client_certificate() {
        // Arrange
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("hello.txt"), "hello-mtls").unwrap();
        let ca = test_ca();
        let ca_pem = ca.pem();
        let (server_cert, server_key) = signed_cert("localhost", &ca);
        let (server_cert_path, server_key_path) =
            write_cert_files(&temp, "server", &server_cert, &server_key);
        let ca_path = temp.path().join("client-ca.crt");
        fs::write(&ca_path, &ca_pem).unwrap();
        let runtime = DaemonRuntime::new(RuntimeOptions::default()).unwrap();
        let port = free_port();
        runtime
            .register(
                request(temp.path().to_path_buf(), "/secure", port),
                mtls_policy(server_cert_path, server_key_path, ca_path),
            )
            .unwrap();
        thread::sleep(Duration::from_millis(100));

        // Act
        let response = try_tls_get(port, "/secure/hello.txt", cert_der_from_pem(&ca_pem), None);

        // Assert
        assert!(response
            .as_ref()
            .map(|response| !response.starts_with("HTTP/1.1 200 OK"))
            .unwrap_or(true));
    }
}
