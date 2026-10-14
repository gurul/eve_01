"""install-lite.sh in a sandbox (WP-12). Each run gets a throwaway HOME and runs the real script with real curl
(file:// URLs), tar, shasum and osacompile, beside a stand-in uv and a stand-in eve-bridge. Every developer tool and
package manager the script must never start is a recording stand-in that exits 99. launchctl is a recording stand-in
too, so no run reaches this Mac's launchd.

What it proves: none of those tools starts (positive control: a copy whose uv PATH is widened lets uv's
install_name_tool lookup through, and the record shows it); a SHA-256 mismatch stops before anything is unpacked; full
eve blocks the install; the env file is 0600 with exactly EVE_PROFILE=lite; stdout is one final line; a second run
changes nothing; --dry-run writes nothing; --uninstall removes eve lite and keeps or deletes the memory as asked, and
the install line takes kept memory back (controls: without the uninstall's marker, or with the env file changed since,
it is refused as full eve's). The release script's --pack and --stamp build what the installer reads here, its dry run
writes nothing, its clean-tree check covers the web page's sources, and its CLI check, which every release runs, fails
an eve-bridge that does not take each command the installer and Eve.app run."""

from __future__ import annotations

import fcntl
import hashlib
import io
import os
import re
import select
import shlex
import shutil
import subprocess
import sys
import tarfile
import termios
import time
from pathlib import Path

import pytest

from eve_bridge import toolchain
from eve_bridge.envfile import parse_env_file

pytestmark = pytest.mark.skipif(sys.platform != "darwin", reason="install-lite.sh installs on macOS")

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "install-lite.sh"
RELEASE = ROOT / "tools" / "release_lite.sh"
VERSION = "0.1.2"
FINAL = "eve lite is running. Setup is open in your browser: http://localhost:48780"
SERVICE_PATH = "/usr/bin:/bin:/usr/sbin:/sbin"
LITE_LABEL = "com.github.eve-bridge.lite"
CANARY = "CANARY-NOT-A-KEY-7f3c"
# Developer tools (each one of /usr/bin's stand-ins that open the "install developer tools" dialog, or xcrun and
# xcode-select themselves) and package managers. The script must start none of them.
FORBIDDEN = ("git", "python3", "pip3", "xcrun", "xcode-select", "clang", "cc", "make", "install_name_tool", "brew",
             "cloudflared", "qrencode")
STATUS = ["getting Python (1/5)", f"getting eve lite {VERSION} (2/5)",
          "installing packages, a few minutes the first time (3/5)", "starting eve (4/5)", "opening setup (5/5)"]

# Plays uv 0.11.27 as far as install-lite.sh needs it, and writes down each call with its whole environment. Like the
# real uv it looks up install_name_tool by name while it installs Python, and probes python3 on PATH unless told to
# use only its own Pythons (the real uv does both; see docs/plans/eve-lite.md M8).
FAKE_UV = r"""#!/bin/bash
TRACE=@TRACE@
{
  printf 'uv'; for a in "$@"; do printf ' [%s]' "$a"; done; printf '\n'
  /usr/bin/env | /usr/bin/sort | /usr/bin/sed 's/^/uv-env /'
} >>"$TRACE"
if [ "$1" = python ] && command -v install_name_tool >/dev/null 2>&1; then install_name_tool -id x x; fi
if [ "${UV_PYTHON_PREFERENCE:-}" != only-managed ] && command -v python3 >/dev/null 2>&1; then python3 --version; fi
prev=""; last=""; py=""; reqs=""; whl=""
for a in "$@"; do
  case "$prev" in --python) py="$a" ;; -r) reqs="$a" ;; esac
  case "$a" in *.whl) whl="$a" ;; esac
  prev="$a"; last="$a"
done
case "$1" in
  --version) echo "uv 0.11.27 (stand-in)" ;;
  python)
    [ "$2" = install ] || exit 2
    d="$UV_PYTHON_INSTALL_DIR/cpython-$3-macos-aarch64-none/bin"
    /bin/mkdir -p "$d"
    printf '#!/bin/sh\necho Python %s\n' "$3" >"$d/python3.13"
    /bin/chmod 755 "$d/python3.13"
    ;;
  venv)
    /bin/mkdir -p "$last/bin"
    /bin/ln -sf "$UV_PYTHON_INSTALL_DIR/cpython-$py-macos-aarch64-none/bin/python3.13" "$last/bin/python"
    printf 'home = %s\nversion_info = %s\n' "$UV_PYTHON_INSTALL_DIR" "$py" >"$last/pyvenv.cfg"
    ;;
  pip)
    [ -x "$py" ] || exit 3
    if [ -n "$reqs" ]; then /usr/bin/grep -q -- '--hash=sha256:' "$reqs" || exit 4; fi
    if [ -n "$whl" ]; then /bin/cp "$whl" "${py%/*}/eve-bridge"; /bin/chmod 755 "${py%/*}/eve-bridge"; fi
    ;;
  *) exit 2 ;;
esac
"""

# The release's "wheel": the stand-in uv installs it as the venv's eve-bridge. `web open` answers as WEB_OPEN says:
# a port to report, "silent" (reports no address), "fail", or "usage" (argparse refusing `--wait 90`, as an
# eve-bridge whose --wait takes no value would).
FAKE_EVE = r"""#!/bin/bash
TRACE=@TRACE@
{
  printf 'eve-bridge'; for a in "$@"; do printf ' [%s]' "$a"; done
  printf ' | EVE_PROFILE=%s PATH=%s PYTHONNOUSERSITE=%s\n' "${EVE_PROFILE-}" "${PATH-}" "${PYTHONNOUSERSITE-}"
} >>"$TRACE"
case "$1 ${2:-}" in
  "install --service")
    /bin/mkdir -p "$HOME/Library/LaunchAgents"
    printf '<plist/>\n' >"$HOME/Library/LaunchAgents/com.github.eve-bridge.lite.plist"
    echo "login service installed"
    ;;
  "web open")
    mode="$(/bin/cat @WEB_OPEN@)"
    case "$mode" in
      silent) ;;
      fail) echo "eve is not answering" >&2; exit 1 ;;
      usage)
        printf 'usage: eve-bridge web open [-h] [--wait]\neve-bridge web open: error: unrecognized arguments: 90\n' >&2
        exit 2
        ;;
      *) echo "eve lite: opened http://localhost:$mode/ in the browser" ;;
    esac
    ;;
  *) exit 2 ;;
esac
"""

REQUIREMENTS = """segno==1.6.6 \\
    --hash=sha256:28c7d081ed0cf935e0411293a465efd4d500704072cdb039778a2ab8736190c7
"""


def _sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _set_pin(text: str, name: str, value: str) -> str:
    pattern = re.compile(rf'^{name}="[^"]*"$', re.M)
    assert len(pattern.findall(text)) == 1, name
    return pattern.sub(f'{name}="{value}"', text)


