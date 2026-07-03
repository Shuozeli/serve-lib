# serve-lib Architecture

## Overview

`serve-lib` is a daemon-backed private file serving system. It separates control-plane actions from data-plane serving:

- The CLI is a short-lived control client.
- The daemon is a long-running process that owns listeners, route state, timeout timers, and request handling.
- Local config supplies machine-specific defaults such as bind target, preferred port, timeout, index file behavior, and URL display preferences.
- HTTP listeners serve files from registered local roots through URL subpaths.
- A local SQLite database stores route lifecycle and HTTP access events with TTL-based cleanup.
- Optional listener-level TLS and mTLS policies protect deployments that need encrypted transport or client certificate identity.

The first version should be local-machine only. It should not attempt public tunneling, remote registry sync, or TLS certificate automation.

## High-Level Diagram

```text
User
  |
  | serve-lib register ./dist --route /app
  v
CLI
  |
  | load local config
  | resolve command defaults
  | send control request
  v
Control Client  ---- Unix socket / local control API ---->  Daemon
                                                            |
                                                            | validate request
                                                           | update registry
                                                            | reconcile listeners
                                                            | write event log
                                                            v
                                                   Listener Manager
                                                            |
                                +---------------------------+---------------------------+
                                |                                                       |
                         100.64.x.y:8088                                         192.168.x.y:8090
                                |                                                       |
                                v                                                       v
                         Route Router                                             Route Router
                                |                                                       |
                         Static File Service                                      Static File Service
```

## Process Model

### CLI Process

The CLI is short-lived and should not open public serving sockets. It is responsible for:

- Parsing user commands.
- Loading local config.
- Resolving defaults for omitted flags.
- Formatting control API requests.
- Rendering human-readable output.
- Returning precise exit codes.

The CLI should be able to fail before contacting the daemon when user input is invalid. It should not duplicate daemon-side validation for filesystem security or route conflict rules.

### Daemon Process

The daemon is the authority for runtime state. It is responsible for:

- Owning all HTTP listeners.
- Owning the active route registry.
- Validating canonical paths.
- Enforcing route conflict rules.
- Scheduling timeout expiry.
- Serving files and directory/index responses.
- Recording route lifecycle and HTTP access events in SQLite.
- Cleaning old event log rows according to retention TTL.
- Reporting state through the control API.

The daemon keeps the first-version registry in memory. If the daemon exits, active routes are lost. Persistence can be added later after the runtime model is stable.

## Data Model

### Logical Bind Target

A logical bind target is a user-facing bind selection before it becomes an IP address:

```text
tailscale
private
0.0.0.0
127.0.0.1
192.168.1.24
```

Logical bind targets are allowed in config and CLI flags. The daemon must resolve them to concrete socket bind addresses before opening listeners.

### Listener Key

A listener is keyed by the resolved bind address and port:

```text
ListenerKey {
  bind_addr: IpAddr,
  port: u16
}
```

Multiple routes can share one listener key.

### Listener Security Policy

A listener can have a security policy:

```text
ListenerSecurity {
  mode: Plain | Tls | Mtls,
  server_cert: Option<PathBuf>,
  server_key: Option<PathBuf>,
  client_ca: Option<PathBuf>
}
```

The listener key remains bind address plus port. TLS policy is listener configuration and must be consistent for all routes sharing the same listener. If a registration tries to reuse an existing listener with a different TLS policy, the daemon should reject it as a listener policy conflict.

### Route Mount

A route mount maps an HTTP subpath to one canonical local root:

```text
RouteMount {
  id: MountId,
  listener: ListenerKey,
  route: NormalizedRoute,
  local_root: CanonicalPath,
  index_file: String,
  spa: bool,
  readonly: bool,
  expires_at: Option<SystemTime>,
  display_name: Option<String>
}
```

The route is a URL path prefix such as `/app` or `/downloads`. File serving must never escape `local_root`.

## Control Plane

The control plane is the private API between CLI and daemon.

First-version preference:

- Unix domain socket on Unix-like systems.
- A platform-specific named pipe or localhost-only fallback can be evaluated later.

Control API operations:

- `DaemonStatus`
- `RegisterMount`
- `DeregisterMount`
- `ListMounts`
- `GetConfigDiagnostics`
- `ShutdownDaemon`

The control API must not be served on any public data-plane listener.

## Data Plane

The data plane is normal HTTP file serving.

Request flow:

1. Accept HTTP request on a listener.
2. Normalize and decode URL path safely.
3. Match the longest registered route prefix for that listener.
4. Translate the remaining path into a local filesystem path under the mount root.
5. If the target is a directory, serve index file first when present.
6. If no index file exists, render a directory listing.
7. If the path is missing and SPA fallback is enabled, serve the mount's index file.
8. Return 404, 403, or 500 with clear structured logging when serving is not possible.

## Component Map

