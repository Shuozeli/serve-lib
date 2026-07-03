# serve-lib Feature Requirements

## Purpose

`serve-lib` provides a daemon-managed file serving layer for private and local networks. It should make temporary file and directory sharing easy without forcing the user to keep a foreground terminal process alive for every share.

The first target is a Rust implementation with a reusable library, a daemon binary, and a CLI control client.

## Product Model

### Background Daemon

The daemon is the long-running process that owns HTTP listeners and the active serve registry.

Requirements:

- Run as a background process started explicitly by the user or by a helper command.
- Maintain an in-memory registry of active serve mounts.
- Optionally persist registry state later, but persistence is not required for the first version.
- Open and close network listeners based on registered ports.
- Support multiple registered mounts on the same port.
- Expose a local control API for the CLI.
- Provide enough introspection for `list`, health checks, and debugging.

Non-goals for the first version:

- Public internet tunneling.
- NAT traversal.
- Multi-machine registry replication.
- TLS certificate automation.

### CLI Control Client

The CLI is the user-facing control plane for the daemon.

Required commands:

```text
serve-lib daemon start
serve-lib daemon stop
serve-lib daemon status

serve-lib config show
serve-lib config set default-bind <bind-target>
serve-lib config set default-port <port>

serve-lib register <local-path> --route <subpath> [--port <port>] [--bind <ip-or-interface>] [--timeout <duration>]
serve-lib deregister --port <port> --route <subpath>
serve-lib list
```

Possible short aliases can be added later, but the initial command names should stay explicit.

### Register

`register` adds a local path to the daemon registry.

Required inputs:

- `local-path`: file or directory to serve.
- `--route`: URL subpath for the mount, such as `/`, `/logs`, `/builds/app`.

Optional inputs:

- `--port`: TCP port owned by the daemon listener; falls back to local config.
- `--bind`: target bind address or interface hint.
- `--timeout`: registration lifetime.
- `--index`: index file name to serve for directory roots, default `index.html`.
- `--spa`: serve the index file for missing paths under this route.
- `--readonly`: default true in the first version.
- `--name`: human-readable label for `list` output.

Behavior:

- If no listener exists for the selected bind+port pair, the daemon opens one.
- If a listener already exists, the daemon adds the route to that listener's router.
- The same bind+port can serve multiple routes.
- Route conflicts must be rejected unless an explicit replace flag is added later.
- The daemon validates that `local-path` exists at registration time.
- The daemon canonicalizes local paths before storing them.

### Local Configuration

`serve-lib` should behave like developer tools that keep local user defaults in a config directory. Users should not need to repeat `--bind tailscale` or a preferred port on every registration.

Config requirements:

- Store user config outside the repository, using the platform config directory.
- Linux default: `$XDG_CONFIG_HOME/serve-lib/config.toml` or `~/.config/serve-lib/config.toml`.
- macOS default: `~/Library/Application Support/serve-lib/config.toml`.
- Support a `SERVE_LIB_CONFIG` override for tests and advanced workflows.
- Never publish machine-specific hostnames, private IPs, or MagicDNS names in repository defaults.
- Keep repo examples generic, using placeholder hostnames such as `machine.tailnet.example.ts.net`.

Initial config shape:

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

[[tls_profiles]]
name = "private-mtls"
mode = "mtls"
server_cert = "/absolute/path/server.crt"
server_key = "/absolute/path/server.key"
client_ca = "/absolute/path/client-ca.crt"

[[profiles]]
name = "mac"
bind = "192.168.1.24"
port = 8088

[[profiles]]
name = "tailscale"
bind = "tailscale"
port = 8088
tls_profile = "private-mtls"
```

CLI behavior:

- `register` uses config defaults when `--bind`, `--port`, `--timeout`, or `--index` are omitted.
- Command-line flags override config defaults for that command only.
- Profiles allow repeatable targets such as `tailscale`, `mac-lan`, or `office-wifi`.
- `serve-lib config show` must redact or clearly mark machine-local values when producing shareable output.

### TLS And mTLS

`serve-lib` should support manually configured TLS and mutual TLS for private-network deployments that need encrypted transport or client certificate identity.

TLS modes:

- `off`: plain HTTP. This remains the default for low-friction local and Tailscale-only workflows.
- `tls`: HTTPS with a server certificate.
- `mtls`: HTTPS with a server certificate and required client certificate verification.

Requirements:

- TLS is configured through local config profiles, not repository defaults.
- Server certificate and key paths must be absolute local paths.
- mTLS requires a client CA bundle path.
- Certificate material must never be committed to the repository.
- TLS/mTLS settings apply to a listener profile, not to each individual route by default.
- Route-level TLS policy can be considered later, but listener-level policy is simpler for v1.
- CLI output should display `https://` URLs when TLS or mTLS is enabled.
- Event log access events should record whether TLS was used and whether client certificate identity was available.

Non-goals:

- Automatic certificate issuance.
- Automatic certificate renewal.
- ACME integration.
- Tailscale Serve automation.

Open mTLS identity question:

- Should the daemon record the client certificate subject, fingerprint, or both in the event log?

### Event Log

The daemon should store operational and access events in a local SQLite database. This is runtime state, not repo state.

Events to record:

- Daemon start and stop.
- Route registered, deregistered, and expired.
- HTTP access for served files, index HTML, directory listings, and SPA fallback.
- HTTP access denial, not found, and internal serving errors.
- Bind resolution failures and listener open/close failures.

Access event fields:

- Timestamp.
- Listener bind and port.
- Route mount id and route path when matched.
- Request method.
- Request path.
- Resolved local path when safe to record.
- Response status.
- Bytes served when available.
- Remote client IP and port.
- User agent when present.

