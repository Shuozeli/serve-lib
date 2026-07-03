# serve-lib Implementation Plan

## Principles

- Build from pure core logic outward to daemon process behavior.
- Keep components independently testable before wiring them into the daemon.
- Avoid Tailscale-specific requirements in early CI by using loopback/private bind tests first.
- Keep repository defaults generic and machine-neutral.
- Use SQLite for event history from the first daemon milestone, not as a later bolt-on.

## Milestone 0: Workspace Scaffold

### Goal

Create a Rust workspace with stable module boundaries and no runtime behavior yet.

Status: complete.

### Tasks

- Create `Cargo.toml` workspace.
- Create `crates/serve-lib-core`.
- Create `crates/serve-lib-daemon`.
- Create `crates/serve-lib-cli`.
- Add top-level `README.md`, docs, and license decision.
- Add formatting and lint baseline.
- Add minimal CI-ready test command documentation.

### Acceptance Criteria

- `cargo check --workspace` passes.
- `cargo test --workspace` passes with placeholder tests.
- No generated build artifacts are committed.

## Milestone 1: Core Types And Error Model

### Goal

Define the shared types used by config, registry, control API, daemon, and tests.

Status: complete for first pass.

### Tasks

- Define `BindTarget`.
- Define `ResolvedBind`.
- Define `ListenerKey`.
- Define `MountId`.
- Define `NormalizedRoute`.
- Define `RouteMount`.
- Define `DurationSpec`.
- Define `ServeError` and component-specific error codes.
- Define request/response DTOs:
  - `RegisterRequest`
  - `RegisterResponse`
  - `DeregisterRequest`
  - `DeregisterResponse`
  - `ListResponse`
  - `DaemonStatus`
  - `ServeEvent`

### Acceptance Criteria

- Types serialize and deserialize where needed.
- Error codes are stable enough for CLI display and tests.
- Unit tests cover basic parsing and validation.

## Milestone 2: Config Store

### Goal

Implement local config path discovery, TOML parsing, validation, and precedence resolution.

Status: partially complete. Schema, TOML parsing, validation, event log config, and precedence resolution are implemented. Filesystem path discovery and config mutation helpers are still pending.

### Tasks

- Implement config path resolution:
  - `SERVE_LIB_CONFIG`
  - `$XDG_CONFIG_HOME/serve-lib/config.toml`
  - `~/.config/serve-lib/config.toml`
  - macOS application support path
- Implement `LocalConfig`, `DefaultConfig`, `ProfileConfig`, and `EventLogConfig`.
- Implement default config generation.
- Implement profile lookup.
- Implement merge precedence:
  - built-in defaults
  - local config defaults
  - selected profile
  - CLI overrides
- Implement config mutation helpers for `config set`.
- Validate:
  - ports
  - durations
  - index file names
  - profile uniqueness
  - event log retention and cleanup interval

### Acceptance Criteria

- Unit tests cover config path override.
- Unit tests cover precedence resolution.
- Unit tests cover invalid config errors.
- No real private IP or MagicDNS value appears in test fixtures unless clearly fake.

## Milestone 3: Route Normalization And Registry

### Goal

Implement the active mount registry without networking.

Status: complete for first pass.

### Tasks

- Implement route normalization.
- Implement route conflict detection.
- Implement route insertion.
- Implement route removal by listener+route and by mount id.
- Implement listing snapshots.
- Implement longest-prefix match table generation.
- Implement rollback-friendly register operation for future listener failures.

### Acceptance Criteria

- `/app` matches `/app` and `/app/...`, not `/application`.
- `/app/api` wins over `/app`.
- Duplicate normalized routes are rejected.
- Same route on different listener keys is allowed.
- Same local path on different routes is allowed.

## Milestone 4: Static File Service

### Goal

Serve files from a matched route safely without daemon or listener management.

Status: complete for first pass.

### Tasks

- Implement URL path decoding and normalization.
- Implement local path join under mount root.
- Reject traversal attempts.
- Reject null bytes.
- Implement symlink escape policy for v1.
- Serve regular files.
- Serve `index.html` for directory requests when present.
- Render directory listing when no index file exists.
- Implement route-level custom `index` filename.
- Implement route-level SPA fallback.
- Detect content type.
- Return structured serve outcomes for event logging.

### Acceptance Criteria

- Unit tests cover traversal rejection.
- Unit tests cover file serving.
- Unit tests cover index file priority.
- Unit tests cover directory listing fallback.
- Unit tests cover SPA fallback.
- Unit tests cover custom index file.

## Milestone 5: SQLite Event Log

