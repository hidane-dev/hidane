"""Real ID tokens for the users of recordings of rules (#82).

The official emulator takes a token it need not verify (`alg: none`) for a user's; production
takes only one its Auth issued. With `replay.py --id-tokens`, a recording's token whose payload is a
`sub` alone (and a `user_id` the same, if any) is sent as a real ID token for that uid: a custom
token, signed as the service account `--signer` with IAM's `signJwt` (the token needs
`iam.serviceAccounts.signJwt` on that account), is exchanged for one with Identity Toolkit's
`accounts:signInWithCustomToken`, which needs an API key of the project (`PRODUCTION_FIREBASE_API_KEY`).
The project's Auth has no sign-in method, so that nobody else can sign in. A token of any other
payload stands for what no real one does (an expired one, another project's), and its case is not
sent.

Each ID token is kept for the hour it lasts, made again `REMAKE_BEFORE` seconds before it ends, and
written nowhere, as the API key is not. Stdlib only.
"""

import base64
import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "oracle"))
from harness import mask  # noqa: E402

API_KEY_ENV = "PRODUCTION_FIREBASE_API_KEY"
IAM_CREDENTIALS = "https://iamcredentials.googleapis.com"
IDENTITY_TOOLKIT = "https://identitytoolkit.googleapis.com"
# The audience of a custom token (Firebase's "Create Custom Tokens").
AUDIENCE = "https://identitytoolkit.googleapis.com/google.identity.identitytoolkit.v1.IdentityToolkit"
# The longest a custom token may last, and how long before an ID token ends it is made again.
LIFETIME = 3600
REMAKE_BEFORE = 300
TRANSIENT = {429, 500, 502, 503, 504}


def unpadded(part):
    return base64.urlsafe_b64decode(part + "=" * (-len(part) % 4))


def uid(auth):
    """The uid a recording's token stands for, if a real one can: a JWT not signed, of a payload
    of a `sub` alone, or a `sub` and a `user_id` the same; or None."""
    if not isinstance(auth, str) or not auth.startswith("Bearer "):
        return None
    parts = auth[len("Bearer "):].split(".")
    try:
        header, payload = json.loads(unpadded(parts[0])), json.loads(unpadded(parts[1]))
    except (IndexError, ValueError):
        return None
    if len(parts) != 3 or parts[2] or header != {"alg": "none"} or not isinstance(payload, dict):
        return None
    sub = payload.get("sub")
    if not isinstance(sub, str) or not sub or set(payload) - {"sub", "user_id"} or payload.get("user_id", sub) != sub:
        return None
    return sub


class IdTokens:
    """ID tokens of the project's Auth, by uid, made as `signer`."""

    def __init__(self, production, signer, api_key, iam=IAM_CREDENTIALS, identity=IDENTITY_TOOLKIT):
        self.production = production
        self.signer = signer
        self.api_key = api_key
        self.iam = iam.rstrip("/")
        self.identity = identity.rstrip("/")
        # uid: (ID token, when it ends, by time.time()).
        self.made = {}

    def bearer(self, auth):
        """The `Authorization` to send for a recording's token that a real one stands for (`uid`)."""
        return f"Bearer {self.token(uid(auth))}"

    def token(self, user):
        made = self.made.get(user)
        if made is None or made[1] - REMAKE_BEFORE <= time.time():
            made = self.made[user] = self.make(user)
        return made[0]

    def post(self, what, base, path, body, **send):
        """A request of the making of a token, tried again while the service is busy; its answer
        as JSON. A refusal stops the run, its message masked: it holds no token."""
        data = json.dumps(body).encode()
        for attempt in range(5):
            status, _, text = self.production.send("POST", path, data, base=base, **send)
            if status not in TRANSIENT:
                break
            time.sleep(2 ** attempt)
        if status != 200:
            raise RuntimeError(f"{what} answered {status}: {mask(text, self.production.project)[:300]}")
        return json.loads(text)

    def make(self, user):
        """A custom token for `user`, signed as the signer, exchanged for an ID token; with when it ends."""
        now = int(time.time())
        claims = {"iss": self.signer, "sub": self.signer, "aud": AUDIENCE, "iat": now, "exp": now + LIFETIME, "uid": user}
        signed = self.post("signing a custom token", self.iam, f"/v1/projects/-/serviceAccounts/{self.signer}:signJwt",
                           {"payload": json.dumps(claims)})
        # The key goes in a header, not the path, which a failed request's log shows.
        answer = self.post("exchanging a custom token", self.identity, "/v1/accounts:signInWithCustomToken",
                           {"token": signed["signedJwt"], "returnSecureToken": True},
                           anonymous=True, headers={"X-Goog-Api-Key": self.api_key})
        return answer["idToken"], now + int(answer.get("expiresIn", LIFETIME))
