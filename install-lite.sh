#!/usr/bin/env bash
# shellcheck disable=SC2088  # a ~ in a message is for the person reading it, not for the shell
# eve lite's one-line installer: eve on an Apple-silicon Mac (macOS 14 or later) with no Xcode tools, no Homebrew and
# no iPhone app (docs/lite.md). Run it with:
#
#   curl -fsSL https://github.com/gurul/eve_01/releases/latest/download/install-lite.sh | bash
#
# One status line shows where it is; the details of each run go to ~/.eve-lite/install.log. It asks no questions:
#   1. getting Python: uv, pinned and checked by SHA-256, into ~/.eve-lite/bin; then uv's own CPython 3.13.14
#   2. getting eve lite: the release tarball, checked by SHA-256 before it is opened, into ~/.eve-lite/release
#   3. installing packages: a venv at ~/.eve-lite/venv, the release's hash-pinned wheels (wheels only), then eve-bridge
#   4. starting eve: EVE_PROFILE=lite in ~/.config/eve-bridge/env (mode 600), Eve.app in ~/Applications, and the
#      login service (`eve-bridge install --service`)
#   5. opening setup: `eve-bridge web open` signs the browser in to http://localhost:48780 and opens it there
# Setup is then two browser sign-ins (OpenRouter, then Composio). No key is typed or pasted anywhere.
#
# Every uv call runs with a PATH that holds only ~/.eve-lite/bin. /usr/bin's developer-tool stand-ins (git, python3,
# install_name_tool and 75 more) open macOS's "install developer tools" dialog when they start, and uv starts
# install_name_tool by name while it installs Python; with nothing on PATH it only warns. This script itself starts
# none of them, and no package manager.
#
# Full eve and eve lite share ~/.config/eve-bridge, so this refuses to run while full eve is installed for this user,
# or while that folder's env file is full eve's: one that does not set EVE_PROFILE=lite, unless eve lite's own
# uninstall left it so (see --uninstall).
#
# Options (after `bash -s --` when piped):
#   --dry-run     check this Mac and print what an install would do; write nothing
#   --uninstall   remove eve lite: its login service, ~/.eve-lite and Eve.app. It asks before deleting
#                 ~/.config/eve-bridge, which holds eve's memory, and keeps it when there is no terminal to ask.
#                 Kept, the folder loses the EVE_PROFILE=lite line (full eve installed later must not start as
#                 lite) and gains .eve-lite-kept, which holds the env file's SHA-256; the install line takes the
#                 folder back while the env file is unchanged
#   --no-start    install into ~/.eve-lite only: no env file, app or login service, and nothing opens
#                 (tools/release_lite.sh checks each release this way, in a scratch HOME)
#
# Running it again updates eve lite and keeps the settings and memory. Everything is inside main(), so a download
# cut short runs nothing.

set -euo pipefail

# The release. tools/release_lite.sh stamps these three lines; a copy without them refuses to install.
LITE_VERSION="0.1.2"
LITE_URL="https://github.com/gurul/eve_01/releases/download/lite-v0.1.2/eve-lite-0.1.2.tar.gz"
LITE_SHA256="0ea140ef2e5c36cf057f32bf09a6be7eac5347c8be67836382bf2623a26ca43e"
# uv, from its GitHub release. The SHA-256 is the release's own uv-aarch64-apple-darwin.tar.gz.sha256 (2026-10-13).
UV_VERSION="0.11.27"
UV_URL="https://github.com/astral-sh/uv/releases/download/0.11.27/uv-aarch64-apple-darwin.tar.gz"
UV_SHA256="34e63cc0de0aebbc8d424767c588c31b685479f045f9ced9e5ef43ff9e0e8d63"
# uv's standalone CPython; uv checks the download against its own hash list. Pinned to the patch: macOS ties privacy
# grants (Local Network) to the binary, and a new patch is a new binary.
PYTHON_VERSION="3.13.14"
# cryptography's macOS wheels are arm64 only, and av's arm64 wheel needs macOS 14.
MACOS_MIN=14

FULL_LABEL="com.github.eve-bridge.daemon"
LITE_LABEL="com.github.eve-bridge.lite"
# eve-bridge runs with the login service's PATH: launchctl is in /bin, open in /usr/bin.
SERVICE_PATH="/usr/bin:/bin:/usr/sbin:/sbin"
STEPS=5

