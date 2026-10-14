#!/usr/bin/env python3
"""What does the official emulator report of a decision that reaches the limit of 1,000 expressions? (#293)

Usage (from the repository root), with a freshly started jar under an English locale whose
standard output goes to a file, as `rules_coverage.py` takes it:
    (cd /tmp && java -Duser.language=en -Duser.country=US -jar …/cloud-firestore-emulator-v1.22.0.jar \\
        --host 127.0.0.1 --port 8086 > emulator.out) &
    python3 -I tools/oracle/rules_expressions_report.py 127.0.0.1:8086 /tmp/emulator.out \\
        > crates/hidane/tests/fixtures/rules_expressions_report.json

Each step keeps what the emulator printed while it ran (`log`), and the steps that read the
coverage report keep its body as sent (`raw`). Conditions cost what `cost(n)` says, through
functions (`rules_expressions.py`); they read no document, so the official emulator evaluates
each once. The cases:

- `debug()`: for each place of a `debug()` call in a condition that begins with `cost(K)`, the
  script finds the largest K that does not reach the limit, and the largest K for which the
  call still prints, and records the read at both and at one more. An expression counts once
  what is in it is evaluated (a call before the function runs), so where the call prints shows
  when each expression counts: operands the last first, the right side of `&&` and `||` after
  one count for the left, a conditional's branch after one count for its condition (two when
  it is false), `is` after one for its type, a `let` after its value, a function's body after
  its arguments, a path's parts the last first;
- coverage reports after a decision that reaches the limit at different places (in a list, in a
  function, between the sides of `&&`, in a conditional, in a `let`, in a second statement):
  what was evaluated before has its values, what goes past the limit and what holds it has none.

Reads are made over REST without an `Authorization` header. Stdlib only.
"""

import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import steps  # noqa: E402
from steps import call, case, dump, rules  # noqa: E402

steps.LOG = sys.argv[2]
# The answers to the rulesets (their warnings) are not kept; the tests compare their statuses.
steps.KEEP_OK = False
E = "/emulator/v1/projects"
LIMIT = "maximum of 1000 expressions"
# Functions whose calls cost 2, 7, 17, 37, 77, 157, 317 and 637 expressions, as in
# rules_expressions.py.
COSTS = [2]
for _ in range(7):
    COSTS.append(2 * COSTS[-1] + 3)
COST_FUNCTIONS = "function d0() { return true; }\n" + "\n".join(
    f"function d{k}() {{ return d{k - 1}() && d{k - 1}(); }}" for k in range(1, len(COSTS)))
FUNCTIONS = """function f(a) { return a; }
function g(a) { return debug(a); }
function h() { let x = debug(7); return true; }
function k(a, b) { return true; }
function m() { let x = 1; let y = debug(7); return true; }"""


def cost(n, true=True):
    """An expression that costs `n` (at least 1, or 2 when false), true or false."""
    if not true:
        return f"!({cost(n - 1)})"
    if n <= 4:
        return ["true", "!false", "!!true", "!!!false"][n - 1]
    k = max(k for k, c in enumerate(COSTS) if c + 3 <= n)
    return f"d{k}() && {cost(n - COSTS[k] - 2)}"


def ruleset(body):
    indent = lambda text: "\n".join("    " + line if line else "" for line in text.split("\n"))
    return ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
            + indent(COST_FUNCTIONS + "\n" + FUNCTIONS) + "\n" + indent(body) + "\n  }\n}\n")


def docs(project):
    return f"/v1/projects/{project}/databases/(default)/documents"


def read(path):
    return call("GET", path, raw=True)


PROJECT = "report-debug"


def decide(condition, record):
    """Loads `allow get: if condition` and reads a missing document: whether the decision reached
    the limit, and whether anything was printed."""
    content = ruleset(f"match /c/{{id}} {{\n  allow get: if {condition};\n}}")
    if record:
        rules(PROJECT, content)
    else:
        status, text = call("PUT", f"{E}/{PROJECT}:securityRules",
                            {"rules": {"files": [{"name": "firestore.rules", "content": content}]}},
                            record=False)
        assert status == 200, text
    since = os.path.getsize(steps.LOG)
    _, text = call("GET", f"{docs(PROJECT)}/c/x", record=record)
    time.sleep(0.15)
    with open(steps.LOG, "rb") as f:
        f.seek(since)
        printed = f.read()
    return LIMIT in text, bool(printed.strip())


