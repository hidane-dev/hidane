//! The WebSocket server on `--websocket_port`: the feed of the Emulator UI's request monitor
//! (`rules::monitor`) at `ws://{host}:{port}/requests`, as the official emulator serves it
//! (`tests/fixtures/websocket_requests.json`). firebase-tools passes the port and tells the
//! Emulator UI where it is (`webSocketHost`, `webSocketPort`).
//!
//! - The handshake is RFC 6455's; the answer holds `upgrade`, `connection` and
//!   `sec-websocket-accept`, and no subprotocol or extension. A request to `/requests` that is
//!   not a handshake gets `400` with the official emulator's message. Any other path, or a query
//!   string, gets `404`: the official emulator does not answer at all.
//! - The server then sends the backlog, the last records, as one text frame holding a JSON
//!   array, then one text frame per record. It answers a ping with a pong and a close with the
//!   same close, and ignores text and binary messages.
//! - A client never holds up a request: records wait for it in a queue, and a client more than
//!   `CLIENT_BUFFER` bytes behind is disconnected (the official emulator waits for it instead).

use std::{
    convert::Infallible,
    io,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use axum::body::Body;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use hyper::{
    Method, Request, Response, StatusCode, body::Incoming, header, server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use sha1::{Digest as _, Sha1};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

use crate::rules::monitor::{Monitor, Subscription};

/// The GUID RFC 6455 appends to the key.
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// The largest message a client may send (Netty's default, which the official emulator uses).
const MAX_PAYLOAD: usize = 64 * 1024;

const TEXT: u8 = 0x1;
const CLOSE: u8 = 0x8;
const PING: u8 = 0x9;
const PONG: u8 = 0xa;

/// Serves the feed of `monitor` on every listener, until dropped.
pub async fn serve_websocket(listeners: Vec<TcpListener>, monitor: Arc<Monitor>) {
    let mut acceptors = tokio::task::JoinSet::new();
    for listener in listeners {
        acceptors.spawn(accept(listener, Arc::clone(&monitor)));
    }
    while acceptors.join_next().await.is_some() {}
}

async fn accept(listener: TcpListener, monitor: Arc<Monitor>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let _ = stream.set_nodelay(true);
                tokio::spawn(connection(stream, Arc::clone(&monitor)));
            }
            Err(err) => {
                // Transient (e.g. too many open files): back off instead of exiting.
                eprintln!("accept failed: {err}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn connection(stream: TcpStream, monitor: Arc<Monitor>) {
    let service = service_fn(move |request| {
        let monitor = Arc::clone(&monitor);
        async move { Ok::<_, Infallible>(handshake(request, monitor)) }
    });
    let _ = http1::Builder::new()
        // The official emulator's answers have no `date`.
        .auto_date_header(false)
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades()
        .await;
}

/// Answers a handshake and, once the connection is upgraded, starts the feed on it.
fn handshake(request: Request<Incoming>, monitor: Arc<Monitor>) -> Response<Body> {
    if request.uri().path() != "/requests" || request.uri().query().is_some() {
        return answer(StatusCode::NOT_FOUND, "Not Found\n".to_owned());
    }
    let headers = request.headers();
    let has = |name: header::HeaderName, token: &str| {
        headers.get_all(name).iter().any(|value| {
            value.to_str().is_ok_and(|value| {
                value
                    .split(',')
                    .any(|t| t.trim().eq_ignore_ascii_case(token))
            })
        })
    };
    if request.method() != Method::GET {
        let message = format!("Invalid WebSocket handshake method: {}", request.method());
        return answer(StatusCode::BAD_REQUEST, message);
    }
    if !has(header::UPGRADE, "websocket") {
        let message = "not a WebSocket handshake request: missing upgrade";
        return answer(StatusCode::BAD_REQUEST, message.to_owned());
    }
    if !has(header::CONNECTION, "upgrade") {
        let message =
            "not a WebSocket request: a |Connection| header must includes a token 'Upgrade'";
        return answer(StatusCode::BAD_REQUEST, message.to_owned());
    }
    let Some(key) = headers.get(header::SEC_WEBSOCKET_KEY) else {
        let message = "not a WebSocket request: missing key";
        return answer(StatusCode::BAD_REQUEST, message.to_owned());
    };
    let version = headers
        .get(header::SEC_WEBSOCKET_VERSION)
        .and_then(|v| v.to_str().ok());
    if !matches!(version, Some("13" | "8" | "7")) {
        let mut response = answer(StatusCode::UPGRADE_REQUIRED, String::new());
        response.headers_mut().insert(
            header::SEC_WEBSOCKET_VERSION,
            header::HeaderValue::from_static("13"),
        );
        return response;
    }
    let mut digest = Sha1::new();
    digest.update(key.as_bytes());
    digest.update(GUID.as_bytes());
    let accept = STANDARD.encode(digest.finalize());
    tokio::spawn(async move {
        if let Ok(upgraded) = hyper::upgrade::on(request).await {
            feed(TokioIo::new(upgraded), monitor.subscribe()).await;
        }
    });
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let headers = response.headers_mut();
    headers.insert(
        header::UPGRADE,
        header::HeaderValue::from_static("websocket"),
    );
    headers.insert(
        header::CONNECTION,
        header::HeaderValue::from_static("upgrade"),
    );
    if let Ok(accept) = header::HeaderValue::from_str(&accept) {
        headers.insert(header::SEC_WEBSOCKET_ACCEPT, accept);
    }
    response
}

/// A plain-text answer without a content type, as the official emulator's.
fn answer(status: StatusCode, body: String) -> Response<Body> {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response
}

/// Sends the backlog, then each record, until the client closes, fails or falls too far
/// behind; answers its pings and its close meanwhile.
async fn feed<S: AsyncRead + AsyncWrite + Unpin>(stream: S, subscription: Subscription) {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut frames = Frames {
        reader,
        buf: Vec::new(),
    };
    let Subscription {
        backlog,
        mut records,
        queued,
        dropped,
    } = subscription;
    // A send gives way when the client is dropped: one that stopped reading would hold it.
    tokio::select! {
        sent = write_frame(&mut writer, TEXT, backlog.as_bytes()) => if sent.is_err() { return },
        () = dropped.notified() => return,
    }
    drop(backlog);
    loop {
        tokio::select! {
            () = dropped.notified() => return,
            record = records.recv() => {
                let Some(record) = record else { return };
                tokio::select! {
                    sent = write_frame(&mut writer, TEXT, record.as_bytes()) => {
                        if sent.is_err() {
                            return;
                        }
                    }
                    () = dropped.notified() => return,
                }
                queued.fetch_sub(record.len(), Ordering::AcqRel);
            }
            frame = frames.next() => match frame {
                Ok(Frame::Ping(payload)) => {
                    if write_frame(&mut writer, PONG, &payload).await.is_err() {
                        return;
                    }
                }
                Ok(Frame::Close(payload)) => {
                    let _ = write_frame(&mut writer, CLOSE, &payload).await;
                    let _ = writer.shutdown().await;
                    return;
                }
                Ok(Frame::Other) => {}
                Err(_) => return,
            },
        }
    }
}

async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    opcode: u8,
    payload: &[u8],
) -> io::Result<()> {
    let mut head = Vec::with_capacity(10);
    head.push(0x80 | opcode);
    match payload.len() {
        n @ 0..=125 => head.push(u8::try_from(n).unwrap_or(125)),
        n @ 126..=0xffff => {
            head.push(126);
            head.extend_from_slice(&u16::try_from(n).unwrap_or(u16::MAX).to_be_bytes());
        }
        n => {
            head.push(127);
            head.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    writer.write_all(&head).await?;
    writer.write_all(payload).await?;
    writer.flush().await
}

/// What a client sent.
#[derive(Debug, PartialEq, Eq)]
enum Frame {
    Ping(Vec<u8>),
    Close(Vec<u8>),
    /// A message, a part of one, or a pong: ignored.
    Other,
}

/// The frames of a client, read so that waiting for one can be cancelled without losing data.
struct Frames<R> {
    reader: R,
    buf: Vec<u8>,
}

impl<R: AsyncRead + Unpin> Frames<R> {
    async fn next(&mut self) -> io::Result<Frame> {
        loop {
            if let Some(frame) = parse(&mut self.buf)? {
                return Ok(frame);
            }
            if self.reader.read_buf(&mut self.buf).await? == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
        }
    }
}

/// The first frame of `buf`, removed from it, if it is all there. A frame from a client must be
/// masked and not too large.
fn parse(buf: &mut Vec<u8>) -> io::Result<Option<Frame>> {
    let invalid = |message: &str| io::Error::new(io::ErrorKind::InvalidData, message.to_owned());
    let [first, second, ..] = buf[..] else {
        return Ok(None);
    };
    if second & 0x80 == 0 {
        return Err(invalid("an unmasked frame"));
    }
    let (length, at) = match second & 0x7f {
        126 => match buf.get(2..4) {
            Some(n) => (usize::from(u16::from_be_bytes([n[0], n[1]])), 4),
            None => return Ok(None),
        },
        127 => match buf.get(2..10).and_then(|n| <[u8; 8]>::try_from(n).ok()) {
            Some(n) => (
                usize::try_from(u64::from_be_bytes(n)).unwrap_or(usize::MAX),
                10,
            ),
            None => return Ok(None),
        },
        n => (usize::from(n), 2),
    };
    if length > MAX_PAYLOAD {
        return Err(invalid("a frame too large"));
    }
    let start = at + 4;
    if buf.len() < start + length {
        return Ok(None);
    }
    let mask = [buf[at], buf[at + 1], buf[at + 2], buf[at + 3]];
    let payload: Vec<u8> = buf[start..start + length]
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ mask[i % 4])
        .collect();
    buf.drain(..start + length);
    match first & 0x0f {
        CLOSE => Ok(Some(Frame::Close(payload))),
        PING => Ok(Some(Frame::Ping(payload))),
        0x0 | TEXT | 0x2 | PONG => Ok(Some(Frame::Other)),
        _ => Err(invalid("an unknown opcode")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn masked(opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mask = [1, 2, 3, 4];
        let mut frame = vec![0x80 | opcode];
        match payload.len() {
            n @ 0..=125 => frame.push(0x80 | n as u8),
            n => {
                frame.push(0x80 | 126);
                frame.extend_from_slice(&(n as u16).to_be_bytes());
            }
        }
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        frame
    }

    #[test]
    fn frames_parse_once_complete() {
        let mut buf = masked(PING, b"hi");
        buf.extend(masked(TEXT, &[b'x'; 300]));
        buf.extend(masked(CLOSE, &[3, 232]));
        let last = buf.pop();
        assert_eq!(parse(&mut buf).unwrap(), Some(Frame::Ping(b"hi".to_vec())));
        assert_eq!(parse(&mut buf).unwrap(), Some(Frame::Other));
        assert_eq!(parse(&mut buf).unwrap(), None);
        buf.extend(last);
        assert_eq!(parse(&mut buf).unwrap(), Some(Frame::Close(vec![3, 232])));
        assert!(buf.is_empty());
    }

    #[test]
    fn unmasked_and_large_frames_are_refused() {
        assert!(parse(&mut vec![0x81, 0x01, b'x']).is_err());
        let mut large = vec![0x82, 0x80 | 127];
        large.extend_from_slice(&(MAX_PAYLOAD as u64 + 1).to_be_bytes());
        assert!(parse(&mut large).is_err());
    }
}