# State, set as the run goes.
LOG=""            # ~/.eve-lite/install.log once it exists
WORK=""           # this run's scratch folder inside ~/.eve-lite, removed on exit
STEP="checking this Mac"
SAID=0            # 1 once a failure has been explained, so the exit trap adds nothing
MACOS_VERSION=""
WHEEL_NAME=""
SETUP_URL=""
KEY=""
VALUE=""
ENV_LEFT=0        # set by drop_profile_line: 1 when the env file holds anything besides EVE_PROFILE
PASS_ENV=()

usage() {
  cat <<'EOF'
install-lite.sh installs eve lite: eve for a Mac with no Xcode tools and no Homebrew (docs/lite.md).

  curl -fsSL https://github.com/gurul/eve_01/releases/latest/download/install-lite.sh | bash
  curl -fsSL https://github.com/gurul/eve_01/releases/latest/download/install-lite.sh | bash -s -- --uninstall

Options:
  --dry-run     check this Mac and print what an install would do; write nothing
  --uninstall   remove eve lite; asks before deleting ~/.config/eve-bridge, which holds eve's memory
  --no-start    install into ~/.eve-lite only: no env file, app or login service, and nothing opens
EOF
}

set_paths() {
  LITE_HOME="$HOME/.eve-lite"
  VENV="$LITE_HOME/venv"
  REL="$LITE_HOME/release"
  LOG_FILE="$LITE_HOME/install.log"
  ENV_DIR="$HOME/.config/eve-bridge"
  ENV_FILE="$ENV_DIR/env"
  KEPT="$ENV_DIR/.eve-lite-kept"
  FULL_PLIST="$HOME/Library/LaunchAgents/$FULL_LABEL.plist"
  LITE_PLIST="$HOME/Library/LaunchAgents/$LITE_LABEL.plist"
  APP="$HOME/Applications/Eve.app"
  DAEMON_LOG="$HOME/Library/Logs/eve-lite.log"
  TMP_DIR="${TMPDIR:-/tmp}"
}

# --- Output: one status line, one final line, details in the log ---------------------------------------------------

tty_err() { [ -t 2 ]; }

clear_line() {
  if tty_err; then printf '\r\033[K' >&2; fi
}

