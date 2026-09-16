//! The daemon's HTTP surface: a health probe, a snapshot, an SSE event stream
//! and the action routes clients POST to. Bound to 127.0.0.1 only.
//!
//! No auth and no framework: loopback is the boundary, and the whole surface is
//! twenty routes of JSON plus one stream. It is hand-rolled on
//! [`TcpListener`] — a dependency would buy nothing here and would cost control
//! of exactly the thing that has bitten this server before, which is how a
//! frame gets written to a client that is no longer there.
//!
//! # Threads
//!
//! One accept thread; one thread per connection, capped at
//! [`MAX_CONNECTIONS`]; one pump thread holding the engine subscription and
//! fanning frames out to every SSE client; and, per SSE client, a writer (which
//! is also where its keep-alive comes from) plus a reader that does nothing but
//! notice the far end going away. A fixed-slot worker pool is the wrong shape
//! here: an SSE client holds its connection open for hours and would sit in a
//! pool slot forever.
//!
//! # Hardening, and the bugs behind it
//!
//! * Node wrote frames straight to the response, and a write to an *ended*
//!   stream raises an asynchronous `error` event rather than throwing — so the
//!   try/catch around it caught nothing and the unhandled error took the whole
//!   daemon down. Here every frame goes through one writer thread whose write
//!   errors are values: any failure drops that client and only that client.
//! * A slow client used to buffer without bound. A client whose unsent backlog
//!   passes [`MAX_CLIENT_BACKLOG`] is dropped instead — see `clients`.
//! * The keep-alive interval had to be declared before the handlers that clear
//!   it, or an early error hit a `const` in the temporal dead zone and threw
//!   from the error path. There is no such ordering hazard here: the ping is a
//!   `recv_timeout` in the same loop that does the writing.

