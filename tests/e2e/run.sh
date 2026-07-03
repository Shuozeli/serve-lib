#!/usr/bin/env bash
set -euo pipefail

CONTROL_ADDR="127.0.0.1:18787"
WORKDIR="$(mktemp -d)"
DAEMON_LOG="$WORKDIR/daemon.log"
DAEMON_PID=""
export SERVE_LIB_CONFIG="$WORKDIR/config.toml"

cleanup() {
  if [[ -n "${DAEMON_PID}" ]] && kill -0 "${DAEMON_PID}" 2>/dev/null; then
    target/release/serve-lib --control "${CONTROL_ADDR}" daemon stop >/dev/null 2>&1 || true
    wait "${DAEMON_PID}" 2>/dev/null || true
  fi
  rm -rf "${WORKDIR}"
}
trap cleanup EXIT

assert_contains() {
  local haystack="$1"
  local needle="$2"
  if [[ "${haystack}" != *"${needle}"* ]]; then
    echo "expected output to contain: ${needle}" >&2
    echo "--- output ---" >&2
    echo "${haystack}" >&2
    exit 1
  fi
}

assert_not_contains() {
  local haystack="$1"
  local needle="$2"
  if [[ "${haystack}" == *"${needle}"* ]]; then
    echo "expected output not to contain: ${needle}" >&2
    echo "--- output ---" >&2
    echo "${haystack}" >&2
    exit 1
  fi
}

assert_command_fails() {
  local output
  set +e
  output="$("$@" 2>&1)"
  local status=$?
  set -e
  if [[ "${status}" -eq 0 ]]; then
    echo "expected command to fail: $*" >&2
    echo "--- output ---" >&2
    echo "${output}" >&2
    exit 1
  fi
  printf '%s' "${output}"
}

wait_for_status() {
  for _ in $(seq 1 100); do
    if target/release/serve-lib --control "${CONTROL_ADDR}" daemon status >/tmp/serve-lib-e2e-status 2>/dev/null; then
      return 0
    fi
    sleep 0.05
  done
  echo "daemon did not become ready" >&2
  cat "${DAEMON_LOG}" >&2 || true
  return 1
}

start_daemon() {
  target/release/serve-lib --control "${CONTROL_ADDR}" daemon run >"${DAEMON_LOG}" 2>&1 &
  DAEMON_PID="$!"
  wait_for_status
}

stop_daemon() {
  local stop_output
  stop_output="$(target/release/serve-lib --control "${CONTROL_ADDR}" daemon stop)"
  assert_contains "${stop_output}" "shutdown requested"
  wait "${DAEMON_PID}"
  DAEMON_PID=""
}

wait_for_http() {
  local url="$1"
  shift
  for _ in $(seq 1 100); do
    if curl --fail --silent --show-error --max-time 2 "$@" "${url}" >/tmp/serve-lib-e2e-http 2>/tmp/serve-lib-e2e-curl; then
      cat /tmp/serve-lib-e2e-http
      return 0
    fi
    sleep 0.05
  done
  echo "HTTP endpoint did not become ready: ${url}" >&2
  cat /tmp/serve-lib-e2e-curl >&2 || true
  cat "${DAEMON_LOG}" >&2 || true
  return 1
}

wait_for_not_served() {
  local url="$1"
  for _ in $(seq 1 100); do
    local status
    status="$(curl --silent --output /tmp/serve-lib-e2e-not-served --write-out '%{http_code}' --max-time 2 "${url}" 2>/tmp/serve-lib-e2e-curl || true)"
    if [[ "${status}" != "200" ]]; then
      return 0
    fi
    sleep 0.05
  done
  echo "expected ${url} to stop serving 200 responses" >&2
  cat /tmp/serve-lib-e2e-curl >&2 || true
  cat "${DAEMON_LOG}" >&2 || true
  return 1
}

generate_certs() {
  local cert_dir="$WORKDIR/certs"
  mkdir -p "${cert_dir}"

  openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
    -subj "/CN=serve-lib-test-ca" \
    -keyout "${cert_dir}/ca.key" \
    -out "${cert_dir}/ca.crt" >/dev/null 2>&1

  openssl req -newkey rsa:2048 -nodes \
    -subj "/CN=localhost" \
    -keyout "${cert_dir}/server.key" \
    -out "${cert_dir}/server.csr" >/dev/null 2>&1
  printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' >"${cert_dir}/server.ext"
  openssl x509 -req -days 1 \
    -in "${cert_dir}/server.csr" \
    -CA "${cert_dir}/ca.crt" \
    -CAkey "${cert_dir}/ca.key" \
    -CAcreateserial \
    -out "${cert_dir}/server.crt" \
    -extfile "${cert_dir}/server.ext" >/dev/null 2>&1

  openssl req -newkey rsa:2048 -nodes \
    -subj "/CN=serve-lib-test-client" \
    -keyout "${cert_dir}/client.key" \
    -out "${cert_dir}/client.csr" >/dev/null 2>&1
  printf 'extendedKeyUsage=clientAuth\n' >"${cert_dir}/client.ext"
  openssl x509 -req -days 1 \
    -in "${cert_dir}/client.csr" \
    -CA "${cert_dir}/ca.crt" \
    -CAkey "${cert_dir}/ca.key" \
    -CAcreateserial \
    -out "${cert_dir}/client.crt" \
    -extfile "${cert_dir}/client.ext" >/dev/null 2>&1
}

