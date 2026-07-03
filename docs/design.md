# serve-lib Component Design

## Design Goals

The implementation should keep runtime authority in the daemon while making the CLI feel simple. Each component should have a narrow job:

- Config components decide defaults.
- Bind components resolve where to listen.
- Registry components decide what is mounted.
- Listener components own sockets.
- Router and static file components decide how requests map to files.
- Render components optionally transform known text formats into HTML.
- Timeout components remove temporary mounts.
- Event log components persist route lifecycle and access history with TTL cleanup.
- Control API components connect the CLI to the daemon.

This document describes the first implementation shape without locking the project into exact Rust module names.

## Component Summary

| Component | Owns | Does Not Own |
|-----------|------|--------------|
| CLI | command parsing, output, control client | route registry, HTTP listeners |
| Config Store | local config file, defaults, profiles | active runtime mounts |
| Bind Resolver | logical bind target to socket address/display URL | opening sockets |
| TLS Config | server cert, key, client CA policy | certificate issuance |
| Render Policy | Markdown/code rendering flags | file path resolution |
| Control API | request/response protocol | business rules |
| Daemon Runtime | lifecycle, task supervision | CLI formatting |
| Registry Manager | active mounts and route conflicts | socket I/O |
| Timeout Scheduler | expiry timers | route matching |
| Listener Manager | HTTP socket lifecycle | local config |
| Route Router | URL prefix matching | filesystem reads |
| Static File Service | path-to-file response | mount registry mutation |
| Event Log Store | SQLite event history | route registry decisions |
| Event Log Cleanup Worker | retention TTL cleanup | access logging decisions |

## CLI Component

### Responsibilities

- Parse commands and flags.
- Load local config before building requests.
- Apply precedence between config, profiles, and command-line flags.
- Connect to the daemon control API.
- Render output for humans.
- Return stable exit codes.

### Commands

```text
serve-lib daemon start
serve-lib daemon stop
serve-lib daemon status

serve-lib config show
serve-lib config path
serve-lib config set default-bind <target>
serve-lib config set default-port <port>
serve-lib config set default-timeout <duration>
serve-lib config set event-log.retention <duration>
serve-lib config set event-log.cleanup-interval <duration>

serve-lib register <local-path> --route <subpath> [--profile <name>] [--bind <target>] [--port <port>] [--timeout <duration>] [--index <file>] [--spa]
serve-lib deregister --route <subpath> [--bind <target>] [--port <port>]
serve-lib list
serve-lib events [--route <subpath>] [--since <duration>] [--status <code>]
```

### Request Building

`register` should resolve omitted values before contacting the daemon:

```text
local path       required from CLI
route            required from CLI
bind             CLI flag -> profile -> defaults -> built-in
port             CLI flag -> profile -> defaults -> built-in
timeout          CLI flag -> profile -> defaults -> none
index            CLI flag -> profile -> defaults -> "index.html"
spa              CLI flag -> profile -> defaults -> false
```

The CLI should send the logical bind target to the daemon, not only the resolved IP. The daemon should still perform final bind resolution because it owns listener creation.

### Output Design

Register success should print:

```text
registered /app -> /absolute/path
url: http://machine.tailnet.example.ts.net:8088/app/
expires: 4h
```

List output should be table-oriented by default and support a future `--json`.

### Errors

The CLI should catch:

- Invalid flag combinations.
- Invalid local config file.
- Unknown profile.
- Invalid duration syntax.
- Missing daemon control socket.

Daemon-side errors should be displayed without rewriting their meaning.

## Config Store Component

### Responsibilities

- Locate config files.
- Parse and validate TOML.
- Provide built-in safe defaults.
- Merge defaults and profiles.
- Persist changes from `config set`.
- Avoid publishing machine-specific values in repo files.

### Paths

```text
SERVE_LIB_CONFIG
$XDG_CONFIG_HOME/serve-lib/config.toml
~/.config/serve-lib/config.toml
~/Library/Application Support/serve-lib/config.toml
```

### Schema

