//! A loopback JSON-RPC server, so the real client is exercised end to end
//! without a network. Every test in this module talks to one of these.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{json, Value};

/// One canned reply. `delay` exists for the timeout test.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub body: String,
    pub delay: Option<Duration>,
}

impl Reply {
    /// A JSON-RPC success carrying `result`.
    pub fn result(result: Value) -> Self {
        Reply::raw(
            200,
            json!({ "jsonrpc": "2.0", "id": 1, "result": result }).to_string(),
        )
    }

    /// Odoo's error shape: the readable text sits in `error.data.message`.
    pub fn error(message: &str) -> Self {
        Reply::raw(
            200,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {
                    "code": 200,
                    "message": "Odoo Server Error",
                    "data": { "message": message },
                },
            })
            .to_string(),
        )
    }

    pub fn raw(status: u16, body: impl Into<String>) -> Self {
        Reply {
            status,
            body: body.into(),
            delay: None,
        }
    }

    pub fn after(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }
}

#[derive(Default)]
struct Shared {
    /// Replies in order; the last one repeats once the queue runs dry, which
    /// keeps "call this twice and check the cache" tests short.
    replies: Vec<Reply>,
    next: usize,
    requests: Vec<Value>,
}

pub struct StubServer {
    addr: SocketAddr,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl StubServer {
    pub fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let shared = Arc::new(Mutex::new(Shared {
            replies,
            ..Shared::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));

        let handle = {
            let shared = Arc::clone(&shared);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    match stream {
                        Ok(stream) => serve_one(stream, &shared),
                        Err(_) => break,
                    }
                }
            })
        };

        StubServer {
            addr,
            shared,
            stop,
            handle: Some(handle),
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The JSON bodies the client posted, in order.
    pub fn requests(&self) -> Vec<Value> {
        self.shared.lock().unwrap().requests.clone()
    }

    pub fn request_count(&self) -> usize {
        self.shared.lock().unwrap().requests.len()
    }

    /// `params.args` of the nth request — what the client actually sent Odoo.
    pub fn args(&self, index: usize) -> Vec<Value> {
        self.requests()[index]["params"]["args"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The `(model, method)` of the nth `execute_kw` request.
    pub fn call_target(&self, index: usize) -> (String, String) {
        let args = self.args(index);
        (
            args[3].as_str().unwrap_or_default().to_string(),
            args[4].as_str().unwrap_or_default().to_string(),
        )
    }

    /// Every `(model, method)` pair the client asked for, in order. Auth calls
    /// (which have no model) are skipped.
    pub fn calls(&self) -> Vec<(String, String)> {
        (0..self.request_count())
            .filter(|index| self.requests()[*index]["params"]["method"] == json!("execute_kw"))
            .map(|index| self.call_target(index))
            .collect()
    }
}

impl Drop for StubServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Unblock the accept loop with one throwaway connection.
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve_one(mut stream: TcpStream, shared: &Arc<Mutex<Shared>>) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut length = 0usize;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
    }

    let mut body = vec![0u8; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }

    let reply = {
        let mut state = shared.lock().unwrap();
        if let Ok(parsed) = serde_json::from_slice::<Value>(&body) {
            state.requests.push(parsed);
        }
        let index = state.next.min(state.replies.len().saturating_sub(1));
        state.next += 1;
        state
            .replies
            .get(index)
            .cloned()
            .unwrap_or_else(|| Reply::result(Value::Null))
    };

    if let Some(delay) = reply.delay {
        thread::sleep(delay);
    }

    let response = format!(
        "HTTP/1.1 {} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        reply.status,
        reply.body.len(),
        reply.body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}
