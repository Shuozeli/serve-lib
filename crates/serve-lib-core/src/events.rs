use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, Row};
use serde::{Deserialize, Serialize};

use crate::{ListenerKey, MountId, NormalizedRoute, ServeError};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  occurred_at_ms INTEGER NOT NULL,
  kind TEXT NOT NULL,
  listener_bind TEXT,
  listener_port INTEGER,
  mount_id TEXT,
  route TEXT,
  method TEXT,
  request_path TEXT,
  local_path TEXT,
  status INTEGER,
  bytes_sent INTEGER,
  remote_ip TEXT,
  remote_port INTEGER,
  user_agent TEXT,
  message TEXT,
  details_json TEXT
);

CREATE INDEX IF NOT EXISTS idx_events_occurred_at_ms ON events (occurred_at_ms);
CREATE INDEX IF NOT EXISTS idx_events_route ON events (route);
CREATE INDEX IF NOT EXISTS idx_events_remote_ip ON events (remote_ip);
CREATE INDEX IF NOT EXISTS idx_events_status ON events (status);
CREATE INDEX IF NOT EXISTS idx_events_kind ON events (kind);
"#;

#[derive(Debug)]
pub struct EventLogStore {
    conn: Connection,
}

impl EventLogStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ServeError> {
        let conn = Connection::open(path).map_err(sql_error)?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self, ServeError> {
        let conn = Connection::open_in_memory().map_err(sql_error)?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn migrate(&self) -> Result<(), ServeError> {
        self.conn.execute_batch(SCHEMA).map_err(sql_error)
    }

    pub fn append(&self, event: &ServeEvent) -> Result<i64, ServeError> {
        let (remote_ip, remote_port) = event
            .remote_addr
            .map(|addr| (Some(addr.ip().to_string()), Some(i64::from(addr.port()))))
            .unwrap_or((None, None));
        let (listener_bind, listener_port) = event
            .listener
            .as_ref()
            .map(|listener| {
                (
                    Some(listener.bind_addr.to_string()),
                    Some(i64::from(listener.port)),
                )
            })
            .unwrap_or((None, None));

        self.conn
            .execute(
                r#"
                INSERT INTO events (
                  occurred_at_ms, kind, listener_bind, listener_port, mount_id,
                  route, method, request_path, local_path, status, bytes_sent,
                  remote_ip, remote_port, user_agent, message, details_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
                "#,
                params![
                    system_time_to_millis(event.occurred_at)?,
                    event.kind.as_str(),
                    listener_bind,
                    listener_port,
                    event.mount_id.map(|id| id.to_string()),
                    event.route.as_ref().map(ToString::to_string),
                    event.method.as_deref(),
                    event.request_path.as_deref(),
                    event
                        .local_path
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string()),
                    event.status.map(i64::from),
                    event.bytes_sent.map(|bytes| bytes as i64),
                    remote_ip,
                    remote_port,
                    event.user_agent.as_deref(),
                    event.message.as_deref(),
                    event.details_json.as_deref(),
                ],
            )
            .map_err(sql_error)?;

        Ok(self.conn.last_insert_rowid())
    }

    pub fn query(&self, query: &EventQuery) -> Result<Vec<EventRow>, ServeError> {
        let mut sql = String::from(
            "SELECT id, occurred_at_ms, kind, listener_bind, listener_port, mount_id, route, \
             method, request_path, local_path, status, bytes_sent, remote_ip, remote_port, \
             user_agent, message, details_json FROM events",
        );
        let mut clauses = Vec::new();
        let mut values = Vec::new();

        if let Some(since) = query.since {
            clauses.push("occurred_at_ms >= ?");
            values.push(Value::Integer(system_time_to_millis(since)?));
        }
        if let Some(route) = &query.route {
            clauses.push("route = ?");
            values.push(Value::Text(route.to_string()));
        }
        if let Some(status) = query.status {
            clauses.push("status = ?");
            values.push(Value::Integer(i64::from(status)));
        }
        if let Some(remote_ip) = query.remote_ip {
            clauses.push("remote_ip = ?");
            values.push(Value::Text(remote_ip.to_string()));
        }
        if let Some(kind) = query.kind {
            clauses.push("kind = ?");
            values.push(Value::Text(kind.as_str().to_string()));
        }

        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY occurred_at_ms DESC, id DESC LIMIT ?");
        values.push(Value::Integer(i64::from(query.limit.unwrap_or(100).max(1))));

        let mut stmt = self.conn.prepare(&sql).map_err(sql_error)?;
        let rows = stmt
            .query_map(params_from_iter(values), EventRow::from_row)
            .map_err(sql_error)?;

        let mut events = Vec::new();
        for row in rows {
            events.push(row.map_err(sql_error)?);
        }
        Ok(events)
    }

    pub fn cleanup_before(&self, cutoff: SystemTime) -> Result<usize, ServeError> {
        self.conn
            .execute(
                "DELETE FROM events WHERE occurred_at_ms < ?1",
                params![system_time_to_millis(cutoff)?],
            )
            .map_err(sql_error)
    }

    pub fn cleanup_older_than(
        &self,
        now: SystemTime,
        retention: Duration,
    ) -> Result<usize, ServeError> {
        let cutoff = now.checked_sub(retention).ok_or_else(|| {
            ServeError::EventLogUnavailable("invalid retention cutoff".to_string())
        })?;
        self.cleanup_before(cutoff)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    DaemonStarted,
    DaemonStopped,
    ListenerOpened,
    ListenerClosed,
    RouteRegistered,
    RouteDeregistered,
    RouteExpired,
    HttpAccessServed,
    HttpAccessDenied,
    HttpNotFound,
    HttpServeError,
    BindResolutionFailed,
    ServeDenied,
    ServeError,
}

/// Single source of truth for EventKind ↔ string mappings.
/// Both `as_str` and `FromStr` derive from this table so adding a new variant
/// only requires one entry here.
static KIND_TABLE: &[(&str, EventKind)] = &[
    ("daemon_started", EventKind::DaemonStarted),
    ("daemon_stopped", EventKind::DaemonStopped),
    ("listener_opened", EventKind::ListenerOpened),
    ("listener_closed", EventKind::ListenerClosed),
    ("route_registered", EventKind::RouteRegistered),
    ("route_deregistered", EventKind::RouteDeregistered),
    ("route_expired", EventKind::RouteExpired),
    ("http_access_served", EventKind::HttpAccessServed),
    ("http_access_denied", EventKind::HttpAccessDenied),
    ("http_not_found", EventKind::HttpNotFound),
    ("http_serve_error", EventKind::HttpServeError),
    ("bind_resolution_failed", EventKind::BindResolutionFailed),
    ("serve_denied", EventKind::ServeDenied),
    ("serve_error", EventKind::ServeError),
];

impl EventKind {
    pub fn as_str(self) -> &'static str {
        KIND_TABLE
            .iter()
            .find(|(_, kind)| *kind == self)
            .map(|(s, _)| *s)
            .expect("all EventKind variants are present in KIND_TABLE")
    }
}

impl FromStr for EventKind {
    type Err = ServeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        KIND_TABLE
            .iter()
            .find(|(s, _)| *s == value)
            .map(|(_, kind)| *kind)
            .ok_or_else(|| ServeError::EventLogUnavailable(format!("unknown event kind: {value}")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeEvent {
    pub occurred_at: SystemTime,
    pub kind: EventKind,
    pub listener: Option<ListenerKey>,
    pub mount_id: Option<MountId>,
    pub route: Option<NormalizedRoute>,
    pub method: Option<String>,
    pub request_path: Option<String>,
    pub local_path: Option<PathBuf>,
    pub status: Option<u16>,
    pub bytes_sent: Option<u64>,
    pub remote_addr: Option<SocketAddr>,
    pub user_agent: Option<String>,
    pub message: Option<String>,
    pub details_json: Option<String>,
}

impl ServeEvent {
    pub fn lifecycle(kind: EventKind, message: impl Into<String>) -> Self {
        Self {
            occurred_at: SystemTime::now(),
            kind,
            listener: None,
            mount_id: None,
            route: None,
            method: None,
            request_path: None,
            local_path: None,
            status: None,
            bytes_sent: None,
            remote_addr: None,
            user_agent: None,
            message: Some(message.into()),
            details_json: None,
        }
    }

    pub fn access(
        kind: EventKind,
        method: impl Into<String>,
        request_path: impl Into<String>,
    ) -> Self {
        let request_path = strip_query(request_path.into());
        Self {
            occurred_at: SystemTime::now(),
            kind,
            listener: None,
            mount_id: None,
            route: None,
            method: Some(method.into()),
            request_path: Some(request_path),
            local_path: None,
            status: None,
            bytes_sent: None,
            remote_addr: None,
            user_agent: None,
            message: None,
            details_json: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EventQuery {
    pub since: Option<SystemTime>,
    pub route: Option<NormalizedRoute>,
    pub status: Option<u16>,
    pub remote_ip: Option<IpAddr>,
    pub kind: Option<EventKind>,
    pub limit: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    pub id: i64,
    pub occurred_at: SystemTime,
    pub kind: EventKind,
    pub listener_bind: Option<String>,
    pub listener_port: Option<u16>,
    pub mount_id: Option<String>,
    pub route: Option<String>,
    pub method: Option<String>,
    pub request_path: Option<String>,
    pub local_path: Option<PathBuf>,
    pub status: Option<u16>,
    pub bytes_sent: Option<u64>,
    pub remote_ip: Option<IpAddr>,
    pub remote_port: Option<u16>,
    pub user_agent: Option<String>,
    pub message: Option<String>,
    pub details_json: Option<String>,
}

impl EventRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let occurred_at_ms: i64 = row.get(1)?;
        let kind: String = row.get(2)?;
        let remote_ip: Option<String> = row.get(12)?;

        Ok(Self {
            id: row.get(0)?,
            occurred_at: millis_to_system_time(occurred_at_ms),
            kind: kind.parse().map_err(|err: ServeError| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(err))
            })?,
            listener_bind: row.get(3)?,
            listener_port: optional_i64_to_u16(row.get(4)?),
            mount_id: row.get(5)?,
            route: row.get(6)?,
            method: row.get(7)?,
            request_path: row.get(8)?,
            local_path: row.get::<_, Option<String>>(9)?.map(PathBuf::from),
            status: optional_i64_to_u16(row.get(10)?),
            bytes_sent: optional_i64_to_u64(row.get(11)?),
            remote_ip: remote_ip.and_then(|ip| ip.parse().ok()),
            remote_port: optional_i64_to_u16(row.get(13)?),
            user_agent: row.get(14)?,
            message: row.get(15)?,
            details_json: row.get(16)?,
        })
    }
}

fn strip_query(path: String) -> String {
    path.split('?').next().unwrap_or(&path).to_string()
}

fn sql_error(error: rusqlite::Error) -> ServeError {
    ServeError::EventLogUnavailable(error.to_string())
}

fn system_time_to_millis(time: SystemTime) -> Result<i64, ServeError> {
    let millis = time
        .duration_since(UNIX_EPOCH)
        .map_err(|err| ServeError::EventLogUnavailable(err.to_string()))?
        .as_millis();
    i64::try_from(millis)
        .map_err(|_| ServeError::EventLogUnavailable("timestamp is too large".to_string()))
}

fn millis_to_system_time(millis: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(millis.max(0) as u64)
}

fn optional_i64_to_u16(value: Option<i64>) -> Option<u16> {
    value.and_then(|value| u16::try_from(value).ok())
}

fn optional_i64_to_u64(value: Option<i64>) -> Option<u64> {
    value.and_then(|value| u64::try_from(value).ok())
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    #[test]
    fn migrates_and_appends_lifecycle_event() {
        let store = EventLogStore::open_in_memory().unwrap();
        let mut event = ServeEvent::lifecycle(EventKind::RouteRegistered, "registered /app");
        event.route = Some("/app".parse().unwrap());

        let id = store.append(&event).unwrap();
        let rows = store
            .query(&EventQuery {
                kind: Some(EventKind::RouteRegistered),
                ..EventQuery::default()
            })
            .unwrap();

        assert_eq!(id, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message.as_deref(), Some("registered /app"));
        assert_eq!(rows[0].route.as_deref(), Some("/app"));
    }

    #[test]
    fn appends_access_event_without_query_string() {
        let store = EventLogStore::open_in_memory().unwrap();
        let mut event = ServeEvent::access(
            EventKind::HttpAccessServed,
            "GET",
            "/app/index.html?token=secret",
        );
        event.status = Some(200);
        event.bytes_sent = Some(42);
        event.remote_addr = Some(SocketAddr::from(([192, 168, 1, 10], 55123)));
        event.user_agent = Some("curl/8".to_string());

        store.append(&event).unwrap();
        let rows = store
            .query(&EventQuery {
                status: Some(200),
                remote_ip: Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10))),
                ..EventQuery::default()
            })
            .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].method.as_deref(), Some("GET"));
        assert_eq!(rows[0].request_path.as_deref(), Some("/app/index.html"));
        assert_eq!(rows[0].bytes_sent, Some(42));
        assert_eq!(rows[0].remote_port, Some(55123));
        assert_eq!(rows[0].user_agent.as_deref(), Some("curl/8"));
    }

    #[test]
    fn filters_by_route_and_since() {
        let store = EventLogStore::open_in_memory().unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let mut old = ServeEvent::lifecycle(EventKind::RouteRegistered, "old");
        old.occurred_at = now - Duration::from_secs(60);
        old.route = Some("/old".parse().unwrap());
        let mut new = ServeEvent::lifecycle(EventKind::RouteRegistered, "new");
        new.occurred_at = now;
        new.route = Some("/new".parse().unwrap());
        store.append(&old).unwrap();
        store.append(&new).unwrap();

        let rows = store
            .query(&EventQuery {
                since: Some(now - Duration::from_secs(1)),
                route: Some("/new".parse().unwrap()),
                ..EventQuery::default()
            })
            .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message.as_deref(), Some("new"));
    }

    #[test]
    fn cleanup_deletes_events_older_than_retention() {
        let store = EventLogStore::open_in_memory().unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(30 * 24 * 60 * 60);
        let mut old = ServeEvent::lifecycle(EventKind::HttpNotFound, "old");
        old.occurred_at = now - Duration::from_secs(8 * 24 * 60 * 60);
        let mut recent = ServeEvent::lifecycle(EventKind::HttpAccessServed, "recent");
        recent.occurred_at = now - Duration::from_secs(60);
        store.append(&old).unwrap();
        store.append(&recent).unwrap();

        let deleted = store
            .cleanup_older_than(now, Duration::from_secs(7 * 24 * 60 * 60))
            .unwrap();
        let rows = store.query(&EventQuery::default()).unwrap();

        assert_eq!(deleted, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message.as_deref(), Some("recent"));
    }
}