### Goal

Implement durable local event history before daemon wiring.

Status: complete for first pass. Synchronous SQLite store, migration, append, query filters, lifecycle/access events, and TTL cleanup API are implemented. Async writer/channel behavior remains part of daemon wiring.

### Tasks

- Implement event database path resolution:
  - config override
  - `$XDG_DATA_HOME/serve-lib/events.sqlite`
  - `~/.local/share/serve-lib/events.sqlite`
  - macOS application support path
- Implement SQLite migrations.
- Implement event append API.
- Implement lifecycle event types.
- Implement access event types.
- Implement query API for recent events.
- Implement retention cleanup API.
- Avoid query string storage by default.

### Acceptance Criteria

- Event schema migration is idempotent.
- Register/deregister/access event structs can be stored and queried.
- Cleanup deletes events older than retention TTL.
- Cleanup does not use max-row-count retention as the primary policy.
- Tests use temporary SQLite databases.

## Milestone 6: Bind Resolver

Status: complete, first pass. The core resolver supports explicit IP, loopback, any-address,
Tailscale command resolution through an injectable command runner, private-address ambiguity
detection from supplied candidates, and configured interface-name resolution. Runtime interface
discovery will be wired in when daemon networking is introduced.

### Goal

Resolve logical bind targets into socket addresses and display hosts.

### Tasks

- Parse explicit IP addresses.
- Implement loopback and any-address targets.
- Implement Tailscale resolver:
  - `tailscale ip -4`
  - `tailscale status --json`
- Implement private IP resolver with ambiguity detection.
- Implement injectable command runner for tests.
- Keep display host separate from bind address.

### Acceptance Criteria

- Unit tests do not require Tailscale installed.
- Tailscale command output is tested with fixtures.
- Multiple private IPs produce an ambiguity error.
- Non-Tailscale bind targets do not invoke Tailscale.

## Milestone 7: Listener Manager And HTTP Router

Status: complete, first pass. A synchronous listener manager opens HTTP
listeners on demand, serves registered static files, supports multiple routes
on one listener, records access events, and closes listeners when their final
route is removed. TLS policy conflict checks exist, and TLS/mTLS socket
acceptance is handled by rustls for manually configured certificates.

### Goal

Open HTTP listeners and route requests to mounted paths.

### Tasks

- Choose HTTP stack.
- Choose TLS stack for listener-level TLS/mTLS.
- Implement listener key to server task mapping.
- Implement listener TLS policy conflict checks.
- Implement desired-vs-actual listener reconciliation.
- Start listener when first route is added.
- Stop listener when last route is removed.
- Implement request router using registry snapshots.
- Emit access events for every request.
- Handle port conflicts with rollback.

### Acceptance Criteria

- Integration test registers two routes on one loopback port.
- Deregistering one route leaves the other route available.
- Last deregister closes the listener.
- Port conflict returns a clear error.
- Access events include remote IP and status code.
- TLS policy conflicts fail cleanly once TLS policy model exists.

## Milestone 8: Timeout Scheduler

Status: complete, first pass. The daemon runtime starts a background timeout
scheduler that expires routes, removes them from the registry, closes empty
listeners, and writes `route_expired` events.

### Goal

Expire route registrations automatically.

### Tasks

- Implement scheduler data structure.
- Schedule timeout on register.
- Cancel timeout on manual deregister.
- Route expiry through the same deregister path.
- Record `route_expired` events.
- Support injectable clock or test-friendly short durations.

### Acceptance Criteria

- Expired route disappears from registry.
- Expired route stops serving.
- Expiry does not stop other routes on same listener.
- Expiry event is written to SQLite.

## Milestone 9: Event Cleanup Worker

Status: complete, first pass. The daemon runtime starts a background cleanup
worker using runtime retention and cleanup interval settings. Config-file path
and retention wiring through CLI defaults remains pending.

### Goal

Keep SQLite storage bounded by retention TTL.

### Tasks

- Start cleanup worker on daemon startup.
- Use `event_log.retention`.
- Use `event_log.cleanup_interval`.
- Delete events older than retention cutoff.
- Record cleanup diagnostics.
- Surface repeated cleanup failures in daemon status.

### Acceptance Criteria

- Cleanup runs periodically.
- Cleanup removes only expired events.
- Cleanup failures do not stop HTTP serving.
- Invalid retention config fails daemon startup.

## Milestone 10: Control API And Daemon Runtime

Status: complete, first pass. The daemon runtime now wires registry, listener
manager, timeout scheduler, event log, and a private HTTP JSON control API.
The control API currently binds to a user-specified local address and supports
status, list, register, deregister, events, and shutdown.

