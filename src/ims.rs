// SPDX-License-Identifier: EUPL-1.2
//! Talks to dongie-ims (separate GPL-3.0 Go service, runs as root) over its unix
//! socket `/run/dongie-ims/api.sock` (root:dongie 0660).
//!
//! - status: fetched by the collector on every poll, shown on the Mobile tab
//! - calls: RAM call log mirrored from dongie-ims (admin only, contains numbers)
//! - SMS that the network delivered over IMS instead of through the modem are
//!   copied into dongie's normal SMS store, so they show up in Messages.

use crate::cmd;
use crate::sms::Sms;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

const TIMEOUT: Duration = Duration::from_secs(4);
const MAX_RESPONSE: usize = 256 * 1024;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ImsStatus {
    pub version: String,
    pub access: String,
    pub status: String,
    pub registered: bool,
    pub since: u64,
    pub started: u64,
    pub registrations: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub last_error_at: u64,
    /// own number as the IMS core sees it (sensitive)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msisdn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inner_ip: Option<String>,
    pub pcscf: Vec<String>,
    pub on_call: String,
    pub active_calls: u64,
}

impl ImsStatus {
    pub fn for_guest(&mut self) {
        self.msisdn = None;
        self.inner_ip = None;
        self.pcscf.clear();
        self.last_error = None; // may contain identities / addresses
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Call {
    pub id: u64,
    pub codec: String,
    pub answered_at: u64,
    pub browser: String,
    pub dir: String,
    pub from: String,
    pub at: u64,
    pub state: String,
    pub ended: bool,
    pub ended_at: u64,
    /// ringing | answered | missed | rejected
    pub result: String,
    pub cause: String,
    pub codecs: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ImsSms {
    id: u64,
    from: String,
    text: String,
    at: u64,
}

/// Minimal HTTP/1.0 over the unix socket. Returns (status code, body).
pub async fn request(socket: &str, method: &str, path: &str) -> Option<(u16, String)> {
    request_body(socket, method, path, "", TIMEOUT).await
}

/// Same with a JSON body and a custom timeout (answering waits for the IMS side).
pub async fn request_body(socket: &str, method: &str, path: &str, body: &str, timeout: Duration) -> Option<(u16, String)> {
    if socket.is_empty() {
        return None;
    }
    let fut = async {
        let mut s = UnixStream::connect(socket).await.ok()?;
        let req = format!(
            "{method} {path} HTTP/1.0\r\nHost: dongie-ims\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).await.ok()?;
        let mut buf = Vec::new();
        s.take(MAX_RESPONSE as u64).read_to_end(&mut buf).await.ok()?;
        parse_response(&buf)
    };
    tokio::time::timeout(timeout, fut).await.ok().flatten()
}

fn parse_response(buf: &[u8]) -> Option<(u16, String)> {
    let text = String::from_utf8_lossy(buf);
    let (head, body) = text.split_once("\r\n\r\n")?;
    let code = head.split_whitespace().nth(1)?.parse().ok()?;
    Some((code, body.to_string()))
}

async fn get_json<T: for<'de> Deserialize<'de>>(socket: &str, path: &str) -> Option<T> {
    match request(socket, "GET", path).await? {
        (200, body) => serde_json::from_str(&body).ok(),
        _ => None,
    }
}

/// Status for the collector; None when dongie-ims isn't running / not installed.
pub async fn status(socket: &str) -> Option<ImsStatus> {
    get_json(socket, "/v1/status").await
}

#[derive(Clone)]
pub struct Ims {
    pub socket: String,
    pub calls: Arc<Mutex<Vec<Call>>>,
}

impl Ims {
    pub fn new(socket: String) -> Ims {
        Ims { socket, calls: Arc::new(Mutex::new(vec![])) }
    }

    /// Mirror the call log and pull IMS-delivered SMS into the SMS store, forever.
    pub async fn run(self, sms: Option<Sms>) {
        #[derive(Deserialize)]
        struct Calls {
            calls: Vec<Call>,
        }
        #[derive(Deserialize)]
        struct Msgs {
            messages: Vec<ImsSms>,
        }
        // dongie-ims restarts reset its ids; track its start time to notice.
        let mut started = 0u64;
        let mut after = 0u64;
        loop {
            if let Some(st) = status(&self.socket).await {
                if st.started != started {
                    started = st.started;
                    after = 0;
                }
                if let Some(c) = get_json::<Calls>(&self.socket, "/v1/calls").await {
                    let mut calls = c.calls;
                    calls.reverse(); // newest first
                    *self.calls.lock().await = calls;
                }
                if let Some(sms) = &sms {
                    if let Some(m) = get_json::<Msgs>(&self.socket, &format!("/v1/messages?after={after}")).await {
                        for msg in m.messages {
                            after = after.max(msg.id);
                            let ts = crate::sms::rfc3339(if msg.at > 0 { msg.at } else { cmd::now() });
                            if sms.store.lock().await.push_incoming(msg.from.clone(), msg.text.clone(), ts, "ims") {
                                eprintln!("sms: received over IMS ({} chars)", msg.text.chars().count());
                            }
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    /// Pass a call-control request through to dongie-ims (answer, hangup, dtmf,
    /// dial). Returns dongie-ims' status code and JSON body.
    pub async fn forward(&self, path: &str, body: &serde_json::Value) -> (u16, serde_json::Value) {
        let body = body.to_string();
        match request_body(&self.socket, "POST", path, &body, Duration::from_secs(40)).await {
            Some((code, text)) => (
                code,
                serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({ "error": "bad reply from dongie-ims" })),
            ),
            None => (502, serde_json::json!({ "error": "dongie-ims not reachable" })),
        }
    }

    pub async fn reject(&self, id: u64) -> Result<(), String> {
        match request(&self.socket, "POST", &format!("/v1/calls/{id}/reject")).await {
            Some((200, _)) => Ok(()),
            Some((_, body)) => Err(serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["error"].as_str().map(String::from))
                .unwrap_or_else(|| "dongie-ims error".into())),
            None => Err("dongie-ims not reachable".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_and_status() {
        let raw = b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{\"status\":\"registered\",\"registered\":true,\"since\":5,\"msisdn\":\"+34600000000\",\"pcscf\":[\"10.0.0.1\"],\"extra\":1}\n";
        let (code, body) = parse_response(raw).unwrap();
        assert_eq!(code, 200);
        let mut s: ImsStatus = serde_json::from_str(&body).unwrap();
        assert!(s.registered);
        assert_eq!(s.since, 5);
        s.for_guest();
        assert!(s.msisdn.is_none() && s.pcscf.is_empty());
        assert!(parse_response(b"garbage").is_none());
    }
}