```toml
[defaults]
bind = "tailscale"
port = 8088
timeout = "2h"
index = "index.html"
spa = false

[event_log]
database_path = "default"
retention = "7d"
cleanup_interval = "1h"

[render]
markdown = true
code_highlight = true

[[tls_profiles]]
name = "private-mtls"
mode = "mtls"
server_cert = "/absolute/path/server.crt"
server_key = "/absolute/path/server.key"
client_ca = "/absolute/path/client-ca.crt"

[[profiles]]
name = "tailscale"
bind = "tailscale"
port = 8088
timeout = "2h"
tls_profile = "private-mtls"

[[profiles]]
name = "mac-lan"
bind = "192.168.1.24"
port = 8088
```

### Types

```text
LocalConfig {
  defaults: DefaultConfig,
  event_log: EventLogConfig,
  render: RenderConfig,
  tls_profiles: Vec<TlsProfileConfig>,
  profiles: Vec<ProfileConfig>
}

DefaultConfig {
  bind: Option<BindTarget>,
  port: Option<u16>,
  timeout: Option<DurationSpec>,
  index: Option<String>,
  spa: Option<bool>,
  render: Option<RenderConfig>
}

ProfileConfig {
  name: String,
  bind: Option<BindTarget>,
  port: Option<u16>,
  timeout: Option<DurationSpec>,
  index: Option<String>,
  spa: Option<bool>,
  render: Option<RenderConfig>,
  tls_profile: Option<String>
}

RenderConfig {
  markdown: Option<bool>,
  code_highlight: Option<bool>
}

EventLogConfig {
  database_path: EventLogDatabasePath,
  retention: DurationSpec,
  cleanup_interval: DurationSpec
}

TlsProfileConfig {
  name: String,
  mode: TlsMode,
  server_cert: Option<PathBuf>,
  server_key: Option<PathBuf>,
  client_ca: Option<PathBuf>
}
```

### Validation

- Profile names must be unique.
- Ports must be in `1..=65535`.
- Timeout strings must parse.
- Index file names must be relative file names, not paths with separators.
- Bind targets can be logical names or IP addresses.
- Event log retention must parse to a positive duration.
- Event log cleanup interval must parse to a positive duration.
- Event log database path may be `default` or an absolute path.
- TLS profile names must be unique.
- TLS and mTLS certificate paths must be absolute paths.
- `tls` mode requires `server_cert` and `server_key`.
- `mtls` mode requires `server_cert`, `server_key`, and `client_ca`.

## Bind Resolver Component

### Responsibilities

- Resolve logical bind targets into concrete socket addresses.
- Provide display host names for successful registrations.
- Keep private machine identity out of committed docs and defaults.

### Inputs

```text
BindTarget::Tailscale
BindTarget::Private
BindTarget::Any
BindTarget::Loopback
BindTarget::Ip(IpAddr)
BindTarget::InterfaceName(String)
```

### Outputs

```text
ResolvedBind {
  target: BindTarget,
  bind_addr: IpAddr,
  display_host: Option<String>,
  source: BindSource
}
```

`display_host` is used for CLI output only. It should prefer MagicDNS when resolving Tailscale and use IP addresses otherwise.

### Tailscale Resolution

Resolution order:

1. Run `tailscale ip -4`.
2. Run `tailscale status --json` for MagicDNS display name.
3. If IP lookup fails, return `BindResolutionError::TailscaleUnavailable`.

The resolver must not require Tailscale for non-Tailscale bind targets.

### Private IP Resolution

`private` is intentionally stricter than `0.0.0.0`:

- Discover non-loopback RFC1918 IPv4 addresses.
- If exactly one exists, use it.
- If multiple exist, return an ambiguous-address error and ask for explicit config.
- If none exist, return not found.

## TLS Config Component

### Responsibilities

- Parse listener-level TLS policy from local config.
- Validate server certificate, key, and client CA path requirements.
- Keep certificate material out of repository files and logs.
- Provide a normalized policy to the listener manager.

### Modes

```text
TlsMode::Off
TlsMode::Tls
TlsMode::Mtls
```

### Policy

```text
ListenerTlsPolicy {
  mode: TlsMode,
  server_cert: Option<PathBuf>,
  server_key: Option<PathBuf>,
  client_ca: Option<PathBuf>
}
```

All routes sharing the same listener key must use the same TLS policy. A registration that would change TLS mode or certificate paths for an already-running listener must fail with a listener policy conflict.