prepare_config() {
  cat >"${SERVE_LIB_CONFIG}" <<EOF
[defaults]
bind = "loopback"
port = 18091
timeout = "5m"
index = "home.html"
spa = false

[event_log]
database_path = "$WORKDIR/events.sqlite"
retention = "2s"
cleanup_interval = "1s"

[[profiles]]
name = "spa-profile"
bind = "loopback"
port = 18092
index = "shell.html"
spa = true

[[profiles]]
name = "render-profile"
bind = "loopback"
port = 18095
render = { markdown = true, code_highlight = true }
EOF
}

register_route() {
  target/release/serve-lib --control "${CONTROL_ADDR}" register "$@"
}

deregister_route() {
  target/release/serve-lib --control "${CONTROL_ADDR}" deregister "$@"
}

test_multi_route_serving() {
  echo "== test: multi-route same-port serving =="
  local port="18088"
  mkdir -p "${WORKDIR}/site" "${WORKDIR}/logs"
  printf 'hello from docker e2e' >"${WORKDIR}/site/hello.txt"
  printf '<main>app</main>' >"${WORKDIR}/site/index.html"
  printf 'log-line' >"${WORKDIR}/logs/out.txt"

  local register_app_output
  register_app_output="$(register_route "${WORKDIR}/site" --route /app --port "${port}" --bind loopback --timeout 5m --index index.html)"
  assert_contains "${register_app_output}" "registered /app"
  assert_contains "${register_app_output}" "http://127.0.0.1:${port}/app"

  local register_log_output
  register_log_output="$(register_route "${WORKDIR}/logs" --route /logs --port "${port}" --bind loopback)"
  assert_contains "${register_log_output}" "registered /logs"

  local list_output
  list_output="$(target/release/serve-lib --control "${CONTROL_ADDR}" list)"
  assert_contains "${list_output}" "/app"
  assert_contains "${list_output}" "/logs"
  assert_contains "${list_output}" "${WORKDIR}/site"
  assert_contains "${list_output}" "${WORKDIR}/logs"

  local app_body log_body index_body
  app_body="$(wait_for_http "http://127.0.0.1:${port}/app/hello.txt")"
  assert_contains "${app_body}" "hello from docker e2e"
  log_body="$(wait_for_http "http://127.0.0.1:${port}/logs/out.txt")"
  assert_contains "${log_body}" "log-line"
  index_body="$(wait_for_http "http://127.0.0.1:${port}/app/")"
  assert_contains "${index_body}" "<main>app</main>"

  local events_output
  events_output="$(target/release/serve-lib --control "${CONTROL_ADDR}" events)"
  assert_contains "${events_output}" "http_access_served"
  assert_contains "${events_output}" "/app/hello.txt"
  assert_contains "${events_output}" "/logs/out.txt"

  local deregister_output
  deregister_output="$(deregister_route --route /logs --port "${port}" --bind loopback)"
  assert_contains "${deregister_output}" "deregistered /logs"

  local post_deregister_list
  post_deregister_list="$(target/release/serve-lib --control "${CONTROL_ADDR}" list)"
  assert_contains "${post_deregister_list}" "/app"
  assert_not_contains "${post_deregister_list}" "/logs"

  local app_body_after
  app_body_after="$(wait_for_http "http://127.0.0.1:${port}/app/hello.txt")"
  assert_contains "${app_body_after}" "hello from docker e2e"
}

test_route_conflict() {
  echo "== test: route conflict =="
  local output
  output="$(assert_command_fails register_route "${WORKDIR}/logs" --route /app --port 18088 --bind loopback)"
  assert_contains "${output}" "route already exists"
}