use std::io::{self, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::scan::ProcessSource;

use super::protocol::{self, sse_frame, SSE_PING};
use super::{lock, Engine};

mod clients;
mod routes;

pub use clients::{over_backlog, MAX_CLIENT_BACKLOG};
use clients::{Clients, Stop};

/// Largest request body accepted, matching the Node server's `MAX_BODY`.
const MAX_BODY: usize = 1_000_000;
/// How often an idle stream gets a `: ping` comment.
const PING_INTERVAL: Duration = Duration::from_secs(25);
/// How long after answering `/shutdown` the server actually stops, so the
/// response is on the wire before the socket closes.
const SHUTDOWN_DELAY: Duration = Duration::from_millis(50);
/// A request must arrive, and a response must be accepted, inside this.
const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the pump sleeps before re-checking the stop flag.
const PUMP_TICK: Duration = Duration::from_millis(200);
/// Concurrent connections. Loopback with a handful of clients never
/// approaches it; it exists so a misbehaving one cannot spawn threads forever.
const MAX_CONNECTIONS: usize = 128;
/// Until Phase 10 supervises deploys, every deploy action is a clean refusal.
const NO_DEPLOYS: &str = "deploys are not available in this build";

// --- the server -------------------------------------------------------------

struct Shared<S: ProcessSource> {
    engine: Arc<Engine<S>>,
    clients: Arc<Clients>,
    stop: Arc<Stop>,
    started: Instant,
    connections: AtomicUsize,
}

/// A running server. Dropping it stops the server and joins its threads.
pub struct ServerHandle {
    port: u16,
    stop: Arc<Stop>,
    clients: Arc<Clients>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl ServerHandle {
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    /// Block until something asks the server to stop. `false` on timeout.
    pub fn wait_for_shutdown(&self, timeout: Duration) -> bool {
        self.stop.wait_timeout(timeout)
    }

    /// Ask the server to stop and wait for its threads.
    pub fn stop(&self) {
        self.stop.request();
        // The accept thread is parked in `accept()`; a throwaway connection to
        // our own port is what wakes it, and it checks the flag before reading.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        // Dropping the senders ends every SSE writer's `recv_timeout` at once
        // rather than up to a ping interval later.
        self.clients.clear();
        for thread in lock(&self.threads).drain(..) {
            let _ = thread.join();
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Take the port. Separated from [`serve`] because a daemon that could not take
/// its port has no way to be reached, so the caller must be able to tell the
/// difference before it starts an engine — the Node server exposed the same
/// outcome as a promise for the same reason.
pub fn bind(port: u16) -> io::Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", port))
}

/// Serve `engine` on an already-bound listener.
pub fn serve<S: ProcessSource + Send + 'static>(
    engine: Arc<Engine<S>>,
    listener: TcpListener,
) -> io::Result<ServerHandle> {
    let port = listener.local_addr()?.port();
    let clients = Arc::new(Clients::default());
    let stop = Arc::new(Stop::default());
    let events = engine.subscribe();
    let shared = Arc::new(Shared {
        engine,
        clients: Arc::clone(&clients),
        stop: Arc::clone(&stop),
        started: Instant::now(),
        connections: AtomicUsize::new(0),
    });

    let pump_shared = Arc::clone(&shared);
    let threads = vec![
        thread::Builder::new()
            .name("claude-sessions-http".into())
            .spawn(move || accept_loop(listener, shared))?,
        thread::Builder::new()
            .name("claude-sessions-sse".into())
            .spawn(move || pump(pump_shared, events))?,
    ];

    Ok(ServerHandle {
        port,
        stop,
        clients,
        threads: Mutex::new(threads),
    })
}

fn accept_loop<S: ProcessSource + Send + 'static>(listener: TcpListener, shared: Arc<Shared<S>>) {
    while let Ok((stream, _)) = listener.accept() {
        if shared.stop.is_stopping() {
            let _ = stream.shutdown(Shutdown::Both);
            break;
        }
        if shared.connections.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            let _ = write_json(&stream, 503, &json!({ "ok": false, "error": "busy" }));
            continue;
        }
        shared.connections.fetch_add(1, Ordering::Relaxed);
        let connection = Arc::clone(&shared);
        let spawned = thread::Builder::new()
            .name("claude-sessions-conn".into())
            .spawn(move || {
                handle(stream, &connection);
                connection.connections.fetch_sub(1, Ordering::Relaxed);
            });
        if spawned.is_err() {
            shared.connections.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// Turn engine events into frames and hand them to the clients.
fn pump<S: ProcessSource>(shared: Arc<Shared<S>>, events: Receiver<super::EngineEvent>) {
    loop {
        match events.recv_timeout(PUMP_TICK) {
            Ok(event) => {
                if let Some(frame) = protocol::event_frame(&event) {
                    shared.clients.broadcast(&Arc::new(frame));
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if shared.stop.is_stopping() {
            return;
        }
    }
}

fn handle<S: ProcessSource + Send + 'static>(stream: TcpStream, shared: &Arc<Shared<S>>) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let Ok(peer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(peer);
    let Ok(Some(head)) = protocol::read_head(&mut reader) else {
        return;
    };
    let (method, target, _) = head.parts();
    let (method, path) = (
        method.to_string(),
        target.split('?').next().unwrap_or("/").to_string(),
    );

    if method == "GET" && path == "/events" {
        serve_events(stream, reader, shared);
        return;
    }

    let body = match protocol::read_body(&mut reader, head.content_length(), MAX_BODY) {
        Ok(body) => body,
        Err(_) => {
            let _ = write_json(
                &stream,
                413,
                &json!({ "ok": false, "error": "body too large" }),
            );
            return;
        }
    };
    let (status, payload) = routes::route(shared, &method, &path, &body);
    let _ = write_json(&stream, status, &payload);
    let _ = stream.shutdown(Shutdown::Both);
}

/// One SSE client: the snapshot, then every frame the pump queues, with a
/// keep-alive whenever nothing has been queued for a while.
fn serve_events<S: ProcessSource>(
    mut stream: TcpStream,
    reader: BufReader<TcpStream>,
    shared: &Arc<Shared<S>>,
) {
    // A stream is quiet for minutes at a time, so neither half of it may time
    // out; a write that cannot complete is caught by the backlog rule instead.
    let _ = stream.set_read_timeout(None);
    let _ = stream.set_write_timeout(None);
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
        Cache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n";
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }

    let (id, frames, pending) = shared.clients.add();
    // `res.on('close')`: a client that simply goes away has to be noticed now,
    // not up to a keep-alive later, or `/health` reports clients that are not
    // there and the daemon holds their buffers. Nothing is ever sent on this
    // socket, so the read only ever returns at end of stream.
    let watcher = {
        let clients = Arc::clone(&shared.clients);
        thread::Builder::new()
            .name("claude-sessions-sse-eof".into())
            .spawn(move || {
                let mut reader = reader;
                let mut byte = [0u8; 1];
                while matches!(reader.read(&mut byte), Ok(read) if read > 0) {}
                clients.remove(id);
            })
            .ok()
    };
    // The snapshot goes first, so a client is fully caught up the moment it
    // connects with no separate GET /state round trip. Registering before
    // writing it is safe: anything the pump queues meanwhile waits in the
    // channel, and this thread is the only writer on the socket.
    let snapshot = serde_json::to_string(&shared.engine.snapshot()).unwrap_or_default();
    let mut alive = write_frame(&mut stream, &sse_frame("snapshot", &snapshot));

    while alive {
        alive = match frames.recv_timeout(PING_INTERVAL) {
            Ok(frame) => {
                pending.fetch_sub(frame.len(), Ordering::Relaxed);
                write_frame(&mut stream, &frame)
            }
            Err(RecvTimeoutError::Timeout) => write_frame(&mut stream, SSE_PING),
            Err(RecvTimeoutError::Disconnected) => false,
        };
    }

    shared.clients.remove(id);
    // Shutting the socket down is also what ends the watcher's read.
    let _ = stream.shutdown(Shutdown::Both);
    if let Some(watcher) = watcher {
        let _ = watcher.join();
    }
}

/// Write one frame, reporting whether the client is still there. This is the
/// single place a frame reaches a socket, which is what keeps a vanished client
/// from being an error anywhere else.
fn write_frame(stream: &mut TcpStream, frame: &str) -> bool {
    stream
        .write_all(frame.as_bytes())
        .and_then(|()| stream.flush())
        .is_ok()
}

fn write_json(mut out: impl Write, status: u16, payload: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(payload).unwrap_or_else(|_| b"{}".to_vec());
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        reason(status),
        body.len()
    );
    out.write_all(head.as_bytes())?;
    out.write_all(&body)?;
    out.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        202 => "Accepted",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        503 => "Service Unavailable",
        _ => "OK",
    }
}