def snapshot(root: Path, skip: tuple[str, ...] = ()) -> dict[str, tuple]:
    """Every path under ROOT with its kind, mode and content hash (or link target)."""
    out: dict[str, tuple] = {}
    if not root.exists():
        return out
    for p in sorted(root.rglob("*")):
        rel = p.relative_to(root).as_posix()
        if rel in skip:
            continue
        mode = p.lstat().st_mode & 0o777
        if p.is_symlink():
            out[rel] = ("link", os.readlink(p))
        elif p.is_dir():
            out[rel] = ("dir", oct(mode))
        else:
            out[rel] = ("file", oct(mode), _sha(p.read_bytes()))
    return out


class Sandbox:
    def __init__(self, tmp: Path, home_name: str = "home") -> None:
        self.tmp = tmp
        self.home = tmp / home_name
        self.shims = tmp / "shims"
        self.scratch = tmp / "tmp"
        self.calls = tmp / "forbidden-calls.log"
        self.trace = tmp / "trace.log"
        self.web_open = tmp / "web-open-mode"
        for d in (self.home, self.shims, self.scratch, tmp / "build", tmp / "rel"):
            d.mkdir(parents=True)
        self.web_open.write_text("48780")
        for name in FORBIDDEN:
            self.shim(name, f'printf "%s %s\\n" {name} "$*" >>{shlex.quote(str(self.calls))}\nexit 99')
        for name in ("curl", "tar", "osacompile"):
            self.shim(name, f'printf "%s %s\\n" {name} "$*" >>{shlex.quote(str(self.trace))}\nexec /usr/bin/{name} "$@"')
        self.shim("launchctl", f'printf "launchctl %s\\n" "$*" >>{shlex.quote(str(self.trace))}\nexit 0')

        trace = shlex.quote(str(self.trace))
        buf = io.BytesIO()
        with tarfile.open(fileobj=buf, mode="w:gz") as tar:
            for name, body in (("uv", FAKE_UV.replace("@TRACE@", trace)), ("uvx", "#!/bin/sh\nexit 0\n")):
                data = body.encode()
                info = tarfile.TarInfo(f"uv-aarch64-apple-darwin/{name}")
                info.size, info.mode = len(data), 0o755
                tar.addfile(info, io.BytesIO(data))
        self.uv_tgz = tmp / "rel" / "uv-aarch64-apple-darwin.tar.gz"
        self.uv_tgz.write_bytes(buf.getvalue())

        wheel = tmp / "build" / f"eve_bridge-{VERSION}-py3-none-any.whl"
        wheel.write_text(FAKE_EVE.replace("@TRACE@", trace).replace("@WEB_OPEN@", shlex.quote(str(self.web_open))))
        reqs = tmp / "build" / "requirements-lite.txt"
        reqs.write_text(REQUIREMENTS)
        packed = self.release("--pack", str(wheel), str(reqs), str(tmp / "rel"))
        self.lite_tgz = Path(packed.stdout.strip())
        assert self.lite_tgz.name == f"eve-lite-{VERSION}.tar.gz"
        self.script = self.stamped(tmp / "build" / "install-lite.sh")

    def shim(self, name: str, body: str) -> None:
        path = self.shims / name
        path.write_text(f"#!/bin/sh\n{body}\n")
        path.chmod(0o755)

    def release(self, *args: str, path: str = SERVICE_PATH) -> subprocess.CompletedProcess:
        done = subprocess.run(["sh", str(RELEASE), *args], capture_output=True, text=True, timeout=120,
                              env={"HOME": str(self.home), "PATH": path, "TMPDIR": str(self.scratch)})
        assert done.returncode == 0, done.stderr
        return done

    def stamped(self, out: Path) -> Path:
        """install-lite.sh, stamped by the release script for the sandbox's tarball, with uv's pins on the stand-in."""
        self.release("--stamp", str(SCRIPT), str(out), VERSION, f"file://{self.lite_tgz}", _sha(self.lite_tgz.read_bytes()))
        text = _set_pin(out.read_text(), "UV_URL", f"file://{self.uv_tgz}")
        out.write_text(_set_pin(text, "UV_SHA256", _sha(self.uv_tgz.read_bytes())))
        return out

    def env(self) -> dict[str, str]:
        return {"HOME": str(self.home), "PATH": f"{self.shims}:{SERVICE_PATH}", "TMPDIR": str(self.scratch),
                "LANG": "en_US.UTF-8", "EVE_SANDBOX_MARK": "only-the-installer-sees-this"}

    def run(self, *args: str, script: Path | None = None) -> subprocess.CompletedProcess:
        return subprocess.run(["/bin/bash", str(script or self.script), *args], env=self.env(), capture_output=True,
                              text=True, timeout=120, stdin=subprocess.DEVNULL, start_new_session=True)

    def run_tty(self, *args: str, answer: str) -> tuple[subprocess.CompletedProcess, str]:
        """Run with a pseudo-terminal as the controlling terminal and ANSWER typed into it; returns what it showed.

        The terminal is read until the script exits: a session leader's exit waits for its terminal's output (the
        echoed answer) to drain, so an unread terminal would hold it forever."""
        master, slave = os.openpty()
        proc = subprocess.Popen(["/bin/bash", str(self.script), *args], env=self.env(), stdin=slave,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True,
                                preexec_fn=lambda: fcntl.ioctl(0, termios.TIOCSCTTY, 0))
        os.close(slave)
        os.write(master, answer.encode())
        shown = b""
        deadline = time.monotonic() + 120
        while True:
            if select.select([master], [], [], 0.05)[0]:
                try:
                    shown += os.read(master, 4096)
                except OSError:  # the terminal's last user is gone
                    pass
            if proc.poll() is not None:
                break
            if time.monotonic() > deadline:
                proc.kill()
                proc.communicate(timeout=10)
                os.close(master)
                pytest.fail(f"install-lite.sh {' '.join(args)} did not finish; the terminal showed {shown!r}")
        out, err = proc.communicate(timeout=10)
        os.close(master)
        return subprocess.CompletedProcess(proc.args, proc.returncode, out, err), shown.decode(errors="replace")

    def forbidden_calls(self) -> list[str]:
        return self.calls.read_text().splitlines() if self.calls.exists() else []

    def traced(self, tool: str) -> list[str]:
        lines = self.trace.read_text().splitlines() if self.trace.exists() else []
        return [ln for ln in lines if ln.startswith(tool + " ")]

    def uv_calls(self) -> list[tuple[list[str], dict[str, str]]]:
        calls: list[tuple[list[str], dict[str, str]]] = []
        for line in self.trace.read_text().splitlines() if self.trace.exists() else []:
            if line.startswith("uv ["):
                calls.append((re.findall(r"\[([^\]]*)\]", line), {}))
            elif line.startswith("uv-env "):
                name, _, value = line[len("uv-env "):].partition("=")
                calls[-1][1][name] = value
        return calls

    def eve_calls(self) -> list[tuple[list[str], str]]:
        out = []
        for line in self.traced("eve-bridge"):
            argv, _, env = line[len("eve-bridge "):].partition(" | ")
            out.append((re.findall(r"\[([^\]]*)\]", argv), env))
        return out

    @property
    def lite_home(self) -> Path:
        return self.home / ".eve-lite"

    @property
    def env_file(self) -> Path:
        return self.home / ".config" / "eve-bridge" / "env"

    @property
    def app(self) -> Path:
        return self.home / "Applications" / "Eve.app"

    @property
    def kept(self) -> Path:
        return self.home / ".config" / "eve-bridge" / ".eve-lite-kept"


