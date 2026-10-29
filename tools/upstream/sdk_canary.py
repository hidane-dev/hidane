"""Runs an SDK harness of tools/oracle/ with a new SDK release against both emulators
(tools/upstream/README.md).

Usage (from the repository root; the Upstream workflow's sdk jobs run it):
    python3 tools/upstream/sdk_canary.py plan --upstream FILE [--outputs FILE]
        Which new SDK releases (watch.py detect's report) have a harness the canary runs, as the
        `sdk_runs` output (JSON).
    python3 tools/upstream/sdk_canary.py run --key ECOSYSTEM:NAME@VERSION --jar JAR --java JAVA
            --hidane BINARY --work DIR --out DIR
        Installs that release into a copy of its harness under --work, then runs each unit of the
        harness (a section, or a script) once on a freshly started official emulator (the pinned
        jar) and once on a freshly started hidane, and compares the two transcripts with the
        committed pair in results/. Writes sdk.json, the transcripts and the logs to --out.

A unit is `identical` when both emulators give the same transcript, and `documented` when every
difference is one the verified release shows too: in the committed pair (results/ and
docs/parity-exceptions.md explain those) or, run the same way on the same runner, at a place where
a pair of the verified release differs too, whatever the values there (some steps depend on
timing, which a runner changes). Otherwise the unit runs
again, and is `different` when the same unexplained differences come back, `unstable` when they
do not. A harness that fails, or prints something that is not JSON, is an `error`. Whether the
SDK's own transcript on the official emulator changed from the committed one is recorded too.

The harnesses: tools/oracle/sdk_go/, sdk_python/, sdk_java/ (sections documents, queries,
transactions, listen) and the tools/oracle/sdk_*.mjs scripts that import the npm package (but
sdk_export_import.mjs, which needs firebase-tools). The other packages a Node script imports stay
at their verified versions (tools/upstream/watch.json). Stdlib only.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import watch  # noqa: E402  (tools/upstream/watch.py)

ROOT = watch.ROOT
ORACLE = "tools/oracle"
START_SH = ".github/actions/official-emulator/start.sh"
STOP_SH = ".github/actions/official-emulator/stop.sh"

# The package -> the harness that runs it.
HARNESSES = {
    "go:cloud.google.com/go/firestore": "go",
    "pypi:google-cloud-firestore": "python",
    "maven:com.google.cloud:google-cloud-firestore": "java",
    "npm:@google-cloud/firestore": "node",
    "npm:firebase": "node",
    "npm:@firebase/rules-unit-testing": "node",
}
SECTIONS = ("documents", "queries", "transactions", "listen")
AUTOMATIC_CHANNELS = (watch.STABLE,)
NODE_PACKAGES = ("firebase", "@google-cloud/firestore", "@firebase/rules-unit-testing")
NODE_SKIPPED = ("sdk_export_import.mjs",)  # runs under firebase-tools
DEPENDENCY_PLUGIN = "org.apache.maven.plugins:maven-dependency-plugin:3.11.0"
JAVA_MAIN = "dev.hidane.oracle.SdkTranscript"
POM_VERSION = re.compile(
    r"(<artifactId>google-cloud-firestore</artifactId>\s*<version>)[^<]+(</version>)")

IDENTICAL, DOCUMENTED, DIFFERENT, UNSTABLE, ERROR = (
    "identical", "documented", "different", "unstable", "error")
OFFICIAL_PORT, HIDANE_PORT = 8090, 8190
UNIT_TIMEOUT = 600


class Stop(Exception):
    pass


def parse_key(key):
    """(ecosystem, name, version) of `ecosystem:name@version`, checked."""
    ident, _, version = key.rpartition("@")
    ecosystem, _, name = ident.partition(":")
    if ident not in HARNESSES or not watch.valid(watch.VERSION, version):
        raise Stop(f"no harness runs {watch.shown(key)}")
    return ecosystem, name, version


def slug(key):
    """A name for the release's artifact."""
    return "sdk-" + re.sub(r"[^A-Za-z0-9.]+", "-", key).strip("-")