### Goal

Wire registry, listener, timeout, event log, and config into a daemon.

### Tasks

- Implement daemon lock.
- Implement control socket path.
- Implement control API server.
- Implement register flow:
  - validate request
  - resolve bind
  - add registry mount
  - reconcile listener
  - schedule timeout
  - write event
- Implement deregister flow.
- Implement list flow.
- Implement status flow.
- Implement shutdown flow.
- Cleanly close listeners, timers, event writer, and control socket.

### Acceptance Criteria

- `daemon status` works.
- `register` changes daemon state.
- `list` reflects active routes.
- `deregister` changes daemon state.
- Daemon shutdown cleans up socket.
- Integration tests do not require external network access.

## Milestone 11: CLI

Status: complete, first pass. The `serve-lib` binary exposes daemon
run/start/stop/status, register, deregister, list, and events commands. Config
file mutation commands and polished output modes are still pending.

### Goal

Expose the daemon behavior through user-facing commands.

### Tasks

- Implement command parser.
- Implement config commands.
- Implement daemon lifecycle commands.
- Implement register command.
- Implement deregister command.
- Implement list command.
- Implement events command.
- Add table output.
- Add JSON output where useful.
- Add precise exit codes.

### Acceptance Criteria

- CLI help is accurate.
- Register works with config defaults.
- CLI flags override config defaults.
- Events command can show recent access and lifecycle events.
- Error messages distinguish invalid config, daemon unavailable, and daemon rejection.

## Milestone 12: End-To-End Tests

### Goal

Prove the system works through real CLI and daemon flows.

Status: Docker e2e runs in a clean Linux container. It runs `cargo test --workspace --no-fail-fast`, runs `cargo build --workspace --release`, writes a temporary local config, generates temporary TLS materials, starts the daemon, and exercises real CLI/daemon flows for multi-route same-port serving, route conflict errors, timeout expiry, directory listing, SPA fallback, config defaults and profiles, SQLite event persistence across daemon restart, TTL retention cleanup, config-gated Markdown rendering, config-gated code highlighting, TLS serving, mTLS client-certificate enforcement, listener TLS policy conflicts, event visibility, deregistration, and shutdown.

### Tasks

- Test daemon start/status/stop.
- Test register file.
- Test register directory.
- Test index HTML serving.
- Test directory listing fallback.
- Test SPA fallback.
- Test two routes on one port.
- Test timeout expiry.
- Test SQLite event logging.
- Test event retention cleanup.
- Test config defaults.
- Test no machine-specific docs or fixtures.

### Acceptance Criteria

- End-to-end tests pass on loopback.
- Tailscale-specific behavior is covered by unit tests with mocked command output.
- Tests do not bind privileged ports.
- Tests clean up temp files, sockets, and SQLite databases.

## Milestone 13: Documentation And Release Readiness

### Goal

Make the first usable version understandable and maintainable.

### Tasks

- Update README quickstart.
- Document config file.
- Document event log retention.
- Document security model.
- Document manual TLS and mTLS setup.
- Document Tailscale setup without real hostnames.
- Document troubleshooting.
- Add architecture diagrams if needed.
- Add changelog.

### Acceptance Criteria

- A new user can start daemon, register a path, access it, list events, and deregister from docs.
- Docs contain no private machine names, private personal hostnames, or real Tailscale tailnet suffixes.

## Cross-Cutting Work

### Security

- Path traversal tests in every file-serving milestone.
- Control API must never bind on public serving listener.
- Query strings are not stored in event log by default.
- Public bind targets require explicit config or CLI flags.
- Certificate material is never committed or logged.
- mTLS client identity logging stores subject/fingerprint only.

### Observability

- Structured daemon logs.
- Event log query command.
- Dropped event counter when event channel is full.
- Daemon status includes listener count, route count, event writer health, and cleanup health.

### Performance

- Avoid blocking request handling on SQLite writes.
- Use bounded channels for event writes.
- Keep route matching snapshot cheap for common request paths.
- Avoid re-canonicalizing mount root on every request.

### Open Implementation Choices

- HTTP stack: `axum`, `hyper`, or lower-level `hyper-util`.
- TLS stack: `rustls` directly, `tokio-rustls`, or integration through chosen HTTP stack.
- Control API payload: JSON first or binary from the start.
- SQLite crate: `rusqlite` with blocking writer task or async wrapper.
- Daemon start mechanism: foreground daemon command first, backgroundize later.
- Symlink policy: reject escape only, or reject all symlinks in v1.