test_timeout_expiry() {
  echo "== test: timeout expiry =="
  local port="18089"
  mkdir -p "${WORKDIR}/timeout"
  printf 'short lived' >"${WORKDIR}/timeout/file.txt"

  local register_output
  register_output="$(register_route "${WORKDIR}/timeout" --route /tmp --port "${port}" --bind loopback --timeout 1s)"
  assert_contains "${register_output}" "registered /tmp"

  local body
  body="$(wait_for_http "http://127.0.0.1:${port}/tmp/file.txt")"
  assert_contains "${body}" "short lived"

  wait_for_not_served "http://127.0.0.1:${port}/tmp/file.txt"

  local list_output
  list_output="$(target/release/serve-lib --control "${CONTROL_ADDR}" list)"
  assert_not_contains "${list_output}" $'/tmp\t'
}

test_directory_listing_and_spa() {
  echo "== test: directory listing and SPA fallback =="
  local port="18090"
  mkdir -p "${WORKDIR}/listing" "${WORKDIR}/spa"
  printf 'visible' >"${WORKDIR}/listing/visible.txt"
  printf '<main>spa shell</main>' >"${WORKDIR}/spa/index.html"

  local listing_output spa_output
  listing_output="$(register_route "${WORKDIR}/listing" --route /listing --port "${port}" --bind loopback)"
  assert_contains "${listing_output}" "registered /listing"
  spa_output="$(register_route "${WORKDIR}/spa" --route /spa --port "${port}" --bind loopback --spa --index index.html)"
  assert_contains "${spa_output}" "registered /spa"

  local listing_body spa_body
  listing_body="$(wait_for_http "http://127.0.0.1:${port}/listing/")"
  assert_contains "${listing_body}" "Index of /listing/"
  assert_contains "${listing_body}" "visible.txt"

  spa_body="$(wait_for_http "http://127.0.0.1:${port}/spa/deep/link")"
  assert_contains "${spa_body}" "<main>spa shell</main>"
}

test_tls_cli_flow() {
  echo "== test: TLS CLI flow =="
  local port="18443"
  mkdir -p "${WORKDIR}/tls" "${WORKDIR}/tls-conflict"
  printf 'hello over tls' >"${WORKDIR}/tls/secure.txt"
  printf 'conflict' >"${WORKDIR}/tls-conflict/file.txt"

  local register_output
  register_output="$(register_route "${WORKDIR}/tls" --route /tls --port "${port}" --bind loopback --tls-mode tls --server-cert "${WORKDIR}/certs/server.crt" --server-key "${WORKDIR}/certs/server.key")"
  assert_contains "${register_output}" "registered /tls"
  assert_contains "${register_output}" "https://127.0.0.1:${port}/tls"

  local body
  body="$(wait_for_http "https://localhost:${port}/tls/secure.txt" --cacert "${WORKDIR}/certs/ca.crt")"
  assert_contains "${body}" "hello over tls"

  local conflict_output
  conflict_output="$(assert_command_fails register_route "${WORKDIR}/tls-conflict" --route /tls-conflict --port "${port}" --bind loopback --tls-mode off)"
  assert_contains "${conflict_output}" "already has a different TLS policy"
}

test_mtls_cli_flow() {
  echo "== test: mTLS CLI flow =="
  local port="18444"
  mkdir -p "${WORKDIR}/mtls"
  printf 'hello over mtls' >"${WORKDIR}/mtls/secure.txt"

  local register_output
  register_output="$(register_route "${WORKDIR}/mtls" --route /mtls --port "${port}" --bind loopback --tls-mode mtls --server-cert "${WORKDIR}/certs/server.crt" --server-key "${WORKDIR}/certs/server.key" --client-ca "${WORKDIR}/certs/ca.crt")"
  assert_contains "${register_output}" "registered /mtls"
  assert_contains "${register_output}" "https://127.0.0.1:${port}/mtls"

  local no_cert_status
  no_cert_status="$(curl --silent --output /tmp/serve-lib-e2e-mtls-no-cert --write-out '%{http_code}' --max-time 2 --cacert "${WORKDIR}/certs/ca.crt" "https://localhost:${port}/mtls/secure.txt" 2>/tmp/serve-lib-e2e-curl || true)"
  if [[ "${no_cert_status}" == "200" ]]; then
    echo "expected mTLS request without client certificate to fail" >&2
    exit 1
  fi

  local body
  body="$(wait_for_http "https://localhost:${port}/mtls/secure.txt" --cacert "${WORKDIR}/certs/ca.crt" --cert "${WORKDIR}/certs/client.crt" --key "${WORKDIR}/certs/client.key")"
  assert_contains "${body}" "hello over mtls"
}