@pytest.fixture
def box(tmp_path: Path) -> Sandbox:
    return Sandbox(tmp_path)


def _assert_installed(box: Sandbox, done: subprocess.CompletedProcess) -> None:
    assert done.returncode == 0, done.stderr
    assert done.stdout == FINAL + "\n"
    assert box.forbidden_calls() == []
    assert box.env_file.read_text() == "EVE_PROFILE=lite\n"
    assert box.env_file.stat().st_mode & 0o777 == 0o600
    assert box.env_file.parent.stat().st_mode & 0o777 == 0o700
    assert box.lite_home.stat().st_mode & 0o777 == 0o700
    assert (box.lite_home / "venv" / "bin" / "eve-bridge").is_file()


# --- The script itself ---------------------------------------------------------------------------------------------


def test_script_parses_and_pins_uv_and_python() -> None:
    assert subprocess.run(["/bin/bash", "-n", str(SCRIPT)]).returncode == 0
    text = SCRIPT.read_text()
    pins = dict(re.findall(r'^([A-Z0-9_]+)="([^"]*)"$', text, re.M))
    assert pins["PYTHON_VERSION"] == "3.13.14"
    assert pins["UV_VERSION"] == "0.11.27"
    assert pins["UV_URL"] == "https://github.com/astral-sh/uv/releases/download/0.11.27/uv-aarch64-apple-darwin.tar.gz"
    assert re.fullmatch(r"[0-9a-f]{64}", pins["UV_SHA256"])
    stamped = [pins["LITE_VERSION"], pins["LITE_URL"], pins["LITE_SHA256"]]
    # between releases the three are empty; a stamped copy has all three
    assert stamped == ["", "", ""] or (stamped[1].startswith("https://github.com/gurul/eve_01/releases/download/")
                                       and re.fullmatch(r"[0-9a-f]{64}", stamped[2]))


def _code_words(text: str, words: tuple[str, ...]) -> list[str]:
    """Whole-word uses of WORDS outside comments."""
    hits = []
    for line in text.splitlines():
        if line.lstrip().startswith("#"):
            continue
        code = re.sub(r"\s#\s.*$", "", line)
        hits += [w for w in words if re.search(rf"(?<![\w-]){re.escape(w)}(?![\w-])", code)]
    return hits


def test_script_names_no_developer_tool_outside_comments() -> None:
    text = SCRIPT.read_text()
    assert _code_words(text, FORBIDDEN) == []
    assert "/bin/launchctl" not in text and "/usr/bin/git" not in text
    # positive control: the scan finds a tool that a line of code starts
    assert _code_words(text + '\n  git --version >/dev/null 2>&1 || true\n', FORBIDDEN) == ["git"]


# --- Install -------------------------------------------------------------------------------------------------------


def test_install_starts_no_developer_tool_and_ends_on_one_line(box: Sandbox) -> None:
    done = box.run()
    _assert_installed(box, done)
    assert done.stderr.splitlines() == [f"eve lite: {s}" for s in STATUS]

    # uv: the pinned Python, the hash-pinned wheels only, then eve-bridge alone
    venv = str(box.lite_home / "venv")
    rel = str(box.lite_home / "release")
    argvs = [argv for argv, _ in box.uv_calls()]
    assert argvs == [
        ["--version"],
        ["python", "install", "3.13.14", "--no-bin"],
        ["venv", "--python", "3.13.14", venv],
        ["pip", "install", "--require-hashes", "--only-binary", ":all:", "--python", f"{venv}/bin/python",
         "-r", f"{rel}/requirements-lite.txt"],
        ["pip", "install", "--no-deps", "--reinstall-package", "eve-bridge", "--python", f"{venv}/bin/python",
         f"{rel}/eve_bridge-{VERSION}-py3-none-any.whl"],
    ]
    # eve-bridge: the service, then the setup page, each with the lite profile and the service's PATH
    assert box.eve_calls() == [
        (["install", "--service"], f"EVE_PROFILE=lite PATH={SERVICE_PATH} PYTHONNOUSERSITE=1"),
        (["web", "open", "--wait", "90"], f"EVE_PROFILE=lite PATH={SERVICE_PATH} PYTHONNOUSERSITE=1"),
    ]
    assert [ln.split()[1] for ln in box.traced("tar")] == ["-xzf", "-xzf"]
    assert len(box.traced("curl")) == 2
    assert box.traced("launchctl") == []
    assert (box.lite_home / "install.log").stat().st_mode & 0o777 == 0o600
    assert not list(box.lite_home.glob("tmp.*")), "the run's scratch folder is gone"


def test_uv_runs_with_nothing_on_path_but_its_own_folder(box: Sandbox) -> None:
    _assert_installed(box, box.run())
    lite = str(box.lite_home)
    calls = box.uv_calls()
    assert len(calls) == 5
    for _argv, env in calls:
        assert env["PATH"] == f"{lite}/bin"
        assert env["UV_PYTHON_PREFERENCE"] == "only-managed"
        assert env["UV_NO_CONFIG"] == "1"
        assert env["UV_PYTHON_INSTALL_DIR"] == f"{lite}/python"
        assert env["UV_PYTHON_INSTALL_BIN"] == "0"
        assert env["UV_CACHE_DIR"] == f"{lite}/cache"
        assert env["HOME"] == str(box.home)
        # env -i: nothing of the caller's environment leaks in (bash adds PWD, SHLVL and _ itself)
        assert set(env) <= {"HOME", "TMPDIR", "PATH", "NO_COLOR", "PWD", "SHLVL", "_"} | {
            k for k in env if k.startswith("UV_")}
        assert "EVE_SANDBOX_MARK" not in env


