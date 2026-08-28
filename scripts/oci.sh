#!/usr/bin/env bash
# Build and run the klepto OCI image.
# Preference: macOS → `container`; Linux → `docker` (override with KLEPTO_OCI_CMD).
#
# Image build does not compile Rust — it packs a prebuilt Linux binary plus
# runtime tools (tmux, rg, node, pi). Provide the binary via:
#   make release-linux-arm64|amd64   → dist/klepto-linux-*
#   or KLEPTO_BINARY=/path/to/klepto
#
# Usage:
#   ./scripts/oci.sh which
#   ./scripts/oci.sh build
#   ./scripts/oci.sh run [extra -v mounts…]
#   ./scripts/oci.sh stop | restart | status | logs [-f] | shell
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
IMAGE="${KLEPTO_IMAGE:-klepto:local}"
NAME="${KLEPTO_CONTAINER_NAME:-klepto}"
# Loopback by default. Remote exposure must be an explicit choice and should use
# an authenticated reverse proxy.
LISTEN_HOST="${KLEPTO_HOST_LISTEN:-127.0.0.1:7420}"

detect_runtime() {
  if [[ -n "${KLEPTO_OCI_CMD:-}" ]]; then
    echo "$KLEPTO_OCI_CMD"
    return
  fi
  local os
  os="$(uname -s 2>/dev/null || echo unknown)"
  case "$os" in
    Darwin)
      if command -v container >/dev/null 2>&1; then
        echo container
        return
      fi
      if command -v docker >/dev/null 2>&1; then
        echo docker
        return
      fi
      ;;
    Linux)
      if command -v docker >/dev/null 2>&1; then
        echo docker
        return
      fi
      if command -v container >/dev/null 2>&1; then
        echo container
        return
      fi
      ;;
    *)
      if command -v docker >/dev/null 2>&1; then
        echo docker
        return
      fi
      if command -v container >/dev/null 2>&1; then
        echo container
        return
      fi
      ;;
  esac
  echo "error: no OCI runtime found (want: macOS → container, Linux → docker)" >&2
  exit 1
}

oci_rm() {
  if [[ "$CMD" == "docker" ]]; then
    docker rm -f "$NAME" >/dev/null 2>&1 || true
  else
    container stop "$NAME" >/dev/null 2>&1 || true
    container rm "$NAME" >/dev/null 2>&1 || true
    container delete "$NAME" >/dev/null 2>&1 || true
  fi
}

image_exists() {
  if [[ "$CMD" == "docker" ]]; then
    docker image inspect "$IMAGE" >/dev/null 2>&1
  else
    # Apple container: missing local tags fall through to docker.io (401 on klepto:local).
    container image inspect "$IMAGE" >/dev/null 2>&1
  fi
}

# Linux glibc binary for the image (host/CI build — not compiled inside Dockerfile).
linux_bin_hint() {
  local arch
  arch="$(uname -m 2>/dev/null || echo unknown)"
  case "$arch" in
    arm64|aarch64) echo "make release-linux-arm64  # → dist/klepto-linux-arm64" ;;
    x86_64|amd64)  echo "make release-linux-amd64  # → dist/klepto-linux-amd64" ;;
    *)             echo "make release-linux        # → dist/klepto-linux-*" ;;
  esac
}

resolve_linux_binary() {
  if [[ -n "${KLEPTO_BINARY:-}" ]]; then
    if [[ ! -f "$KLEPTO_BINARY" ]]; then
      echo "error: KLEPTO_BINARY=$KLEPTO_BINARY not found" >&2
      exit 1
    fi
    echo "$KLEPTO_BINARY"
    return
  fi

  local arch candidates=()
  arch="$(uname -m 2>/dev/null || echo unknown)"
  case "$arch" in
    arm64|aarch64)
      candidates+=("$ROOT/dist/klepto-linux-arm64")
      ;;
    x86_64|amd64)
      candidates+=("$ROOT/dist/klepto-linux-amd64")
      ;;
  esac
  # Native Linux release binary (make release-host on Linux).
  if [[ "$(uname -s 2>/dev/null || true)" == "Linux" ]]; then
    candidates+=("$ROOT/dist/klepto")
  fi
  candidates+=("$ROOT/dist/klepto-linux-arm64" "$ROOT/dist/klepto-linux-amd64")

  local c
  for c in "${candidates[@]}"; do
    if [[ -f "$c" ]]; then
      echo "$c"
      return
    fi
  done

  echo "error: no Linux klepto binary found for the OCI image." >&2
  echo "Build one first (or set KLEPTO_BINARY=/path/to/klepto):" >&2
  echo "  $(linux_bin_hint)" >&2
  exit 1
}

stage_linux_binary() {
  local src dest
  src="$(resolve_linux_binary)"
  dest="$ROOT/.oci/klepto"
  mkdir -p "$ROOT/.oci"
  cp "$src" "$dest"
  chmod +x "$dest"
  echo "==> staged $src → .oci/klepto"
}

oci_build() {
  stage_linux_binary
  echo "==> building $IMAGE with $CMD (runtime tools + prebuilt binary)"
  if [[ "$CMD" == "docker" ]]; then
    docker build -t "$IMAGE" "$ROOT"
  else
    container build --tag "$IMAGE" "$ROOT"
  fi
}

# Local tags like klepto:local are not on a registry. Ensure they exist before
# run/restart so the runtime does not attempt a docker.io pull.
ensure_image() {
  if image_exists; then
    return
  fi
  echo "==> image $IMAGE not found locally; building before run"
  oci_build
  if ! image_exists; then
    echo "error: failed to build local image $IMAGE" >&2
    exit 1
  fi
}