test_config_defaults_and_profile() {
  echo "== test: config defaults and profile =="
  mkdir -p "${WORKDIR}/config-default" "${WORKDIR}/config-profile"
  printf '<main>config default index</main>' >"${WORKDIR}/config-default/home.html"
  printf '<main>profile spa shell</main>' >"${WORKDIR}/config-profile/shell.html"

  local default_output
  default_output="$(register_route "${WORKDIR}/config-default" --route /cfg)"
  assert_contains "${default_output}" "registered /cfg"
  assert_contains "${default_output}" "http://127.0.0.1:18091/cfg"

  local default_body
  default_body="$(wait_for_http "http://127.0.0.1:18091/cfg/")"
  assert_contains "${default_body}" "config default index"

  local profile_output
  profile_output="$(register_route "${WORKDIR}/config-profile" --route /profile --profile spa-profile)"
  assert_contains "${profile_output}" "registered /profile"
  assert_contains "${profile_output}" "http://127.0.0.1:18092/profile"

  local profile_body
  profile_body="$(wait_for_http "http://127.0.0.1:18092/profile/deep/link")"
  assert_contains "${profile_body}" "profile spa shell"
}

test_sqlite_event_log_restart_and_retention() {
  echo "== test: SQLite event log restart and retention =="
  mkdir -p "${WORKDIR}/persist" "${WORKDIR}/retention"
  printf 'persisted access' >"${WORKDIR}/persist/file.txt"
  printf 'old access' >"${WORKDIR}/retention/old.txt"
  printf 'new access' >"${WORKDIR}/retention/new.txt"

  local persist_output
  persist_output="$(register_route "${WORKDIR}/persist" --route /persist --port 18093 --bind loopback)"
  assert_contains "${persist_output}" "registered /persist"
  wait_for_http "http://127.0.0.1:18093/persist/file.txt" >/dev/null

  stop_daemon
  start_daemon

  local restarted_events
  restarted_events="$(target/release/serve-lib --control "${CONTROL_ADDR}" events)"
  assert_contains "${restarted_events}" "/persist/file.txt"

  local restarted_list
  restarted_list="$(target/release/serve-lib --control "${CONTROL_ADDR}" list)"
  assert_contains "${restarted_list}" "no active mounts"
  wait_for_not_served "http://127.0.0.1:18093/persist/file.txt"

  local retention_output
  retention_output="$(register_route "${WORKDIR}/retention" --route /retention --port 18094 --bind loopback)"
  assert_contains "${retention_output}" "registered /retention"
  wait_for_http "http://127.0.0.1:18094/retention/old.txt" >/dev/null
  sleep 4
  wait_for_http "http://127.0.0.1:18094/retention/new.txt" >/dev/null
  sleep 1

  local retention_events
  retention_events="$(target/release/serve-lib --control "${CONTROL_ADDR}" events)"
  assert_not_contains "${retention_events}" "/retention/old.txt"
  assert_contains "${retention_events}" "/retention/new.txt"
}

test_rendering_config() {
  echo "== test: rendering config =="
  mkdir -p "${WORKDIR}/rendered" "${WORKDIR}/raw-render"
  printf '# Rendered Title\n\n- item\n' >"${WORKDIR}/rendered/README.md"
  printf 'const value = 42;\nconsole.log(value);\n' >"${WORKDIR}/rendered/app.js"
  printf '# Raw Title\n' >"${WORKDIR}/raw-render/README.md"

  local raw_output
  raw_output="$(register_route "${WORKDIR}/raw-render" --route /raw-render --port 18096 --bind loopback)"
  assert_contains "${raw_output}" "registered /raw-render"
  local raw_body
  raw_body="$(wait_for_http "http://127.0.0.1:18096/raw-render/README.md")"
  assert_contains "${raw_body}" "# Raw Title"
  assert_not_contains "${raw_body}" "<h1"

  local render_output
  render_output="$(register_route "${WORKDIR}/rendered" --route /rendered --profile render-profile)"
  assert_contains "${render_output}" "registered /rendered"

  local markdown_body
  markdown_body="$(wait_for_http "http://127.0.0.1:18095/rendered/README.md")"
  assert_contains "${markdown_body}" "<h1>Rendered Title</h1>"
  assert_contains "${markdown_body}" "<li>item</li>"

  local code_body
  code_body="$(wait_for_http "http://127.0.0.1:18095/rendered/app.js")"
  assert_contains "${code_body}" "<html>"
  assert_contains "${code_body}" "const"
  assert_contains "${code_body}" "console"
}

echo "== workspace tests =="
cargo test --workspace --no-fail-fast

echo "== release build =="
cargo build --workspace --release

echo "== prepare certificates =="
generate_certs

echo "== prepare config =="
prepare_config

echo "== start daemon =="
start_daemon

test_multi_route_serving
test_route_conflict
test_timeout_expiry
test_directory_listing_and_spa
test_tls_cli_flow
test_mtls_cli_flow
test_config_defaults_and_profile
test_sqlite_event_log_restart_and_retention
test_rendering_config

echo "== stop daemon =="
stop_daemon

echo "e2e ok"