def test_positive_control_a_widened_uv_path_lets_install_name_tool_through(box: Sandbox) -> None:
    text = box.script.read_text()
    assert text.count('PATH="$LITE_HOME/bin"') == 1
    widened = box.tmp / "build" / "install-lite-widened.sh"
    widened.write_text(text.replace('PATH="$LITE_HOME/bin"', 'PATH="$PATH"'))
    done = box.run(script=widened)
    assert done.returncode == 0, done.stderr
    assert "install_name_tool -id x x" in box.forbidden_calls()


def test_second_run_changes_nothing(box: Sandbox) -> None:
    _assert_installed(box, box.run())
    before = snapshot(box.home, skip=(".eve-lite/install.log",))
    curls = len(box.traced("curl"))
    venvs = sum(argv[:1] == ["venv"] for argv, _ in box.uv_calls())
    done = box.run()
    _assert_installed(box, done)
    assert snapshot(box.home, skip=(".eve-lite/install.log",)) == before
    # uv is already there and the venv already has the pinned Python: only the release is fetched again
    assert len(box.traced("curl")) == curls + 1
    assert sum(argv[:1] == ["venv"] for argv, _ in box.uv_calls()) == venvs
    log = (box.lite_home / "install.log").read_text()
    assert "uv 0.11.27: already in ~/.eve-lite/bin" in log and "venv: kept" in log and "Eve.app: kept" in log


def test_sha256_mismatch_of_the_release_stops_before_unpacking(box: Sandbox) -> None:
    with box.lite_tgz.open("ab") as f:
        f.write(b"tampered")
    done = box.run()
    assert done.returncode == 1
    assert done.stdout == ""
    assert done.stderr.splitlines()[-1].startswith(f"eve lite: the eve lite {VERSION} download does not match its SHA-256")
    tars = box.traced("tar")
    assert len(tars) == 1 and "uv.tar.gz" in tars[0], "only uv was unpacked"
    assert not (box.lite_home / "release").exists()
    assert not list(box.lite_home.glob("tmp.*"))
    assert not box.env_file.exists()
    assert box.eve_calls() == [] and box.forbidden_calls() == []
    assert not any(argv[:1] == ["pip"] for argv, _ in box.uv_calls())


def test_sha256_mismatch_of_uv_stops_before_unpacking(box: Sandbox) -> None:
    with box.uv_tgz.open("ab") as f:
        f.write(b"tampered")
    done = box.run()
    assert done.returncode == 1
    assert "eve lite: the uv 0.11.27 download does not match its SHA-256" in done.stderr
    assert box.traced("tar") == []
    assert not (box.lite_home / "bin" / "uv").exists()
    assert box.uv_calls() == []


def test_refuses_when_full_eve_is_installed(box: Sandbox) -> None:
    agents = box.home / "Library" / "LaunchAgents"
    agents.mkdir(parents=True)
    (agents / "com.github.eve-bridge.daemon.plist").write_text("<plist/>\n")
    before = snapshot(box.home)
    done = box.run()
    assert done.returncode == 1
    assert "eve lite: full eve is installed for this macOS user" in done.stderr
    assert snapshot(box.home) == before
    assert box.traced("curl") == [] and box.forbidden_calls() == []


def test_refuses_full_eves_env_file_and_never_prints_it(box: Sandbox) -> None:
    box.env_file.parent.mkdir(parents=True)
    box.env_file.write_text(f"OPENROUTER_API_KEY={CANARY}\nEVE_TELEGRAM=app\n")
    before = snapshot(box.home)
    done = box.run()
    assert done.returncode == 1
    assert "belongs to full eve" in done.stderr
    assert CANARY not in done.stdout + done.stderr
    assert snapshot(box.home) == before


@pytest.mark.parametrize("text", [
    "EVE_PROFILE=lite\n",
    "EVE_PROFILE=lite",
    "export EVE_PROFILE='lite'\n",
    '  EVE_PROFILE = "lite"  \r\n',
    f"OPENROUTER_API_KEY={CANARY}\nEVE_PROFILE=lite\n",
    "EVE_PROFILE=full\nEVE_PROFILE=lite\n",
    "EVE_PROFILE=lite\nEVE_PROFILE=full\n",
    "#EVE_PROFILE=lite\n",
    "EVE_PROFILE=lite # mine\n",
    "EVE_PROFILE=LITE\n",
    "EVE_PROFILE=\n",
    "",
])
def test_reads_the_profile_as_envfile_py_does(box: Sandbox, text: str) -> None:
    box.env_file.parent.mkdir(parents=True)
    box.env_file.write_text(text)
    done = box.run("--dry-run")
    accepted = parse_env_file(text).get("EVE_PROFILE") == "lite"
    assert (done.returncode == 0) is accepted, done.stderr
    assert CANARY not in done.stdout + done.stderr


@pytest.mark.parametrize("fakes, refused", [
    ({"sw_vers": "echo 13.6.1"}, "needs macOS 14 or later; this Mac has 13.6.1"),
    ({"uname": 'case "$1" in -m) echo x86_64 ;; *) echo Darwin ;; esac', "sysctl": "echo 0"}, "needs a Mac with Apple silicon"),
    ({"uname": "echo Linux"}, "runs on a Mac"),
    ({"id": 'if [ "$1" = -u ]; then echo 0; else exec /usr/bin/id "$@"; fi'}, "not with sudo"),
    ({"uname": 'case "$1" in -m) echo x86_64 ;; *) echo Darwin ;; esac', "sysctl": "echo 1"}, None),  # Rosetta
])
def test_checks_the_mac_first(box: Sandbox, fakes: dict[str, str], refused: str | None) -> None:
    for name, body in fakes.items():
        box.shim(name, body)
    before = snapshot(box.home)
    done = box.run("--dry-run")
    if refused is None:
        assert done.returncode == 0, done.stderr
    else:
        assert done.returncode == 1
        assert refused in done.stderr
    assert snapshot(box.home) == before


def test_dry_run_writes_nothing(box: Sandbox) -> None:
    before = snapshot(box.home), snapshot(box.scratch)
    done = box.run("--dry-run")
    assert done.returncode == 0, done.stderr
    assert done.stdout.splitlines()[-1] == "Nothing was written."
    assert "write ~/.config/eve-bridge/env with EVE_PROFILE=lite, mode 600" in done.stdout
    assert (snapshot(box.home), snapshot(box.scratch)) == before
    assert not box.trace.exists() and box.forbidden_calls() == []


def test_a_copy_that_is_not_stamped_refuses(box: Sandbox) -> None:
    text = SCRIPT.read_text()
    for name in ("LITE_VERSION", "LITE_URL", "LITE_SHA256"):
        text = _set_pin(text, name, "")
    plain = box.tmp / "build" / "install-lite-plain.sh"
    plain.write_text(text)
    done = box.run(script=plain)
    assert done.returncode == 1
    assert "is not from a release" in done.stderr
    assert snapshot(box.home) == {}


