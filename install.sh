#!/usr/bin/env bash
# eve's one-line installer for the Mac side (the daemon). Run it with:
#
#   curl -fsSL https://raw.githubusercontent.com/gurul/eve_01/main/install.sh | bash
#
# It does README.md's "Get started → 1. The Mac" for you:
#   - clones github.com/gurul/eve_01 to ~/eve (or pulls it when it is already there),
#   - builds bridge/.venv on Python 3.12 and installs eve-bridge into it, editable,
#     so `eve-bridge update` and the "code: … @ <commit>" log line keep working,
#   - installs cloudflared and qrencode with Homebrew,
#   - creates ~/.config/eve-bridge/env (mode 600) and asks for the keys it is missing,
#   - installs the Claude Code hooks and the login service.
#
# It never overwrites a key you already set, and it never prints one.
#
# Options (after `bash -s --` when piped):
#   --dir PATH     clone to PATH instead of ~/eve (or set EVE_DIR)
#   --no-service   skip the hooks and the login service (install the code only)
#   --yes          do not ask for keys; leave the missing ones for you to fill in
#
# Everything is inside main(), so a download cut short runs nothing.

set -euo pipefail

REPO_URL="https://github.com/gurul/eve_01"
ENV_DIR="$HOME/.config/eve-bridge"
ENV_FILE="$ENV_DIR/env"
SERVICE_LABEL="com.github.eve-bridge.daemon"

say()  { printf '\033[1m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# The keys eve needs to start. Each line: NAME|prompt.
REQUIRED_KEYS=(
  "OPENROUTER_API_KEY|OpenRouter API key (openrouter.ai → Keys)"
  "EVE_TELEGRAM_TOKEN|Telegram bot token (from @BotFather, /newbot)"
  "EVE_TELEGRAM_OWNER|Your numeric Telegram user id (from @userinfobot)"
)
# Settings with a fixed value, written only when the file does not set them yet.
DEFAULTS=(
  "EVE_TELEGRAM=app"
  "EVE_MINIAPP=1"
  "EVE_MEMORY=1"
  "EVE_SHARE=1"
)

has_key() {  # has_key NAME → true when the env file sets NAME to a non-empty value
  grep -qE "^$1=.+" "$ENV_FILE" 2>/dev/null
}

find_python() {
  local candidate
  for candidate in python3.12 /opt/homebrew/bin/python3.12 /usr/local/bin/python3.12; do
    if command -v "$candidate" >/dev/null 2>&1; then
      command -v "$candidate"
      return 0
    fi
  done
  return 1
}

main() {
  local eve_dir="${EVE_DIR:-$HOME/eve}" service=1 ask=1
  while [ $# -gt 0 ]; do
    case "$1" in
      --dir) [ $# -ge 2 ] || die "--dir needs a path"; eve_dir="$2"; shift 2 ;;
      --no-service) service=0; shift ;;
      --yes|-y) ask=0; shift ;;
      -h|--help) sed -n '2,22p' "$0" 2>/dev/null || true; return 0 ;;
      *) die "unknown option: $1 (see --help)" ;;
    esac
  done
  # Asking needs a terminal. Piped from curl, stdin is the script, so read /dev/tty.
  if [ "$ask" = 1 ] && ! { : </dev/tty; } 2>/dev/null; then ask=0; fi

  # 1. Prerequisites.
  [ "$(uname -s)" = "Darwin" ] || die "eve's daemon runs on macOS (14 or later). This is $(uname -s)."
  command -v git >/dev/null 2>&1 || die "git is missing. Run: xcode-select --install"
  if ! command -v brew >/dev/null 2>&1; then
    for b in /opt/homebrew/bin/brew /usr/local/bin/brew; do
      [ -x "$b" ] && eval "$("$b" shellenv)" && break
    done
  fi
  command -v brew >/dev/null 2>&1 || die "Homebrew is missing. Install it from https://brew.sh, then run this again."

  local python
  if ! python="$(find_python)"; then
    say "Installing Python 3.12 (Homebrew)"
    brew install python@3.12
    python="$(find_python)" || die "python3.12 is still not on PATH after brew install python@3.12"
  fi
  say "Python: $python ($("$python" -c 'import sys; print(sys.version.split()[0])'))"

  local tools=() t
  for t in cloudflared qrencode; do command -v "$t" >/dev/null 2>&1 || tools+=("$t"); done
  if [ ${#tools[@]} -gt 0 ]; then
    say "Installing ${tools[*]} (Homebrew)"
    brew install "${tools[@]}"
  fi

  # 2. The code: a git checkout, so self-update works.
  if [ -d "$eve_dir/.git" ]; then
    local origin
    origin="$(git -C "$eve_dir" remote get-url origin 2>/dev/null || true)"
    case "$origin" in
      *gurul/eve_01*) ;;
      *) die "$eve_dir is a git checkout of '${origin:-no origin}', not eve. Pick another place with --dir." ;;
    esac
    if [ -n "$(git -C "$eve_dir" status --porcelain --untracked-files=no)" ]; then
      warn "$eve_dir has local changes; leaving the code as it is."
    else
      say "Updating $eve_dir"
      git -C "$eve_dir" pull --ff-only --quiet || warn "git pull did not fast-forward; leaving the code as it is."
    fi
  elif [ -e "$eve_dir" ] && [ -n "$(ls -A "$eve_dir" 2>/dev/null)" ]; then
    die "$eve_dir exists and is not empty. Pick another place with --dir."
  else
    say "Cloning $REPO_URL to $eve_dir"
    git clone --quiet "$REPO_URL" "$eve_dir"
  fi

  # 3. The venv. ".[laya]" on Apple silicon matches what `eve-bridge update` installs.
  local venv="$eve_dir/bridge/.venv" extras="miniapp"
  [ "$(uname -m)" = "arm64" ] && extras="miniapp,laya"
  [ -x "$venv/bin/python" ] || { say "Creating $venv"; "$python" -m venv "$venv"; }
  say "Installing eve-bridge[$extras] (a few minutes the first time)"
  "$venv/bin/python" -m pip install --quiet --upgrade pip
  (cd "$eve_dir/bridge" && "$venv/bin/python" -m pip install --quiet -e ".[$extras]")
  local eve="$venv/bin/eve-bridge"
  [ -x "$eve" ] || die "the install finished but $eve is missing"

  # 4. The env file: keys stay on this Mac, mode 600, never printed.
  mkdir -p "$ENV_DIR"
  [ -f "$ENV_FILE" ] || { : >"$ENV_FILE"; say "Created $ENV_FILE"; }
  chmod 600 "$ENV_FILE"
  local line name prompt value missing=()
  for line in "${DEFAULTS[@]}"; do
    name="${line%%=*}"
    grep -qE "^$name=" "$ENV_FILE" || printf '%s\n' "$line" >>"$ENV_FILE"
  done
  for line in "${REQUIRED_KEYS[@]}"; do
    name="${line%%|*}"; prompt="${line#*|}"
    has_key "$name" && continue
    value=""
    if [ "$ask" = 1 ]; then
      printf '%s (Enter to skip): ' "$prompt" >/dev/tty
      IFS= read -rs value </dev/tty || value=""
      printf '\n' >/dev/tty
    fi
    if [ -n "$value" ]; then
      # Drop an empty "NAME=" line first, so the file holds one line per key.
      sed -i '' "/^$name=\$/d" "$ENV_FILE"
      printf '%s=%s\n' "$name" "$value" >>"$ENV_FILE"
    else
      grep -qE "^$name=" "$ENV_FILE" || printf '%s=\n' "$name" >>"$ENV_FILE"
      missing+=("$name")
    fi
  done

  # 5. Hooks and the login service.
  if [ "$service" = 1 ]; then
    say "Installing the Claude Code hooks"
    "$eve" install
    say "Installing the login service"
    "$eve" install --service --serial-port '/dev/cu.usbmodem*'
    launchctl kickstart -k "gui/$(id -u)/$SERVICE_LABEL" >/dev/null 2>&1 || true
  fi

  # 6. What is left for the person.
  local real_python
  real_python="$("$venv/bin/python" -c 'import os, sys; print(os.path.realpath(sys.executable))')"
  cat <<EOF

