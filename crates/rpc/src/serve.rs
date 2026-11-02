//! Duplex, loopback, and Unix socket sessions. All call the same dispatch.
//!
//! Each connection is a reader loop plus a dedicated writer task, so no read
//! future is ever dropped mid-frame by a push. Replies go ahead of queued pushes.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, DuplexStream};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tokio_tungstenite::accept_async_with_config;
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

use crate::boot::Identity;
use crate::codec::{self, FrameError, FrameReader};
use crate::dispatch::{self, Step, Transport};
use crate::host::{EngineHost, Outbound};
use crate::{HELLO_TIMEOUT, MAX_FRAME_BYTES};

/// Pause after a failed `accept` (e.g. EMFILE) before trying again.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
/// How long a closing session may spend flushing its last reply.
const DRAIN_BUDGET: Duration = Duration::from_secs(1);

pub struct EngineState {
    pub identity: Identity,
    pub token: String,
    pub host: RwLock<Option<Arc<dyn EngineHost>>>,
}

impl EngineState {
    /// One request through dispatch. Logs the first successful `hello`.
    fn step(
        &self,
        text: &str,
        hello_done: &mut bool,
        transport: Transport,
        outbound: &Outbound,
    ) -> Step {
        let host = self.host.read().expect("host").clone();
        let was_done = *hello_done;
        let step = dispatch::step(
            text,
            hello_done,
            transport,
            &self.identity,
            &self.token,
            host.as_deref(),
            outbound,
        );
        if !was_done && *hello_done {
            tracing::info!(target: "keel_rpc", transport = transport.name(), "client attached");
        }
        step
    }
}

pub async fn serve_duplex(
    io: DuplexStream,
    state: Arc<EngineState>,
    shutdown: watch::Receiver<bool>,
) {
    session_framed(io, state, shutdown, Transport::Duplex).await;
}

pub async fn serve_ws(
    listener: TcpListener,
    state: Arc<EngineState>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        let accepted = tokio::select! {
            _ = shutdown.changed() => break,
            accepted = listener.accept() => accepted,
        };
        match accepted {
            Ok((stream, _)) => {
                let _ = stream.set_nodelay(true);
                tokio::spawn(session_ws(stream, Arc::clone(&state), shutdown.clone()));
            }
            Err(err) => {
                tracing::warn!(target: "keel_rpc", "loopback accept failed: {err}");
                if backoff(&mut shutdown).await {
                    break;
                }
            }
        }
    }
}

#[cfg(unix)]
pub async fn serve_unix(
    listener: tokio::net::UnixListener,
    state: Arc<EngineState>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        let accepted = tokio::select! {
            _ = shutdown.changed() => break,
            accepted = listener.accept() => accepted,
        };
        match accepted {
            Ok((stream, _)) => {
                tokio::spawn(session_unix(stream, Arc::clone(&state), shutdown.clone()));
            }
            Err(err) => {
                tracing::warn!(target: "keel_rpc", "unix accept failed: {err}");
                if backoff(&mut shutdown).await {
                    break;
                }
            }
        }
    }
}

/// Sleep briefly. True when shutdown fired instead.
async fn backoff(shutdown: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        _ = shutdown.changed() => true,
        _ = tokio::time::sleep(ACCEPT_BACKOFF) => false,
    }
}

#[cfg(unix)]
async fn session_unix(
    stream: tokio::net::UnixStream,
    state: Arc<EngineState>,
    shutdown: watch::Receiver<bool>,
) {
    match sniff_ws(&stream).await {
        Some(true) => session_ws(stream, state, shutdown).await,
        Some(false) => session_framed(stream, state, shutdown, Transport::Socket).await,
        None => tracing::info!(target: "keel_rpc", "hello rejected: unix peer sent nothing"),
    }
}