def test_no_start_installs_and_starts_nothing(box: Sandbox) -> None:
    done = box.run("--no-start")
    assert done.returncode == 0, done.stderr
    assert done.stdout == f"eve lite {VERSION} is installed in ~/.eve-lite; nothing was started (--no-start).\n"
    assert done.stderr.splitlines()[-1] == "eve lite: installing packages, a few minutes the first time (3/3)"
    assert (box.lite_home / "venv" / "bin" / "eve-bridge").is_file()
    assert not box.env_file.exists() and not box.app.exists()
    assert box.eve_calls() == [] and box.forbidden_calls() == []


@pytest.mark.parametrize("mode, url", [("48782", "http://localhost:48782"), ("silent", "http://localhost:48780")])
def test_final_line_has_the_port_eve_reports(box: Sandbox, mode: str, url: str) -> None:
    box.web_open.write_text(mode)
    done = box.run()
    assert done.returncode == 0, done.stderr
    assert done.stdout == f"eve lite is running. Setup is open in your browser: {url}\n"


def _applescript_command(app: Path) -> str:
    """The shell command inside Eve.app, read back from the compiled script."""
    source = subprocess.run(["/usr/bin/osadecompile", str(app)], capture_output=True, text=True, check=True).stdout
    literal = re.fullmatch(r'do shell script "((?:[^"\\]|\\.)*)"\s*', source)
    assert literal, source
    return re.sub(r"\\(.)", r"\1", literal.group(1))


def _sh_quote(text: str) -> str:
    return "'" + text.replace("'", "'\\''") + "'"


def test_a_setup_page_that_does_not_open_is_explained(box: Sandbox) -> None:
    box.web_open.write_text("fail")
    done = box.run()
    assert done.returncode == 1
    assert done.stdout == ""
    assert done.stderr.splitlines()[-1].startswith("eve lite: eve did not answer within 90 seconds. Open Eve from Applications")
    assert "/.eve-lite/venv/bin/eve-bridge" in _applescript_command(box.app), "the app is there to try again with"


def test_an_eve_bridge_that_refuses_the_wait_is_not_blamed_on_eve(box: Sandbox) -> None:
    """argparse's refusal is not a daemon that never answered: the last line says which it was."""
    box.web_open.write_text("usage")
    done = box.run()
    assert done.returncode == 1
    assert done.stdout == ""
    last = done.stderr.splitlines()[-1]
    assert last.startswith("eve lite: eve lite is installed, but its eve-bridge does not take `web open --wait 90`")
    assert "90 seconds" not in last
    assert "unrecognized arguments: 90" in (box.lite_home / "install.log").read_text()


def test_eve_app_is_signed_untouched_and_survives_an_odd_home(tmp_path: Path) -> None:
    box = Sandbox(tmp_path, home_name="Jo O'Neil home")
    _assert_installed(box, box.run())
    eve = box.lite_home / "venv" / "bin" / "eve-bridge"
    command = _applescript_command(box.app)
    assert command == f"EVE_PROFILE=lite PYTHONNOUSERSITE=1 {_sh_quote(str(eve))} web open"
    # osacompile signs the applet; the installer adds nothing to the bundle, so the signature still holds
    assert subprocess.run(["/usr/bin/codesign", "--verify", "--strict", str(box.app)], capture_output=True).returncode == 0
    # `do shell script` runs the command with /bin/sh; so does this, and the stand-in eve-bridge sees `web open`
    subprocess.run(["/bin/sh", "-c", command], env={"HOME": str(box.home), "PATH": SERVICE_PATH}, check=True,
                   capture_output=True)
    assert box.eve_calls()[-1] == (["web", "open"], f"EVE_PROFILE=lite PATH={SERVICE_PATH} PYTHONNOUSERSITE=1")


def test_leaves_an_eve_app_it_did_not_create_alone(box: Sandbox) -> None:
    theirs = box.app / "Contents" / "MacOS"
    theirs.mkdir(parents=True)
    (theirs / "Eve").write_text("someone else's app\n")
    before = snapshot(box.app)
    _assert_installed(box, box.run())
    assert snapshot(box.app) == before
    assert box.traced("osacompile") == []
    box.run("--uninstall")
    assert snapshot(box.app) == before


# --- Uninstall -----------------------------------------------------------------------------------------------------


def _installed_with_a_key(box: Sandbox) -> None:
    _assert_installed(box, box.run())
    with box.env_file.open("a") as f:
        f.write(f"OPENROUTER_API_KEY={CANARY}\n")
    logs = box.home / "Library" / "Logs"
    logs.mkdir(parents=True)
    (logs / "eve-lite.log").write_text("daemon log\n")


KEPT_LINE = "eve lite is uninstalled. Its memory and settings stay in ~/.config/eve-bridge; the install line takes them back.\n"


def test_uninstall_without_a_terminal_keeps_the_memory(box: Sandbox) -> None:
    _installed_with_a_key(box)
    done = box.run("--uninstall")
    assert done.returncode == 0, done.stderr
    assert done.stdout == KEPT_LINE
    assert box.traced("launchctl") == [f"launchctl bootout gui/{os.getuid()}/{LITE_LABEL}"]
    assert not box.lite_home.exists() and not box.app.exists()
    assert not (box.home / "Library" / "LaunchAgents" / f"{LITE_LABEL}.plist").exists()
    assert not (box.home / "Library" / "Logs" / "eve-lite.log").exists()
    # no EVE_PROFILE line stays behind (full eve installed here later would boot as lite); a marker says whose it was
    assert box.env_file.read_text() == f"OPENROUTER_API_KEY={CANARY}\n"
    assert box.env_file.stat().st_mode & 0o777 == 0o600
    assert box.kept.read_text().splitlines()[-1] == f"env_sha256={_sha(box.env_file.read_bytes())}"
    assert box.kept.stat().st_mode & 0o777 == 0o600
    assert CANARY not in done.stdout + done.stderr + box.kept.read_text()
    assert box.forbidden_calls() == []


def _memory(box: Sandbox) -> Path:
    note = box.env_file.parent / "memory" / "2026-10-13.md"
    note.parent.mkdir(parents=True)
    note.write_text("eve lite's memory\n")
    return note


