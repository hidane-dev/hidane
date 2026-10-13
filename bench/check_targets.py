"""Checks a `hidane benchmark` result against hidane's targets (#47).

Usage (from the repository root):
    python3 -I bench/check_targets.py bench/targets.json result.json   # Markdown on stdout
    python3 -I bench/check_targets.py --self-test

Compares hidane's median of each metric `bench/targets.json` names with its bound, prints a
Markdown table of them for a job summary, and exits 1 when a median is not below its bound, a
metric is missing, or a run failed. Medians only, as the claim policy has it (bench/README.md);
the bounds are the targets of docs/ROADMAP.md §1, not hidane's current values, so a GitHub-hosted
runner's noise does not cross them. Stdlib only.
"""

import json
import sys


def value(median, unit):
    if unit == "bytes":
        return f"{median / (1024 * 1024):.1f} MiB"
    return f"{median:.2f} {unit}"


def check(targets, result):
    """The Markdown report and whether every target holds."""
    lines = [
        "| Target | Median | Bound | Runs | |",
        "|---|---:|---:|---:|---|",
    ]
    ok = True
    for t in targets["targets"]:
        metric = (
            result.get("results", {})
            .get(t["scenario"], {})
            .get("hidane", {})
            .get(t["metric"])
        )
        bound = value(t["below"], t["unit"])
        if metric is None:
            ok = False
            lines.append(f"| {t['what']} | missing | < {bound} | 0 | ✗ |")
            continue
        stats = metric["stats"]
        holds = metric["unit"] == t["unit"] and stats["median"] < t["below"]
        ok = ok and holds
        lines.append(
            f"| {t['what']} (`{t['scenario']}.{t['metric']}`) | "
            f"{value(stats['median'], metric['unit'])} | < {bound} | {stats['n']} | "
            f"{'✓' if holds else '✗'} |"
        )
    errors = [e for e in result.get("errors", []) if e.get("target") == "hidane"]
    for e in errors:
        ok = False
        lines.append(
            f"\nRun {e['run']} of `{e['scenario']}` failed: {e['message']}"
        )
    machine = result.get("machine", {})
    stability = result.get("stability", {})
    lines.append(
        "\nMedians of hidane's measured runs on "
        f"`{machine.get('id') or 'this machine'}` ({machine.get('cpu_model') or 'unknown CPU'}, "
        f"{machine.get('cpu_threads') or '?'} threads); the run was "
        f"{'stable' if stability.get('stable') else 'not stable'} by the benchmark's checks. A "
        "check of the targets, not a number to cite (bench/README.md, Claim policy)."
    )
    return "\n".join(lines) + "\n", ok


def self_test():
    targets = {
        "targets": [
            {"scenario": "startup", "metric": "time_to_port", "below": 100, "unit": "ms",
             "what": "Time until the port accepts"},
            {"scenario": "memory", "metric": "rss_idle", "below": 50 << 20, "unit": "bytes",
             "what": "RSS idle"},
        ]
    }

    def result(port_ms, rss, errors=()):
        stat = lambda m, unit: {"unit": unit, "stats": {"median": m, "n": 10}}
        return {
            "results": {
                "startup": {"hidane": {"time_to_port": stat(port_ms, "ms")}},
                "memory": {"hidane": {"rss_idle": stat(rss, "bytes")}},
            },
            "errors": list(errors),
            "stability": {"stable": True},
            "machine": {"cpu_model": "CPU", "cpu_threads": 4},
        }

    text, ok = check(targets, result(1.6, 8 << 20))
    assert ok and "| 1.60 ms | < 100.00 ms | 10 | ✓ |" in text, text
    assert "| 8.0 MiB | < 50.0 MiB | 10 | ✓ |" in text, text
    # At the bound is a crossing.
    assert not check(targets, result(100.0, 8 << 20))[1]
    assert not check(targets, result(1.6, 50 << 20))[1]
    missing = result(1.6, 8 << 20)
    del missing["results"]["memory"]
    text, ok = check(targets, missing)
    assert not ok and "| RSS idle | missing |" in text, text
    failed = result(1.6, 8 << 20, [{"scenario": "startup", "target": "hidane", "run": 2,
                                    "warmup": False, "message": "no port"}])
    text, ok = check(targets, failed)
    assert not ok and "Run 2 of `startup` failed: no port" in text, text
    # The official emulator's failures are not hidane's.
    other = result(1.6, 8 << 20, [{"scenario": "startup", "target": "official", "run": 1,
                                   "warmup": False, "message": "x"}])
    assert check(targets, other)[1]
    print("check_targets.py: self-test passed")


def main(argv):
    if argv == ["--self-test"]:
        self_test()
        return 0
    if len(argv) != 2:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    with open(argv[0], encoding="utf-8") as f:
        targets = json.load(f)
    with open(argv[1], encoding="utf-8") as f:
        result = json.load(f)
    text, ok = check(targets, result)
    sys.stdout.write(text)
    if not ok:
        print("check_targets.py: a target was crossed or a run failed", file=sys.stderr)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
