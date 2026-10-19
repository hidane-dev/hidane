"""Replays recordings of how the official emulator compiles Security Rules, and of which documents
their `match` paths cover, against the production Rules API (#82).

`replay.py` hands these recordings here. Each ruleset is tested with the Rules API's
`projects.test` (`rules_tests`, as the conditions of `rules_eval.json` are), and production's
answer is put as the recording put the official emulator's:

- `rules_compile.json` (`tools/oracle/rules_compile.py`): rulesets the official emulator loaded
  with its own `PUT /emulator/v1/projects/{p}:securityRules`. Each is tested without a test
  suite, which compiles it. Where the official emulator refused it (`400`, "Error compiling
  rules:" and a line `L<line>:<column> <description>` for each issue), production's issues, one
  of which is an `ERROR`, are put in the same lines; where it compiled, its warnings (`issues`)
  are compared with production's, positions and all. A case of the emulator's request is sent
  with the request's `rules` as the source, if they hold one file: the Rules API tests one
  ("Currently only one file source can be passed to firebase rules.").
- `rules_warnings.json` (`tools/oracle/rules_warnings.py`): the same, each warning as the recorder
  put it, `[line, column, currentOffset, endOffset, severity, description]`.
- `rules_paths.json` (`tools/oracle/rules_paths.py`): each ruleset is tested with a `get` of each
  of the recording's documents without `auth`, as the recorder read them over REST; whether each
  read is allowed is compared, and the warnings' descriptions.

Differences the parity exceptions record count as known (`tally`, `known_rules`). Stdlib only.
"""

from replay import first_difference, known_rules, mask, parsed, rules_tests, same_json, tally

# How the official emulator begins its answer to a ruleset it does not compile; a line follows for
# each issue, `L<line>:<column> <description>`.
COMPILE_ERROR = "Error compiling rules:"
# A test case's state, for a read expected to be allowed.
READS = {"SUCCESS": "allow", "FAILURE": "deny"}


def source(content):
    """A ruleset as the Rules API takes it, in the file the recorders name."""
    return {"files": [{"name": "firestore.rules", "content": content}]}


def compile_failure(message):
    """The official emulator's message for a ruleset it did not compile, as it is compared: its
    lines, one for each issue."""
    if isinstance(message, str) and message.startswith(COMPILE_ERROR + "\n"):
        return {"outcome": "compile", "errors": message.split("\n")[1:]}
    return {"outcome": "error", "error": message}


def official_compiled(status, answer):
    """The official emulator's answer to a ruleset it was sent (`rules_compile.json`), as it is
    compared: the lines of its message, if it did not compile it; its warnings, if it did; or the
    error it answered."""
    error = answer.get("error") if isinstance(answer, dict) else None
    message = error.get("message") if isinstance(error, dict) else None
    if status == 400 and isinstance(message, str) and message.startswith(COMPILE_ERROR + "\n"):
        return compile_failure(message)
    if status == 200 and isinstance(answer, dict):
        return {"outcome": "compiled", "issues": answer.get("issues", [])}
    return {"outcome": str(status), "error": error if error is not None else answer}


def production_compiled(status, text, issue=lambda i: i):
    """The Rules API's answer to a ruleset, as the official emulator's is compared. If an issue is
    an `ERROR`, every issue, in the lines of the official emulator's message (a position it lacks
    as `L0:0`); else the warnings, each as `issue` puts it; or the error it answered, for a request
    it refused."""
    answer = parsed(text)
    if status != 200 or not isinstance(answer, dict):
        return {"outcome": str(status), "error": answer.get("error", answer) if isinstance(answer, dict) else answer}
    issues = answer.get("issues", [])
    if any(i.get("severity") == "ERROR" for i in issues):
        message = COMPILE_ERROR
        for i in issues:
            position = i.get("sourcePosition") or {}
            message += f"\nL{position.get('line', 0)}:{position.get('column', 0)} {i.get('description', '')}"
        # Split as the official emulator's: a description that holds a line break breaks there too.
        return compile_failure(message)
    return {"outcome": "compiled", "issues": [issue(i) for i in issues]}


def warning(issue):
    """A warning as `rules_warnings.json` puts it."""
    position = issue.get("sourcePosition") or {}
    return [position.get("line"), position.get("column"), position.get("currentOffset"), position.get("endOffset"),
            issue.get("severity"), issue.get("description")]


