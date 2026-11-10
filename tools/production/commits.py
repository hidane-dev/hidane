"""Replays the official emulator's recording of how a commit's writes are checked
(`commit_checks.json`, `tools/oracle/commit_checks.py`) against production Cloud Firestore (#82).

`replay.py` hands the recording here. Each case of `order` is a commit with problems, which the
recorder sent over REST `:commit`, gRPC `Commit`, REST `:batchWrite` and the Write stream, each
in a project of its own (`<project>-<transport>`). The two REST ones are sent here, to the
project's `(default)` database; production's gRPC services take the same writes, and are not
sent. What each answers is put as the recorder put it and compared: the code and message of the
first error, the number of write results, or `:batchWrite`'s status for each write. A failed
precondition is compared on its code alone: the official emulator's message prints its internal
key. The database is emptied after a write goes through, so that each commit meets an empty one,
as each did in its own project.

Production stores an array in an array (D119): where the official emulator refused one first,
production's answer stands, and counts as known. So does its refusal of a document over 1 MiB,
which it makes with a write's other checks, write by write and before reading any precondition,
where the official emulator refused something else first (D075). `other_database`, writes to a
document of another database or project, is not sent: the project has no other. A `:batchWrite`
whose every write production refused for the bandwidth the database allows wrote nothing, and is
waited out and sent again, as `Production.send` does a 429. Stdlib only.
"""

import json
import re
import time

from replay import NESTED_ARRAYS, THROTTLED_WAITS, first_difference, log, mask, parsed, tally

# The transports sent, as the recording names them, and their verbs.
VERBS = {"commit": "commit", "batch_write": "batchWrite"}
# `{repeat:<c>:<n>}` stands for `<c>` repeated `<n>` times.
REPEAT = re.compile(r"\{repeat:(.+?):(\d+)\}")
# The codes of a failed precondition, whose messages are not compared.
PRECONDITION = {"NOT_FOUND", "ALREADY_EXISTS"}
# Production's refusal of a document over 1 MiB (D075).
TOO_LARGE = re.compile(r"^Document '.*' cannot be written because its size \([\d,]+ bytes\) exceeds ")
# Production's refusal of a write of a database written faster than it allows.
BANDWIDTH = "exceeded their maximum bandwidth for writes"
# The names of the codes of `google.rpc.Status`, by number.
CODES = ["OK", "CANCELLED", "UNKNOWN", "INVALID_ARGUMENT", "DEADLINE_EXCEEDED", "NOT_FOUND", "ALREADY_EXISTS",
         "PERMISSION_DENIED", "RESOURCE_EXHAUSTED", "FAILED_PRECONDITION", "ABORTED", "OUT_OF_RANGE",
         "UNIMPLEMENTED", "INTERNAL", "UNAVAILABLE", "DATA_LOSS", "UNAUTHENTICATED"]


def expand(text):
    return REPEAT.sub(lambda m: m.group(1) * int(m.group(2)), text)


def outcome(status, text):
    """A REST answer as the recorder summed it up: an error's status, code and message; or
    `:batchWrite`'s status for each write; or the number of write results."""
    body = parsed(text)
    if status != 200:
        error = body.get("error") if isinstance(body, dict) and isinstance(body.get("error"), dict) else {}
        return {"status": status, "code": error.get("status"), "message": error.get("message")}
    if isinstance(body, dict) and isinstance(body.get("status"), list):
        return {"status": status, "code": "OK", "statuses": [
            {"code": CODES[s.get("code", 0)] if 0 <= s.get("code", 0) < len(CODES) else str(s.get("code")),
             "message": s.get("message", "")} for s in body["status"]]}
    return {"status": status, "code": "OK", "results": len(body.get("writeResults", [])) if isinstance(body, dict) else 0}


def throttled(answered):
    """Whether production refused every write of a `:batchWrite` for the database's bandwidth:
    then none was written."""
    statuses = answered.get("statuses") or []
    return bool(statuses) and all(s["code"] == "RESOURCE_EXHAUSTED" and BANDWIDTH in s["message"] for s in statuses)


def compared(outcome):
    """An outcome as it is compared, its errors shaped as production's (`code`, `status`,
    `message`), so that the parity exceptions' pairs of errors apply; a failed precondition
    without its message."""
    def error(code, message):
        return {"status": code} if code in PRECONDITION else {"status": code, "message": message}
    shaped = {"code": outcome["status"], **error(outcome["code"], outcome.get("message"))}
    if outcome["code"] == "OK":
        shaped.pop("message", None)
    if "statuses" in outcome:
        shaped["statuses"] = [error(s["code"], s["message"]) for s in outcome["statuses"]]
    if "results" in outcome:
        shaped["results"] = outcome["results"]
    return shaped


def set_aside(theirs, ours, known):
    """Where the official emulator refused an array in an array, which production stores (D119),
    or refused anything while production refused a document over 1 MiB, which it checks earlier
    (D075), production's answer stands; its entry is added to `known`."""
    if "statuses" in theirs and len(theirs["statuses"]) == len(ours.get("statuses", [])):
        statuses = []
        for t, o in zip(theirs["statuses"], ours["statuses"]):
            if NESTED_ARRAYS in str(t.get("message")) and NESTED_ARRAYS not in str(o.get("message")):
                known.add("D119")
                t = o
            statuses.append(t)
        return {**theirs, "statuses": statuses}
    if NESTED_ARRAYS in str(theirs.get("message")) and NESTED_ARRAYS not in str(ours.get("message")):
        known.add("D119")
        return ours
    if TOO_LARGE.search(str(ours.get("message"))) and (
            theirs["status"] != "OK" or any(s["status"] != "OK" for s in theirs.get("statuses", []))):
        known.add("D075")
        return ours
    return theirs


def replay_commit_checks(production, recording, result):
    project = production.project
    documents = f"projects/{project}/databases/(default)/documents"
    for n, case in enumerate(recording["order"], 1):
        if n % 100 == 0:
            log(f"{result['fixture']}: case {n}, {production.sent} requests so far")
        for transport, recorded in case["outcomes"].items():
            if transport not in VERBS:
                result["skipped"]["over gRPC"] = result["skipped"].get("over gRPC", 0) + 1
                continue
            # The recorder's project for this transport, in its messages, is the project's here.
            own = f"{case['project']}-{transport.replace('_', '-')}"
            official = json.loads(expand(json.dumps(recorded)).replace(own, "{project}"))
            writes = expand(json.dumps(case["writes"]).replace("{base}", documents))
            path = f"/v1/{documents}:{VERBS[transport]}"
            name = f"{case['name']} / {transport}"
            for wait in (*THROTTLED_WAITS, None):
                status, _, text = production.send("POST", path, f'{{"writes": {writes}}}'.encode())
                answered = outcome(status, mask(text, project))
                if wait is None or not throttled(answered):
                    break
                log(f"{name}: every write refused for the database's bandwidth, sending it again in {wait} s")
                time.sleep(wait)
            result["sent"] += 1
            if status == 200 and any(s["code"] == "OK" for s in answered.get("statuses", [answered])):
                production.empty()
            known = set()
            theirs = set_aside(compared(official), compared(answered), known)
            tally(result, theirs, compared(answered), lambda: {
                "name": name, "method": "POST", "path": path.replace(project, "{project}"),
                "difference": first_difference(theirs, compared(answered)) or "the answer",
                "official": official, "production": answered}, name, known)
    other = recording.get("other_database", [])
    if other:
        result["skipped"]["another database's or project's"] = len(other)
