use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serve_lib_core::{
    DeregisterRequest, EventQuery, EventRow, RegisterRequest, RegisterResponse, ServeError,
    TlsPolicy,
};

use crate::runtime::{DaemonRuntime, DaemonStatus, ListMount};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlRequest {
    Register {
        request: RegisterRequest,
        tls_policy: TlsPolicy,
    },
    Deregister {
        request: DeregisterRequest,
    },
    List,
    Status,
    Events {
        query: EventQueryWire,
    },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlResponse {
    Register { response: RegisterResponse },
    Deregister { route: String },
    List { mounts: Vec<ListMount> },
    Status { status: DaemonStatus },
    Events { events: Vec<EventRowWire> },
    Shutdown,
    Error { code: String, message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EventQueryWire {
    pub limit: Option<u16>,
}

impl From<EventQueryWire> for EventQuery {
    fn from(value: EventQueryWire) -> Self {
        Self {
            limit: value.limit,
            ..EventQuery::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRowWire {
    pub id: i64,
    pub kind: String,
    pub route: Option<String>,
    pub method: Option<String>,
    pub request_path: Option<String>,
    pub status: Option<u16>,
    pub remote_ip: Option<String>,
    pub message: Option<String>,
}

impl From<EventRow> for EventRowWire {
    fn from(value: EventRow) -> Self {
        Self {
            id: value.id,
            kind: value.kind.as_str().to_string(),
            route: value.route,
            method: value.method,
            request_path: value.request_path,
            status: value.status,
            remote_ip: value.remote_ip.map(|ip| ip.to_string()),
            message: value.message,
        }
    }
}

pub fn run_control_server(runtime: Arc<DaemonRuntime>, addr: SocketAddr) -> Result<(), ServeError> {
    let listener =
        TcpListener::bind(addr).map_err(|err| ServeError::DaemonUnavailable(err.to_string()))?;
    listener
        .set_nonblocking(true)
        .map_err(|err| ServeError::DaemonUnavailable(err.to_string()))?;

    while !runtime.is_shutdown() {
        match listener.accept() {
            Ok((stream, _)) => handle_control_connection(stream, Arc::clone(&runtime)),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(ServeError::DaemonUnavailable(error.to_string())),
        }
    }
    Ok(())
}

fn handle_control_connection(mut stream: TcpStream, runtime: Arc<DaemonRuntime>) {
    let response = match read_http_request(&mut stream) {
        Ok((method, path, body)) => route_control_request(&runtime, &method, &path, &body),
        Err(error) => ControlResponse::Error {
            code: format!("{:?}", error.code()),
            message: error.to_string(),
        },
    };
    let status = if matches!(response, ControlResponse::Error { .. }) {
        400
    } else {
        200
    };
    let body = serde_json::to_vec(&response).unwrap_or_else(|_| b"{}".to_vec());
    write_json_response(&mut stream, status, &body);
}

fn route_control_request(
    runtime: &DaemonRuntime,
    method: &str,
    path: &str,
    body: &[u8],
) -> ControlResponse {
    let result = match (method, path) {
        ("GET", "/status") => runtime
            .status()
            .map(|status| ControlResponse::Status { status }),
        ("GET", "/list") => runtime
            .list()
            .map(|mounts| ControlResponse::List { mounts }),
        ("GET", "/events") => {
            runtime
                .events(EventQuery::default())
                .map(|events| ControlResponse::Events {
                    events: events.into_iter().map(EventRowWire::from).collect(),
                })
        }
        ("POST", "/register") => serde_json::from_slice::<ControlRequest>(body)
            .map_err(|err| ServeError::InvalidRequest(err.to_string()))
            .and_then(|request| match request {
                ControlRequest::Register {
                    request,
                    tls_policy,
                } => runtime
                    .register(request, tls_policy)
                    .map(|response| ControlResponse::Register { response }),
                _ => Err(ServeError::InvalidRequest(
                    "expected register request".to_string(),
                )),
            }),
        ("POST", "/deregister") => serde_json::from_slice::<ControlRequest>(body)
            .map_err(|err| ServeError::InvalidRequest(err.to_string()))
            .and_then(|request| match request {
                ControlRequest::Deregister { request } => {
                    runtime
                        .deregister(request)
                        .map(|response| ControlResponse::Deregister {
                            route: response.removed.route.to_string(),
                        })
                }
                _ => Err(ServeError::InvalidRequest(
                    "expected deregister request".to_string(),
                )),
            }),
        ("POST", "/shutdown") => {
            runtime.shutdown();
            Ok(ControlResponse::Shutdown)
        }
        _ => Err(ServeError::InvalidRequest(format!(
            "unknown control endpoint: {method} {path}"
        ))),
    };

    result.unwrap_or_else(|error| ControlResponse::Error {
        code: format!("{:?}", error.code()),
        message: error.to_string(),
    })
}

fn read_http_request(stream: &mut TcpStream) -> Result<(String, String, Vec<u8>), ServeError> {
    let mut buffer = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let read = stream
            .read(&mut chunk)
            .map_err(|err| ServeError::DaemonUnavailable(err.to_string()))?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if buffer.len() > 1024 * 1024 {
            return Err(ServeError::InvalidRequest(
                "control request is too large".to_string(),
            ));
        }
    }

    let header_end = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| ServeError::InvalidRequest("missing HTTP headers".to_string()))?;
    let headers = String::from_utf8_lossy(&buffer[..header_end]);
    let first = headers
        .lines()
        .next()
        .ok_or_else(|| ServeError::InvalidRequest("missing request line".to_string()))?;
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let content_length = headers
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);

    let mut body = buffer[(header_end + 4)..].to_vec();
    while body.len() < content_length {
        let read = stream
            .read(&mut chunk)
            .map_err(|err| ServeError::DaemonUnavailable(err.to_string()))?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    Ok((method, path, body))
}

fn write_json_response(stream: &mut TcpStream, status: u16, body: &[u8]) {
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
}

#[derive(Debug, Clone)]
pub struct ControlClient {
    addr: SocketAddr,
}

impl ControlClient {
    pub fn new(addr: SocketAddr) -> Self {
        Self { addr }
    }

    pub fn get(&self, path: &str) -> Result<ControlResponse, ServeError> {
        self.request("GET", path, &[])
    }

    pub fn post(
        &self,
        path: &str,
        request: &ControlRequest,
    ) -> Result<ControlResponse, ServeError> {
        let body = serde_json::to_vec(request)
            .map_err(|err| ServeError::InvalidRequest(err.to_string()))?;
        self.request("POST", path, &body)
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Result<ControlResponse, ServeError> {
        let mut stream = TcpStream::connect(self.addr)
            .map_err(|err| ServeError::DaemonUnavailable(err.to_string()))?;
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.addr,
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|err| ServeError::DaemonUnavailable(err.to_string()))?;
        stream
            .write_all(body)
            .map_err(|err| ServeError::DaemonUnavailable(err.to_string()))?;
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .map_err(|err| ServeError::DaemonUnavailable(err.to_string()))?;
        let body_start = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4)
            .ok_or_else(|| ServeError::DaemonUnavailable("invalid control response".to_string()))?;
        serde_json::from_slice(&response[body_start..])
            .map_err(|err| ServeError::DaemonUnavailable(err.to_string()))
    }
}
