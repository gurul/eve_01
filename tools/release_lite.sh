#!/usr/bin/env bash
# shellcheck disable=SC2088  # a ~ in a message is for the person reading it, not for the shell
# Build an eve lite release and, after a yes, draft it on GitHub (docs/lite.md). The owner runs it on the commit that
# is to ship, with uv, pnpm and gh on PATH:
#
#   sh tools/release_lite.sh --dry-run   check everything and print the plan; writes nothing; ends with DRY_RUN_OK
#   sh tools/release_lite.sh             build lite-v<version> (bridge/pyproject.toml's version), then ask before
#                                        creating the draft release (AGENTS rule 1: nothing leaves this Mac unasked)
#
# The build, into bridge/dist/lite-v<version>/ (git-ignored by bridge/.gitignore), is made from the commit alone: it
# refuses while anything it builds from (TREE) is not committed, then works in a scratch copy of the commit's bridge/,
# ios/EveLink/Web/ and install-lite.sh (`git archive`), so no ignored file (node_modules, a .env.local, a page built
# earlier) reaches it and this tree is not written to, apart from step 5's stamp:
#   1. pnpm install --frozen-lockfile and pnpm build:web there write eve lite's page into the copy's
#      bridge/src/eve_bridge/web_lite/
#   2. uv export writes requirements-lite.txt: the core dependencies and the lite extra from uv.lock, hash-pinned,
#      minus the pyautogui family (no wheels on PyPI; only the computer-use helpers import pyautogui, inside functions)
#   3. uv build --wheel from the copy; the wheel must hold web_lite/index.html
#   4. smoke: install-lite.sh, stamped with a file:// URL, installs the tarball with --no-start into a scratch HOME
#      (the real uv and CPython pins and the real wheels, from the internet). Then a boot check imports eve lite
#      there under the lite profile's process firewall, and a CLI check has that eve-bridge parse, without running
#      them, the commands install-lite.sh and its Eve.app run (EVE_COMMANDS)
#   5. the tarball (the wheel, requirements-lite.txt, SHA256SUMS), install-lite.sh stamped with the release's URL and
#      SHA-256 (the release asset, and this repository's copy, which you commit), and the release's SHA256SUMS
#   6. after a yes: gh release create lite-v<version> --draft --target <HEAD>, with those three files
#
# The steps on their own (tests/test_install_lite_sh.py and tests/test_lite_requirements.py use them):
#   --requirements FILE                      step 2 for this tree's bridge/, into FILE
#   --pack WHEEL REQUIREMENTS DIR            step 5's tarball, into DIR; prints its path
#   --stamp SCRIPT OUT VERSION URL SHA256    step 5's stamp: SCRIPT with the three release pins set, written to OUT
#   --cli-check PYTHON [COMMAND...]          step 4's CLI check of PYTHON's eve-bridge (default: EVE_COMMANDS)
#   --tree-changes                           what the real run refuses to build over: TREE's uncommitted files

# `sh tools/release_lite.sh` starts this in sh; the rest is bash, out of POSIX mode.
if [ -z "${BASH_VERSION:-}" ] || (shopt -oq posix) 2>/dev/null; then exec /bin/bash "$0" "$@"; fi

set -euo pipefail

REPO="gurul/eve_01"
# No wheel on PyPI for the first six, and pymsgbox is only theirs. desktop_worker.py and desk_lane.py import pyautogui
# inside functions, and eve lite offers neither.
NO_WHEEL="pyautogui mouseinfo pygetwindow pyrect pyscreeze pytweening pymsgbox"
# What a release is built from. node_modules and the page build:web writes are not in it (tree_changes leaves them
# out): neither is a source, and the build makes both afresh from the commit.
TREE="bridge/src bridge/pyproject.toml bridge/uv.lock ios/EveLink/Web install-lite.sh tools/release_lite.sh"
# The eve-bridge commands install-lite.sh and its Eve.app run, one per line. Every release's eve-bridge must take each
# of them (step 4); tests/test_install_lite_sh.py matches the list against what a sandboxed install runs.
EVE_COMMANDS="install --service
web open --wait 90
web open"
# A clean shell's PATH, as on a Mac with nothing installed: the smoke install runs with it.
PLAIN_PATH="/usr/bin:/bin:/usr/sbin:/sbin"

