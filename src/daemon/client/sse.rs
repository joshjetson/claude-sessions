//! The event stream: frame reassembly and the subscription that feeds it.
//!
//! An SSE event can straddle any number of reads, and a naive
//! split-on-blank-line reader drops the tail of every chunk — which is why
//! [`SseParser`] is a byte buffer with a public `push`, driven in the tests
//! with every adversarial split rather than hoped about.

use std::io::{BufReader, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::Value;

use crate::daemon::lock;
use crate::daemon::protocol::read_head;

use super::{loopback, BACKOFF_MAX, BACKOFF_MIN, PROBE_TIMEOUT};

/// How long a read waits before looking up.
///
/// Comfortably longer than the daemon's 25-second keep-alive, so a healthy
/// stream never sees it — this is not a liveness check, it is the thing that
/// stops the reader thread being parked in a syscall that only somebody else's
/// `shutdown()` can end. A half-open socket used to wedge the feed thread for
/// good, and closing the subscription relied on `shutdown()` waking a blocked
/// `read` — which Unix guarantees and Winsock does not.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// One server-sent event: the name and its still-serialised payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

impl SseEvent {
    pub fn json(&self) -> Option<Value> {
        serde_json::from_str(&self.data).ok()
    }
}

/// What a subscriber is told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseMessage {
    Connected,
    Disconnected,
    Event(SseEvent),
}

/// Reassembles SSE frames from a byte stream.
///
/// A frame is whatever precedes a blank line, and a frame of any size can
/// straddle any number of reads — a naive split-on-blank-line reader drops the
/// tail of every chunk. Bytes rather than text, because a read can also land
/// in the middle of a multi-byte character.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
}

impl SseParser {
    pub fn new() -> Self {
        SseParser::default()
    }

    /// Feed one chunk; get back every frame that is now complete.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(end) = find(&self.buf, b"\n\n") {
            let frame = String::from_utf8_lossy(&self.buf[..end]).into_owned();
            self.buf.drain(..end + 2);
            if let Some(event) = parse_frame(&frame) {
                events.push(event);
            }
        }
        events
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// One frame's `event:` name and joined `data:` lines. Comments (`: ping`) and
/// frames with no data are nothing to report.
fn parse_frame(frame: &str) -> Option<SseEvent> {
    if frame.starts_with(':') {
        return None;
    }
    let mut event = "message".to_string();
    let mut data: Vec<&str> = Vec::new();
    for line in frame.split('\n') {
        let line = line.trim_end_matches('\r');
        if let Some(name) = line.strip_prefix("event:") {
            event = name.trim().to_string();
        } else if let Some(chunk) = line.strip_prefix("data:") {
            data.push(chunk.trim());
        }
    }
    if data.is_empty() {
        return None;
    }
    Some(SseEvent {
        event,
        data: data.join("\n"),
    })
}

/// A live subscription. Dropping it closes the stream.
pub struct Subscription {
    stopped: Arc<AtomicBool>,
    socket: Arc<Mutex<Option<TcpStream>>>,
    worker: Option<JoinHandle<()>>,
}

impl Subscription {
    /// Stop for good. The socket is shut down from here so the reader thread
    /// wakes immediately rather than at the next keep-alive.
    pub fn close(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(socket) = lock(&self.socket).take() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.close();
    }
}

/// Subscribe to `/events`, reconnecting with backoff until closed.
pub fn subscribe(port: u16, mut handler: impl FnMut(SseMessage) + Send + 'static) -> Subscription {
    let stopped = Arc::new(AtomicBool::new(false));
    let socket: Arc<Mutex<Option<TcpStream>>> = Arc::new(Mutex::new(None));
    let (flag, slot) = (Arc::clone(&stopped), Arc::clone(&socket));

    let worker = thread::Builder::new()
        .name("claude-sessions-events".into())
        .spawn(move || {
            let mut backoff = BACKOFF_MIN;
            while !flag.load(Ordering::SeqCst) {
                if stream_events(port, &flag, &slot, &mut handler) {
                    backoff = BACKOFF_MIN;
                }
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                handler(SseMessage::Disconnected);
                thread::sleep(backoff);
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        })
        .ok();

    Subscription {
        stopped,
        socket,
        worker,
    }
}

/// One connection's worth of streaming. True if it ever connected, which is
/// what resets the backoff.
fn stream_events(
    port: u16,
    stopped: &AtomicBool,
    slot: &Mutex<Option<TcpStream>>,
    handler: &mut impl FnMut(SseMessage),
) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&loopback(port), PROBE_TIMEOUT) else {
        return false;
    };
    // A quiet stream is normal — the daemon's `: ping` every 25 seconds is what
    // proves the socket is still there — so the deadline below is long enough
    // never to interrupt one. See [`READ_TIMEOUT`] for why there is one at all.
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let head = format!(
        "GET /events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\n\r\n"
    );
    if stream.write_all(head.as_bytes()).is_err() || stream.flush().is_err() {
        return false;
    }
    match stream.try_clone() {
        Ok(handle) => *lock(slot) = Some(handle),
        Err(_) => return false,
    }

    let mut reader = BufReader::new(stream);
    // The response head is consumed with the same reader, so any bytes of the
    // first frame that arrived in the same packet stay buffered.
    let Ok(Some(head)) = read_head(&mut reader) else {
        return false;
    };
    if head.parts().1 != "200" {
        return false;
    }
    handler(SseMessage::Connected);

    let mut parser = SseParser::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                for event in parser.push(&chunk[..read]) {
                    handler(SseMessage::Event(event));
                }
            }
            // The deadline passing is not the stream ending: nothing was
            // consumed, so the loop checks the stop flag and waits again.
            Err(error) if timed_out(&error) => {}
            Err(_) => break,
        }
        if stopped.load(Ordering::SeqCst) {
            break;
        }
    }
    *lock(slot) = None;
    true
}

/// A read that ran out of time rather than out of stream. Unix reports
/// `WouldBlock` for a timed-out read and Windows reports `TimedOut`; both mean
/// the same thing here.
fn timed_out(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}