Retention requirements:

- Event retention is configured as a time-to-live, not a max row count.
- Default retention should be conservative, such as `7d`.
- The daemon should run a background cleanup task that deletes events older than the retention window.
- Cleanup interval should be configurable.
- SQLite storage path should live in the platform data directory by default.
- The event log should be useful for `serve-lib events` or future diagnostics.
- Access logging should avoid storing sensitive query strings by default.

### Deregister

`deregister` removes one route registration.

Behavior:

- Removing a route must not stop other routes on the same port.
- If the last route on a listener is removed, the daemon may close that listener.
- Deregistering a missing route should return a clear not-found error.

### Multiple Paths On One Port

One port can serve many local paths by routing URL subpaths:

```text
http://host:8080/logs/       -> /var/log/myapp
http://host:8080/builds/ui/  -> /home/user/builds/ui
http://host:8080/share/      -> /home/user/share
```

Routing requirements:

- Longest-prefix matching for nested routes.
- URL path normalization before matching.
- Reject route definitions that allow ambiguous traversal.
- Directory listings should be scoped to the mounted local path.
- File serving must not escape the registered root after URL decoding.

### Timeout

Every registration can have a timeout.

Requirements:

- `--timeout <duration>` accepts values like `30s`, `10m`, `2h`, `1d`.
- When the timeout expires, the daemon automatically deregisters that route.
- Expired registrations should be visible in logs or event history.
- A missing timeout means no automatic expiry for the first version.
- Future versions may support default timeout policies.

### Bind Address

The daemon must support binding to Tailscale or other private network addresses.

Requirements:

- `--bind <ip>` binds the listener to an explicit IP address.
- `--bind tailscale` resolves the current machine's Tailscale IPv4 address when used directly or through config defaults.
- `--bind private` may later select a private RFC1918 address when unambiguous.
- `--bind 0.0.0.0` is allowed but should be explicit.
- CLI output should show URLs using the best reachable host name when available.

Tailscale behavior:

- Detect Tailscale IPv4 through `tailscale ip -4` when available.
- Detect MagicDNS hostname through `tailscale status --json` when available.
- Prefer displaying full MagicDNS names for user-facing URLs.
- Do not require Tailscale to be installed for non-Tailscale binds.

### HTTP Serving

Initial HTTP requirements:

- Serve files and directories.
- Serve an index HTML file for directory requests when present.
- Default index file name is `index.html`.
- If a directory has an index file, return it before generating a directory listing.
- Directory listing is the fallback when no index file exists.
- Optional SPA fallback should return the index file for non-existent paths under a route.
- Directory index HTML with file name, size, modified time, and links.
- Correct content type when reasonably detectable.
- Range requests for large files if practical in the first version.
- Optional no-cache headers.
- Clear 404 and 403 responses.

First-version access model:

- No authentication by default.
- Bind safety is the primary protection.
- Authentication can be added later as a route-level option.
- mTLS is an optional listener-level identity and transport security policy.

### Control API

The daemon needs a local control API for CLI commands.

Initial options to evaluate:

- Unix domain socket with JSON messages.
- Localhost HTTP control endpoint.
- gRPC over Unix socket.

Requirement:

- The control API must not be exposed on the public serving listener.
- The control API should support structured errors.
- The API should be stable enough for tests to exercise daemon behavior.

## Example Workflows

Start daemon:

```bash
serve-lib daemon start
```

Serve two directories on one Tailscale-bound port:

```bash
serve-lib config set default-bind tailscale
serve-lib config set default-port 8088
serve-lib config set event-log.retention 7d
serve-lib register ~/Downloads --route /downloads --timeout 2h
serve-lib register ./target/release --route /artifacts --timeout 30m
```

List active routes:

```text
BIND       PORT  ROUTE       PATH                       EXPIRES
tailscale  8088  /downloads  /home/cyuan/Downloads      1h59m
tailscale  8088  /artifacts  /repo/target/release       29m
```

Deregister one route:

```bash
serve-lib deregister --port 8088 --route /artifacts
```

## Key Differences From miniserve

- `miniserve` starts a foreground server for one command invocation; `serve-lib` uses a daemon plus CLI control model.
- `miniserve` primarily serves one root per process; `serve-lib` must support multiple route-mounted roots on one port.
- `serve-lib` has first-class registration lifetimes through timeouts.
- `serve-lib` targets private network workflows, especially Tailscale-bound serving.
- `serve-lib` keeps machine-local bind defaults in user config instead of publishing them in repo files.
- `serve-lib` should expose a reusable library surface for embedding the daemon/router behavior in other Shuozeli tools.

## Open Questions

- Should the daemon auto-start on first `register`?
- Should route registrations persist across daemon restart?
- Should auth be global, per listener, or per route?
- Should mTLS client identity map to names/labels in config?
- Should upload support exist in v1 or stay read-only?
- Should the CLI support named profiles for common bind+port combinations?
- Should deregister also infer the default port from config when omitted?
- Should event queries support filters by route, client IP, and status code in v1?
- Should the project expose WebDAV-compatible behavior later?

## Initial Milestones

1. Define CLI grammar and config types.
2. Implement in-memory registry with route conflict detection and expiry.
3. Implement daemon control API.
4. Implement HTTP listener manager keyed by bind address and port.
5. Implement static file and directory serving with path traversal protection.
6. Add Tailscale bind resolution and URL display.
7. Add SQLite event log and retention cleanup.
8. Add integration tests for register, deregister, timeout, and multi-route serving.