```text
serve-lib-cli
  - command parser
  - local config loader
  - control API client
  - output formatter

serve-lib-daemon
  - daemon lifecycle
  - control API server
  - registry manager
  - listener manager
  - timeout scheduler
  - event log store
  - event log cleanup worker
  - request router
  - static file service

serve-lib-core
  - config schema
  - command/request DTOs
  - bind resolver
  - route normalization
  - path safety helpers
  - duration parsing
  - shared error model
```

The actual crate layout can change, but these boundaries should stay stable.

## Configuration Flow

Configuration is local and machine-specific.

```text
repo defaults
  |
  v
built-in safe defaults
  |
  v
local config file
  |
  v
selected profile
  |
  v
CLI flags
  |
  v
control request
```

Precedence rules:

1. CLI flags have highest precedence.
2. Selected profile overrides global defaults.
3. Local config defaults override built-in defaults.
4. Built-in defaults must be safe and generic.

No private IP, MagicDNS hostname, or personal machine name should be committed to repo defaults or docs.

Event log settings are also local config. They should configure retention TTL, cleanup interval, and optional database path. They should not be expressed as a max row count because event volume varies by served workload.

Render settings are local config as well. They control whether Markdown and
known source files are transformed into HTML before serving. Rendering must be
opt-in so asset-serving workflows can continue to receive raw JavaScript,
Markdown, and other text files.

## Event Log Flow

The event log is a local SQLite database owned by the daemon.

```text
Control request
  |
  +-- register/deregister/status failure events
  v
Event Log Store

HTTP request
  |
  +-- access/denied/not_found/error events
  v
Event Log Store

Cleanup Worker
  |
  +-- delete events older than retention TTL
  v
SQLite
```

Default database path should live under the platform data directory:

```text
$XDG_DATA_HOME/serve-lib/events.sqlite
~/.local/share/serve-lib/events.sqlite
~/Library/Application Support/serve-lib/events.sqlite
```

The event log should record enough information to answer:

- Who accessed a route and when?
- Which route was registered, deregistered, or expired?
- Which bind and port served a request?
- Which remote IP made a request?
- How often do 404/403/500 responses happen?

Access logging should avoid storing query strings by default. Future versions can expose explicit opt-in query logging if needed.

## Listener Reconciliation

The daemon should treat listeners as derived state from the registry.

On register:

1. Add the route to the registry.
2. If no listener exists for the listener key, open one.
3. If a listener exists, verify the requested TLS policy matches the existing listener policy.
4. Attach or update that listener's route router.
5. Record a `route_registered` event.

On deregister:

1. Remove the route from the registry.
2. Update the listener's router.
3. If no routes remain for the listener key, close the listener.
4. Record a `route_deregistered` event.

On timeout expiry:

1. Mark the route expired.
2. Deregister it through the same path as explicit deregistration.
3. Record a `route_expired` event.

## Failure Boundaries

Expected failures should return explicit errors:

- Config file parse error.
- Event database open or migration failure.
- Unknown profile.
- Bind target cannot be resolved.
- Port is already owned by another process.
- Route conflicts with an existing mount.
- Local path does not exist.
- Local path is not readable.
- URL path attempts traversal outside mount root.
- Control socket unavailable.
- Daemon not running.

The CLI should distinguish daemon-not-running from request-rejected-by-daemon.

## Security Model

First-version security is based on bind scope and filesystem safety.

Required protections:

- Do not bind to public interfaces unless explicitly configured.
- Normalize routes before insertion.
- Decode URL paths carefully.
- Reject path traversal attempts.
- Canonicalize local roots at registration time.
- Re-check filesystem metadata at serve time.
- Do not follow symlinks outside the mount root unless a future explicit policy allows it.
- Do not expose the control API through HTTP serving ports.
- When TLS is enabled, load server certificate and key only from local absolute paths.
- When mTLS is enabled, require a client CA and reject clients without a valid certificate.
- Do not log certificate PEM material.
- If client certificate identity is logged, store only subject and/or fingerprint.

Authentication is intentionally deferred but should be designed as a future route or listener policy.

## Observability

The daemon should provide basic operational visibility:

- `serve-lib daemon status`
- `serve-lib list`
- Human-readable register/deregister output.
- Structured logs for listener open/close, route add/remove, timeout expiry, and serve errors.
- SQLite event history for route lifecycle and HTTP access events.
- Configurable event log retention TTL and cleanup interval.

## First-Version Decisions

- Registry is in-memory.
- Event log is SQLite-backed.
- Event log cleanup is TTL-based.
- Serving is read-only.
- Plain HTTP is the default.
- TLS and mTLS are manually configured listener-level policies.
- Config is local TOML.
- Route matching uses longest-prefix matching.
- Directory requests prefer index file before generated listing.
- SPA fallback is opt-in per mount.
- Tailscale is a logical bind target resolved locally.
- Published docs and examples use placeholder hostnames only.

## Deferred Work

- Persistent registry across daemon restarts.
- Authentication.
- Upload support.
- TLS certificate automation and Tailscale Serve integration.
- WebDAV.
- Multiple users sharing one daemon.
- Remote management.
- Public tunneling.