@pytest.mark.parametrize("with_key, answer", [(False, None), (True, None), (True, "\n")])
def test_install_again_after_an_uninstall_that_kept_the_memory(box: Sandbox, with_key: bool, answer: str | None) -> None:
    """The way back the uninstall promises: kept by default (no terminal) or by Return at [y/N], then the line again."""
    if with_key:
        _installed_with_a_key(box)
    else:
        _assert_installed(box, box.run())
    note = _memory(box)
    if answer is None:
        gone = box.run("--uninstall")
    else:
        gone, shown = box.run_tty("--uninstall", answer=answer)
        assert "[y/N]" in shown
    assert gone.returncode == 0, gone.stderr
    assert gone.stdout == KEPT_LINE
    if with_key:
        assert box.env_file.read_text() == f"OPENROUTER_API_KEY={CANARY}\n"
        assert box.kept.read_text().splitlines()[-1] == f"env_sha256={_sha(box.env_file.read_bytes())}"
    else:
        # nothing but EVE_PROFILE=lite was in it, so the env file goes rather than stay empty
        assert not box.env_file.exists()
        assert box.kept.read_text().splitlines()[-1] == "env_sha256=none"

    back = box.run()
    assert back.returncode == 0, back.stderr
    assert back.stdout == FINAL + "\n"
    assert box.env_file.read_text() == (f"OPENROUTER_API_KEY={CANARY}\n" if with_key else "") + "EVE_PROFILE=lite\n"
    assert box.env_file.stat().st_mode & 0o777 == 0o600
    assert not box.kept.exists()
    assert note.read_text() == "eve lite's memory\n"
    assert CANARY not in gone.stdout + gone.stderr + back.stdout + back.stderr
    assert box.forbidden_calls() == []
    # and from there it is an ordinary install: a third run changes nothing
    before = snapshot(box.home, skip=(".eve-lite/install.log",))
    _ = box.run()
    assert snapshot(box.home, skip=(".eve-lite/install.log",)) == before


def test_a_kept_env_file_without_the_marker_is_refused(box: Sandbox) -> None:
    """Positive control: the marker is what lets a kept env file through; without it the file is full eve's."""
    _installed_with_a_key(box)
    assert box.run("--uninstall").returncode == 0
    box.kept.unlink()
    before, curls = snapshot(box.home), len(box.traced("curl"))
    done = box.run()
    assert done.returncode == 1
    assert "belongs to full eve (it does not set EVE_PROFILE=lite)" in done.stderr
    assert snapshot(box.home) == before
    assert len(box.traced("curl")) == curls, "refused before any download"
    assert box.forbidden_calls() == []


@pytest.mark.parametrize("with_key", [True, False])
def test_a_kept_folder_whose_env_file_changed_since_is_refused(box: Sandbox, with_key: bool) -> None:
    """The marker holds the env file the uninstall left (or none): a file changed or written since is not eve lite's
    to take back, say full eve installed and removed in between."""
    if with_key:
        _installed_with_a_key(box)
    else:
        _assert_installed(box, box.run())
    assert box.run("--uninstall").returncode == 0
    with box.env_file.open("a") as f:
        f.write("EVE_TELEGRAM=app\n")
    box.env_file.chmod(0o600)
    before = snapshot(box.home)
    done = box.run()
    assert done.returncode == 1
    assert "~/.config/eve-bridge/env changed after eve lite was uninstalled" in done.stderr
    assert snapshot(box.home) == before
    assert CANARY not in done.stdout + done.stderr


def test_a_kept_folder_is_not_taken_back_while_full_eve_is_installed(box: Sandbox) -> None:
    _installed_with_a_key(box)
    assert box.run("--uninstall").returncode == 0
    agents = box.home / "Library" / "LaunchAgents"
    agents.mkdir(parents=True, exist_ok=True)
    (agents / "com.github.eve-bridge.daemon.plist").write_text("<plist/>\n")
    before = snapshot(box.home)
    done = box.run()
    assert done.returncode == 1
    assert "full eve is installed for this macOS user" in done.stderr
    assert snapshot(box.home) == before


def test_dry_run_on_a_kept_folder_says_it_adds_the_profile_line(box: Sandbox) -> None:
    _installed_with_a_key(box)
    assert box.run("--uninstall").returncode == 0
    before = snapshot(box.home)
    done = box.run("--dry-run")
    assert done.returncode == 0, done.stderr
    assert "add EVE_PROFILE=lite to ~/.config/eve-bridge/env, which eve lite's uninstall kept" in done.stdout
    assert snapshot(box.home) == before


def test_a_second_uninstall_still_offers_the_kept_memory(box: Sandbox) -> None:
    _installed_with_a_key(box)
    _memory(box)
    assert box.run("--uninstall").stdout == KEPT_LINE
    dry = box.run("--uninstall", "--dry-run")
    assert "nothing else of eve lite's is left to remove." in dry.stdout
    assert "ask before deleting ~/.config/eve-bridge" in dry.stdout
    done, shown = box.run_tty("--uninstall", answer="y\n")
    assert done.returncode == 0, done.stderr
    assert "Also delete eve's memory, chats and sign-ins in ~/.config/eve-bridge? [y/N]" in shown
    assert done.stdout == "eve lite is uninstalled, with its memory and settings.\n"
    assert not (box.home / ".config" / "eve-bridge").exists()


def test_uninstall_deletes_the_memory_after_a_yes(box: Sandbox) -> None:
    _installed_with_a_key(box)
    done, shown = box.run_tty("--uninstall", answer="y\n")
    assert done.returncode == 0, done.stderr
    assert "Also delete eve's memory, chats and sign-ins in ~/.config/eve-bridge? [y/N]" in shown
    assert done.stdout == "eve lite is uninstalled, with its memory and settings.\n"
    assert not (box.home / ".config" / "eve-bridge").exists()
    assert not box.lite_home.exists()


def _full_eve(box: Sandbox, env_text: str) -> Path:
    agents = box.home / "Library" / "LaunchAgents"
    agents.mkdir(parents=True, exist_ok=True)
    (agents / "com.github.eve-bridge.daemon.plist").write_text("<plist/>\n")
    box.env_file.parent.mkdir(parents=True, exist_ok=True)
    box.env_file.write_text(env_text)
    box.env_file.chmod(0o600)
    return agents / "com.github.eve-bridge.daemon.plist"


def test_uninstall_never_offers_full_eves_folder(box: Sandbox) -> None:
    plist = _full_eve(box, f"OPENROUTER_API_KEY={CANARY}\n")
    before = snapshot(box.home / ".config")
    done, shown = box.run_tty("--uninstall", answer="y\n")
    assert done.returncode == 0, done.stderr
    assert done.stdout == "eve lite is uninstalled. Full eve stays as it is.\n"
    assert "[y/N]" not in shown
    assert snapshot(box.home / ".config") == before
    assert plist.exists()


def test_uninstall_takes_back_its_profile_line_from_full_eves_env_file(box: Sandbox) -> None:
    """Full eve installed over eve lite (install.sh does not check) would boot as lite while the line stays."""
    plist = _full_eve(box, f"EVE_TELEGRAM=app\nEVE_PROFILE=lite\nOPENROUTER_API_KEY={CANARY}\n")
    done, shown = box.run_tty("--uninstall", answer="y\n")
    assert done.returncode == 0, done.stderr
    assert done.stdout == "eve lite is uninstalled. Full eve stays as it is.\n"
    assert "[y/N]" not in shown
    assert box.env_file.read_text() == f"EVE_TELEGRAM=app\nOPENROUTER_API_KEY={CANARY}\n"
    assert box.env_file.stat().st_mode & 0o777 == 0o600
    assert not box.kept.exists(), "the folder is full eve's: eve lite marks nothing there to take back"
    assert plist.exists()
    assert CANARY not in done.stdout + done.stderr + shown


