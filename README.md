# serve-lib

Daemon-backed local file serving for private networks.

`serve-lib` is a Shuozeli project for serving local directories and files through a long-running background daemon. Users interact with it through a CLI that registers and deregisters path mounts instead of starting one foreground server process per share.

The project is inspired by tools like `miniserve`, but the core model is different:

- A background daemon owns listeners and routing state.
- The CLI is a control client for registering, deregistering, and inspecting serves.
- One port can serve multiple local paths through distinct URL subpaths.
- Registrations can expire automatically through timeouts.
- Listeners can bind to Tailscale IPs or other private network interfaces.
- Markdown and source files can be rendered as HTML when enabled in local config.

## Experimental Quickstart

The first runtime/CLI pass exists. It is useful for local development and
testing. TLS/mTLS serving works with manually configured certificates; config
file reading works through `SERVE_LIB_CONFIG` or platform default paths. Config
mutation commands are not wired yet.

```bash
cargo build --workspace --release

target/release/serve-lib --control 127.0.0.1:7878 daemon run
```

In another terminal:

```bash
target/release/serve-lib --control 127.0.0.1:7878 register ./dist --route /app --port 8088 --bind loopback --timeout 30m
target/release/serve-lib --control 127.0.0.1:7878 list
curl http://127.0.0.1:8088/app/
target/release/serve-lib --control 127.0.0.1:7878 events
target/release/serve-lib --control 127.0.0.1:7878 deregister --route /app --port 8088 --bind loopback
target/release/serve-lib --control 127.0.0.1:7878 daemon stop
```

Optional local config:

```toml
[defaults]
bind = "loopback"
port = 8088
timeout = "30m"
index = "index.html"
spa = false

[event_log]
database_path = "default"
retention = "7d"
cleanup_interval = "1h"

[render]
markdown = true
code_highlight = true
```

`register` merges CLI flags over a selected `--profile`, then global defaults,
then built-in fallback values. The daemon uses `[event_log]` for its SQLite
path and retention cleanup settings.

Markdown rendering uses `pulldown-cmark`. Source highlighting uses `syntect`
with bundled Sublime-compatible grammars. Both are disabled unless local config
enables them.

`--tls-mode tls` and `--tls-mode mtls` enable rustls-backed HTTPS listeners.
mTLS requires `--client-ca` and clients must present a certificate trusted by
that CA.

## Docs

- [Feature requirements](docs/feature-requirements.md)
- [Architecture](docs/architecture.md)
- [Component design](docs/design.md)
- [Codelabs](docs/codelabs.md)
- [Implementation plan](docs/implementation-plan.md)
- [Tasks](docs/tasks.md)