tilde() {  # tilde PATH: PATH with $HOME shown as ~
  case "$1" in
    "$HOME"/*) printf '~/%s' "${1#"$HOME"/}" ;;
    *) printf '%s' "$1" ;;
  esac
}

log() {
  if [ -n "$LOG" ]; then printf '%s\n' "$*" >>"$LOG"; fi
}

status() {  # status N LABEL [HINT]: the status line, rewritten in place in a terminal, a plain line otherwise
  local text="$2"
  STEP="$2"
  if [ $# -ge 3 ]; then text="$2, $3"; fi
  log "== ($1/$STEPS) $2"
  if tty_err; then
    printf '\r\033[Keve lite: %s (%s/%s)' "$text" "$1" "$STEPS" >&2
  else
    printf 'eve lite: %s (%s/%s)\n' "$text" "$1" "$STEPS" >&2
  fi
}

die() {  # die TEXT: one plain line on stderr and in the log, then stop
  clear_line
  printf 'eve lite: %s\n' "$*" >&2
  log "error: $*"
  SAID=1
  exit 1
}

finish() {  # finish TEXT: the line the run ends with, the only one on stdout
  clear_line
  log "done: $*"
  printf '%s\n' "$*"
}

run() {  # run CMD...: the command's output goes to the log, and it reads nothing (piped from curl, stdin is this script)
  if [ -n "$LOG" ]; then "$@" </dev/null >>"$LOG" 2>&1; else "$@" </dev/null >/dev/null 2>&1; fi
}

on_exit() {
  local rc=$?
  if [ -n "$WORK" ]; then rm -rf "$WORK"; fi
  if [ "$rc" -ne 0 ] && [ "$SAID" = 0 ]; then
    clear_line
    printf 'eve lite: stopped while %s.\n' "$STEP" >&2
    if [ -n "$LOG" ] && [ -s "$LOG" ]; then
      printf 'The end of %s:\n' "$(tilde "$LOG")" >&2
      tail -n 8 "$LOG" | sed 's/^/  /' >&2 || true
    fi
  fi
}

# --- Small helpers ---------------------------------------------------------------------------------------------------

is_sha256() {
  case "$1" in ''|*[!0-9a-f]*) return 1 ;; esac
  [ ${#1} -eq 64 ]
}

sha256_of() {  # sha256_of FILE: prints the lowercase hex SHA-256; shasum first, openssl when shasum cannot
  local line=""
  if command -v shasum >/dev/null 2>&1; then line="$(shasum -a 256 "$1" 2>/dev/null)" || line=""; fi
  if [ -z "$line" ]; then line="$(openssl dgst -sha256 -r "$1" 2>/dev/null)" || line=""; fi
  line="${line%% *}"
  is_sha256 "$line" || return 1
  printf '%s\n' "$line"
}

sh_quote() {  # sh_quote TEXT: TEXT as one /bin/sh word
  local s="$1" q="'\\''"
  printf "'%s'" "${s//\'/$q}"
}

as_quote() {  # as_quote TEXT: TEXT for inside an AppleScript string literal
  local s="$1"
  s="${s//\\/\\\\}"
  printf '%s' "${s//\"/\\\"}"
}

env_assignment() {  # env_assignment LINE: sets KEY and VALUE the way envfile.parse_env_file reads LINE; KEY is empty for any other line
  local line="$1"
  KEY=""
  VALUE=""
  line="${line#"${line%%[![:space:]]*}"}"
  line="${line%"${line##*[![:space:]]}"}"
  case "$line" in
    ''|'#'*) return 0 ;;
    'export '*) line="${line#export }"; line="${line#"${line%%[![:space:]]*}"}" ;;
  esac
  case "$line" in *=*) ;; *) return 0 ;; esac
  KEY="${line%%=*}"
  KEY="${KEY%"${KEY##*[![:space:]]}"}"
  case "$KEY" in ''|[!A-Za-z_]*|*[!A-Za-z0-9_]*) KEY=""; return 0 ;; esac
  VALUE="${line#*=}"
  VALUE="${VALUE#"${VALUE%%[![:space:]]*}"}"
  VALUE="${VALUE%"${VALUE##*[![:space:]]}"}"
  if [ ${#VALUE} -ge 2 ]; then
    case "$VALUE" in
      \'*\'|\"*\") VALUE="${VALUE:1:$((${#VALUE} - 2))}" ;;
    esac
  fi
  return 0
}

env_profile() {  # prints the EVE_PROFILE the env file sets (the last line wins, as in envfile.py), or nothing
  local line profile=""
  if [ ! -r "$ENV_FILE" ]; then printf 'unreadable'; return 0; fi
  while IFS= read -r line || [ -n "$line" ]; do
    env_assignment "$line"
    if [ "$KEY" = EVE_PROFILE ]; then profile="$VALUE"; fi
  done <"$ENV_FILE"
  printf '%s' "$profile"
}

kept_by_lite() {  # true while ~/.config/eve-bridge is as eve lite's uninstall kept it: the env file is unchanged since
  local line want="" got=none
  [ -f "$KEPT" ] || return 1
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in env_sha256=*) want="${line#env_sha256=}" ;; esac
  done <"$KEPT"
  if [ -e "$ENV_FILE" ] || [ -L "$ENV_FILE" ]; then got="$(sha256_of "$ENV_FILE")" || return 1; fi
  [ -n "$want" ] && [ "$want" = "$got" ]
}

pass_env() {  # proxy and certificate settings survive uv's `env -i`, so uv reaches the internet the way curl does
  local name value
  PASS_ENV=()
  for name in HTTPS_PROXY https_proxy HTTP_PROXY http_proxy ALL_PROXY all_proxy NO_PROXY no_proxy SSL_CERT_FILE SSL_CERT_DIR; do
    eval "value=\${$name:-}"
    if [ -n "$value" ]; then PASS_ENV+=("$name=$value"); fi
  done
}

uv_env() {  # uv_env PROGRAM ARGS...: run PROGRAM (uv) with nothing on PATH but ~/.eve-lite/bin
  env -i HOME="$HOME" TMPDIR="$TMP_DIR" PATH="$LITE_HOME/bin" \
    UV_NO_CONFIG=1 UV_PYTHON_PREFERENCE=only-managed UV_PYTHON_INSTALL_DIR="$LITE_HOME/python" \
    UV_PYTHON_INSTALL_BIN=0 UV_CACHE_DIR="$LITE_HOME/cache" UV_NO_MODIFY_PATH=1 UV_NO_PROGRESS=1 NO_COLOR=1 \
    ${PASS_ENV[@]+"${PASS_ENV[@]}"} "$@"
}

uv_do() {  # uv_do ARGS...: ~/.eve-lite/bin/uv, output to the log
  log "+ uv $*"
  uv_env "$LITE_HOME/bin/uv" "$@" </dev/null >>"$LOG" 2>&1
}

eve_do() {  # eve_do ARGS...: the venv's eve-bridge with the login service's PATH and the lite profile
  log "+ eve-bridge $*"
  env -u PYTHONPATH -u PYTHONHOME PATH="$SERVICE_PATH" EVE_PROFILE=lite PYTHONNOUSERSITE=1 \
    "$VENV/bin/eve-bridge" "$@" </dev/null
}

fetch() {  # fetch URL FILE SHA256 WHAT: download, and delete it unopened unless the SHA-256 matches
  local url="$1" out="$2" want="$3" what="$4" got=""
  log "+ curl $url"
  # -q (first): no ~/.curlrc, so curl, like uv, takes only the environment's proxy settings
  run curl -q -fsSL --proto '=https,file' --proto-redir '=https' --retry 3 --connect-timeout 20 -o "$out" "$url" \
    || die "could not download $what. Check the internet connection, then run the line again."
  got="$(sha256_of "$out")" || die "cannot compute a SHA-256 on this Mac (shasum and openssl both failed)."
  if [ "$got" != "$want" ]; then
    rm -f "$out"
    log "sha256 of $what: expected $want, got $got"
    die "the $what download does not match its SHA-256, so it was deleted without being opened. Run the line again; if this repeats, report it."
  fi
  log "sha256 of $what: ok"
}

# --- Checks ----------------------------------------------------------------------------------------------------------

preflight() {
  local major
  [ "$(uname -s)" = Darwin ] || die "eve lite runs on a Mac (macOS $MACOS_MIN or later); this is $(uname -s)."
  # Under Rosetta uname says x86_64, so ask the hardware too.
  if [ "$(uname -m)" != arm64 ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" != 1 ]; then
    die "eve lite needs a Mac with Apple silicon (M1 or later)."
  fi
  MACOS_VERSION="$(sw_vers -productVersion 2>/dev/null || true)"
  major="${MACOS_VERSION%%.*}"
  case "$major" in ''|*[!0-9]*) die "cannot read this Mac's macOS version." ;; esac
  if [ "$major" -lt "$MACOS_MIN" ]; then die "eve lite needs macOS $MACOS_MIN or later; this Mac has $MACOS_VERSION."; fi
  if [ "$(id -u)" = 0 ]; then die "run this as yourself, not with sudo."; fi
  if [ -e "$FULL_PLIST" ]; then
    die "full eve is installed for this macOS user ($(tilde "$FULL_PLIST")). Full eve and eve lite share ~/.config/eve-bridge, so one user can have only one of them. Nothing was changed."
  fi
  if [ -e "$ENV_FILE" ] && [ "$(env_profile)" != lite ]; then
    [ -r "$ENV_FILE" ] || die "cannot read ~/.config/eve-bridge/env. Nothing was changed."
    # eve lite's uninstall takes its EVE_PROFILE line out when it keeps the folder, and leaves its marker instead
    if [ -e "$KEPT" ] && ! kept_by_lite; then
      die "~/.config/eve-bridge/env changed after eve lite was uninstalled, and it does not set EVE_PROFILE=lite, so it may be full eve's now. Full eve and eve lite share that folder, so one user can have only one of them. If the folder is eve lite's, move that env file away and run the line again: setup then asks for the OpenRouter sign-in again. Nothing was changed."
    fi
    kept_by_lite || die "~/.config/eve-bridge/env belongs to full eve (it does not set EVE_PROFILE=lite). Full eve and eve lite share that folder, so one user can have only one of them. Nothing was changed."
  fi
  if [ -z "$LITE_VERSION" ] || [ -z "$LITE_URL" ] || ! is_sha256 "$LITE_SHA256"; then
    die "this copy of install-lite.sh is not from a release, so it has no eve lite to install. Use the line in docs/lite.md."
  fi
  command -v curl >/dev/null 2>&1 || die "curl is missing from this Mac."
  command -v tar >/dev/null 2>&1 || die "tar is missing from this Mac."
}

# --- The install -----------------------------------------------------------------------------------------------------

get_uv() {
  local uv="$LITE_HOME/bin/uv" stamp="$LITE_HOME/bin/uv.sha256" have="" version=""
  have="$(cat "$stamp" 2>/dev/null || true)"
  if [ -x "$uv" ] && [ "$have" = "$UV_SHA256" ]; then
    log "uv $UV_VERSION: already in ~/.eve-lite/bin"
  else
    fetch "$UV_URL" "$WORK/uv.tar.gz" "$UV_SHA256" "uv $UV_VERSION"
    mkdir "$WORK/uv"
    run tar -xzf "$WORK/uv.tar.gz" -C "$WORK/uv" || die "could not unpack uv."
    [ -f "$WORK/uv/uv-aarch64-apple-darwin/uv" ] || die "the uv download does not hold uv."
    mkdir -p "$LITE_HOME/bin"
    rm -f "$stamp"
    mv -f "$WORK/uv/uv-aarch64-apple-darwin/uv" "$uv"
    chmod 755 "$uv"
    printf '%s\n' "$UV_SHA256" >"$stamp"
  fi
  version="$(uv_env "$uv" --version </dev/null 2>>"$LOG")" || die "uv does not start on this Mac."
  case "$version" in
    "uv $UV_VERSION"|"uv $UV_VERSION "*) log "$version" ;;
    *) die "~/.eve-lite/bin/uv is not uv $UV_VERSION." ;;
  esac
}

check_sums() {  # check_sums DIR: every file DIR/SHA256SUMS lists matches, and the list holds the wheel and the requirements
  local dir="$1" line want rest name got reqs=0
  WHEEL_NAME=""
  while IFS= read -r line || [ -n "$line" ]; do
    if [ -z "$line" ]; then continue; fi
    want="${line:0:64}"
    rest="${line:64}"
    case "$rest" in
      "  "*|" *"*) name="${rest:2}" ;;
      *) die "the release's SHA256SUMS has a line this script cannot read." ;;
    esac
    is_sha256 "$want" || die "the release's SHA256SUMS has a line this script cannot read."
    case "$name" in
      ''|/*|*..*|*[!A-Za-z0-9._+/-]*) die "the release's SHA256SUMS names a file this script will not open." ;;
    esac
    [ -f "$dir/$name" ] || die "the release is missing $name."
    got="$(sha256_of "$dir/$name")" || die "cannot compute a SHA-256 on this Mac (shasum and openssl both failed)."
    [ "$got" = "$want" ] || die "the release's $name does not match its SHA256SUMS."
    case "$name" in
      "eve_bridge-$LITE_VERSION-"*.whl) WHEEL_NAME="$name" ;;
      requirements-lite.txt) reqs=1 ;;
    esac
  done <"$dir/SHA256SUMS"
  if [ -z "$WHEEL_NAME" ] || [ "$reqs" != 1 ]; then
    die "the release does not hold eve-bridge $LITE_VERSION's wheel and its requirements."
  fi
  log "release files: match SHA256SUMS"
}

get_release() {
  local top="$WORK/release/eve-lite-$LITE_VERSION"
  fetch "$LITE_URL" "$WORK/release.tar.gz" "$LITE_SHA256" "eve lite $LITE_VERSION"
  mkdir "$WORK/release"
  run tar -xzf "$WORK/release.tar.gz" -C "$WORK/release" || die "could not unpack the eve lite release."
  [ -f "$top/SHA256SUMS" ] || die "the eve lite release has no SHA256SUMS."
  check_sums "$top"
  rm -rf "$REL.old"
  if [ -e "$REL" ]; then mv "$REL" "$REL.old"; fi
  mv "$top" "$REL"
  rm -rf "$REL.old"
}

make_venv() {
  if [ -x "$VENV/bin/python" ] && grep -qxF "version_info = $PYTHON_VERSION" "$VENV/pyvenv.cfg" 2>/dev/null; then
    log "venv: kept (CPython $PYTHON_VERSION)"
    return 0
  fi
  rm -rf "$VENV"
  uv_do venv --python "$PYTHON_VERSION" "$VENV" || die "uv could not create the venv."
}

install_packages() {
  uv_do pip install --require-hashes --only-binary :all: --python "$VENV/bin/python" -r "$REL/requirements-lite.txt" \
    || die "uv could not install eve lite's packages. Check the internet connection, then run the line again."
  uv_do pip install --no-deps --reinstall-package eve-bridge --python "$VENV/bin/python" "$REL/$WHEEL_NAME" \
    || die "uv could not install eve-bridge."
  [ -x "$VENV/bin/eve-bridge" ] || die "the install finished, but the venv has no eve-bridge."
}

write_env_file() {  # EVE_PROFILE=lite in the env file; the file stays mode 600 and is never printed
  local tmp
  mkdir -p "$ENV_DIR"
  chmod 700 "$ENV_DIR"
  if [ -f "$ENV_FILE" ] && [ "$(env_profile)" = lite ]; then
    chmod 600 "$ENV_FILE"
    log "env file: kept, EVE_PROFILE=lite already set"
  elif [ -f "$ENV_FILE" ]; then
    # what eve lite's uninstall kept (preflight checked; asked again, as minutes have passed): every line stays and
    # EVE_PROFILE=lite goes last, where it wins
    kept_by_lite || die "~/.config/eve-bridge/env changed while this ran and does not set EVE_PROFILE=lite, so it was left as it is and eve lite was not started. Run the line again."
    tmp="$(mktemp "$ENV_DIR/.env.XXXXXX")"
    cat "$ENV_FILE" >"$tmp"
    if [ -n "$(tail -c 1 "$ENV_FILE")" ]; then printf '\n' >>"$tmp"; fi
    printf 'EVE_PROFILE=lite\n' >>"$tmp"
    chmod 600 "$tmp"
    mv -f "$tmp" "$ENV_FILE"
    log "env file: kept from the last uninstall; EVE_PROFILE=lite added"
  else
    tmp="$(mktemp "$ENV_DIR/.env.XXXXXX")"
    printf 'EVE_PROFILE=lite\n' >"$tmp"
    chmod 600 "$tmp"
    mv -f "$tmp" "$ENV_FILE"
    log "env file: written, EVE_PROFILE=lite"
  fi
  # the folder is eve lite's again, by its env file
  if [ -f "$KEPT" ]; then rm -f "$KEPT"; fi
}

app_source() {  # the AppleScript inside Eve.app, or nothing when it holds none
  osadecompile "$APP" </dev/null 2>/dev/null || true
}

app_is_ours() {  # an Eve.app whose script runs an eve lite venv's eve-bridge
  local src
  [ -d "$APP" ] || return 1
  src="$(app_source)"
  case "$src" in *"/.eve-lite/venv/bin/eve-bridge"*) return 0 ;; esac
  return 1
}

make_app() {  # ~/Applications/Eve.app runs `eve-bridge web open`; optional, so a failure is only logged
  local cmd script built="$WORK/Eve.app"
  cmd="EVE_PROFILE=lite PYTHONNOUSERSITE=1 $(sh_quote "$VENV/bin/eve-bridge") web open"
  script="do shell script \"$(as_quote "$cmd")\""
  # osacompile signs the applet ad hoc, so the bundle is never edited afterwards; it is recognised by its script
  if [ -e "$APP" ]; then
    if ! app_is_ours; then
      log "Eve.app: one is there that eve lite did not create; left as it is"
      return 0
    fi
    if [ "$(app_source)" = "$script" ]; then
      log "Eve.app: kept"
      return 0
    fi
  fi
  if ! command -v osacompile >/dev/null 2>&1; then
    log "Eve.app: osacompile is missing; skipped"
    return 0
  fi
  log "+ osacompile -o Eve.app"
  if ! run osacompile -o "$built" -e "$script"; then
    log "Eve.app: osacompile failed; skipped"
    return 0
  fi
  if ! mkdir -p "$HOME/Applications"; then
    log "Eve.app: cannot create ~/Applications; skipped"
    return 0
  fi
  rm -rf "$APP"
  if ! mv "$built" "$APP"; then log "Eve.app: could not move it into ~/Applications"; fi
  return 0
}

open_setup() {  # sets SETUP_URL to the address `eve-bridge web open` reports (48780 unless it took a fallback port)
  local out="" rc=0 err="$WORK/web-open.err" line rest port=""
  out="$(eve_do web open --wait 90 2>"$err")" || rc=$?
  cat "$err" >>"$LOG"
  if [ "$rc" -ne 0 ]; then
    # argparse's refusal (exit 2, its usage line first) is this release's eve-bridge and these arguments disagreeing,
    # not eve failing to answer (tools/release_lite.sh checks every release for it)
    if [ "$rc" -eq 2 ] && grep -q '^usage: eve-bridge' "$err"; then
      die "eve lite is installed, but its eve-bridge does not take \`web open --wait 90\`, so setup did not open. Open Eve from Applications, and please report it."
    fi
    die "eve did not answer within 90 seconds. Open Eve from Applications in a minute, or see ~/Library/Logs/eve-lite.log."
  fi
  while IFS= read -r line; do
    case "$line" in
      *http://localhost:*)
        rest="${line#*http://localhost:}"
        port="${rest%%[!0-9]*}"
        if [ -n "$port" ]; then break; fi
        ;;
    esac
  done <<EOF
$out
EOF
  SETUP_URL="http://localhost:${port:-48780}"
  log "setup page: $SETUP_URL"
}

install_lite() {
  local start="$1"
  if [ "$start" = 0 ]; then STEPS=3; fi
  umask 077
  mkdir -p "$LITE_HOME"
  chmod 700 "$LITE_HOME"
  LOG="$LOG_FILE"
  : >"$LOG"
  log "eve lite $LITE_VERSION installer on macOS $MACOS_VERSION, $(date -u '+%Y-%m-%d %H:%M:%S') UTC"
  rm -rf "$LITE_HOME"/tmp.*
  WORK="$(mktemp -d "$LITE_HOME/tmp.XXXXXX")"

  status 1 "getting Python"
  get_uv
  uv_do python install "$PYTHON_VERSION" --no-bin || die "uv could not install Python $PYTHON_VERSION."

  status 2 "getting eve lite $LITE_VERSION"
  get_release

  status 3 "installing packages" "a few minutes the first time"
  make_venv
  install_packages

  if [ "$start" = 0 ]; then
    finish "eve lite $LITE_VERSION is installed in ~/.eve-lite; nothing was started (--no-start)."
    return 0
  fi

  status 4 "starting eve"
  write_env_file
  make_app
  eve_do install --service >>"$LOG" 2>&1 || die "eve-bridge could not install the login service. Details: ~/.eve-lite/install.log"

  status 5 "opening setup"
  open_setup
  finish "eve lite is running. Setup is open in your browser: $SETUP_URL"
}

dry_run() {
  local start="$1" env_line
  if [ -f "$ENV_FILE" ] && [ "$(env_profile)" = lite ]; then
    env_line="keep ~/.config/eve-bridge/env, which already sets EVE_PROFILE=lite"
  elif [ -f "$ENV_FILE" ]; then
    env_line="add EVE_PROFILE=lite to ~/.config/eve-bridge/env, which eve lite's uninstall kept (mode 600)"
  else
    env_line="write ~/.config/eve-bridge/env with EVE_PROFILE=lite, mode 600"
  fi
  cat <<EOF
eve lite $LITE_VERSION, dry run on macOS $MACOS_VERSION: this Mac passes the checks. An install would:
  1. download uv $UV_VERSION from $UV_URL,
     check its SHA-256 and put uv in ~/.eve-lite/bin (skipped when that uv is already there)
  2. have uv install CPython $PYTHON_VERSION into ~/.eve-lite/python, with PATH=~/.eve-lite/bin and nothing else
  3. download $LITE_URL,
     check its SHA-256 and unpack it into ~/.eve-lite/release
  4. create ~/.eve-lite/venv and install the release's hash-pinned wheels (wheels only), then eve-bridge
EOF
  if [ "$start" = 1 ]; then
    cat <<EOF
  5. $env_line
  6. create ~/Applications/Eve.app and the login service $LITE_LABEL (eve-bridge install --service)
  7. open the setup page (eve-bridge web open)
EOF
  fi
  printf 'Nothing was written.\n'
}

# --- Uninstall -------------------------------------------------------------------------------------------------------

ask() {  # ask QUESTION: true on y or yes typed at the terminal; no terminal means no
  local answer=""
  { : </dev/tty; } 2>/dev/null || return 1
  clear_line
  printf '%s' "$1" >/dev/tty
  IFS= read -r answer </dev/tty || answer=""
  case "$answer" in [Yy]|[Yy][Ee][Ss]) return 0 ;; esac
  return 1
}

drop_profile_line() {  # remove EVE_PROFILE from the env file; every other line stays, and so does mode 600
  local tmp line
  ENV_LEFT=0
  tmp="$(mktemp "$ENV_DIR/.env.XXXXXX")"
  while IFS= read -r line || [ -n "$line" ]; do
    env_assignment "$line"
    if [ "$KEY" = EVE_PROFILE ]; then continue; fi
    case "$line" in *[![:space:]]*) ENV_LEFT=1 ;; esac
    printf '%s\n' "$line" >>"$tmp"
  done <"$ENV_FILE"
  chmod 600 "$tmp"
  mv -f "$tmp" "$ENV_FILE"
}

mark_kept() {  # leave the marker that lets the install line take the kept folder back: the env file's SHA-256, or none
  local tmp sum=none
  if [ -e "$ENV_FILE" ]; then
    sum="$(sha256_of "$ENV_FILE")" || die "cannot compute a SHA-256 on this Mac (shasum and openssl both failed)."
  fi
  tmp="$(mktemp "$ENV_DIR/.eve-lite-kept.XXXXXX")"
  {
    printf '# eve lite was uninstalled and kept this folder. Its install line takes the folder back while env is\n'
    printf '# what the uninstall left: the file with this SHA-256, or none at all.\n'
    printf 'env_sha256=%s\n' "$sum"
  } >"$tmp"
  chmod 600 "$tmp"
  mv -f "$tmp" "$KEPT"
}

uninstall() {
  local dry="$1" profile=0 kept=0 full=0 ours=0 listed=0 path
  # EVE_PROFILE=lite is eve lite's own line (only this script writes it), so it always goes. The folder is eve lite's
  # alone when no full eve is beside it and its env file says lite, or an earlier uninstall kept it (the marker):
  # only then is it offered for deletion, and kept, marked for the install line to take back.
  if [ -f "$ENV_FILE" ] && [ "$(env_profile)" = lite ]; then profile=1; fi
  if kept_by_lite; then kept=1; fi
  if [ -e "$FULL_PLIST" ]; then full=1; fi
  if [ "$full" = 0 ] && { [ "$profile" = 1 ] || [ "$kept" = 1 ]; }; then ours=1; fi
  if [ "$dry" = 1 ]; then
    printf 'eve lite --uninstall, dry run. It would stop the login service %s if it is loaded' "$LITE_LABEL"
    for path in "$LITE_PLIST" "$LITE_HOME" "$DAEMON_LOG" "$APP"; do
      if [ "$path" = "$APP" ]; then app_is_ours || continue; elif [ ! -e "$path" ]; then continue; fi
      if [ "$listed" = 0 ]; then printf ', and remove:\n'; listed=1; fi
      printf '  %s\n' "$(tilde "$path")"
    done
    if [ "$listed" = 0 ]; then printf "; nothing else of eve lite's is left to remove.\n"; fi
    if [ "$ours" = 1 ]; then
      printf "Then it would ask before deleting ~/.config/eve-bridge, which holds eve's memory. If it is kept, the\n"
      if [ "$profile" = 1 ]; then
        printf 'folder loses only the EVE_PROFILE=lite line (the env file goes when nothing else is in it), and the\n'
        printf 'install line takes it back.\n'
      else
        printf 'folder stays as an earlier uninstall left it, and the install line takes it back.\n'
      fi
    elif [ "$profile" = 1 ]; then
      printf 'It would also take the EVE_PROFILE=lite line out of ~/.config/eve-bridge/env, which full eve keeps.\n'
    fi
    printf 'Nothing was written.\n'
    return 0
  fi
  STEP="removing eve lite"
  # The service first, so nothing runs from ~/.eve-lite while it goes.
  launchctl bootout "gui/$(id -u)/$LITE_LABEL" </dev/null >/dev/null 2>&1 || true
  rm -f "$LITE_PLIST"
  if app_is_ours; then rm -rf "$APP"; fi
  rm -rf "$LITE_HOME"
  rm -f "$DAEMON_LOG"
  if [ "$ours" = 1 ] && ask "Also delete eve's memory, chats and sign-ins in ~/.config/eve-bridge? [y/N] "; then
    rm -rf "$ENV_DIR"
    finish "eve lite is uninstalled, with its memory and settings."
    return 0
  fi
  if [ "$profile" = 1 ]; then
    drop_profile_line
    # an env file that held nothing but the profile goes, rather than stay behind empty
    if [ "$ours" = 1 ] && [ "$ENV_LEFT" = 0 ]; then rm -f "$ENV_FILE"; fi
  fi
  if [ "$full" = 1 ]; then
    # the folder is full eve's: nothing of eve lite's may take it back
    if [ -f "$KEPT" ]; then rm -f "$KEPT"; fi
    finish "eve lite is uninstalled. Full eve stays as it is."
  elif [ "$ours" = 1 ]; then
    mark_kept
    finish "eve lite is uninstalled. Its memory and settings stay in ~/.config/eve-bridge; the install line takes them back."
  else
    finish "eve lite is uninstalled."
  fi
}

main() {
  local mode=install dry=0 start=1
  trap on_exit EXIT
  while [ $# -gt 0 ]; do
    case "$1" in
      --dry-run) dry=1 ;;
      --uninstall) mode=uninstall ;;
      --no-start) start=0 ;;
      -h|--help) usage; return 0 ;;
      *) die "unknown option: $1 (see --help)" ;;
    esac
    shift
  done
  if [ -z "${HOME:-}" ] || [ ! -d "$HOME" ]; then die "HOME is not set to a folder."; fi
  set_paths
  pass_env
  if [ "$mode" = uninstall ]; then
    uninstall "$dry"
    return 0
  fi
  preflight
  if [ "$dry" = 1 ]; then
    dry_run "$start"
    return 0
  fi
  install_lite "$start"
}

main "$@"