ROOT="$(cd "$(dirname "$0")/.." && pwd -P)"
BRIDGE="$ROOT/bridge"
INSTALLER="$ROOT/install-lite.sh"
WORK=""

say() { printf '%s\n' "$*"; }
die() { printf 'release_lite: %s\n' "$*" >&2; exit 1; }
cleanup() { if [ -n "$WORK" ]; then rm -rf "$WORK"; fi; }
trap cleanup EXIT

tilde() {
  case "$1" in
    "$HOME"/*) printf '~/%s' "${1#"$HOME"/}" ;;
    *) printf '%s' "$1" ;;
  esac
}

is_sha256() {
  case "$1" in ''|*[!0-9a-f]*) return 1 ;; esac
  [ ${#1} -eq 64 ]
}

sha256_of() {
  local line
  line="$(shasum -a 256 "$1")" || return 1
  printf '%s\n' "${line%% *}"
}

find_tool() {  # find_tool NAME: its path, from PATH or the usual install places; nothing when it is missing
  local p
  if p="$(command -v "$1" 2>/dev/null)"; then printf '%s\n' "$p"; return 0; fi
  for p in "/opt/homebrew/bin/$1" "/usr/local/bin/$1" "$HOME/.local/bin/$1" "$HOME/.cargo/bin/$1"; do
    if [ -x "$p" ]; then printf '%s\n' "$p"; return 0; fi
  done
  return 1
}

pin() {  # pin NAME FILE: the value of the line NAME="value" in install-lite.sh; fails unless exactly one such line
  local n
  n="$(grep -c "^$1=" "$2" || true)"
  [ "$n" = 1 ] || return 1
  sed -n "s/^$1=\"\\(.*\\)\"\$/\\1/p" "$2"
}

project_version() {
  awk -F'"' '/^version = "/ { print $2; exit }' "$BRIDGE/pyproject.toml"
}

ask() {  # ask QUESTION: true on y or yes typed at the terminal; no terminal means no
  local answer=""
  { : </dev/tty; } 2>/dev/null || return 1
  printf '%s' "$1" >/dev/tty
  IFS= read -r answer </dev/tty || answer=""
  case "$answer" in [Yy]|[Yy][Ee][Ss]) return 0 ;; esac
  return 1
}

# --- The steps -------------------------------------------------------------------------------------------------------

export_requirements() {  # export_requirements [DIR]: DIR's (default this tree's bridge/) hash-pinned lite requirements
  local dir="${1:-$BRIDGE}" uv args=() name   # on stdout; --locked fails when uv.lock is stale
  uv="$(find_tool uv)" || die "uv is missing (https://docs.astral.sh/uv/)."
  for name in $NO_WHEEL; do args+=(--no-emit-package "$name"); done
  (cd "$dir" && "$uv" export --locked --offline --no-cache --no-config --no-dev --extra lite --no-emit-project \
    --format requirements-txt "${args[@]}")
}

pack() {  # pack WHEEL REQUIREMENTS DIR: DIR/eve-lite-<version>.tar.gz holding eve-lite-<version>/{wheel, requirements, SHA256SUMS}
  local wheel="$1" reqs="$2" out="$3" name version stage
  name="$(basename "$wheel")"
  case "$name" in eve_bridge-*-py3-none-any.whl) ;; *) die "not eve-bridge's pure wheel: $name" ;; esac
  version="${name#eve_bridge-}"
  version="${version%%-*}"
  [ -f "$wheel" ] || die "no such wheel: $wheel"
  [ -f "$reqs" ] || die "no such requirements file: $reqs"
  stage="$(mktemp -d "${TMPDIR:-/tmp}/eve-lite-pack.XXXXXX")"
  mkdir "$stage/eve-lite-$version"
  cp "$wheel" "$stage/eve-lite-$version/$name"
  cp "$reqs" "$stage/eve-lite-$version/requirements-lite.txt"
  (cd "$stage/eve-lite-$version" && shasum -a 256 "$name" requirements-lite.txt >SHA256SUMS)
  mkdir -p "$out"
  # COPYFILE_DISABLE: no AppleDouble ._ files from the build Mac's extended attributes
  COPYFILE_DISABLE=1 tar -czf "$out/eve-lite-$version.tar.gz" --uid 0 --gid 0 --uname root --gname wheel \
    -C "$stage" "eve-lite-$version"
  rm -rf "$stage"
  printf '%s\n' "$out/eve-lite-$version.tar.gz"
}

stamp() {  # stamp SCRIPT OUT VERSION URL SHA256: SCRIPT with LITE_VERSION, LITE_URL and LITE_SHA256 set, written to OUT
  local in="$1" out="$2" version="$3" url="$4" sha="$5" name tmp
  case "$version" in ''|*[!0-9A-Za-z.+!-]*) die "not a release version: $version" ;; esac
  case "$url" in https://*|file:///*) ;; *) die "the release URL must be https:// (or file:// for a smoke test): $url" ;; esac
  case "$url" in *[!A-Za-z0-9._~:/%+@=-]*) die "the release URL has a character a pin cannot hold: $url" ;; esac
  is_sha256 "$sha" || die "not a SHA-256: $sha"
  for name in LITE_VERSION LITE_URL LITE_SHA256; do
    [ "$(grep -c "^$name=" "$in" || true)" = 1 ] || die "$in must have exactly one $name= line"
  done
  tmp="$out.stamp.$$"
  sed -e "s|^LITE_VERSION=.*\$|LITE_VERSION=\"$version\"|" \
      -e "s|^LITE_URL=.*\$|LITE_URL=\"$url\"|" \
      -e "s|^LITE_SHA256=.*\$|LITE_SHA256=\"$sha\"|" "$in" >"$tmp"
  if ! /bin/bash -n "$tmp"; then rm -f "$tmp"; die "the stamped installer does not parse"; fi
  if [ "$(pin LITE_VERSION "$tmp")" != "$version" ] || [ "$(pin LITE_URL "$tmp")" != "$url" ] \
      || [ "$(pin LITE_SHA256 "$tmp")" != "$sha" ]; then
    rm -f "$tmp"
    die "the stamp did not take"
  fi
  chmod 755 "$tmp"
  mv -f "$tmp" "$out"
}

# The boot check, run by the smoke install's own Python: eve lite's composition imports under the process firewall,
# pulls in no BLE, audio or Quartz, starts no program and opens no private folder, and the web page is in the package.
# Positive control: once the import is done, the firewall refuses a harmless program, so it really was on.
BOOT_CHECK='
import importlib.resources
import os
import platform
import subprocess
import sys

from eve_bridge import profile

profile.apply(os.environ)
profile.install_firewall()
import eve_bridge.lite  # noqa: E402,F401

banned = sorted(m for m in ("bleak", "eve_bridge.ble", "eve_bridge.daemon", "sounddevice", "Quartz") if m in sys.modules)
blocked = sorted(profile.firewall_counts())
page = importlib.resources.files("eve_bridge").joinpath("web_lite", "index.html").is_file()
python = platform.python_version()
try:
    subprocess.run(["/usr/bin/true"], check=False)
    enforcing = False
except PermissionError:
    enforcing = True
if banned or blocked or not page or not enforcing or python != os.environ["EXPECT_PYTHON"]:
    print("BOOT_FAIL banned=%s blocked=%s page=%s enforcing=%s python=%s" % (banned, blocked, page, enforcing, python))
    sys.exit(1)
print("BOOT_OK python", python)
'

# The CLI check, also run by the smoke install's own Python: its eve-bridge parses each command given (one per
# argument) and runs none of them. The outermost parse ends main() with what it parsed, so no command starts; were one
# to start anyway, the firewall refuses every program it would run and the alarm ends one that waits. A command
# argparse refuses, or whose words it leaves over (a --wait that takes no value, given "90"), fails the check.
CLI_CHECK='
import argparse
import signal
import sys


class Parsed(BaseException):
    """The outermost parse, ending main() before the command runs."""


depth = 0


def parse_only(real):
    def parse(self, args=None, namespace=None):
        global depth
        depth += 1
        try:
            got = real(self, args, namespace)
        finally:
            depth -= 1
        if depth == 0:
            raise Parsed(got)
        return got

    return parse


argparse.ArgumentParser.parse_known_args = parse_only(argparse.ArgumentParser.parse_known_args)
argparse.ArgumentParser.parse_known_intermixed_args = parse_only(argparse.ArgumentParser.parse_known_intermixed_args)
signal.alarm(60)

from eve_bridge import cli, profile  # noqa: E402

profile.install_firewall()
failed = []
for command in sys.argv[1:]:
    try:
        cli.main(command.split())
    except Parsed as parsed:
        left = parsed.args[0][1]
        if left:
            failed.append("%s: argparse left %s over" % (command, " ".join(left)))
        else:
            print("CLI_OK eve-bridge", command)
    except SystemExit as stop:
        failed.append("%s: refused (exit %s; argparse says why on stderr)" % (command, stop.code))
    except Exception as error:
        failed.append("%s: raised %r before it was parsed" % (command, error))
    else:
        failed.append("%s: ran instead of only parsing" % command)
for line in failed:
    print("CLI_FAIL eve-bridge", line)
sys.exit(1 if failed else 0)
'

cli_check() {  # cli_check PYTHON HOME [COMMAND...]: PYTHON's eve-bridge takes each COMMAND (default: EVE_COMMANDS)
  local python="$1" home="$2" line commands=()
  shift 2
  if [ $# -gt 0 ]; then
    commands=("$@")
  else
    while IFS= read -r line; do commands+=("$line"); done <<EOF
$EVE_COMMANDS
EOF
  fi
  # PATH holds nothing to start, on top of the firewall; -B: no bytecode written next to the package checked
  env -i HOME="$home" TMPDIR="$home" PATH=/var/empty EVE_PROFILE=lite \
    "$python" -I -B -c "$CLI_CHECK" "${commands[@]}" </dev/null
}

smoke() {  # smoke TARBALL VERSION INSTALLER: INSTALLER installs TARBALL into a scratch HOME (--no-start); boot and CLI checks
  local tarball="$1" version="$2" installer="$3" home="$WORK/smoke-home" script="$WORK/install-lite-smoke.sh" python out
  stamp "$installer" "$script" "$version" "file://$tarball" "$(sha256_of "$tarball")"
  python="$(pin PYTHON_VERSION "$installer")" || die "install-lite.sh has no PYTHON_VERSION pin"
  mkdir -p "$home" "$WORK/smoke-tmp"
  if ! out="$(env -i HOME="$home" TMPDIR="$WORK/smoke-tmp" PATH="$PLAIN_PATH" /bin/bash "$script" --no-start </dev/null)"; then
    tail -n 30 "$home/.eve-lite/install.log" >&2 || true
    die "smoke: install-lite.sh did not install the release"
  fi
  [ "$out" = "eve lite $version is installed in ~/.eve-lite; nothing was started (--no-start)." ] \
    || die "smoke: install-lite.sh ended with an unexpected line: $out"
  env -i HOME="$home" TMPDIR="$WORK/smoke-tmp" PATH="$PLAIN_PATH" EVE_PROFILE=lite EXPECT_PYTHON="$python" \
    "$home/.eve-lite/venv/bin/python" -I -c "$BOOT_CHECK" || die "smoke: the boot check failed"
  cli_check "$home/.eve-lite/venv/bin/python" "$home" \
    || die "smoke: this eve-bridge does not take every command install-lite.sh and Eve.app run"
}

tree_changes() {  # TREE's uncommitted files, as `git status --porcelain` lines; read-only (no index refresh)
  local path specs=()
  for path in $TREE; do specs+=("$path"); done
  git -C "$ROOT" --no-optional-locks status --porcelain --untracked-files=all -- "${specs[@]}" \
    ':(exclude)ios/EveLink/Web/node_modules' ':(exclude)bridge/src/eve_bridge/web_lite'
}

# --- The two modes ---------------------------------------------------------------------------------------------------

dry_run() {
  local version tool reqs n hashed excluded uv_version uv_url uv_sha python stamped commit changes package
  version="$(project_version)"
  [ -n "$version" ] || die "bridge/pyproject.toml has no version"
  say "eve lite release, dry run: nothing is built, stamped, uploaded or written."
  say "  version     $version (bridge/pyproject.toml), tag lite-v$version on $REPO"
  if commit="$(git -C "$ROOT" rev-parse --short=12 HEAD 2>/dev/null)"; then
    changes="$( (tree_changes 2>/dev/null || true) | wc -l | tr -d ' ')"
    if [ "$changes" = 0 ]; then
      say "  commit      $commit; nothing uncommitted in what a release is built from:"
    else
      say "  commit      $commit; $changes uncommitted files in what a release is built from. The real run stops"
      say "              until they are committed (\`sh tools/release_lite.sh --tree-changes\` lists them):"
    fi
    say "              ${TREE// /, }"
  else
    say "  commit      git cannot read $ROOT; the real run needs it for the release's target"
  fi

  # The installer's pins, which the smoke install and every user's install use as they are.
  uv_version="$(pin UV_VERSION "$INSTALLER")" || die "install-lite.sh must have exactly one UV_VERSION line"
  uv_url="$(pin UV_URL "$INSTALLER")" || die "install-lite.sh must have exactly one UV_URL line"
  uv_sha="$(pin UV_SHA256 "$INSTALLER")" || die "install-lite.sh must have exactly one UV_SHA256 line"
  python="$(pin PYTHON_VERSION "$INSTALLER")" || die "install-lite.sh must have exactly one PYTHON_VERSION line"
  stamped="$(pin LITE_VERSION "$INSTALLER")" || die "install-lite.sh must have exactly one LITE_VERSION line"
  pin LITE_URL "$INSTALLER" >/dev/null || die "install-lite.sh must have exactly one LITE_URL line"
  pin LITE_SHA256 "$INSTALLER" >/dev/null || die "install-lite.sh must have exactly one LITE_SHA256 line"
  is_sha256 "$uv_sha" || die "install-lite.sh's UV_SHA256 is not a SHA-256"
  case "$uv_url" in "https://github.com/astral-sh/uv/releases/download/$uv_version/"*) ;;
    *) die "install-lite.sh's UV_URL is not uv $uv_version's GitHub release" ;; esac
  /bin/bash -n "$INSTALLER" || die "install-lite.sh does not parse"
  say "  installer   uv $uv_version (SHA-256 pinned), CPython $python, stamped now: ${stamped:-not yet}"

  for tool in uv pnpm gh shasum tar unzip git; do
    if find_tool "$tool" >/dev/null; then
      say "  tool        $tool: found"
    else
      say "  tool        $tool: MISSING; the real run needs it"
    fi
  done

  find_tool uv >/dev/null || die "uv is missing, so the lock cannot be checked (https://docs.astral.sh/uv/)"
  reqs="$(export_requirements 2>/dev/null)" \
    || die "uv export failed: uv.lock has no lite extra, or it is out of date with bridge/pyproject.toml (uv lock)"
  n="$(printf '%s\n' "$reqs" | grep -cE '^[A-Za-z0-9][A-Za-z0-9._-]*==' || true)"
  hashed="$(printf '%s\n' "$reqs" | awk '/^[A-Za-z0-9]/ { open = 1 } /--hash=sha256:/ { if (open) { h++; open = 0 } } END { print h + 0 }')"
  [ "$n" -gt 0 ] && [ "$n" = "$hashed" ] || die "uv export gave $n requirements but $hashed with a hash"
  excluded="$(printf '%s\n' "$reqs" | grep -E "^($(printf '%s' "$NO_WHEEL" | tr ' ' '|'))==" || true)"
  [ -z "$excluded" ] || die "uv export still lists the pyautogui family: $excluded"
  say "  lock        uv.lock matches pyproject.toml; the lite closure is $n hash-pinned requirements (all platforms),"
  say "              without $NO_WHEEL"
  # the real run builds the page from the commit, so the commit's package.json is the one that counts
  package="$(git -C "$ROOT" show HEAD:ios/EveLink/Web/package.json 2>/dev/null || true)"
  case "$package" in
    *'"build:web"'*) say "  web         the commit's ios/EveLink/Web has a build:web script" ;;
    *) say "  web         the commit's ios/EveLink/Web has no build:web script; the real run stops at step 1 until it does" ;;
  esac
  say "  commands    each release's eve-bridge must take: ${EVE_COMMANDS//$'\n'/; }"

  cat <<EOF
The real run would, in a scratch copy of the commit's bridge/, ios/EveLink/Web/ and install-lite.sh (git archive):
  1. pnpm install --frozen-lockfile && pnpm build:web in its ios/EveLink/Web (writes its bridge/src/eve_bridge/web_lite/)
  2. uv export --locked --offline --no-dev --extra lite --no-emit-project --format requirements-txt
       $(for t in $NO_WHEEL; do printf -- '--no-emit-package %s ' "$t"; done)> requirements-lite.txt
  3. uv build --wheel from the copy (eve_bridge-$version-py3-none-any.whl, which must hold
     eve_bridge/web_lite/index.html)
  4. install the tarball with a file://-stamped install-lite.sh --no-start in a scratch HOME (uv $uv_version,
     CPython $python and the wheels from the internet), then the boot check under the lite profile's firewall,
     and the CLI check of the commands above
  5. write bridge/dist/lite-v$version/: eve-lite-$version.tar.gz, install-lite.sh (stamped with
     https://github.com/$REPO/releases/download/lite-v$version/eve-lite-$version.tar.gz and its SHA-256) and
     SHA256SUMS; stamp this repository's install-lite.sh the same way, for you to commit
  6. ask, then: gh release create lite-v$version --repo $REPO --draft --target <HEAD> with those three files
DRY_RUN_OK
EOF
}

build() {
  local version out wheel tarball sha commit changes notes tool src pnpm uv
  version="$(project_version)"
  [ -n "$version" ] || die "bridge/pyproject.toml has no version"
  for tool in uv pnpm gh shasum tar unzip git; do find_tool "$tool" >/dev/null || die "$tool is missing"; done
  pnpm="$(find_tool pnpm)"
  uv="$(find_tool uv)"
  changes="$(tree_changes)" || die "git cannot read $ROOT"
  [ -z "$changes" ] || die "commit these first; a release is built from a commit:
$changes"
  out="$BRIDGE/dist/lite-v$version"
  [ ! -e "$out" ] || die "$(tilde "$out") is already there; move it away to build lite-v$version again"
  commit="$(git -C "$ROOT" rev-parse HEAD)"
  WORK="$(mktemp -d "${TMPDIR:-/tmp}/eve-lite-release.XXXXXX")"
  src="$WORK/src"
  mkdir "$src"
  git -C "$ROOT" archive --format=tar "$commit" bridge ios/EveLink/Web install-lite.sh | tar -x -C "$src" \
    || die "git archive could not copy $commit"

  say "1/6 web page"
  # vite.web.config.ts writes ../../../bridge/src/eve_bridge/web_lite: in the copy, the copy's bridge/
  (cd "$src/ios/EveLink/Web" && "$pnpm" install --frozen-lockfile && "$pnpm" build:web)
  [ -f "$src/bridge/src/eve_bridge/web_lite/index.html" ] || die "pnpm build:web wrote no web_lite/index.html"

  say "2/6 requirements"
  export_requirements "$src/bridge" >"$WORK/requirements-lite.txt"

  say "3/6 wheel"
  (cd "$src/bridge" && "$uv" build --wheel --no-config --out-dir "$WORK/dist")
  wheel="$WORK/dist/eve_bridge-$version-py3-none-any.whl"
  [ -f "$wheel" ] || die "uv build did not write $(basename "$wheel")"
  unzip -Z1 "$wheel" >"$WORK/wheel-files.txt"
  grep -qx 'eve_bridge/web_lite/index.html' "$WORK/wheel-files.txt" || die "the wheel has no eve_bridge/web_lite/index.html"

  say "4/6 smoke install, boot check and CLI check (downloads uv, CPython and the wheels)"
  tarball="$(pack "$wheel" "$WORK/requirements-lite.txt" "$WORK/pack")"
  smoke "$tarball" "$version" "$src/install-lite.sh"

  say "5/6 release files"
  mkdir -p "$out"
  mv "$tarball" "$out/"
  tarball="$out/eve-lite-$version.tar.gz"
  sha="$(sha256_of "$tarball")"
  stamp "$src/install-lite.sh" "$out/install-lite.sh" "$version" \
    "https://github.com/$REPO/releases/download/lite-v$version/eve-lite-$version.tar.gz" "$sha"
  stamp "$src/install-lite.sh" "$INSTALLER" "$version" \
    "https://github.com/$REPO/releases/download/lite-v$version/eve-lite-$version.tar.gz" "$sha"
  (cd "$out" && shasum -a 256 "eve-lite-$version.tar.gz" install-lite.sh >SHA256SUMS)
  say "    $(tilde "$out"): eve-lite-$version.tar.gz, install-lite.sh, SHA256SUMS"
  say "    install-lite.sh in this repository is stamped for lite-v$version; commit it."

  say "6/6 draft release"
  if [ -z "$(git -C "$ROOT" branch -r --contains "$commit" 2>/dev/null)" ]; then
    say "    $commit is not on a remote branch yet. Push it (with permission), then run:"
    say "    gh release create lite-v$version --repo $REPO --draft --target $commit $out/*.tar.gz $out/install-lite.sh $out/SHA256SUMS"
    return 0
  fi
  if ! ask "Create the draft release lite-v$version on $REPO at ${commit:0:12} with these three files? [y/N] "; then
    say "    Not published. The files stay in $(tilde "$out")."
    return 0
  fi
  notes="$WORK/notes.md"
  {
    printf 'Install eve lite on an Apple-silicon Mac with macOS 14 or later:\n\n'
    printf '    curl -fsSL https://github.com/%s/releases/latest/download/install-lite.sh | bash\n\n' "$REPO"
    printf 'SHA-256:\n\n'
    sed 's/^/    /' "$out/SHA256SUMS"
  } >"$notes"
  "$(find_tool gh)" release create "lite-v$version" --repo "$REPO" --draft --target "$commit" \
    --title "eve lite $version" --notes-file "$notes" \
    "$tarball" "$out/install-lite.sh" "$out/SHA256SUMS"
  say "    Drafted lite-v$version. Publish it on GitHub when it looks right; the install line serves the latest release."
}

main() {
  local python
  case "${1:-}" in
    --dry-run) [ $# -eq 1 ] || die "--dry-run takes nothing else"; dry_run ;;
    --requirements)
      [ $# -eq 2 ] || die "usage: --requirements FILE"
      export_requirements >"$2.part" || { rm -f "$2.part"; die "uv export failed"; }
      mv -f "$2.part" "$2"
      ;;
    --pack) [ $# -eq 4 ] || die "usage: --pack WHEEL REQUIREMENTS DIR"; pack "$2" "$3" "$4" ;;
    --stamp) [ $# -eq 6 ] || die "usage: --stamp SCRIPT OUT VERSION URL SHA256"; stamp "$2" "$3" "$4" "$5" "$6" ;;
    --cli-check)
      [ $# -ge 2 ] || die "usage: --cli-check PYTHON [COMMAND...]"
      python="$2"
      shift 2
      [ -x "$python" ] || die "no such Python: $python"
      # a HOME of its own, with nothing in it, removed on exit
      WORK="$(mktemp -d "${TMPDIR:-/tmp}/eve-lite-cli.XXXXXX")"
      cli_check "$python" "$WORK" "$@"
      ;;
    --tree-changes) [ $# -eq 1 ] || die "--tree-changes takes nothing else"; tree_changes ;;
    '') build ;;
    -h|--help) awk 'NR > 2 && /^#/ { print; next } NR > 2 { exit }' "$0" ;;
    *) die "unknown option: $1 (see --help)" ;;
  esac
}

main "$@"
