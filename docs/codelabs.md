# serve-lib Codelabs

This document shows how to use `serve-lib` as it exists today and how the
planned daemon/CLI workflow is intended to feel. It is written for both human
users and coding agents.

Current state: the core Rust library, first-pass daemon runtime, private HTTP
JSON control API, CLI, HTTP listener manager, event logging, and timeout
scheduler exist. TLS/mTLS listener sockets are wired through rustls with
manually configured certificates.

## Codelab 1: Inspect The Project

### Goal

Understand what has been implemented and where to continue.

### Steps

```bash
cd /home/cyuan/projects/shuozeli/_wip/serve-lib
find docs crates tests -maxdepth 3 -type f | sort
```

Read these docs in order:

1. [Feature requirements](feature-requirements.md)
2. [Architecture](architecture.md)
3. [Component design](design.md)
4. [Implementation plan](implementation-plan.md)
5. [Tasks](tasks.md)

Open the visual dashboard:

```text
index.html
```

When serving the dashboard for review, bind the static server to the local
Tailscale IP and share the full MagicDNS URL outside repository files. Do not
commit real machine hostnames or private IPs.

## Codelab 2: Verify The Current Core Library

### Goal

Run the same local checks expected by CI.

### Steps

```bash
cd /home/cyuan/projects/shuozeli/_wip/serve-lib
cargo fmt --all -- --check
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets -- -D warnings
CARGO_INCREMENTAL=0 cargo test --workspace --no-fail-fast
CARGO_INCREMENTAL=0 cargo build --workspace --release
RUSTDOCFLAGS='-D rustdoc::broken-intra-doc-links -D warnings' \
  CARGO_INCREMENTAL=0 cargo doc --workspace --no-deps --document-private-items
```

Expected result:

- format check passes
- clippy passes with warnings denied
- unit tests pass
- release build succeeds
- docs build without broken links

Run the Docker e2e test when you want the same clean Linux container check used
by GitHub Actions. It runs workspace tests, a release build, and a real
CLI/daemon flow that covers multi-route serving, route conflict errors,
timeout expiry, directory listing, SPA fallback, config defaults/profiles,
SQLite event persistence across daemon restart, TTL cleanup, Markdown/code
rendering, TLS, mTLS, listener TLS policy conflicts, events, deregistration,
and shutdown.

```bash
docker compose -f tests/e2e/docker-compose.yml up --build --abort-on-container-exit --exit-code-from core-e2e
docker compose -f tests/e2e/docker-compose.yml down --volumes
```

## Codelab 3: Use The Core Types In Rust

### Goal

Understand the library surface before the daemon/CLI exists.

### Example

```rust
use std::net::IpAddr;
use std::path::PathBuf;

use serve_lib_core::{
    BindResolver, BindResolverConfig, BindTarget, ListenerKey, MountId, Registry, RouteMount,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listener = ListenerKey {
        bind_addr: "127.0.0.1".parse::<IpAddr>()?,
        port: 8088,
    };

    let mut registry = Registry::default();
    let mount = RouteMount {
        id: MountId::new(),
        listener: listener.clone(),
        route: "/logs".parse()?,
        local_root: PathBuf::from("/var/log/myapp"),
        index_file: "index.html".to_string(),
        spa: false,
        readonly: true,
        expires_at: None,
        display_name: Some("app logs".to_string()),
    };

    registry.insert(mount)?;
    assert!(registry.match_request(&listener, "/logs/app.log").is_some());

    let resolver = BindResolver::with_config(
        serve_lib_core::SystemCommandRunner,
        BindResolverConfig::default(),
    );
    let bind = resolver.resolve(&BindTarget::Loopback)?;
    assert_eq!(bind.bind_addr.to_string(), "127.0.0.1");

    Ok(())
}
```

The example is conceptual. Prefer the unit tests in `crates/serve-lib-core/src`
as the source of truth for exact constructor signatures while the API is still
moving.

## Codelab 4: Human CLI Flow

### Goal

Serve multiple paths on one private-network port with the experimental CLI.

### Setup

```bash
cargo build --workspace --release
target/release/serve-lib --control 127.0.0.1:7878 daemon run
```

The CLI reads config from `SERVE_LIB_CONFIG` when set, otherwise from the
platform default path. The config should live outside the repository:

```text
Linux:  ~/.config/serve-lib/config.toml
macOS:  ~/Library/Application Support/serve-lib/config.toml
Tests:  SERVE_LIB_CONFIG=/tmp/serve-lib/config.toml
```

Repository examples must use placeholders:

```toml
[defaults]
bind = "loopback"
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

[[profiles]]
name = "ui"
bind = "loopback"
port = 8088
timeout = "30m"
```

Do not commit real Tailscale MagicDNS names, private IPs, certificate paths, or
machine-local profile names.

Markdown rendering is powered by `pulldown-cmark`; source highlighting is
powered by `syntect`. Leave `[render]` disabled or omit it when raw file bytes
should be served.

In another terminal:

```bash
target/release/serve-lib --control 127.0.0.1:7878 register ~/builds/ui --route /ui --port 8088 --bind loopback --timeout 30m
target/release/serve-lib --control 127.0.0.1:7878 register ~/builds/ui --route /ui --profile ui
target/release/serve-lib --control 127.0.0.1:7878 register ~/logs/myapp --route /logs --port 8088 --bind loopback
target/release/serve-lib --control 127.0.0.1:7878 list
```

Expected behavior:

- one daemon owns the listener
- one bind+port can serve many routes
- each route points to one local file or directory
- route conflicts fail clearly
- timeout removes the route through the same deregister path
- URLs use the resolved display host
- Tailscale display hosts are supported through the bind resolver, but local
  loopback is the safest development target

Example output shape:

```text
ROUTE         LOCAL PATH              URL
/ui          /home/user/builds/ui     http://127.0.0.1:8088/ui
/logs        /home/user/logs/myapp    http://127.0.0.1:8088/logs
```

### Deregister flow

```bash
target/release/serve-lib --control 127.0.0.1:7878 deregister --port 8088 --route /logs --bind loopback
target/release/serve-lib --control 127.0.0.1:7878 list
target/release/serve-lib --control 127.0.0.1:7878 daemon stop
```

Expected behavior:

- `/logs` is removed
- `/ui` and `/release.zip` stay available
- the listener closes only when the last route for that bind+port is removed

## Codelab 5: Event Log Flow

### Goal

Show how users should inspect activity and how storage should stay bounded.

### Planned config

```toml
[event_log]
database_path = "default"
retention = "7d"
cleanup_interval = "1h"
```

### Commands

```bash
target/release/serve-lib --control 127.0.0.1:7878 events
```

Expected behavior:

- access events include timestamp, route, request path, response status, bytes,
  remote IP, and user agent when available
- query strings are not stored by default
- retention is time-to-live based, not max-row-count based
- a background cleanup worker deletes events older than the configured TTL
- first-pass daemon runs use an in-memory event log unless a runtime path is
  supplied by embedding code

## Codelab 6: TLS And mTLS Policy Flow

### Goal

Show the configuration model for private-network HTTPS and mTLS. The current
runtime accepts TLS and mTLS sockets through rustls.

### Planned config

```toml
[[tls_profiles]]
name = "private-mtls"
mode = "mtls"
server_cert = "/absolute/path/server.crt"
server_key = "/absolute/path/server.key"
client_ca = "/absolute/path/client-ca.crt"

[[profiles]]
name = "tailscale-secure"
bind = "tailscale"
port = 8443
tls_profile = "private-mtls"
```

### Planned commands

```bash
target/release/serve-lib --control 127.0.0.1:7878 register ~/secure-share --route /secure --port 8443 --bind loopback --tls-mode mtls --server-cert /absolute/path/server.crt --server-key /absolute/path/server.key --client-ca /absolute/path/client-ca.crt
```

Expected behavior:

- policy validation requires absolute certificate paths
- listener URLs use `https://`
- mTLS requires a client certificate signed by `client_ca`
- certificate material is never copied into repository files
- listener-level TLS policy conflicts fail before mutating the registry

## Agent Playbook

Use this section when an agent continues development.

### Before editing

1. Work from `/home/cyuan/projects/shuozeli/_wip/serve-lib`.
2. Read [tasks.md](tasks.md) and [implementation-plan.md](implementation-plan.md).
3. Check `git status --short`; this repo may still be entirely untracked.
4. Do not edit or push anything in `/home/cyuan/projects/thirdparty/miniserve`.
5. Do not publish, deploy, or push unless the user explicitly asks.

### Development rules

- Keep machine-specific values out of repo files.
- Use fake Tailscale command output in tests.
- Use placeholder hostnames such as `machine.tailnet.example.ts.net`.
- Prefer `cargo check` for quick compile checks and release builds for artifacts.
- Use `CARGO_INCREMENTAL=0` for large one-off Rust checks.
- Add tests using Arrange-Act-Assert comments for new behavior.
- Keep docs current when adding or finishing a milestone.

### Sensitive value check

Run this before summarizing work:

```bash
grep -RIn "REAL_MACHINE_HOSTNAME\|REAL_TAILNET_SUFFIX\|REAL_TAILSCALE_IP" \
  README.md docs index.html crates .github tests Cargo.toml Cargo.lock \
  .dockerignore .gitignore || true
```

Expected result: no output.

### Recommended verification

```bash
cargo fmt --all -- --check
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets -- -D warnings
CARGO_INCREMENTAL=0 cargo test --workspace --no-fail-fast
CARGO_INCREMENTAL=0 cargo build --workspace --release
RUSTDOCFLAGS='-D rustdoc::broken-intra-doc-links -D warnings' \
  CARGO_INCREMENTAL=0 cargo doc --workspace --no-deps --document-private-items
```

### Next implementation candidates

Follow [implementation-plan.md](implementation-plan.md). The natural next
milestone is the listener manager and HTTP router:

1. choose the HTTP stack
2. define listener task ownership
3. route requests through registry snapshots
4. emit access events
5. preserve multiple routes on one bind+port
6. fail cleanly on port conflicts and TLS policy conflicts

Do not start by building the CLI if the listener/runtime contract is still
unclear. The CLI should be a thin control client over stable daemon behavior.
