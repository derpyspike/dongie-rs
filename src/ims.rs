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
    /// configured mode (dongie-ims >= VoLTE support; empty from older ones)
    #[serde(default)]
    pub mode: String,
    /// running on the mode's second access
    #[serde(default)]
    pub fallback: bool,
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
    /// dongie side: which dongie-ims run the id belongs to (ids restart with it)
    pub session: u64,
    /// dongie side: from the kept call log, not dongie-ims' live list (no call control)
    pub archived: bool,
    pub codec: String,
    pub answered_at: u64,
    pub browser: String,
    pub dir: String,
    pub from: String,
    pub at: u64,
    pub state: String,
    pub ended: bool,
    pub ended_at: u64,
    /// ringing | answered | voicemail | missed | rejected
    pub result: String,
    /// id of the recording when the stick's voicemail took the call
    pub voicemail: String,
    pub voicemail_secs: u64,
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
pub async fn request_body(
    socket: &str,
    method: &str,
    path: &str,
    body: &str,
    timeout: Duration,
) -> Option<(u16, String)> {
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

/// GET with a binary body (voicemail WAV), up to 12 MB.
pub async fn request_raw(socket: &str, path: &str) -> Option<(u16, Vec<u8>)> {
    if socket.is_empty() {
        return None;
    }
    let fut = async {
        let mut s = UnixStream::connect(socket).await.ok()?;
        s.write_all(format!("GET {path} HTTP/1.0\r\nHost: dongie-ims\r\n\r\n").as_bytes()).await.ok()?;
        let mut buf = Vec::new();
        s.take(12 << 20).read_to_end(&mut buf).await.ok()?;
        let i = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
        let head = String::from_utf8_lossy(&buf[..i]);
        let code = head.split_whitespace().nth(1)?.parse().ok()?;
        Some((code, buf[i + 4..].to_vec()))
    };
    tokio::time::timeout(Duration::from_secs(15), fut).await.ok().flatten()
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
    /// Ended calls, kept across dongie-ims restarts (and, with persist = true,
    /// across dongie restarts), oldest first, at most ARCHIVE_MAX.
    pub archive: Arc<Mutex<Vec<Call>>>,
}

pub const ARCHIVE_MAX: usize = 200;

/// Live calls plus archived ones that aren't in the live list, newest first.
/// Archived entries get id 0 so they can't be mistaken for a live call.
pub fn merged(live: &[Call], archive: &[Call]) -> Vec<Call> {
    let mut out: Vec<Call> = live.to_vec();
    for a in archive.iter().rev() {
        if !live.iter().any(|l| l.session == a.session && l.id == a.id) {
            out.push(Call { id: 0, archived: true, ..a.clone() });
        }
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.at));
    out.truncate(ARCHIVE_MAX);
    out
}

fn archive_ended(archive: &mut Vec<Call>, calls: &[Call]) {
    for c in calls.iter().filter(|c| c.ended) {
        match archive.iter_mut().find(|a| a.session == c.session && a.id == c.id) {
            Some(a) => *a = c.clone(),
            None => archive.push(c.clone()),
        }
    }
    if archive.len() > ARCHIVE_MAX {
        let extra = archive.len() - ARCHIVE_MAX;
        archive.drain(..extra);
    }
}

impl Ims {
    pub fn new(socket: String) -> Ims {
        Ims { socket, calls: Arc::new(Mutex::new(vec![])), archive: Arc::new(Mutex::new(vec![])) }
    }

    /// Mirror the call log and pull IMS-delivered SMS into the SMS store, forever.
    pub async fn run(self, sms: Option<Sms>, events: Option<crate::notify::Sender>) {
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
        // call notifications: ids already notified as ringing / missed; on the first
        // look after a (re)start everything present counts as seen (no flood of old calls)
        let mut rang = std::collections::HashSet::new();
        let mut missed = std::collections::HashSet::new();
        let mut voicemails = std::collections::HashSet::new();
        let mut primed = false;
        loop {
            if let Some(st) = status(&self.socket).await {
                if st.started != started {
                    started = st.started;
                    after = 0;
                    rang.clear();
                    missed.clear();
                    voicemails.clear();
                    primed = false;
                }
                if let Some(c) = get_json::<Calls>(&self.socket, "/v1/calls").await {
                    let mut calls = c.calls;
                    calls.iter_mut().for_each(|c| c.session = started);
                    archive_ended(&mut *self.archive.lock().await, &calls);
                    for call in calls.iter().filter(|c| c.dir == "in") {
                        if !call.ended && rang.insert(call.id) && primed {
                            notify(&events, crate::notify::Event::Call { from: call.from.clone() });
                        }
                        if call.ended && call.result == "missed" && missed.insert(call.id) && primed {
                            notify(&events, crate::notify::Event::MissedCall { from: call.from.clone() });
                        }
                        // the recording is saved a moment after the call ends
                        if call.result == "voicemail"
                            && !call.voicemail.is_empty()
                            && voicemails.insert(call.id)
                            && primed
                        {
                            notify(
                                &events,
                                crate::notify::Event::Voicemail { from: call.from.clone(), secs: call.voicemail_secs },
                            );
                        }
                        // taken by the voicemail but nothing said after the beep
                        if call.ended
                            && call.result == "voicemail"
                            && call.voicemail.is_empty()
                            && cmd::now().saturating_sub(call.ended_at) > 15
                            && voicemails.insert(call.id)
                            && primed
                        {
                            notify(&events, crate::notify::Event::MissedCall { from: call.from.clone() });
                        }
                    }
                    primed = true;
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
                serde_json::from_str(&text)
                    .unwrap_or_else(|_| serde_json::json!({ "error": "bad reply from dongie-ims" })),
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

#[cfg(test)]
mod archive_tests {
    use super::*;

    fn call(session: u64, id: u64, at: u64, ended: bool) -> Call {
        Call { session, id, at, ended, dir: "in".into(), ..Default::default() }
    }

    #[test]
    fn archive_and_merge() {
        let mut arch = vec![];
        // dongie-ims run 1: one ended, one still ringing
        archive_ended(&mut arch, &[call(1, 1, 100, true), call(1, 2, 200, false)]);
        assert_eq!(arch.len(), 1);
        // run 2 restarts the ids at 1
        let live = vec![call(2, 1, 300, false)];
        let m = merged(&live, &arch);
        assert_eq!(
            m.iter().map(|c| (c.id, c.at, c.archived)).collect::<Vec<_>>(),
            vec![(1, 300, false), (0, 100, true)]
        );
        // an ended live call is not listed twice
        archive_ended(&mut arch, &[call(2, 1, 300, true)]);
        assert_eq!(merged(&[call(2, 1, 300, true)], &arch).len(), 2);
        for i in 0..250 {
            archive_ended(&mut arch, &[call(3, i, 1000 + i, true)]);
        }
        assert_eq!(arch.len(), ARCHIVE_MAX);
    }
}

fn notify(events: &Option<crate::notify::Sender>, ev: crate::notify::Event) {
    if let Some(tx) = events {
        let _ = tx.send(ev);
    }
}