oci_run() {
  local host_port="${LISTEN_HOST##*:}"
  local bind_host="${LISTEN_HOST%%:*}"
  local mounts=()
  local security=()
  local environment=(-e KLEPTO_LISTEN=0.0.0.0:7420 -e KLEPTO_IN_OCI=1)
  mounts+=(-v "klepto-data:/home/klepto/.klepto")
  if [[ -n "${ALL_PROXY:-}" ]]; then
    if [[ "$ALL_PROXY" != socks5h://* ]]; then
      echo "error: ALL_PROXY must use socks5h:// so DNS resolves through the proxy" >&2
      exit 1
    fi
    environment+=(-e "ALL_PROXY=$ALL_PROXY" -e "all_proxy=$ALL_PROXY")
  fi
  if [[ "${KLEPTO_NETWORK_MODE:-direct}" == "none" ]]; then
    security+=(--network none)
    environment+=(-e KLEPTO_NETWORK_ENFORCED=1)
  elif [[ "${KLEPTO_DENY_DIRECT:-0}" == "1" ]]; then
    echo "error: deny-direct SOCKS routing requires an external enforced proxy network" >&2
    exit 1
  fi
  if [[ -d "${HOME}/.pi" ]]; then
    mounts+=(-v "${HOME}/.pi:/home/klepto/.pi")
  fi
  # Optional same-path workspace: KLEPTO_MOUNT=/path/to/ws
  if [[ -n "${KLEPTO_MOUNT:-}" ]]; then
    mounts+=(-v "${KLEPTO_MOUNT}:${KLEPTO_MOUNT}")
  fi

  if [[ "$CMD" == "docker" ]]; then
    security+=(--cap-drop ALL --security-opt no-new-privileges --pids-limit 512)
    security+=(--memory "${KLEPTO_MEMORY_LIMIT:-4g}" --cpus "${KLEPTO_CPU_LIMIT:-4}")
    security+=(--tmpfs /tmp:rw,noexec,nosuid,size=512m)
    docker run -d --name "$NAME" \
      -p "${bind_host}:${host_port}:7420" \
      "${security[@]}" \
      "${environment[@]}" \
      "${mounts[@]}" \
      "$@" \
      "$IMAGE" \
      serve --listen 0.0.0.0:7420
  else
    # Apple container CLI
    container run --name "$NAME" --detach \
      --publish "${bind_host}:${host_port}:7420" \
      --env KLEPTO_LISTEN=0.0.0.0:7420 \
      --env KLEPTO_IN_OCI=1 \
      "${mounts[@]}" \
      "$@" \
      "$IMAGE" \
      serve --listen 0.0.0.0:7420
  fi
}

CMD="$(detect_runtime)"
ACTION="${1:-which}"
shift || true

case "$ACTION" in
  which)
    echo "$CMD"
    ;;
  build)
    oci_build
    ;;
  run)
    echo "==> running $NAME ($IMAGE) via $CMD on $LISTEN_HOST"
    ensure_image
    oci_rm
    oci_run "$@"
    echo "klepto listening on http://${LISTEN_HOST}"
    ;;
  stop)
    echo "==> stopping $NAME ($CMD)"
    oci_rm
    echo "stopped $NAME"
    ;;
  restart)
    echo "==> restarting $NAME ($CMD)"
    ensure_image
    oci_rm
    oci_run "$@"
    echo "klepto listening on http://${LISTEN_HOST}"
    ;;
  status)
    echo "runtime: $CMD"
    echo "image:   $IMAGE"
    if image_exists; then
      echo "image status: present locally"
    else
      echo "image status: missing locally (run will build, or: make image)"
    fi
    echo "name:    $NAME"
    echo "listen:  http://${LISTEN_HOST}"
    if [[ "$CMD" == "docker" ]]; then
      docker ps -a --filter "name=^/${NAME}$" --format 'table {{.ID}}\t{{.Status}}\t{{.Ports}}' 2>/dev/null \
        || docker ps -a --filter "name=${NAME}" --format 'table {{.ID}}\t{{.Status}}\t{{.Ports}}'
    else
      container list 2>/dev/null || container ls 2>/dev/null || true
    fi
    # 0.0.0.0 is a bind address; probe via loopback for health checks.
    probe_host="${LISTEN_HOST}"
    if [[ "${LISTEN_HOST%%:*}" == "0.0.0.0" ]]; then
      probe_host="127.0.0.1:${LISTEN_HOST##*:}"
    fi
    if curl -fsS "http://${probe_host}/v1/health" >/dev/null 2>&1; then
      echo "health:  ok"
      curl -fsS "http://${probe_host}/v1/health"
      echo
    else
      echo "health:  unreachable"
    fi
    ;;
  logs)
    if [[ "$CMD" == "docker" ]]; then
      docker logs "$@" "$NAME"
    else
      container logs "$@" "$NAME" 2>/dev/null || container logs "$NAME" "$@"
    fi
    ;;
  shell)
    if [[ "$CMD" == "docker" ]]; then
      docker exec -it "$NAME" bash || docker exec -it "$NAME" sh
    else
      container exec -it "$NAME" bash 2>/dev/null || container exec "$NAME" bash \
        || container exec -it "$NAME" sh
    fi
    ;;
  *)
    echo "usage: $0 {which|build|run|stop|restart|status|logs|shell} [args…]" >&2
    exit 1
    ;;
esac