SHAPES = [
    "X && debug(true)",
    "X && debug(7) == 7",
    "X && 7 == debug(7)",
    "X && debug(debug(7)) == 7",
    "X && debug(1 + 1) == 2",
    "X && -debug(7) == -7",
    "X && debug(7) in [7]",
    "X && [debug(7)] == [7]",
    "X && {'a': debug(7)}.a == 7",
    "X && debug('abc')[0:1] == 'a'",
    "X && 'abc'[debug(0):1] == 'a'",
    "X && [7][debug(0)] == 7",
    "X && debug([7])[0] == 7",
    "X && 'a'.matches(debug('a'))",
    "X && debug('a').matches('a')",
    "X && math.abs(debug(-7)) == 7",
    "X && (debug(true) && true)",
    "X && (debug(true) || false)",
    "X && (false || debug(true))",
    "X && (true ? debug(7) : 0) == 7",
    "X && (false ? 0 : debug(7)) == 7",
    "X && (debug(true) ? true : false)",
    "X && debug(7) is int",
    "X && f(debug(7)) == 7",
    "X && g(7) == 7",
    "X && k(1, debug(2))",
    "X && h()",
    "X && m()",
    "X && /a/$(debug('b'))/c != null",
]
for shape in SHAPES:
    at = lambda k, record=False, shape=shape: decide(shape.replace("X", cost(k)), record)
    lo, hi = 5, 1000
    assert not at(lo)[0] and at(hi)[0], shape
    while hi - lo > 1:
        mid = (lo + hi) // 2
        lo, hi = (mid, hi) if not at(mid)[0] else (lo, mid)
    printed = lo
    while at(printed + 1)[1]:
        printed += 1
    case(f"debug: {shape} (X costs at most {lo}; it prints up to {printed})")
    for k in sorted({lo, printed, printed + 1}):
        at(k, True)


def report(name, project, body, paths=("/c/x",)):
    """Loads `body`, reads each of `paths`, then reads the coverage report."""
    case(f"coverage: {name}", project=project)
    rules(project, ruleset(body))
    for path in paths:
        call("GET", f"{docs(project)}{path}")
    read(f"{E}/{project}:ruleCoverage")


# 1,000 expressions, then the 1,001st at each place: `cost(n) && LIST && debug(7) == 7` costs
# n + 15, its list's second item is the n + 4th expression to count, the `&&` that ends the list
# the n + 9th, the second `&&`'s left side the n + 10th.
LIST = "[1, 1, 1].size() > 0"
CONDITION = lambda n: f"{cost(n)} && {LIST} && debug(7) == 7"
report("1000 expressions", "report-a", f"match /c/{{id}} {{\n  allow get: if {CONDITION(985)};\n}}")
report("past the limit in a list", "report-b", f"match /c/{{id}} {{\n  allow get: if {CONDITION(997)};\n}}")
report("past the limit at the end of &&", "report-c", f"match /c/{{id}} {{\n  allow get: if {CONDITION(992)};\n}}")
report("past the limit between the sides of &&", "report-d",
       f"match /c/{{id}} {{\n  allow get: if {CONDITION(991)};\n}}")
# `d1()` costs 7, its second `d0()` call the 5th.
report("past the limit in a function's body", "report-e",
       f"match /c/{{id}} {{\n  allow get: if {cost(995)} && d1() && debug(7) == 7;\n}}")
# The condition's `debug(1)` is the n + 4th, the branch's `debug(2)` the n + 9th.
report("past the limit in a conditional's branch", "report-f",
       f"match /c/{{id}} {{\n  allow get: if {cost(992)} && (debug(1) == 1 ? debug(2) == 2 : false);\n}}")
# `m()`'s second `let` counts after its `debug(7)`, the n + 7th.
report("past the limit at a let", "report-g", f"match /c/{{id}} {{\n  allow get: if {cost(994)} && m();\n}}")
report("past the limit in a second statement", "report-h",
       f"match /c/{{id}} {{\n  allow get: if {cost(600, False)};\n  allow get: if {CONDITION(397)};\n"
       f"  allow get: if debug(true);\n}}")
# For x, `debug()` is the n + 10th; y is allowed by the left side.
report("past the limit, then an allowed read", "report-i",
       f"match /c/{{id}} {{\n  allow get: if id == 'y' || {cost(991)} && debug([1, 2]).size() == 2;\n}}",
       paths=("/c/x", "/c/y"))

dump()
