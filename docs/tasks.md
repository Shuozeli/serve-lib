# Tasks

## Completed

- Created Rust workspace scaffold.
- Added GitHub Actions CI for format, clippy, tests, release build, and docs.
- Added Docker e2e test for clean Linux container workspace tests, release build, and real CLI/daemon multi-route, route conflict, timeout expiry, directory listing, SPA fallback, config defaults/profiles, SQLite restart/retention, Markdown/code rendering, TLS, mTLS, TLS policy conflict, events, deregister, and shutdown flows.
- Created `serve-lib-core` crate.
- Defined shared core types and error model.
- Implemented bind target parsing.
- Implemented duration parsing and TOML serde.
- Implemented route normalization and request matching.
- Implemented local config schema, event log config, profile/default/override merge, and validation.
- Implemented in-memory registry, route conflict detection, mount removal, listener grouping, and longest-prefix route matching.
- Implemented static file response planning, safe path resolution, percent decoding, symlink escape rejection, index file priority, directory listing fallback, SPA fallback, and content type detection.
- Implemented config-gated Markdown rendering through `pulldown-cmark`.
- Implemented config-gated source highlighting through `syntect`.
- Implemented SQLite event log migration, append, query filters, access/lifecycle event types, query-string stripping, and TTL cleanup API.
- Implemented bind resolver for explicit IPs, loopback, any-address, Tailscale command lookup, private-address ambiguity detection, and configured interface names.
- Implemented first-pass daemon runtime crate with listener manager, HTTP static serving, route deregistration cleanup, event logging, and timeout expiry.
- Implemented private HTTP JSON control API for status, list, register, deregister, events, and shutdown.
- Implemented first-pass `serve-lib` CLI for daemon run/start/stop/status, register, deregister, list, and events.
- Wired CLI config discovery through `SERVE_LIB_CONFIG` or platform default config paths.
- Wired register defaults and `--profile` selection into the CLI.
- Wired daemon SQLite event log path, retention, and cleanup interval from local config.
- Implemented rustls-backed TLS and mTLS listener socket support with manually configured certs and client CA verification.
- Added unit tests for core/config/registry behavior.
- Added unit tests for static file serving behavior and path safety.
- Added unit tests for event log migration, lifecycle/access events, query filters, and retention cleanup.
- Added bind resolver unit tests using fake Tailscale command output and fake network candidates.
- Added codelabs for human usage flows, current runtime verification, experimental CLI behavior, and agent continuation workflow.
- Added daemon runtime tests for multi-route listeners, HTTP serving, timeout expiry, TLS serving, mTLS serving, and mTLS client-cert rejection.

## Pending

- Review and tighten [architecture.md](architecture.md).
- Review and tighten [design.md](design.md).
- Review and tighten [implementation-plan.md](implementation-plan.md).
- Add config mutation helpers.
- Define TLS/mTLS profile schema and listener policy model.
- Add config commands for showing and setting defaults.
- Add config commands for event log retention and cleanup interval.
- Add config commands for TLS/mTLS profiles.
- Integrate Tailscale IP and MagicDNS detection into daemon registration flow.
- Wire real runtime network-interface discovery into bind resolver config.
- Add CLI docs/examples for curl/browser access to TLS and mTLS listeners.
- Add tests for TLS profile validation.
- Add tests proving machine-specific MagicDNS/private IP values are not required in repo defaults.

## Design Checklist

- High-level process model documented.
- CLI/control-plane/data-plane split documented.
- Config component documented.
- Bind resolver component documented.
- Registry and route matching documented.
- Listener lifecycle documented.
- Static file serving and index HTML behavior documented.
- Timeout scheduler documented.
- SQLite event log and retention cleanup documented.
- TLS/mTLS support boundaries documented.
- Initial test plan documented.

## Notes

- Keep the upstream `miniserve` clone in `/home/cyuan/projects/thirdparty/miniserve` as reference only.
- Do not push changes from `thirdparty/miniserve`.