/// Peek until three bytes arrive. `Some(true)` for an HTTP upgrade (`GET`),
/// `Some(false)` for a length prefix, `None` on EOF, error, or timeout.
#[cfg(unix)]
async fn sniff_ws(stream: &tokio::net::UnixStream) -> Option<bool> {
    use std::os::fd::AsRawFd;
    use tokio::io::Interest;

    let deadline = Instant::now() + HELLO_TIMEOUT;
    let fd = stream.as_raw_fd();
    loop {
        tokio::time::timeout_at(deadline, stream.readable())
            .await
            .ok()?
            .ok()?;
        let mut peek = [0u8; 3];
        let peeked = stream.try_io(Interest::READABLE, || {
            // SAFETY: `fd` is owned by `stream`, which outlives this call, and
            // `peek` is a valid writable buffer of `peek.len()` bytes. MSG_PEEK
            // leaves the bytes queued for the session that reads them next.
            let n = unsafe { libc::recv(fd, peek.as_mut_ptr().cast(), peek.len(), libc::MSG_PEEK) };
            if n < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(n as usize)
            }
        });
        match peeked {
            Ok(0) => return None,
            Ok(n) if n >= peek.len() => return Some(&peek == b"GET"),
            // Fewer than 3 bytes queued; readiness stays set, so wait a beat.
            Ok(_) => {
                tokio::time::sleep_until(deadline.min(Instant::now() + Duration::from_millis(5)))
                    .await
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
        if Instant::now() >= deadline {
            return None;
        }
    }
}

/// Length-prefixed session (in-process duplex, or a Unix socket that did not send `GET`).
async fn session_framed<S>(
    io: S,
    state: Arc<EngineState>,
    mut shutdown: watch::Receiver<bool>,
    transport: Transport,
) where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (mut rd, wr) = tokio::io::split(io);
    let (outbound, pushed) = Outbound::pair();
    let (reply_tx, reply_rx) = mpsc::unbounded_channel::<Option<String>>();
    let mut writer = tokio::spawn(write_framed(wr, reply_rx, pushed));
    let mut frames = FrameReader::default();
    let mut hello_done = false;
    // The duplex is in-process and may sit idle until the UI takes it.
    let needs_hello_by = matches!(transport, Transport::Socket);
    let hello_deadline = tokio::time::sleep_until(Instant::now() + HELLO_TIMEOUT);
    tokio::pin!(hello_deadline);
    loop {
        let frame = tokio::select! {
            _ = shutdown.changed() => break,
            _ = &mut hello_deadline, if needs_hello_by && !hello_done => {
                tracing::info!(target: "keel_rpc", transport = transport.name(), "hello rejected: timed out");
                break;
            }
            frame = frames.next(&mut rd) => frame,
        };
        let text = match frame.map(String::from_utf8) {
            Ok(Ok(text)) => text,
            Ok(Err(_)) => break,
            Err(FrameError::TooLarge) => {
                tracing::warn!(target: "keel_rpc", transport = transport.name(), "frame over the size cap, closing");
                break;
            }
            Err(FrameError::Io(_)) => break,
        };
        match state.step(&text, &mut hello_done, transport, &outbound) {
            Step::Reply(body) => {
                if reply_tx.send(Some(body)).is_err() {
                    break;
                }
            }
            Step::ReplyClose(body) => {
                let _ = reply_tx.send(Some(body));
                break;
            }
            Step::Close => break,
        }
    }
    let _ = reply_tx.send(None);
    if tokio::time::timeout(DRAIN_BUDGET, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
}

/// Writes replies first, then pushes. `None` on the reply channel closes after
/// the replies queued before it.
async fn write_framed<W>(
    mut wr: W,
    mut replies: mpsc::UnboundedReceiver<Option<String>>,
    mut pushed: mpsc::UnboundedReceiver<String>,
) where
    W: AsyncWrite + Unpin,
{
    loop {
        let body = tokio::select! {
            biased;
            reply = replies.recv() => match reply {
                Some(Some(body)) => body,
                _ => break,
            },
            push = pushed.recv() => match push {
                Some(body) => body,
                None => break,
            },
        };
        match codec::write_frame(&mut wr, body.as_bytes()).await {
            Ok(()) => {}
            Err(FrameError::TooLarge) => {
                let Some(fail) = too_large_failure(&body) else {
                    tracing::warn!(target: "keel_rpc", bytes = body.len(), "dropped oversize push");
                    continue;
                };
                if codec::write_frame(&mut wr, fail.as_bytes()).await.is_err() {
                    break;
                }
            }
            Err(FrameError::Io(_)) => break,
        }
    }
    let _ = wr.shutdown().await;
}

async fn session_ws<S>(stream: S, state: Arc<EngineState>, mut shutdown: watch::Receiver<bool>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let started = Instant::now();
    let config = WebSocketConfig {
        max_message_size: Some(MAX_FRAME_BYTES),
        max_frame_size: Some(MAX_FRAME_BYTES),
        ..WebSocketConfig::default()
    };
    // The HTTP upgrade counts against the hello budget.
    let socket = match tokio::time::timeout(
        HELLO_TIMEOUT,
        accept_async_with_config(stream, Some(config)),
    )
    .await
    {
        Ok(Ok(socket)) => socket,
        Ok(Err(err)) => {
            tracing::info!(target: "keel_rpc", "hello rejected: websocket handshake failed: {err}");
            return;
        }
        Err(_) => {
            tracing::info!(target: "keel_rpc", "hello rejected: websocket handshake timed out");
            return;
        }
    };
    let (sink, mut source) = socket.split();
    let (outbound, pushed) = Outbound::pair();
    let (reply_tx, reply_rx) = mpsc::unbounded_channel::<Option<String>>();
    let mut writer = tokio::spawn(write_ws(sink, reply_rx, pushed));
    let mut hello_done = false;
    let hello_deadline = tokio::time::sleep_until(started + HELLO_TIMEOUT);
    tokio::pin!(hello_deadline);
    loop {
        let incoming = tokio::select! {
            _ = shutdown.changed() => break,
            _ = &mut hello_deadline, if !hello_done => {
                tracing::info!(target: "keel_rpc", transport = "ws", "hello rejected: timed out");
                break;
            }
            incoming = source.next() => incoming,
        };
        let text = match incoming {
            Some(Ok(Message::Text(text))) => text,
            // tungstenite answers pings on its own.
            Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
            Some(Err(err)) => {
                tracing::debug!(target: "keel_rpc", "websocket read ended: {err}");
                break;
            }
            _ => break,
        };
        match state.step(&text, &mut hello_done, Transport::Socket, &outbound) {
            Step::Reply(body) => {
                if reply_tx.send(Some(body)).is_err() {
                    break;
                }
            }
            Step::ReplyClose(body) => {
                let _ = reply_tx.send(Some(body));
                break;
            }
            Step::Close => break,
        }
    }
    let _ = reply_tx.send(None);
    if tokio::time::timeout(DRAIN_BUDGET, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
}

async fn write_ws<K>(
    mut sink: K,
    mut replies: mpsc::UnboundedReceiver<Option<String>>,
    mut pushed: mpsc::UnboundedReceiver<String>,
) where
    K: futures::Sink<Message> + Unpin,
{
    loop {
        let mut body = tokio::select! {
            biased;
            reply = replies.recv() => match reply {
                Some(Some(body)) => body,
                _ => break,
            },
            push = pushed.recv() => match push {
                Some(body) => body,
                None => break,
            },
        };
        if body.len() > MAX_FRAME_BYTES {
            match too_large_failure(&body) {
                Some(fail) => body = fail,
                None => {
                    tracing::warn!(target: "keel_rpc", bytes = body.len(), "dropped oversize push");
                    continue;
                }
            }
        }
        if sink.send(Message::Text(body)).await.is_err() {
            break;
        }
    }
    let _ = sink.send(Message::Close(None)).await;
}

/// A reply that would exceed the cap becomes `PayloadTooLarge` for the same id.
/// Pushes have no id and return `None`.
fn too_large_failure(body: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Id {
        id: Option<String>,
    }
    let id = serde_json::from_str::<Id>(body).ok()?.id?;
    tracing::warn!(target: "keel_rpc", bytes = body.len(), "reply over the size cap");
    Some(dispatch::failure(
        &id,
        keel_proto::RpcErrorCode::PayloadTooLarge,
        "reply exceeds 1 MiB",
    ))
}