### Event Log Integration

Access events should later include:

- TLS mode.
- Client certificate subject when available.
- Client certificate fingerprint when available.

Do not store PEM bodies or private key contents in access events.

## Control API Component

### Responsibilities

- Provide private CLI-to-daemon communication.
- Serialize structured requests and responses.
- Preserve daemon-side error codes.

### Transport

First-version default should be Unix domain socket with JSON or MessagePack payloads. JSON is easier to inspect; MessagePack can be deferred.

Socket path:

```text
$XDG_RUNTIME_DIR/serve-lib/daemon.sock
/tmp/serve-lib-$UID/daemon.sock fallback
```

### Operations

```text
Status() -> DaemonStatus
Register(RegisterRequest) -> RegisterResponse
Deregister(DeregisterRequest) -> DeregisterResponse
List(ListRequest) -> ListResponse
ListEvents(ListEventsRequest) -> ListEventsResponse
Shutdown() -> ShutdownResponse
```

### Error Shape

```text
ControlError {
  code: ErrorCode,
  message: String,
  details: Map<String, String>
}
```

Error codes should include:

- `DaemonUnavailable`
- `InvalidRequest`
- `BindResolutionFailed`
- `PortUnavailable`
- `RouteConflict`
- `PathNotFound`
- `PathNotReadable`
- `MountNotFound`
- `EventLogUnavailable`
- `Internal`

## Daemon Runtime Component

### Responsibilities

- Start the control API server.
- Own shared daemon state.
- Supervise listener tasks.
- Supervise timeout scheduler tasks.
- Supervise event log cleanup task.
- Shut down cleanly.

### State

```text
DaemonState {
  registry: RegistryManager,
  listeners: ListenerManager,
  scheduler: TimeoutScheduler,
  event_log: EventLogStore,
  event_cleanup: EventLogCleanupWorker
}
```

The daemon should use one state mutation path for register, deregister, and timeout expiry. That avoids inconsistent cleanup logic.

### Lifecycle

Start:

1. Acquire daemon lock or detect existing daemon.
2. Create control socket.
3. Open SQLite event log database and run migrations.
4. Start event log cleanup worker.
5. Start control API server.
6. Initialize empty registry.

Stop:

1. Stop accepting control requests.
2. Close HTTP listeners.
3. Cancel timers and cleanup worker.
4. Flush event log writes.
5. Remove control socket.

## Registry Manager Component

### Responsibilities

- Store active route mounts.
- Validate route conflicts.
- Provide lookup data to routers.
- Support list and diagnostics.

### Internal Shape

```text
Registry {
  mounts_by_id: Map<MountId, RouteMount>,
  mounts_by_listener: Map<ListenerKey, RouteTable>
}
```

### Route Conflict Rules

Reject:

- Same listener, same route.
- Route that differs only by trailing slash.
- Route that normalizes to an existing route.

Allow:

- Same port on different bind address.
- Same local path on different routes.
- Nested routes when longest-prefix matching is unambiguous.

Example:

```text
/app       -> /srv/app
/app/api   -> /srv/api-fixtures
```

This is allowed because longest-prefix matching makes `/app/api/users.json` route to `/app/api`.

### Route Normalization

- Always start with `/`.
- Remove duplicate slashes.
- Remove trailing slash except root.
- Reject `.` and `..` segments.
- Reject percent-decoded traversal segments.

## Timeout Scheduler Component

### Responsibilities

- Track expiration times.
- Trigger deregistration when a mount expires.
- Avoid orphan timers when a mount is deregistered manually.

### Model

Each mount with `expires_at` gets one scheduled task or heap entry.

On expiry:

```text
TimeoutScheduler -> DaemonRuntime -> RegistryManager.remove(mount_id, reason=expired)
                                -> ListenerManager.reconcile(listener_key)
                                -> EventLog.record(expired)
```

Manual deregistration should cancel pending expiry.

### Testing

Use injectable clock or short controlled durations in integration tests. Do not rely on long sleeps.

## Listener Manager Component

### Responsibilities

- Own HTTP listener sockets.
- Start listeners when the first route appears.
- Stop listeners when the last route disappears.
- Provide route snapshots to request handlers.

### Listener Key

