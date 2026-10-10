# WebChannel

firebase-js-sdk in the browser (and Flutter Web through it) does not speak gRPC. It carries the
Listen and Write streams over **WebChannel** (closure-library's `goog.labs.net.webchannel`,
protocol version 8), plain HTTP/1.1 requests. This page records the wire as the official emulator
v1.22.0 serves it. It was captured between Chromium running firebase-js-sdk 13.0.0 and the
official emulator (`tools/oracle/webchannel/`), and hidane follows it (`crates/hidane/src/webchannel.rs`).

## Requests

Everything goes to `/google.firestore.v1.Firestore/Listen/channel` or `…/Write/channel`, on the
same port as gRPC and REST (requests are told apart by content type).

| Request | Query | Body |
|---|---|---|
| Handshake | `POST ?VER=8&database=…&RID=<n>&CVER=22&X-HTTP-Session-Id=gsessionid&zx=…&t=1` | form: `headers`, `count`, `ofs=0`, `req0___data__`, … |
| Back channel | `GET ?VER=8&database=…&RID=rpc&SID=<sid>&AID=<last id received>&CI=0\|1&TYPE=xmlhttp&zx=…&t=1` | none |
| Forward channel | `POST ?VER=8&database=…&SID=<sid>&RID=<n+1>&AID=<last id received>&zx=…&t=1` | form: `count`, `ofs=<position of the first message>`, `req{i}___data__` |
| Terminate | `POST ?VER=8&database=…&SID=<sid>&RID=<n>&TYPE=terminate&zx=…` | empty, `text/plain` |

- Client messages are `ListenRequest` / `WriteRequest` in ProtoJSON, numbered by `ofs`; the
  handshake carries the first ones (a Listen target, the Write handshake).
- `headers` holds the request headers the browser cannot set on a cross-origin request, one
  `Name:Value` per line (`X-Goog-Api-Client`, `Content-Type`, `x-goog-api-key`, and
  `Authorization: Bearer <token>` with `mockUserToken` or a signed-in user).
- None of these requests needs a CORS preflight.

## Answers

Every answer body is one or more frames, `<length>\n<JSON>`, where the length counts **UTF-16 code
units** (what the browser's string length is), not bytes.

- **Handshake**: `[[0,["c","<sid>","",8,12,30000]]]`. The session ID is 16 random bytes in
  URL-safe base64. No `X-HTTP-Session-Id` header comes back, so the client sends no
  `gsessionid`.
- **Back channel**: the server's messages as numbered arrays, `[[id,[message]],…]`, ids counting
  up from 1 per session, a message per array: a `ListenResponse` / `WriteResponse` in ProtoJSON,
  `"noop"`, or a stream error `{"error":{"message":"INVALID_ARGUMENT: …","status":"INVALID_ARGUMENT"}}`
  (after which the client terminates the session). A back channel resends what came after its
  `AID`.
  - `CI=0` (streaming): what is queued, then a `noop`, then each message as it comes; a `noop`
    every 30 s; after 60 s a last `noop` ends the response and the client opens another.
  - `CI=1` (long polling, `experimentalForceLongPolling`): what is queued ends the response at
    once; otherwise a `noop`, and the response ends with the next data.
- **Forward channel**: `[<1 if a back channel is open, else 0>,<last id sent>,7]`. The last
  number is closure's "outstanding bytes"; the official emulator always sends 7.
- **Terminate**: `200`, empty (the official emulator spells one header `Contenttype`).
- An unknown session (`SID`) is `400` with an empty body, which makes the client start over.

Every answer exposes `x-client-wire-protocol, x-http-session-id, x-http-initial-response`
(`Access-Control-Expose-Headers`), varies by `Origin` and has the CORS headers of any HTTP path;
back channels also send `X-Content-Type-Options: nosniff` and `Cache-Control: private,
max-age=0`.

## Where hidane differs

See [parity-exceptions.md](parity-exceptions.md): connection reuse and content lengths, `noop`
placement and batching while long polling, and idle sessions, which hidane drops after five
minutes without a back channel or a request.