def test_uninstall_dry_run_writes_nothing(box: Sandbox) -> None:
    _installed_with_a_key(box)
    before = snapshot(box.home)
    done = box.run("--uninstall", "--dry-run")
    assert done.returncode == 0, done.stderr
    assert "~/.eve-lite" in done.stdout and "~/Applications/Eve.app" in done.stdout
    assert done.stdout.splitlines()[-1] == "Nothing was written."
    assert snapshot(box.home) == before
    assert box.traced("launchctl") == []


# --- The release script's side -------------------------------------------------------------------------------------


def test_release_pack_holds_exactly_what_the_installer_checks(box: Sandbox) -> None:
    with tarfile.open(box.lite_tgz) as tar:
        names = sorted(m.name for m in tar.getmembers())
        sums = tar.extractfile(f"eve-lite-{VERSION}/SHA256SUMS").read().decode()
        wheel = tar.extractfile(f"eve-lite-{VERSION}/eve_bridge-{VERSION}-py3-none-any.whl").read()
        assert {(m.uid, m.gid) for m in tar.getmembers()} == {(0, 0)}
    assert names == [f"eve-lite-{VERSION}", f"eve-lite-{VERSION}/SHA256SUMS",
                     f"eve-lite-{VERSION}/eve_bridge-{VERSION}-py3-none-any.whl",
                     f"eve-lite-{VERSION}/requirements-lite.txt"]
    assert sums.splitlines() == [f"{_sha(wheel)}  eve_bridge-{VERSION}-py3-none-any.whl",
                                 f"{_sha(REQUIREMENTS.encode())}  requirements-lite.txt"]


def test_release_stamp_sets_only_the_three_pins_and_refuses_odd_values(box: Sandbox) -> None:
    out = box.tmp / "build" / "stamped.sh"
    url = f"https://github.com/gurul/eve_01/releases/download/lite-v{VERSION}/eve-lite-{VERSION}.tar.gz"
    box.release("--stamp", str(SCRIPT), str(out), VERSION, url, "a" * 64)
    old, new = SCRIPT.read_text().splitlines(), out.read_text().splitlines()
    changed = [b for a, b in zip(old, new, strict=True) if a != b]
    assert changed == [f'LITE_VERSION="{VERSION}"', f'LITE_URL="{url}"', f'LITE_SHA256="{"a" * 64}"']
    assert out.stat().st_mode & 0o777 == 0o755
    for bad in ([VERSION, "http://example.com/x.tar.gz", "a" * 64], [VERSION, url + "&x=1", "a" * 64],
                [VERSION, url, "A" * 64], [VERSION + " 1", url, "a" * 64], [VERSION, url.replace("lite-v", "lite v"), "a" * 64]):
        refused = subprocess.run(["sh", str(RELEASE), "--stamp", str(SCRIPT), str(box.tmp / "build" / "bad.sh"), *bad],
                                 capture_output=True, text=True, env={"PATH": SERVICE_PATH, "HOME": str(box.home)})
        assert refused.returncode != 0, bad
        assert not (box.tmp / "build" / "bad.sh").exists()


def _uv_dir() -> str | None:
    for candidate in (shutil.which("uv"), "/opt/homebrew/bin/uv", "/usr/local/bin/uv"):
        if candidate and Path(candidate).is_file():
            return str(Path(candidate).parent)
    return None


def test_release_dry_run_prints_its_marker_and_writes_nothing(tmp_path: Path) -> None:
    uv_dir = _uv_dir()
    if uv_dir is None:
        pytest.skip("uv is not installed here")
    home, scratch = tmp_path / "home", tmp_path / "tmp"
    home.mkdir()
    scratch.mkdir()
    installer = SCRIPT.read_bytes()
    dist = sorted((ROOT / "bridge" / "dist").glob("lite-v*")) if (ROOT / "bridge" / "dist").exists() else []
    done = subprocess.run(["sh", str(RELEASE), "--dry-run"], capture_output=True, text=True, timeout=120, cwd=ROOT / "bridge",
                          env={"HOME": str(home), "TMPDIR": str(scratch), "PATH": f"{uv_dir}:{SERVICE_PATH}"})
    assert done.returncode == 0, done.stderr
    assert done.stdout.splitlines()[-1] == "DRY_RUN_OK"
    assert "the lite closure is" in done.stdout
    assert ("bridge/src, bridge/pyproject.toml, bridge/uv.lock, ios/EveLink/Web, install-lite.sh, tools/release_lite.sh"
            in done.stdout)
    assert f"must take: {'; '.join(_eve_commands())}\n" in done.stdout
    assert snapshot(home) == {} and snapshot(scratch) == {}
    assert SCRIPT.read_bytes() == installer
    assert (sorted((ROOT / "bridge" / "dist").glob("lite-v*")) if (ROOT / "bridge" / "dist").exists() else []) == dist


def test_release_tree_changes_cover_the_web_page_and_both_scripts(tmp_path: Path) -> None:
    """What the real run refuses to build over, in a new repository where nothing is committed yet: the web page's
    sources and both scripts count with the bridge's. node_modules and the page build:web writes do not, even with no
    .gitignore to hide them, and neither do files the release is not built from."""
    if not toolchain.git_usable({"PATH": SERVICE_PATH}):
        pytest.skip("git here would be macOS's developer-tools stand-in")
    repo = tmp_path / "repo"
    ships = {
        "bridge/src/eve_bridge/lite.py": True,
        "bridge/pyproject.toml": True,
        "bridge/uv.lock": True,
        "ios/EveLink/Web/package.json": True,
        "ios/EveLink/Web/pnpm-lock.yaml": True,
        "ios/EveLink/Web/web.html": True,
        "ios/EveLink/Web/src/web/main.tsx": True,
        "install-lite.sh": True,
        "tools/release_lite.sh": True,
        "ios/EveLink/Web/node_modules/vite/index.js": False,
        "bridge/src/eve_bridge/web_lite/index.html": False,
        "bridge/tests/test_lite.py": False,
        "ios/EveLink/App/ChatWeb/chat.js": False,
        "docs/lite.md": False,
    }
    for rel in ships:
        (repo / rel).parent.mkdir(parents=True, exist_ok=True)
        (repo / rel).write_text("x\n")
    shutil.copy(RELEASE, repo / "tools" / "release_lite.sh")
    env = {"HOME": str(tmp_path), "PATH": SERVICE_PATH, "TMPDIR": str(tmp_path)}
    try:
        subprocess.run(["git", "init", "-q", str(repo)], check=True, capture_output=True, env=env)
    except (OSError, subprocess.CalledProcessError) as e:
        pytest.skip(f"git does not run here ({e})")
    done = subprocess.run(["sh", str(repo / "tools" / "release_lite.sh"), "--tree-changes"], capture_output=True,
                          text=True, timeout=60, env=env)
    assert done.returncode == 0, done.stderr
    lines = done.stdout.splitlines()
    assert sorted(line[3:] for line in lines) == sorted(rel for rel, shipped in ships.items() if shipped)
    assert {line[:3] for line in lines} == {"?? "}


