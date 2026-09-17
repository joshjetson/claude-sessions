//! Assertions shared by more than one test module. Compiled only under `cfg(test)`.

/// Every `http://` or `https://` reference in a rendered page that would make
/// the browser fetch something.
///
/// Both generated pages — the journal viewer and the pipeline viewer — are
/// opened as `file://` on a laptop that may have no network at all, so a
/// stylesheet, script, font or image pulled from a URL is a bug, not a
/// nicety. Links the reader clicks are fine; resources the page loads are not.
///
/// Deliberately crude: it looks for a URL inside the attributes that cause a
/// load (`src`, `href` on a `<link>`, `url(...)` in CSS) rather than parsing
/// HTML, because a false positive here is cheap and a miss is the whole point.
pub fn external_resource_refs(html: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for (attribute, quoted) in [
        ("src=\"", true),
        ("<link", false),
        ("url(", false),
        ("@import", false),
    ] {
        let mut from = 0;
        while let Some(offset) = html[from..].find(attribute) {
            let start = from + offset;
            from = start + attribute.len();
            let window_end = (start + 400).min(html.len());
            let mut window_end = window_end;
            while !html.is_char_boundary(window_end) {
                window_end -= 1;
            }
            let window = &html[start..window_end];
            let scope = if quoted {
                window.split('"').nth(1).unwrap_or("")
            } else {
                window.split(['>', ';', ')']).next().unwrap_or("")
            };
            if scope.contains("http://") || scope.contains("https://") {
                hits.push(scope.to_string());
            }
        }
    }
    hits
}

/// What `/health` answered before a daemon said which program it was — the
/// shape the Node tool of the same name serves, and the shape this tool's own
/// v1.0.1 served.
pub const UNMARKED_HEALTH: &str =
    r#"{"ok":true,"daemon":true,"pid":4242,"uptime":12.5,"clients":1}"#;

/// A loopback daemon that is not this program.
///
/// It stands for the failure that cost three developers days: something else
/// holds the port. The TCP connect succeeds, `/health` may even answer, and a
/// dashboard that subscribes to it receives a stream it cannot read — so it
/// shows an empty list forever with nothing wrong anywhere. Every deadline and
/// every identity check on the client side exists for this, and none of them
/// can be trusted without a server that reproduces it.
pub struct StubDaemon {
    port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl StubDaemon {
    /// Accepts connections and then says nothing at all, ever.
    pub fn silent() -> Self {
        StubDaemon::start(None)
    }

    /// Answers `/health` without an implementation marker, and goes silent on
    /// everything else — a daemon of the older tool, seen from here.
    pub fn unmarked() -> Self {
        StubDaemon::start(Some(UNMARKED_HEALTH))
    }

    fn start(health: Option<&'static str>) -> Self {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        // Port 0: an ephemeral port, never a fixed one — a suite that pinned a
        // port would fight the user's own daemon on a working machine.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        listener.set_nonblocking(true).expect("non-blocking accept");
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let worker = std::thread::Builder::new()
            .name("stub-daemon".into())
            .spawn(move || {
                // Held, not dropped: closing a connection would be an answer,
                // and the point is that none arrives.
                let mut accepted = Vec::new();
                while !flag.load(Ordering::SeqCst) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    // macOS hands out accepted sockets with the listener's
                    // non-blocking flag, so without this the read below
                    // returns WouldBlock whenever the request has not landed
                    // yet and the stub answers nothing at random.
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
                    let mut buffer = [0u8; 1024];
                    let read = stream.read(&mut buffer).unwrap_or(0);
                    let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                    match health {
                        Some(body) if request.starts_with("GET /health") => {
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = stream.write_all(response.as_bytes());
                            let _ = stream.flush();
                        }
                        _ => accepted.push(stream),
                    }
                }
            })
            .expect("spawn");
        StubDaemon {
            port,
            stop,
            worker: Some(worker),
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for StubDaemon {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
