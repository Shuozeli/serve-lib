# serve-lib Agent Guide

This guide is for coding agents working on `serve-lib`. It describes how to
inspect, modify, validate, and present the project without leaking local
machine details.

## Operating Rules

- Treat `serve-lib` as a Rust workspace with three crates:
  `serve-lib-core`, `serve-lib-daemon`, and `serve-lib-cli`.
- Keep machine-local configuration outside the repository. Use
  `SERVE_LIB_CONFIG=/tmp/serve-lib/config.toml` for demos and tests.
- Do not commit real Tailscale MagicDNS names, private IPs, certificate paths,
  or local profile names.
- Prefer release-mode builds for runnable artifacts:
  `CARGO_INCREMENTAL=0 cargo build --workspace --release`.
- Keep `target/` ignored and unstaged.
- For new tests, use Arrange-Act-Assert comments and keep each test focused on
  one behavior.

## Orientation

Read the project docs in this order:

1. `README.md` for the product shape and quickstart.
2. `docs/feature-requirements.md` for user-facing requirements.
3. `docs/architecture.md` for system boundaries.
4. `docs/design.md` for component contracts.
5. `docs/implementation-plan.md` for planned execution.
6. `docs/tasks.md` for current status and next work.
7. `docs/codelabs.md` for human and agent usage flows.

Code ownership map:

- `crates/serve-lib-core`: types, config, bind resolution, route registry,
  static file resolution, event storage.
- `crates/serve-lib-daemon`: runtime, listeners, HTTP serving, control API,
  timeout cleanup, TLS/mTLS, Markdown and code rendering.
- `crates/serve-lib-cli`: command parsing, config merge, control client calls.
- `tests/e2e`: Docker-based end-to-end CLI/daemon coverage.

## Common Workflows

### Validate The Workspace

```bash
cargo fmt --all -- --check
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets -- -D warnings
CARGO_INCREMENTAL=0 cargo test --workspace --no-fail-fast
CARGO_INCREMENTAL=0 cargo build --workspace --release
RUSTDOCFLAGS='-D rustdoc::broken-intra-doc-links -D warnings' \
  CARGO_INCREMENTAL=0 cargo doc --workspace --no-deps --document-private-items
```

Run the Docker e2e suite before publishing broad runtime changes:

```bash
docker compose -f tests/e2e/docker-compose.yml up --build --abort-on-container-exit --exit-code-from core-e2e
docker compose -f tests/e2e/docker-compose.yml down --volumes
```

### Start A Local Demo

Use a config outside the repository:

```bash
cat >/tmp/serve-lib-demo.toml <<'TOML'
[defaults]
bind = "loopback"
port = 8088
index = "index.html"
spa = false

[event_log]
database_path = "default"
retention = "7d"
cleanup_interval = "1h"

[render]
markdown = true
code_highlight = true
TOML
```

Run the daemon:

```bash
SERVE_LIB_CONFIG=/tmp/serve-lib-demo.toml \
  target/release/serve-lib --control 127.0.0.1:7878 daemon run
```

Register a path from another terminal:

```bash
SERVE_LIB_CONFIG=/tmp/serve-lib-demo.toml \
  target/release/serve-lib --control 127.0.0.1:7878 \
  register . --route /serve-lib --port 8088 --bind loopback
```

Inspect:

```bash
target/release/serve-lib --control 127.0.0.1:7878 list
target/release/serve-lib --control 127.0.0.1:7878 events
curl -I http://127.0.0.1:8088/serve-lib/
```

Stop:

```bash
target/release/serve-lib --control 127.0.0.1:7878 daemon stop
```

## Rendering Notes

Markdown and source rendering are disabled unless config enables them.

```toml
[render]
markdown = true
code_highlight = true
```

- Markdown rendering uses `pulldown-cmark`.
- Source highlighting uses `syntect`.
- Raw Markdown responses use `text/markdown`.
- Rendered Markdown responses use `text/html`.

When debugging rendering, confirm both the route and the content type:

```bash
target/release/serve-lib --control 127.0.0.1:7878 list
curl -I http://127.0.0.1:8088/serve-lib/docs/architecture.md
```

## Sensitive Data Check

Before committing or publishing, scan for local network values:

```bash
grep -RIn "tailnet\\|ts.net\\|100\\." \
  README.md docs index.html crates .github tests Cargo.toml Cargo.lock \
  .dockerignore .gitignore 2>/dev/null || true
```

If the scan finds a real hostname, private IP, or certificate path, replace it
with a placeholder before committing.

## Standard Skill Format

Use a skill when agents should repeatedly perform a well-defined `serve-lib`
workflow. Keep it small and procedural. A standard skill directory should look
like this:

```text
serve-lib/
├── SKILL.md
├── agents/
│   └── openai.yaml
└── references/
    ├── cli.md
    └── validation.md
```

Only `SKILL.md` is required. `agents/openai.yaml` is recommended for UI
metadata. `references/` is optional and should contain details that agents load
only when needed.

Minimal `SKILL.md`:

```markdown
---
name: serve-lib
description: Use when working on the serve-lib Rust workspace, including daemon runtime, CLI registration flows, Markdown/code rendering, TLS/mTLS serving, Docker e2e tests, or publishing checks.
---

# serve-lib

## Workflow

1. Inspect `README.md`, `docs/architecture.md`, and `docs/tasks.md`.
2. Keep local config outside the repo; prefer `SERVE_LIB_CONFIG=/tmp/...`.
3. Make scoped changes in the relevant crate.
4. Run format, clippy, tests, release build, and Docker e2e when runtime behavior changes.
5. Scan for real Tailscale hostnames, private IPs, and certificate paths before commit.

## References

- Read `references/cli.md` for CLI demo commands.
- Read `references/validation.md` for CI-equivalent validation commands.
```

Recommended `agents/openai.yaml`:

```yaml
display_name: serve-lib
short_description: Work on the serve-lib daemon, CLI, rendering, and e2e tests.
default_prompt: Inspect the current serve-lib task, make a scoped change, validate it, and summarize the result.
```

Skill authoring rules:

- Put trigger conditions in the `description`; this is what agents see before
  loading the skill body.
- Keep `SKILL.md` concise. Move long command lists and examples to
  `references/`.
- Do not add README, changelog, or installation docs inside the skill folder.
- Do not include machine-local secrets or private hostnames in the skill.
- Prefer scripts only for fragile or repeated commands that should be
  deterministic.
