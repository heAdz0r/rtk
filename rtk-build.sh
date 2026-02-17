#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

DEBUG_BIN="$ROOT_DIR/target/debug/rtk"
RELEASE_BIN="$ROOT_DIR/target/release/rtk"
USER_BIN="$HOME/.cargo/bin/rtk"
USR_LOCAL_BIN="/usr/local/bin/rtk"

BUILD_DEBUG=1
BUILD_RELEASE=1
INSTALL_USER=1
INSTALL_USR_LOCAL=1
VERIFY=1
SYMLINK_USR_LOCAL=0
USE_SUDO=1
SET_VERSION=""

usage() {
  cat <<'EOF'
Usage: ./rtk-build.sh [options]

Build and update all rtk binaries in one run.

Options:
  --no-debug            Skip `cargo build`
  --no-release          Skip `cargo build --release`
  --skip-user           Skip install to ~/.cargo/bin/rtk
  --skip-usr-local      Skip install to /usr/local/bin/rtk
  --symlink-usr-local   Set /usr/local/bin/rtk -> ~/.cargo/bin/rtk symlink
  --set-version X       Update version in Cargo.toml + src/main.rs (e.g. 0.20.0-fork.2)
  --no-verify           Skip post-build verification
  --no-sudo             Never call sudo (same as RTK_BUILD_NO_SUDO=1)
  -h, --help            Show this help

Env:
  RTK_BUILD_NO_SUDO=1   Disable sudo usage
EOF
}

log() {
  printf '[rtk-build] %s\n' "$*"
}

warn() {
  printf '[rtk-build] WARN: %s\n' "$*" >&2
}

die() {
  printf '[rtk-build] ERROR: %s\n' "$*" >&2
  exit 1
}

sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

install_with_optional_sudo() {
  local src="$1"
  local dst="$2"

  # Fast path: existing writable file (no directory write required).
  if [ -f "$dst" ] && [ -w "$dst" ]; then
    cp "$src" "$dst"
    chmod 755 "$dst"
    return 0
  fi

  # New file / replace path when directory is writable.
  if [ -w "$(dirname "$dst")" ]; then
    install -m 755 "$src" "$dst"
    return 0
  fi
  if [ "${USE_SUDO}" -eq 1 ] && command -v sudo >/dev/null 2>&1; then
    sudo install -m 755 "$src" "$dst"
    return 0
  fi
  warn "No permissions to update $dst (skipped)"
  return 1
}

link_with_optional_sudo() {
  local src="$1"
  local dst="$2"
  if [ -w "$dst" ] || [ -L "$dst" ] || [ -w "$(dirname "$dst")" ]; then
    ln -sfn "$src" "$dst"
    return 0
  fi
  if [ "${USE_SUDO}" -eq 1 ] && command -v sudo >/dev/null 2>&1; then
    sudo ln -sfn "$src" "$dst"
    return 0
  fi
  warn "No permissions to update $dst (skipped)"
  return 1
}

verify_binary() {
  local path="$1"
  echo "== $path =="
  if [ ! -e "$path" ]; then
    echo "missing"
    echo
    return 0
  fi

  ls -l "$path"
  if command -v realpath >/dev/null 2>&1; then
    realpath "$path" || true
  fi
  echo "sha256: $(sha256_file "$path")"
  "$path" --version
  if "$path" ssh --help 2>&1 | rg -q "SSH with smart output filtering"; then
    echo "ssh-subcommand: present"
  else
    echo "ssh-subcommand: NOT present"
  fi
  echo
}

validate_version() {
  local v="$1"
  if [[ ! "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z._-]+)?$ ]]; then
    die "Invalid version: $v"
  fi
}

set_project_version() {
  local v="$1"
  log "set version -> $v"

  perl -0777 -i -pe "s/^version\\s*=\\s*\\\"[^\\\"]+\\\"/version = \\\"$v\\\"/m" Cargo.toml
  perl -0777 -i -pe "s/version\\s*=\\s*\\\"[^\\\"]+\\\"/version = \\\"$v\\\"/m" src/main.rs
}

while [ $# -gt 0 ]; do
  arg="$1"
  case "$arg" in
    --no-debug) BUILD_DEBUG=0 ;;
    --no-release) BUILD_RELEASE=0 ;;
    --skip-user) INSTALL_USER=0 ;;
    --skip-usr-local) INSTALL_USR_LOCAL=0 ;;
    --symlink-usr-local) SYMLINK_USR_LOCAL=1 ;;
    --no-verify) VERIFY=0 ;;
    --no-sudo) USE_SUDO=0 ;;
    --set-version)
      [ $# -ge 2 ] || die "--set-version requires a value"
      SET_VERSION="$2"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      warn "Unknown option: $arg"
      usage
      exit 1
      ;;
  esac
  shift
done

if [ "${RTK_BUILD_NO_SUDO:-0}" = "1" ]; then
  USE_SUDO=0
fi

if [ -n "$SET_VERSION" ]; then
  validate_version "$SET_VERSION"
  set_project_version "$SET_VERSION"
fi

if [ "$BUILD_DEBUG" -eq 1 ]; then
  log "cargo build"
  cargo build
fi

if [ "$BUILD_RELEASE" -eq 1 ]; then
  log "cargo build --release"
  cargo build --release
fi

if [ ! -x "$RELEASE_BIN" ]; then
  warn "Release binary not found: $RELEASE_BIN"
  exit 1
fi

if [ "$INSTALL_USER" -eq 1 ]; then
  mkdir -p "$(dirname "$USER_BIN")"
  log "install -> $USER_BIN"
  install -m 755 "$RELEASE_BIN" "$USER_BIN"
fi

if [ "$INSTALL_USR_LOCAL" -eq 1 ]; then
  if [ "$SYMLINK_USR_LOCAL" -eq 1 ]; then
    log "symlink -> $USR_LOCAL_BIN -> $USER_BIN"
    link_with_optional_sudo "$USER_BIN" "$USR_LOCAL_BIN" || true
  else
    log "install -> $USR_LOCAL_BIN"
    install_with_optional_sudo "$RELEASE_BIN" "$USR_LOCAL_BIN" || true
  fi
fi

if [ "$VERIFY" -eq 1 ]; then
  log "verification (4 binaries)"
  verify_binary "$DEBUG_BIN"
  verify_binary "$RELEASE_BIN"
  verify_binary "$USER_BIN"
  verify_binary "$USR_LOCAL_BIN"
fi

log "done"
