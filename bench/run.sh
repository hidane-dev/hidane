#!/usr/bin/env bash
# The canonical benchmark: hidane and the official Cloud Firestore emulator v1.22.0, side by
# side on this machine, with the parameters in bench/canonical.json. See bench/README.md.
#
# Writes exactly two files into the output directory: <name>.json (the result, as
# bench/schema/result.schema.json describes it) and <name>.md (rendered from that JSON).
#
# Exit status: 0 when the run completed, stable or not (`stability.stable` in the JSON says
# which); 1 when it completed but some runs failed (both files are written; `errors` lists the
# runs); 2 when the benchmark could not run (no files are written).

set -euo pipefail

usage() {
  cat <<'EOF'
usage: bench/run.sh [options]

  --kind KIND              local (default), pull-request, nightly or release
  --machine-id ID          the machine's name in result histories
  --out DIR                output directory (default: bench/out)
  --name NAME              file name without extension (default: <UTC date>-<kind>)
  --quick                  a few runs of everything, to check the setup (not canonical)

  --engine PATH            the hidane binary that measures (`hidane benchmark`)
  --engine-commit SHA      the commit it was built from
  --hidane PATH            the hidane binary measured (default: the engine)
  --hidane-commit SHA      the commit it was built from
                           Without --engine, the engine is built from this checkout
                           (cargo build --release) and the commits are read from git.

  --official-jar PATH      the official emulator's jar (default: download the pinned
                           v1.22.0 jar into the cache and check its SHA-256)
  --cache-dir DIR          where the downloaded jar is kept (default: ~/.cache/hidane-bench)
  --java PATH              the Java 21+ that runs it (default: java on PATH)
  --firebase PATH          firebase-tools' firebase, for the ci_firebase scenario
                           (skipped, with the reason recorded, without it)

  --environment-file PATH  the operator's environment file (bench/README.md)
  --no-imds                do not ask the EC2 instance metadata service for the instance type
EOF
}

root=$(cd "$(dirname "$0")/.." && pwd)
kind=local machine_id="" out="$root/bench/out" name="" quick=0
engine="" engine_commit="" hidane="" hidane_commit=""
official_jar="" cache_dir="" java="" firebase="" environment_file="" no_imds=0

need() {
  if [ $# -lt 2 ] || [ -z "$2" ]; then
    echo "bench/run.sh: $1 needs a value" >&2
    exit 2
  fi
}
while [ $# -gt 0 ]; do
  case "$1" in
    --kind) need "$@"; kind=$2; shift 2 ;;
    --machine-id) need "$@"; machine_id=$2; shift 2 ;;
    --out) need "$@"; out=$2; shift 2 ;;
    --name) need "$@"; name=$2; shift 2 ;;
    --quick) quick=1; shift ;;
    --engine) need "$@"; engine=$2; shift 2 ;;
    --engine-commit) need "$@"; engine_commit=$2; shift 2 ;;
    --hidane) need "$@"; hidane=$2; shift 2 ;;
    --hidane-commit) need "$@"; hidane_commit=$2; shift 2 ;;
    --official-jar) need "$@"; official_jar=$2; shift 2 ;;
    --cache-dir) need "$@"; cache_dir=$2; shift 2 ;;
    --java) need "$@"; java=$2; shift 2 ;;
    --firebase) need "$@"; firebase=$2; shift 2 ;;
    --environment-file) need "$@"; environment_file=$2; shift 2 ;;
    --no-imds) no_imds=1; shift ;;
    -h | --help) usage; exit 0 ;;
    *) echo "bench/run.sh: unknown option $1" >&2; usage >&2; exit 2 ;;
  esac
done

case "$kind" in
  local | pull-request | nightly | release) ;;
  *) echo "bench/run.sh: --kind must be local, pull-request, nightly or release" >&2; exit 2 ;;
esac

# The engine: given, or built from this checkout.
dirty=0
if [ -z "$engine" ]; then
  if command -v cargo >/dev/null 2>&1; then
    echo "bench/run.sh: building hidane (release) from this checkout" >&2
    (cd "$root" && cargo build --release --locked -p hidane >&2) || exit 2
  fi
  engine="${CARGO_TARGET_DIR:-$root/target}/release/hidane"
  if [ ! -x "$engine" ]; then
    echo "bench/run.sh: no hidane binary at $engine; install Rust or pass --engine" >&2
    exit 2
  fi
  if [ -z "$engine_commit" ] && git -C "$root" rev-parse --git-dir >/dev/null 2>&1; then
    engine_commit=$(git -C "$root" rev-parse HEAD)
    if [ -n "$(git -C "$root" status --porcelain)" ]; then
      dirty=1
    fi
  fi
fi
# The hidane measured: given, or the engine itself.
if [ -z "$hidane" ]; then
  hidane=$engine
  hidane_commit=${hidane_commit:-$engine_commit}
else
  dirty=0
fi

mkdir -p "$out"
if [ -z "$name" ]; then
  name="$(date -u +%Y-%m-%d)-$kind"
  if [ -e "$out/$name.json" ] || [ -e "$out/$name.md" ]; then
    name="$name-$(date -u +%H%M%S)"
  fi
fi
json="$out/$name.json"
md="$out/$name.md"

args=(benchmark
  --config "$root/bench/canonical.json"
  --kind "$kind"
  --compare-official
  --hidane "$hidane"
  --format json
  --out "$json")
[ "$quick" = 1 ] && args+=(--quick)
[ -n "$machine_id" ] && args+=(--machine-id "$machine_id")
[ -n "$engine_commit" ] && args+=(--engine-commit "$engine_commit")
[ -n "$hidane_commit" ] && args+=(--hidane-commit "$hidane_commit")
[ "$dirty" = 1 ] && args+=(--hidane-dirty)
if [ -n "$official_jar" ]; then
  args+=(--official-jar "$official_jar")
else
  args+=(--official-download)
  [ -n "$cache_dir" ] && args+=(--cache-dir "$cache_dir")
fi
[ -n "$java" ] && args+=(--java "$java")
[ -n "$firebase" ] && args+=(--firebase "$firebase")
[ -n "$environment_file" ] && args+=(--environment-file "$environment_file")
[ "$no_imds" = 1 ] && args+=(--no-imds)

# The engine checks the machine before and after the run (load, memory, CPU steal and
# temperature, known background jobs) and marks the result `stable: false` with its reasons.
status=0
"$engine" "${args[@]}" || status=$?
if [ ! -s "$json" ]; then
  echo "bench/run.sh: the benchmark did not run (exit status $status)" >&2
  rm -f "$json"
  exit 2
fi
if ! "$engine" benchmark render "$json" >"$md"; then
  echo "bench/run.sh: could not render $json" >&2
  rm -f "$md"
  exit 2
fi
echo "bench/run.sh: wrote $json and $md" >&2
if [ "$status" -ne 0 ]; then
  echo "bench/run.sh: some runs failed; see \`errors\` in the result" >&2
  exit 1
fi
exit 0