```text
ListenerKey {
  bind_addr: IpAddr,
  port: u16
}
```

### Reconciliation

The listener manager should reconcile from desired state:

```text
desired listener keys = registry.listener_keys()
actual listener keys = running listener tasks
```

It starts missing listeners and stops extra listeners.

When a listener uses TLS or mTLS, the listener manager also owns the TLS acceptor. Certificate loading errors should fail listener startup and roll back the route registration.

### Port Conflicts

If the port is already taken:

- Registration should fail.
- Registry mutation should roll back.
- Error should identify bind address and port.

### TLS Policy Conflicts

If the listener already exists with a different TLS policy:

- Registration should fail.
- Registry mutation should roll back.
- Error should identify the listener key and the conflicting policy names.

## Route Router Component

### Responsibilities

- Match incoming URL paths to route mounts.
- Use longest-prefix matching.
- Produce the remaining path relative to the mount root.

### Request Match

```text
RouteMatch {
  mount: RouteMountSnapshot,
  relative_url_path: NormalizedRelativePath
}
```

### Matching Rules

- `/app` matches `/app` and `/app/...`.
- `/app` does not match `/application`.
- `/` matches anything if no more specific route exists.
- More specific routes win.

## Static File Service Component

### Responsibilities

- Map matched URL paths to local filesystem paths.
- Serve files.
- Serve index HTML for directories when present.
- Serve generated directory listing when no index exists.
- Serve SPA fallback when configured.
- Render Markdown and supported source files as HTML when mount config enables it.
- Prevent path traversal.
- Emit access events after a response decision is made.

### Directory Resolution

For a matched route and relative path:

1. Join `mount.local_root` with normalized relative path.
2. Ensure the final path stays inside `mount.local_root`.
3. If final path is a file, serve file.
4. If final path is a directory, check `index_file`.
5. If `index_file` exists, serve it.
6. Otherwise render directory listing.
7. If final path does not exist and `spa` is true, serve root index file.
8. Otherwise return 404.

### Rendered Files

Rendering is opt-in through config and becomes part of the mount policy sent to
the daemon. When disabled, files are returned with their normal content type and
bytes.

When enabled:

- Markdown extensions `.md` and `.markdown` render to an HTML page through `pulldown-cmark`.
- Source extensions such as `.js`, `.ts`, `.rs`, `.py`, `.sh`, `.json`, `.toml`, `.yaml`, and `.html` render to syntax-highlighted HTML through `syntect`.
- Rendered responses use `text/html; charset=utf-8`.
- Rendering does not change path safety checks, route matching, or event logging.

### Access Event Emission

For every HTTP request, the static file service or surrounding request middleware should emit one access event after response classification:

```text
AccessEvent {
  occurred_at: Timestamp,
  listener: ListenerKey,
  mount_id: Option<MountId>,
  route: Option<NormalizedRoute>,
  method: String,
  request_path: String,
  local_path: Option<PathBuf>,
  status: u16,
  bytes_sent: Option<u64>,
  remote_addr: SocketAddr,
  user_agent: Option<String>,
  tls_mode: Option<TlsMode>,
  client_cert_subject: Option<String>,
  client_cert_fingerprint: Option<String>,
  outcome: AccessOutcome
}
```

`request_path` should omit query strings by default. `local_path` should only be recorded after the path has passed safety checks.

### Path Safety

The service should avoid trusting raw URL paths:

- Percent-decode carefully.
- Reject null bytes.
- Normalize separators.
- Reject `..`.
- Canonicalize where possible.
- Do not allow symlink escape outside root in v1.

### Directory Listing

First version listing should include:

- File name.
- File type indicator.
- Size.
- Modified time.
- Link URL.

Generated HTML should be simple and deterministic for tests.

## Event Log Store Component

### Responsibilities

- Store route lifecycle and HTTP access events in SQLite.
- Run schema migrations on daemon start.
- Provide query APIs for `serve-lib events`.
- Provide append APIs that do not block request serving for long periods.
- Feed `daemon status` and diagnostics.

Events:

- daemon started/stopped
- listener opened/closed
- route registered/deregistered
- route expired
- HTTP access served
- HTTP access denied
- HTTP not found
- HTTP serve error
- bind resolution failed
- serve denied
- serve error