def runs_by_itself(sdk):
    """Whether the canary runs a release's harness without being asked: a stable release only.
    A prerelease, or a `latest` that is not the stable release, runs only when asked for by
    hand (the workflow's `sdk` input), since its code is the least reviewed."""
    return sdk["channel"] in AUTOMATIC_CHANNELS or bool(sdk.get("requested"))


def plan(upstream):
    """The new releases of verified SDKs that have a harness and run (runs_by_itself)."""
    _, sdks = watch.canary_inputs(upstream)
    runs = []
    for sdk in sdks:
        ident = f"{sdk['ecosystem']}:{sdk['name']}"
        if sdk["verified"] and ident in HARNESSES and runs_by_itself(sdk):
            runs.append({"key": sdk["key"], "slug": slug(sdk["key"]), "name": sdk["name"],
                         "version": sdk["version"], "harness": HARNESSES[ident]})
    return runs


def node_imports(text):
    """The npm packages a script imports."""
    names = []
    for spec in re.findall(r"""^\s*import\b[^'"]*?['"]([^'"]+)['"]""", text, re.M):
        if spec.startswith((".", "node:")):
            continue
        parts = spec.split("/")
        name = "/".join(parts[:2]) if spec.startswith("@") else parts[0]
        if name not in names:
            names.append(name)
    return names


def units(harness, name, root=ROOT):
    """[(unit, transcript name in results/)] of a harness, for the package `name`."""
    if harness in ("go", "python", "java"):
        return [(section, f"sdk-{harness}-{section}") for section in SECTIONS]
    found = []
    for script in sorted((root / ORACLE).glob("sdk_*.mjs")):
        if script.name in NODE_SKIPPED:
            continue
        if name in node_imports(script.read_text(encoding="utf-8")):
            found.append((script.name, "sdk-" + script.stem[len("sdk_"):].replace("_", "-")))
    return found


def committed(transcript, pinned, root=ROOT):
    """The committed pair of a unit: (official, hidane), parsed."""
    official = root / "results" / f"{transcript}-official-v{pinned}.json"
    hidane = root / "results" / f"{transcript}-hidane.json"
    if not official.is_file() or not hidane.is_file():
        raise Stop(f"results/{transcript}-*.json: the committed pair is missing")
    return (json.loads(official.read_text(encoding="utf-8")),
            json.loads(hidane.read_text(encoding="utf-8")))


# Comparing --------------------------------------------------------------------------------------

_ABSENT = "<absent>"


def differences(a, b, path="$"):
    """Where two JSON values differ: sorted (path, a there, b there), compared as JSON text so
    that 1, 1.0 and true stay apart."""
    if isinstance(a, dict) and isinstance(b, dict):
        out = []
        for name in sorted(set(a) | set(b)):
            out += differences(a.get(name, _ABSENT), b.get(name, _ABSENT), f"{path}.{name}")
        return out
    if isinstance(a, list) and isinstance(b, list):
        out = []
        for i in range(max(len(a), len(b))):
            out += differences(a[i] if i < len(a) else _ABSENT, b[i] if i < len(b) else _ABSENT,
                               f"{path}[{i}]")
        return out
    left, right = (json.dumps(v, sort_keys=True) for v in (a, b))
    return [] if left == right else [(path, left, right)]


def unexplained(pair, allowed, places=frozenset()):
    """The differences of a pair (official, hidane) that are not among `allowed`, nor at one of
    `places` (paths): [] when it is identical or differs only as the verified release does."""
    return [d for d in differences(*pair) if d not in allowed and d[0] not in places]


# Emulators --------------------------------------------------------------------------------------

def quiet_env(**extra):
    """The environment for a child, without the step's GitHub files (no stray outputs)."""
    env = {k: v for k, v in os.environ.items()
           if k not in ("GITHUB_OUTPUT", "GITHUB_STEP_SUMMARY", "GITHUB_ENV", "GITHUB_PATH")}
    env.update(extra)
    return env


