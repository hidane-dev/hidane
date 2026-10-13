"""Minimises a fuzz case that shows a difference, by delta debugging over its structure.

`minimize(case, reproduces)` keeps only what the difference needs: first the steps (ddmin over
the steps before the one that differs, the ones after it dropped), then each remaining step's
body, top down: list elements (ddmin), object keys one at a time, long strings cut in half. A
candidate is kept only when `reproduces(candidate)` says the same difference (the same
signature) still shows, so every case it returns still fails the same way. `budget` bounds how
many candidates it tries (each one runs on both emulators) and `deadline` when it stops trying.
Stdlib only.
"""

import copy
import time


class Tester:
    def __init__(self, reproduces, budget, deadline):
        self.reproduces = reproduces
        self.budget = budget
        self.deadline = deadline
        self.tests = 0

    def __call__(self, candidate):
        if self.tests >= self.budget or (self.deadline is not None and time.monotonic() > self.deadline):
            return False
        self.tests += 1
        return self.reproduces(candidate)


def ddmin(items, test, keep_empty=False):
    """Zeller's ddmin, by complements: the smallest sublist (by this search) that `test` accepts."""
    if keep_empty and items and test([]):
        return []
    n = 2
    while len(items) >= 2:
        size = len(items) // n
        chunks = [items[i:i + size] for i in range(0, len(items), size)] if size else [[x] for x in items]
        reduced = False
        for i in range(len(chunks)):
            complement = [x for j, chunk in enumerate(chunks) if j != i for x in chunk]
            if (complement or keep_empty) and test(complement):
                items = complement
                n = max(n - 1, 2)
                reduced = True
                break
        if not reduced:
            if n >= len(items):
                break
            n = min(n * 2, len(items))
    if len(items) == 1 and keep_empty and test([]):
        return []
    return items


def reduce_json(value, test):
    """A smaller value that `test` still accepts (`value` itself if none is found)."""
    if isinstance(value, list):
        value = ddmin(list(value), test, keep_empty=True)
        for i in range(len(value)):
            def at(v, i=i, base=value):
                return base[:i] + [v] + base[i + 1:]
            value = at(reduce_json(value[i], lambda v, at=at: test(at(v))))
        return value
    if isinstance(value, dict):
        for key in list(value):
            without = {k: v for k, v in value.items() if k != key}
            if test(without):
                value = without
        for key in list(value):
            def put(v, key=key, base=value):
                return {**base, key: v}
            value = put(reduce_json(value[key], lambda v, put=put: test(put(v))))
        return value
    if isinstance(value, str) and len(value) > 32 and "{" not in value:
        while len(value) > 32:
            shorter = value[:len(value) // 2]
            if not test(shorter):
                break
            value = shorter
    return value


def minimize(case, step, reproduces, budget=200, deadline=None):
    """The case reduced, and how many candidates were run. `step` is the step that differs."""
    tester = Tester(reproduces, budget, deadline)
    case = copy.deepcopy(case)
    steps = case["steps"][:step + 1]
    if step + 1 < len(case["steps"]) and not tester({**case, "steps": steps}):
        steps = case["steps"]
    # A smaller body can make an earlier step unneeded (a transaction that no read names any
    # more), so the passes repeat while they remove something.
    for _ in range(3):
        before = steps
        last, earlier = steps[-1], steps[:-1]
        earlier = ddmin(earlier, lambda candidate: tester({**case, "steps": candidate + [last]}), keep_empty=True)
        steps = earlier + [last]
        for n in range(len(steps)):
            if "body" not in steps[n]:
                continue

            def with_body(body, n=n):
                return {**case, "steps": steps[:n] + [{**steps[n], "body": body}] + steps[n + 1:]}
            body = reduce_json(steps[n]["body"], lambda b: tester(with_body(b)))
            steps = with_body(body)["steps"]
        if steps == before:
            break
    case["steps"] = steps
    return case, tester.tests