### Storage

Default database paths:

```text
$XDG_DATA_HOME/serve-lib/events.sqlite
~/.local/share/serve-lib/events.sqlite
~/Library/Application Support/serve-lib/events.sqlite
```

The database path can be overridden in local config for tests and advanced setups.

### Schema

Initial schema can use one append-only `events` table:

```sql
CREATE TABLE events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  occurred_at TEXT NOT NULL,
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

CREATE INDEX idx_events_occurred_at ON events (occurred_at);
CREATE INDEX idx_events_route ON events (route);
CREATE INDEX idx_events_remote_ip ON events (remote_ip);
CREATE INDEX idx_events_status ON events (status);
```

This schema is intentionally general. If access volume becomes high, access events and lifecycle events can split into separate tables later.

### Write Path

Request serving should not be held hostage by slow SQLite writes.

Preferred first-version design:

- Request handlers send event structs to a bounded async channel.
- A single event writer task batches inserts into SQLite.
- If the channel is full, drop low-priority access events and increment a dropped-event counter.
- Lifecycle events should have higher priority than access events.

### Query Path

`serve-lib events` should support basic filters:

- `--since <duration>`
- `--route <subpath>`
- `--status <code>`
- `--remote-ip <ip>`
- `--kind <event-kind>`

The first version can limit result count for CLI display, but retention is still TTL-based, not count-based.

## Event Log Cleanup Worker Component

### Responsibilities

- Periodically delete events older than retention TTL.
- Keep SQLite storage bounded over time.
- Emit cleanup metrics or events.

### Configuration

```text
event_log.retention = "7d"
event_log.cleanup_interval = "1h"
```

Retention is a time-to-live policy:

```sql
DELETE FROM events WHERE occurred_at < cutoff;
```

The design should not use a max-number-of-events policy as the primary retention mechanism.

### Failure Handling

- Cleanup failure should be logged but should not stop serving.
- Repeated cleanup failures should appear in daemon status.
- Invalid retention config should fail daemon startup because it changes storage behavior.

## Library API Surface

The library should expose reusable pieces without forcing callers to run the CLI.

Candidate public modules:

```text
serve_lib::config
serve_lib::bind
serve_lib::registry
serve_lib::router
serve_lib::static_files
serve_lib::control
serve_lib::daemon
serve_lib::events
```

Initial stable-ish public types:

```text
RegisterRequest
DeregisterRequest
ListResponse
BindTarget
ResolvedBind
RouteMount
MountId
ServeError
ServeEvent
EventLogConfig
TlsMode
ListenerTlsPolicy
```

The HTTP framework should remain an implementation detail unless embedding requires explicit hooks.

## Test Plan

### Unit Tests

- Config precedence.
- Duration parsing.
- Bind target parsing.
- TLS profile validation.
- Route normalization.
- Route conflict detection.
- Longest-prefix route matching.
- Path traversal rejection.
- Index file resolution.
- Event log retention cutoff calculation.
- Event schema serialization.

### Integration Tests

- Start daemon, register route, fetch file.
- Register two routes on one port.
- Deregister one route without stopping the other.
- Expire route after timeout.
- Bind to loopback in CI.
- SPA fallback serves root index.
- Directory listing only appears when index file is absent.
- Control socket unavailable produces useful CLI error.
- Register, deregister, access, and expiry events are stored in SQLite.
- Event cleanup deletes rows older than retention TTL.
- Event logging records remote IP and avoids query strings by default.
- mTLS config validation rejects missing client CA.

### Manual Tests

- Tailscale bind on a real machine.
- MagicDNS display URL.
- Private IP ambiguity handling.
- Large file serving.
- Symlink behavior.

## Implementation Order

1. Core types and error model.
2. Config store and precedence resolver.
3. Route normalization and registry manager.
4. Static file service with path safety and index file behavior.
5. Listener manager on loopback only.
6. Control API and daemon runtime.
7. CLI commands.
8. Timeout scheduler.
9. SQLite event log store and cleanup worker.
10. Tailscale/private bind resolver.
11. TLS/mTLS config validation and listener policy model.
12. Full integration tests.

This order keeps high-risk routing and filesystem safety testable before daemon process management adds complexity.