def compare(result, name, official, answered, content, text, rules):
    """Counts a case: the same, known (`tally`, or `known_rules` for what the Rules API does
    differently with the ruleset `content`, answering `text`), or different, with what the report
    shows of it: the ruleset or condition (`rules`), and where production's answer first
    differs."""
    if not same_json(official, answered) and known_rules(result, content, text):
        return
    tally(result, official, answered, lambda: {
        "name": name, "method": "POST", "path": "/v1/projects/{project}:test",
        "difference": first_difference(official, answered) or "the outcome",
        "official": {"status": official["outcome"], **official},
        "production": {"status": answered["outcome"], **answered}, "rules": rules}, name)


def replay_compile(production, recording, result):
    """`rules_compile.json`: each ruleset tested without a test suite, and compared as
    `official_compiled` and `production_compiled` put the answers. A case of the emulator's own
    request is sent with its `rules` as the source and the rest as it is (an unknown field), if its
    rules hold one file; those that hold several, none, or are not JSON are not sent."""
    tested, bodies = [], []
    for case in recording:
        if "content" in case:
            body = {"source": source(case["content"])}
        else:
            request = case.get("request")
            rules = request.get("rules") if isinstance(request, dict) else None
            files = rules.get("files") if isinstance(rules, dict) else None
            if not isinstance(files, list) or not files or not all(isinstance(f, dict) for f in files):
                why = "the emulator's own request"
            elif len(files) > 1:
                why = "several files: the Rules API tests one"
            else:
                why = None
            if why:
                result["skipped"][why] = result["skipped"].get(why, 0) + 1
                continue
            body = {("source" if k == "rules" else k): v for k, v in request.items()}
        tested.append(case)
        bodies.append(body)
    for case, body, (status, text) in zip(tested, bodies, rules_tests(production, bodies)):
        result["sent"] += 1
        compare(result, case["name"], official_compiled(case["status"], case["response"]),
                production_compiled(status, mask(text, production.project)),
                str(body["source"]["files"][0].get("content")), text,
                case["content"] if "content" in case else case["request"])


def replay_warnings(production, recording, result):
    """`rules_warnings.json`: each condition, in the recording's ruleset, or each whole ruleset,
    tested without a test suite; its warnings are compared as the recorder put them, positions
    and all, or the lines of its errors."""
    cases = recording["results"]
    sources = [case.get("rules") or recording["rules"].replace("%s", case["condition"], 1) for case in cases]
    answers = rules_tests(production, [{"source": source(s)} for s in sources])
    for case, content, (status, text) in zip(cases, sources, answers):
        result["sent"] += 1
        if "error" in case:
            official = compile_failure(case["error"])
        else:
            official = {"outcome": "compiled", "issues": case.get("issues", [])}
        compare(result, case["name"], official, production_compiled(status, mask(text, production.project), warning),
                content, text, case.get("condition") or content)


def paths_ruleset(version, nested, condition):
    """The ruleset `tools/oracle/rules_paths.py` loads: `allow read: if <condition>;` in the
    `match` paths, outermost first, under the documents of rules version 1 or 2."""
    lines = [] if version == 1 else ["rules_version = '2';"]
    lines += ["service cloud.firestore {", "  match /databases/{database}/documents {"]
    indent = "    "
    for path in nested:
        lines.append(f"{indent}match {path} {{")
        indent += "  "
    lines.append(f"{indent}allow read: if {condition};")
    for _ in nested:
        indent = indent[:-2]
        lines.append(f"{indent}}}")
    lines += ["  }", "}"]
    return "\n".join(lines) + "\n"


def replay_paths(production, recording, result):
    """`rules_paths.json`: each ruleset tested with a `get` of each of the recording's documents,
    without `auth`. Whether each is allowed is compared, with the descriptions of the warnings, or
    the lines of the ruleset's errors."""
    documents, cases = recording["documents"], recording["results"]
    suite = {"testCases": [{"expectation": "ALLOW", "request": {
        "auth": None, "method": "get", "path": f"/databases/(default)/documents/{document}"}} for document in documents]}
    sources = [paths_ruleset(case["version"], case["match"], case["condition"]) for case in cases]
    answers = rules_tests(production, [{"source": source(s), "testSuite": suite} for s in sources])
    for case, content, (status, text) in zip(cases, sources, answers):
        result["sent"] += 1
        if "compile" in case:
            official = compile_failure(case["compile"].get("message"))
        else:
            official = {"outcome": "compiled", "issues": case.get("issues", []), "reads": case["reads"]}
        answered = production_compiled(status, mask(text, production.project), lambda i: i.get("description"))
        if answered["outcome"] == "compiled":
            states = [r.get("state") for r in parsed(text).get("testResults", [])]
            answered["reads"] = {d: READS.get(s, str(s)) for d, s in zip(documents, states)}
        compare(result, f"version {case['version']}, {' '.join(case['match'])}: {case['condition']}", official,
                answered, content, text, content)