eve is installed in $eve_dir.

Left to do:
EOF
  local n=1
  if [ ${#missing[@]} -gt 0 ]; then
    if [ "$service" = 1 ]; then
      printf '  %d. Fill in %s in %s, then restart eve:\n' "$n" "${missing[*]}" "$ENV_FILE"
      # shellcheck disable=SC2016  # printed for the person to copy, so $(id -u) stays literal
      printf '       launchctl kickstart -k gui/$(id -u)/%s\n' "$SERVICE_LABEL"
    else
      printf '  %d. Fill in %s in %s.\n' "$n" "${missing[*]}" "$ENV_FILE"
    fi
    n=$((n + 1))
  fi
  if [ "$service" = 0 ]; then
    printf '  %d. Install the hooks and the service:\n' "$n"
    printf '       %s install\n' "$eve"
    printf "       %s install --service --serial-port '/dev/cu.usbmodem*'\n" "$eve"
    n=$((n + 1))
  fi
  printf '  %d. System Settings → Privacy & Security: add this Python to\n' "$n"
  printf '     Microphone, Accessibility and Screen Recording:\n'
  printf '       %s\n' "$real_python"
  n=$((n + 1))
  printf '  %d. Check: %s status   (hooks listed, "Service (launchd): loaded")\n' "$n" "$eve"
  printf '            tail -40 ~/Library/Logs/eve-bridge.log   (a "miniapp: live at" line)\n'
  n=$((n + 1))
  printf '  %d. The iPhone app and pairing: README.md, "Get started → 2. The iPhone".\n' "$n"
}

main "$@"
