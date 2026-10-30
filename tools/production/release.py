#!/usr/bin/env python3
"""Deploys Security Rules to the replay's project, and takes them down (#82).

The replay of recordings that load rules (`replay.py --deploy-rules`) releases each of their
rulesets to the project's Cloud Firestore and sends the recording's requests, those without
credentials as they were sent to the official emulator, for the deployed rules to decide. The
project is kept without a release, so that it refuses every request without credentials, and a
run leaves it so: it deletes the release and the rulesets it made when it ends, whatever
happened. Each ruleset it makes ends with `MARK`, so that a ruleset a stopped run left is known
and deleted. A release of rules it did not make is left alone, and the run stops.

Usage, to take down what a run left (the workflow does it after every run, whatever happened):
    PRODUCTION_ACCESS_TOKEN=… python3 -I -B tools/production/release.py --project <id>

Taking rulesets down needs `firebaserules.releases.delete` and `firebaserules.rulesets.delete`,
and deploying them `firebaserules.rulesets.create` and `firebaserules.releases.create` and
`update`. Stdlib only.
"""

import argparse
import json
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "oracle"))
sys.path.insert(0, str(Path(__file__).resolve().parent))  # `-I` leaves it out
from harness import mask  # noqa: E402

# The last line of each ruleset the replay makes.
MARK = "// hidane production parity\n"
RELEASE = "cloud.firestore"
# How long a new release is given before the replay sends requests it decides: on 2026-10-30, a
# new release decided reads from 1.3 to 2.2 s after it was made.
SETTLE = 5
TRANSIENT = {429, 500, 502, 503, 504}
# The Rules API allows a project 72 requests a minute that make, change or delete rulesets and
# releases; the replay sends at most this many of those and its reads of them.
PER_MINUTE = 60


class Rules:
    """The project's release of rules, and the rulesets the replay makes for it."""

    def __init__(self, production):
        self.production = production
        self.project = production.project
        self.released = False
        self.next = 0.0

    def api(self, method, path, body=None):
        """A request to the Rules API about the project, at most `PER_MINUTE` a minute, tried
        again while the API is busy, a minute later if it is the quota: (status, answer as JSON, or
        as text if it is not)."""
        data = None if body is None else json.dumps(body).encode()
        for attempt in range(5):
            time.sleep(max(0.0, self.next - time.monotonic()))
            self.next = time.monotonic() + 60 / PER_MINUTE
            status, _, text = self.production.send(method, f"/v1/projects/{self.project}{path}", data,
                                                   base=self.production.rules)
            if status not in TRANSIENT:
                break
            time.sleep(60 if status == 429 else 2 ** attempt)
        try:
            return status, json.loads(text) if text.strip() else {}
        except ValueError:
            return status, text

    def refused(self, what, status, answer):
        return RuntimeError(f"{what} answered {status}: {mask(json.dumps(answer), self.project)[:300]}")

    def ours(self, name):
        """Whether a ruleset (`projects/…/rulesets/<id>`) is one the replay made."""
        status, answer = self.api("GET", f"/rulesets/{name.rsplit('/', 1)[1]}")
        if status == 404:
            return False
        if status != 200:
            raise self.refused("reading a ruleset", status, answer)
        return any(f.get("content", "").endswith(MARK) for f in answer.get("source", {}).get("files", []))

    def deploy(self, source):
        """Makes a ruleset of `source` and releases it to the project's databases, then waits
        `SETTLE` seconds. Answers how making the ruleset went (status, answer): one that does not
        compile (400) is not released, and the release stays as it was; another refusal, such as
        the want of a permission, stops the run. The rulesets are deleted when the rules are taken
        down, not before: a server may still hold the one released before."""
        content = source + ("" if source.endswith("\n") else "\n") + MARK
        status, answer = self.api("POST", "/rulesets", {"source": {"files": [{"name": "firestore.rules", "content": content}]}})
        if status == 400:
            return status, answer
        if status != 200:
            raise self.refused("making a ruleset", status, answer)
        release = {"name": f"projects/{self.project}/releases/{RELEASE}", "rulesetName": answer["name"]}
        if self.released:
            done, said = self.api("PATCH", f"/releases/{RELEASE}", {"release": release})
        else:
            done, said = self.api("POST", "/releases", release)
        if done != 200:
            raise self.refused("releasing a ruleset", done, said)
        self.released = True
        time.sleep(SETTLE)
        return status, answer

    def delete(self, name):
        status, answer = self.api("DELETE", f"/rulesets/{name.rsplit('/', 1)[1]}")
        if status not in (200, 404):
            raise self.refused("deleting a ruleset", status, answer)

    def take_down(self):
        """Deletes the release, if the replay made it, and every ruleset the replay made: this
        run's, and those a stopped run left. A release of rules the replay did not make is left
        as it is, and stops the run. Answers whether there was a release, and how many rulesets
        went."""
        status, answer = self.api("GET", f"/releases/{RELEASE}")
        if status == 200:
            if not self.ours(answer["rulesetName"]):
                raise RuntimeError("the project has a release of rules the replay did not make; it is left as it is")
            status, answer = self.api("DELETE", f"/releases/{RELEASE}")
            if status not in (200, 404):
                raise self.refused("deleting the release", status, answer)
            released = True
        elif status == 404:
            released = False
        else:
            raise self.refused("reading the release", status, answer)
        self.released = False
        names, page = [], ""
        while True:
            status, answer = self.api("GET", "/rulesets?pageSize=100" + (f"&pageToken={page}" if page else ""))
            if status != 200:
                raise self.refused("listing the rulesets", status, answer)
            names += [r["name"] for r in answer.get("rulesets", []) if self.ours(r["name"])]
            page = answer.get("nextPageToken")
            if not page:
                break
        for name in names:
            self.delete(name)
        return released, len(names)


def main(argv=None):
    from replay import RULES_API, TOKEN_ENV, Production
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--project", required=True, help="the project of the maintainers' own")
    parser.add_argument("--rules-base", default=RULES_API)
    args = parser.parse_args(argv)
    token = os.environ.get(TOKEN_ENV)
    if not token:
        parser.error(f"{TOKEN_ENV} is not set")
    released, rulesets = Rules(Production("", args.project, token, 0, args.rules_base)).take_down()
    print(f"release {'deleted' if released else 'absent'}; {rulesets} rulesets of the replay's deleted")


if __name__ == "__main__":
    main()