def _eve_commands() -> list[str]:
    found = re.findall(r'^EVE_COMMANDS="([^"]*)"$', RELEASE.read_text(), re.M)
    assert len(found) == 1, "tools/release_lite.sh must set EVE_COMMANDS exactly once"
    return found[0].splitlines()


def test_release_checks_each_command_the_installer_and_eve_app_run(box: Sandbox) -> None:
    """EVE_COMMANDS, which every release's eve-bridge must take, is what a sandboxed install and its Eve.app run."""
    _assert_installed(box, box.run())
    ran = [" ".join(argv) for argv, _ in box.eve_calls()]
    words = shlex.split(_applescript_command(box.app))
    ran.append(" ".join(words[words.index(str(box.lite_home / "venv" / "bin" / "eve-bridge")) + 1:]))
    assert sorted(ran) == sorted(_eve_commands())


def _cli_check(tmp: Path, *commands: str) -> subprocess.CompletedProcess:
    """The release's CLI check (--cli-check) with this tree's eve-bridge: the Python running these tests."""
    scratch = tmp / "cli-tmp"
    scratch.mkdir(exist_ok=True)
    return subprocess.run(["sh", str(RELEASE), "--cli-check", sys.executable, *commands], capture_output=True, text=True,
                          timeout=120, env={"HOME": str(tmp), "PATH": SERVICE_PATH, "TMPDIR": str(scratch)})


def test_release_cli_check_only_parses_and_fails_a_refused_command(tmp_path: Path) -> None:
    """On commands this tree's eve-bridge has had all along. `status` run would print the install state: the check's
    own lines are all of stdout, so it only parsed. Controls: a flag the command lacks is left over and a command the
    CLI lacks is refused, and either fails the check."""
    done = _cli_check(tmp_path, "status", "install --service")
    assert done.returncode == 0, done.stdout + done.stderr
    assert done.stdout.splitlines() == ["CLI_OK eve-bridge status", "CLI_OK eve-bridge install --service"]
    bad = _cli_check(tmp_path, "install --service --no-such-flag", "no-such-command", "status")
    assert bad.returncode == 1
    assert bad.stdout.splitlines() == [
        "CLI_OK eve-bridge status",
        "CLI_FAIL eve-bridge install --service --no-such-flag: argparse left --no-such-flag over",
        "CLI_FAIL eve-bridge no-such-command: refused (exit 2; argparse says why on stderr)",
    ]
    assert "invalid choice: 'no-such-command'" in bad.stderr
    assert snapshot(tmp_path / "cli-tmp") == {}, "the check's own HOME is gone"


# A stand-in eve_bridge for the CLI check: the shape WP-10's `web open` is given, with its --wait as @WAIT@ says. A
# command that ran would leave a line in @RAN@.
FAKE_CLI = '''import argparse


def main(argv=None):
    parser = argparse.ArgumentParser(prog="eve-bridge")
    sub = parser.add_subparsers(dest="cmd")
    sub.add_parser("install").add_argument("--service", action="store_true")
    web = sub.add_parser("web").add_subparsers(dest="web_cmd")
    web.add_parser("open").add_argument("--wait", @WAIT@)
    args = parser.parse_args(argv)
    with open(@RAN@, "a") as ran:
        ran.write(repr(vars(args)) + "\\n")
    return 0
'''


@pytest.mark.parametrize("wait, takes_90", [('type=float, default=0.0, metavar="SECONDS"', True),
                                            ('action="store_true"', False)])
def test_release_cli_check_catches_a_wait_that_takes_no_value(tmp_path: Path, wait: str, takes_90: bool) -> None:
    """Positive control for the check below, which waits on WP-10: an eve-bridge whose --wait is a bare flag would
    stop every install at step 5, and the check fails it; one whose --wait takes the seconds passes. Neither runs."""
    venv = tmp_path / "venv"
    subprocess.run([sys.executable, "-m", "venv", "--without-pip", str(venv)], check=True, capture_output=True)
    package = next(venv.glob("lib/python3.*/site-packages")) / "eve_bridge"
    package.mkdir()
    ran = tmp_path / "ran.log"
    (package / "__init__.py").write_text("")
    (package / "profile.py").write_text("def install_firewall(mode='enforce'):\n    pass\n")
    (package / "cli.py").write_text(FAKE_CLI.replace("@WAIT@", wait).replace("@RAN@", repr(str(ran))))
    scratch = tmp_path / "tmp"
    scratch.mkdir()
    done = subprocess.run(["sh", str(RELEASE), "--cli-check", str(venv / "bin" / "python")], capture_output=True,
                          text=True, timeout=120, env={"HOME": str(tmp_path), "PATH": SERVICE_PATH, "TMPDIR": str(scratch)})
    if takes_90:
        assert done.returncode == 0, done.stdout + done.stderr
        assert done.stdout.splitlines() == [f"CLI_OK eve-bridge {command}" for command in _eve_commands()]
    else:
        assert done.returncode == 1
        assert done.stdout.splitlines() == ["CLI_OK eve-bridge install --service", "CLI_OK eve-bridge web open",
                                            "CLI_FAIL eve-bridge web open --wait 90: argparse left 90 over"]
    assert not ran.exists(), "the check parsed; nothing ran"


def test_this_trees_eve_bridge_takes_each_command_the_installer_runs(tmp_path: Path) -> None:
    """The check each release runs, on this tree: `web open --wait 90` must parse, so a --wait that takes no value
    fails here rather than at every user's step 5. It applies once `eve-bridge web` exists (WP-10)."""
    done = _cli_check(tmp_path)
    fails = [line for line in done.stdout.splitlines() if line.startswith("CLI_FAIL")]
    if (fails and "invalid choice: 'web'" in done.stderr
            and all(line.startswith("CLI_FAIL eve-bridge web ") for line in fails)):
        pytest.skip("this tree's eve-bridge has no `web` command yet (WP-10)")
    assert done.returncode == 0, done.stdout + done.stderr
    assert done.stdout.splitlines() == [f"CLI_OK eve-bridge {command}" for command in _eve_commands()]