class Official:
    """The official emulator, started and stopped with the action's own scripts."""

    def __init__(self, jar, java, version, logs, root=ROOT):
        self.jar, self.java, self.version, self.logs, self.root = jar, java, version, logs, root

    def start(self, name):
        outputs = self.logs / f"{name}.official.outputs"
        outputs.write_text("")
        env = quiet_env(JAVA=self.java, JAR=self.jar, VERSION=self.version, HOST="127.0.0.1",
                        PORT=str(OFFICIAL_PORT), JAVA_OPTIONS="", ARGS="", TIMEOUT="120",
                        LOG=str(self.logs / f"{name}.official.log"), GITHUB_OUTPUT=str(outputs))
        subprocess.run(["bash", str(self.root / START_SH)], env=env, check=True,
                       stdout=subprocess.DEVNULL)
        values = dict(line.split("=", 1) for line in outputs.read_text().splitlines() if "=" in line)
        return values["address"], values["pid"]

    def stop(self, handle):
        address, pid = handle
        subprocess.run(["bash", str(self.root / STOP_SH)], env=quiet_env(ADDRESS=address, PID=pid),
                       stdout=subprocess.DEVNULL, check=False)


class Hidane:
    def __init__(self, binary, logs):
        self.binary, self.logs = binary, logs

    def start(self, name):
        address = f"127.0.0.1:{HIDANE_PORT}"
        log = open(self.logs / f"{name}.hidane.log", "wb")
        process = subprocess.Popen([self.binary, "--host", "127.0.0.1", "--port", str(HIDANE_PORT)],
                                   stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(f"http://{address}/", timeout=2):
                    return address, process, log
            except (urllib.error.URLError, OSError):
                if process.poll() is not None:
                    break
                time.sleep(0.2)
        process.kill()
        log.close()
        raise Stop("hidane did not start")

    def stop(self, handle):
        address, process, log = handle
        try:
            request = urllib.request.Request(f"http://{address}/shutdown", method="POST")
            urllib.request.urlopen(request, timeout=5).close()
        except (urllib.error.URLError, OSError):
            pass
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        log.close()


def transcript(emulator, side, name, command, cwd, logs):
    """Runs a unit's command against a fresh emulator: its transcript, or None and why."""
    handle = emulator.start(name)
    try:
        done = subprocess.run(command, cwd=cwd, env=quiet_env(FIRESTORE_EMULATOR_HOST=handle[0]),
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                              timeout=UNIT_TIMEOUT, stdin=subprocess.DEVNULL)
    except subprocess.TimeoutExpired:
        return None, f"no transcript within {UNIT_TIMEOUT} s"
    finally:
        emulator.stop(handle)
    (logs / f"{name}.{side}.stderr").write_bytes(done.stderr)
    if done.returncode != 0:
        (logs / f"{name}.{side}.stdout").write_bytes(done.stdout)
        return None, f"exited with {done.returncode}"
    try:
        return json.loads(done.stdout), None
    except ValueError:
        (logs / f"{name}.{side}.stdout").write_bytes(done.stdout)
        return None, "printed something that is not JSON"


# Harnesses --------------------------------------------------------------------------------------

def run_tool(command, cwd, logs, name):
    """Runs a preparation command; its output goes to a log, a failure stops the run."""
    with open(logs / f"prepare-{name}.log", "wb") as log:
        done = subprocess.run(command, cwd=cwd, stdout=log, stderr=subprocess.STDOUT,
                              env=quiet_env(), stdin=subprocess.DEVNULL)
    if done.returncode != 0:
        raise Stop(f"preparing the harness: `{' '.join(map(str, command[:4]))}` failed")


def prepare(harness, name, version, work, logs, watched, root=ROOT, run=run_tool):
    """Installs the release into a copy of its harness; the command that runs one unit."""
    oracle = root / ORACLE
    if harness == "go":
        source = work / "sdk_go"
        shutil.copytree(oracle / "sdk_go", source)
        run(["go", "get", f"{name}@v{version}"], source, logs, "go-get")
        run(["go", "build", "-o", str(work / "sdk_go_bin"), "."], source, logs, "go-build")
        return lambda unit: ([str(work / "sdk_go_bin"), unit], work)
    if harness == "python":
        venv = work / "venv"
        run([sys.executable, "-m", "venv", str(venv)], work, logs, "venv")
        pip = str(venv / "bin" / "pip")
        run([pip, "install", "--only-binary=:all:", "-r",
             str(oracle / "sdk_python" / "requirements.txt")], work, logs, "pip-requirements")
        run([pip, "install", "--only-binary=:all:", f"{name}=={version}"], work, logs, "pip")
        script = str(oracle / "sdk_python" / "transcript.py")
        return lambda unit: ([str(venv / "bin" / "python"), "-I", script, unit], work)
    if harness == "java":
        source = work / "sdk_java"
        shutil.copytree(oracle / "sdk_java", source)
        pom = source / "pom.xml"
        text, count = POM_VERSION.subn(lambda m: m.group(1) + version + m.group(2),
                                       pom.read_text(encoding="utf-8"))
        if count != 1:
            raise Stop("tools/oracle/sdk_java/pom.xml: the google-cloud-firestore version was "
                       "not found once")
        pom.write_text(text, encoding="utf-8")
        run(["mvn", "-q", "-B", "compile"], source, logs, "mvn-compile")
        # Run the class with `java`, not through `mvn exec:java`: nothing but the transcript on
        # standard output, and no Maven start-up per unit.
        run(["mvn", "-q", "-B", f"{DEPENDENCY_PLUGIN}:build-classpath",
             "-Dmdep.outputFile=classpath.txt"], source, logs, "mvn-classpath")
        classpath = os.pathsep.join([str(source / "target" / "classes"),
                                     (source / "classpath.txt").read_text().strip()])
        return lambda unit: (["java", "-cp", classpath, JAVA_MAIN, unit], source)
    if harness == "node":
        source = work / "node"
        source.mkdir()
        (source / "package.json").write_text('{"private": true}\n')
        for script in oracle.glob("sdk_*.mjs"):
            shutil.copy(script, source / script.name)
        specs = []
        for package in NODE_PACKAGES:
            wanted = version if package == name else watched.get(f"npm:{package}")
            if not watch.valid(watch.VERSION, wanted):
                raise Stop(f"npm:{package}: no version to install")
            specs.append(f"{package}@{wanted}")
        run(["npm", "install", "--no-save", "--ignore-scripts", "--no-audit", "--no-fund",
             *specs], source, logs, "npm-install")
        return lambda unit: (["node", str(source / unit)], source)
    raise Stop(f"{harness}: not a harness")


class Control:
    """The harness with the verified release, prepared the first time a unit needs it."""

    def __init__(self, prepare_once):
        self.prepare_once, self.command, self.error = prepare_once, None, None

    def get(self):
        if self.command is None and self.error is None:
            try:
                self.command = self.prepare_once()
            except (Stop, OSError) as err:
                self.error = str(err) if isinstance(err, Stop) else type(err).__name__
        if self.error is not None:
            raise Stop(f"the verified release: {self.error}")
        return self.command


def run_release(key, official, hidane, work, out, root=ROOT, prepare_harness=prepare):
    """Runs every unit of the release's harness on both emulators; the result for sdk.json."""
    ecosystem, name, version = parse_key(key)
    harness = HARNESSES[f"{ecosystem}:{name}"]
    watch_list = watch.load_watch(root / watch.WATCH)
    watched = {f"{p['ecosystem']}:{p['name']}": p["version"] for p in watch_list["packages"]}
    pinned = watch_list["emulator"]["version"]
    verified = watched[f"{ecosystem}:{name}"]
    result = {"key": key, "ecosystem": ecosystem, "name": name, "version": version,
              "harness": harness, "verified": verified, "prepared": False, "error": None,
              "units": []}
    logs = out / "logs"
    transcripts = out / "transcripts"
    for directory in (work, logs, transcripts):
        directory.mkdir(parents=True, exist_ok=True)
    try:
        (work / "release").mkdir(exist_ok=True)
        command = prepare_harness(harness, name, version, work / "release", logs, watched, root)
    except (Stop, OSError) as err:
        result["error"] = str(err) if isinstance(err, Stop) else type(err).__name__
        return result
    result["prepared"] = True
    control = None
    if version != verified:
        def prepare_control():
            (work / "verified").mkdir(exist_ok=True)
            return prepare_harness(harness, name, verified, work / "verified", logs, watched, root)
        control = Control(prepare_control)
    for unit, name_in_results in units(harness, name, root):
        entry = {"unit": unit, "transcript": name_in_results}
        try:
            entry.update(run_unit(unit, name_in_results, command, control, official, hidane,
                                  logs, transcripts, pinned, root))
        except (Stop, OSError, subprocess.CalledProcessError) as err:
            entry.update(result=ERROR, why=str(err) if isinstance(err, Stop)
                         else type(err).__name__)
        result["units"].append(entry)
    return result


def run_pair(unit, name, command, official, hidane, logs, transcripts):
    """A unit on a fresh official emulator and a fresh hidane: (official, hidane)."""
    args, cwd = command(unit)
    found = []
    for emulator, side in ((official, "official"), (hidane, "hidane")):
        data, why = transcript(emulator, side, name, args, cwd, logs)
        if data is None:
            raise Stop(f"{side}: {why}")
        (transcripts / f"{name}-{side}.json").write_text(
            json.dumps(data, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
        found.append(data)
    return tuple(found)


def run_unit(unit, name, command, control, official, hidane, logs, transcripts, pinned, root):
    committed_pair = committed(name, pinned, root)
    allowed = set(differences(*committed_pair))
    pair = run_pair(unit, name, command, official, hidane, logs, transcripts)
    found = differences(*pair)
    outcome = {"differences": len(found),
               "official_changed": bool(differences(pair[0], committed_pair[0]))}
    if not found:
        return dict(outcome, result=IDENTICAL)
    places = set()
    left = unexplained(pair, allowed)
    if left and control is not None:
        # The verified release, the same way on the same runner. Where it differs too, the step
        # depends on the runner, and its values with it (a count of attempts, 5 in one run and 4
        # in the next): the place explains a difference, whatever the values.
        verified = run_pair(unit, f"{name}.verified", control.get(), official, hidane, logs,
                            transcripts)
        places = {path for path, _, _ in differences(*verified)}
        left = unexplained(pair, allowed, places)
        outcome["explained_by_verified_run"] = not left
    if not left:
        return dict(outcome, result=DOCUMENTED)
    # Unexplained differences count once the same ones come back.
    again = run_pair(unit, f"{name}.again", command, official, hidane, logs, transcripts)
    confirmed = unexplained(again, allowed, places) == left
    return dict(outcome, result=DIFFERENT if confirmed else UNSTABLE,
                paths=[path for path, _, _ in left][:20])


def write_outputs(path, runs):
    with open(path, "a", encoding="utf-8") as f:
        f.write(f"sdk_runs={json.dumps(runs, separators=(',', ':'))}\n")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("plan")
    p.add_argument("--upstream", required=True)
    p.add_argument("--outputs")
    p = sub.add_parser("run")
    for flag in ("--key", "--jar", "--java", "--hidane", "--work", "--out"):
        p.add_argument(flag, required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == "plan":
            with open(args.upstream, encoding="utf-8") as f:
                runs = plan(json.load(f))
            if args.outputs:
                write_outputs(args.outputs, runs)
            print(json.dumps(runs, indent=1))
            return 0
        out = Path(args.out)
        logs = out / "logs"
        logs.mkdir(parents=True, exist_ok=True)
        parse_key(args.key)
        pinned = watch.load_watch()["emulator"]["version"]
        result = run_release(args.key, Official(args.jar, args.java, pinned, logs),
                             Hidane(args.hidane, logs), Path(args.work), out)
        with open(out / "sdk.json", "w", encoding="utf-8") as f:
            json.dump(result, f, indent=1, sort_keys=True)
            f.write("\n")
        print(json.dumps(result, indent=1, sort_keys=True))
        return 0
    except Stop as err:
        print(f"error: {err}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
