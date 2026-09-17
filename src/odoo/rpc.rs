//! The JSON-RPC envelope and the HTTP transport underneath it.
//!
//! Odoo speaks JSON-RPC over HTTPS (not XML-RPC): one POST to `{url}/jsonrpc`
//! carrying `{jsonrpc, method: "call", params: {service, method, args}, id}`.
//! Everything else in this module is built on the two services that envelope
//! exposes — `common.authenticate` and `object.execute_kw`.
//!
//! The transport is a trait so the tests can drive the real client against a
//! `std::net::TcpListener` stub instead of the network, and so the daemon can
//! substitute a recorded transport later.

use std::fmt;
use std::time::Duration;

use serde_json::{json, Value};

/// How long a single request may take before it is abandoned.
///
/// Deviation from the Node original, which passed no timeout to `fetch`: a
/// hung Odoo (or a VPN that dropped while the dashboard was open) froze the
/// refresh thread until the process was killed.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Response bodies are read up to this size. A full board is a megabyte or two;
/// the cap exists so a misconfigured URL answering with a video cannot be read
/// into memory.
const MAX_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;

/// Everything that can go wrong between "we have credentials" and "we have a
/// result". Kept as distinct variants because the caller acts on them
/// differently: [`OdooError::AuthRejected`] is the one that invalidates the
/// cached uid, the rest are reported and retried on the next refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OdooError {
    /// Credentials are incomplete — the named fields are empty.
    MissingCredentials(Vec<&'static str>),
    /// Odoo answered the authenticate call with `false`.
    AuthRejected(String),
    /// The request never completed (DNS, TLS, connection, timeout).
    Transport(String),
    /// Odoo returned a JSON-RPC `error` object.
    Rpc(String),
    /// A 200 that was not the JSON we expected.
    BadResponse(String),
}

impl OdooError {
    /// True when the cached uid should be dropped and authentication retried.
    pub fn is_auth(&self) -> bool {
        matches!(
            self,
            OdooError::AuthRejected(_) | OdooError::MissingCredentials(_)
        )
    }
}

impl fmt::Display for OdooError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OdooError::MissingCredentials(missing) => write!(
                f,
                "Missing Odoo creds: {} (set ODOO_* env or odoo block in ~/.claude-sessions.json)",
                missing.join(", ")
            ),
            OdooError::AuthRejected(detail) => write!(f, "auth rejected — {detail}"),
            OdooError::Transport(detail) => write!(f, "Odoo unreachable: {detail}"),
            OdooError::Rpc(message) => write!(f, "{message}"),
            OdooError::BadResponse(detail) => write!(f, "unexpected Odoo response: {detail}"),
        }
    }
}

impl std::error::Error for OdooError {}

pub type Result<T> = std::result::Result<T, OdooError>;

/// One POST of a JSON body, returning the response body as text.
///
/// Status codes are deliberately not interpreted here: Odoo answers a failed
/// call with a 200 carrying an `error` object, and a 500 carrying the same
/// shape, so the body is what decides.
pub trait Transport: Send + Sync {
    fn post_json(&self, url: &str, body: &str) -> Result<String>;
}

/// The real transport: a pooled [`ureq`] agent with a global timeout.
pub struct HttpTransport {
    agent: ureq::Agent,
}

impl HttpTransport {
    pub fn new(timeout: Duration) -> Self {
        HttpTransport {
            // `false`: Odoo puts the useful error in the body of a non-2xx
            // response, so the status must not short-circuit reading it.
            agent: crate::http::agent(timeout, false),
        }
    }
}

impl Default for HttpTransport {
    fn default() -> Self {
        HttpTransport::new(DEFAULT_TIMEOUT)
    }
}

impl Transport for HttpTransport {
    fn post_json(&self, url: &str, body: &str) -> Result<String> {
        let mut response = self
            .agent
            .post(url)
            .header("Content-Type", "application/json")
            .send(body)
            .map_err(|err| OdooError::Transport(err.to_string()))?;
        response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_string()
            .map_err(|err| OdooError::Transport(err.to_string()))
    }
}

/// The JSON-RPC envelope: build the request, post it, unwrap `result`.
pub fn call(
    transport: &dyn Transport,
    base_url: &str,
    service: &str,
    method: &str,
    args: Vec<Value>,
) -> Result<Value> {
    let body = json!({
        "jsonrpc": "2.0",
        "method": "call",
        "params": { "service": service, "method": method, "args": args },
        "id": 1,
    });
    let text = transport.post_json(&format!("{base_url}/jsonrpc"), &body.to_string())?;
    let parsed: Value = serde_json::from_str(&text).map_err(|err| {
        OdooError::BadResponse(format!("{err} (body starts: {})", preview(&text)))
    })?;

    if let Some(error) = parsed.get("error") {
        // Odoo nests the readable message under data.message; the outer message
        // is usually the generic "Odoo Server Error".
        let message = error
            .pointer("/data/message")
            .and_then(Value::as_str)
            .or_else(|| error.get("message").and_then(Value::as_str))
            .unwrap_or("Odoo RPC error");
        return Err(OdooError::Rpc(message.to_string()));
    }
    Ok(parsed.get("result").cloned().unwrap_or(Value::Null))
}

/// The first bytes of a body, for an error message that says what arrived
/// instead of JSON (usually an HTML login page from a wrong URL).
fn preview(text: &str) -> String {
    let head: String = text.chars().take(80).collect();
    head.replace('\n', " ")
}
