// SPDX-License-Identifier: EUPL-1.2
//! Notifications for incoming SMS and calls, to any number of `[[notify]]` targets:
//!
//! - `ntfy`: an ntfy topic URL (ntfy.sh or self-hosted), optional access token
//! - `webhook`: POST of a small JSON object to any URL, optional bearer token
//! - `meshtastic`: a text message over the Meshtastic TCP API, either a node with
//!   Wi-Fi/Ethernet (`host:4403`) or a muxtastic multiplexer (`host:4404`)
//! - `lxmf`: an LXMF message over Reticulum, sent with Leviculum's `lnmsg`
//!   through a running Reticulum daemon (`lnsd` or `rnsd`) on the stick
//!
//! HTTP goes through the `curl` binary, so dongie stays pure Rust without a TLS
//! stack of its own. curl gets its whole config (URL, token, body) on stdin, so
//! nothing secret shows up in the process list.
//!
//! The Meshtastic messages are encoded by hand from the documented protobuf wire
//! format (a handful of field numbers); no Meshtastic code or .proto file is used.

use crate::config::NotifyTarget;
use crate::contacts::Contacts;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::sync::RwLock;

pub const EVENTS: &[&str] = &["sms", "call", "missed_call", "voicemail"];

#[derive(Debug, Clone)]
pub enum Event {
    Sms {
        from: String,
        text: String,
        via: &'static str,
    },
    Call {
        from: String,
    },
    MissedCall {
        from: String,
    },
    /// the stick's voicemail recorded a message (secs long)
    Voicemail {
        from: String,
        secs: u64,
    },
    Test,
}

impl Event {
    pub fn key(&self) -> &'static str {
        match self {
            Event::Sms { .. } => "sms",
            Event::Call { .. } => "call",
            Event::MissedCall { .. } => "missed_call",
            Event::Voicemail { .. } => "voicemail",
            Event::Test => "test",
        }
    }

    fn from(&self) -> &str {
        match self {
            Event::Sms { from, .. }
            | Event::Call { from }
            | Event::MissedCall { from }
            | Event::Voicemail { from, .. } => from,
            Event::Test => "",
        }
    }

    fn title(&self, name: Option<&str>) -> String {
        let from = match (name, self.from()) {
            (_, "") => "unknown number".to_string(),
            (Some(n), f) => format!("{n} ({f})"),
            (None, f) => f.to_string(),
        };
        match self {
            Event::Sms { .. } => format!("SMS from {from}"),
            Event::Call { .. } => format!("Incoming call from {from}"),
            Event::MissedCall { .. } => format!("Missed call from {from}"),
            Event::Voicemail { .. } => format!("Voicemail from {from}"),
            Event::Test => "dongie test notification".into(),
        }
    }

    fn body(&self, include_text: bool) -> String {
        match self {
            Event::Sms { text, .. } if include_text => text.clone(),
            Event::Sms { text, .. } => format!("({} characters, text not included)", text.chars().count()),
            Event::Call { .. } => "Ringing on the stick (Wi-Fi calling)".into(),
            Event::MissedCall { .. } => "Nobody answered".into(),
            Event::Voicemail { secs, .. } => {
                format!("{}:{:02} message, listen to it in dongie (Mobile tab)", secs / 60, secs % 60)
            }
            Event::Test => "If you can read this, this notification target works.".into(),
        }
    }

    fn ntfy_tag(&self) -> &'static str {
        match self {
            Event::Sms { .. } => "speech_balloon",
            Event::Call { .. } => "telephone_receiver",
            Event::MissedCall { .. } => "no_bell",
            Event::Voicemail { .. } => "mailbox_with_mail",
            Event::Test => "white_check_mark",
        }
    }
}

pub type Sender = mpsc::UnboundedSender<Event>;

/// "ntfy ntfy.sh" / "meshtastic 192.168.1.5:4404": for logs and the UI, without
/// paths or tokens.
pub fn label(t: &NotifyTarget) -> String {
    let what = match t.kind.as_str() {
        "meshtastic" => t.host.clone(),
        "lxmf" => parse_lxmf(&t.to).map(|a| format!("{}…", &a[..8])).unwrap_or_default(),
        _ => t.url.split("://").nth(1).unwrap_or("").split('/').next().unwrap_or("").to_string(),
    };
    if t.name.is_empty() {
        format!("{} {what}", t.kind)
    } else {
        format!("{} ({} {what})", t.name, t.kind)
    }
}

pub fn validate(t: &NotifyTarget) -> Result<(), String> {
    let clean = |s: &str| !s.chars().any(|c| c.is_control() || c.is_whitespace());
    match t.kind.as_str() {
        "ntfy" | "webhook" => {
            if !(t.url.starts_with("https://") || t.url.starts_with("http://")) || !clean(&t.url) {
                return Err("url must be http(s):// without spaces".into());
            }
            if t.kind == "ntfy" && ntfy_split(&t.url).is_none() {
                return Err("ntfy url must end in the topic, e.g. https://ntfy.sh/mytopic".into());
            }
            if !clean(&t.token) {
                return Err("token must not contain spaces".into());
            }
        }
        "meshtastic" => {
            if t.host.rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok()).is_none() {
                return Err("host must be host:port (node 4403, muxtastic 4404)".into());
            }
            if !t.to.is_empty() && parse_node(&t.to).is_none() {
                return Err("to must be empty (broadcast) or a node id like !a1b2c3d4".into());
            }
            if t.channel > 7 {
                return Err("channel must be 0-7".into());
            }
        }
        "lxmf" => {
            if parse_lxmf(&t.to).is_none() {
                return Err("to must be an LXMF address: 32 hex characters".into());
            }
            if !["", "auto", "direct", "propagated"].contains(&t.via.as_str()) {
                return Err("via must be auto, direct or propagated".into());
            }
            if !t.pn.is_empty() && parse_lxmf(&t.pn).is_none() {
                return Err("pn (propagation node) must be empty or 32 hex characters".into());
            }
        }
        k => return Err(format!("unknown kind {k:?} (ntfy, webhook, meshtastic, lxmf)")),
    }
    if let Some(e) = t.events.iter().find(|e| !EVENTS.contains(&e.as_str())) {
        return Err(format!("unknown event {e:?} (sms, call, missed_call)"));
    }
    Ok(())
}

/// Start the sender task. Invalid targets are logged and skipped.
pub fn start(targets: &[NotifyTarget], contacts: Arc<RwLock<Contacts>>) -> Option<Sender> {
    let targets: Vec<NotifyTarget> = targets
        .iter()
        .filter(|t| match validate(t) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("notify: skipping {}: {e}", label(t));
                false
            }
        })
        .cloned()
        .collect();
    if targets.is_empty() {
        return None;
    }
    eprintln!("notify: {} target(s): {}", targets.len(), targets.iter().map(label).collect::<Vec<_>>().join(", "));
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let name = contacts.read().await.name_for(ev.from());
            for t in &targets {
                if t.events.iter().any(|e| e == ev.key()) {
                    let (t, ev, name) = (t.clone(), ev.clone(), name.clone());
                    tokio::spawn(async move {
                        match send(&t, &ev, name.as_deref()).await {
                            Ok(d) if !d.is_empty() => eprintln!("notify {}: {}: {d}", label(&t), ev.key()),
                            Ok(_) => {}
                            Err(e) => eprintln!("notify {}: {}: {e}", label(&t), ev.key()),
                        }
                    });
                }
            }
        }
    });
    Some(tx)
}

/// Ok(detail): detail says more than "sent" where there is more to say (LXMF:
/// delivered directly vs. left at a propagation node), else it is empty.
/// Every attempt is remembered for the UI (`recent()`).
pub async fn send(t: &NotifyTarget, ev: &Event, name: Option<&str>) -> Result<String, String> {
    let r = send_inner(t, ev, name).await;
    remember(t, ev, &r);
    r
}

async fn send_inner(t: &NotifyTarget, ev: &Event, name: Option<&str>) -> Result<String, String> {
    let (title, body) = (ev.title(name), ev.body(t.include_text));
    match t.kind.as_str() {
        "ntfy" => {
            let (base, topic) = ntfy_split(&t.url).ok_or("bad ntfy url")?;
            let msg = json!({ "topic": topic, "title": title, "message": body, "tags": [ev.ntfy_tag()] });
            curl_post(&format!("{base}/"), &msg.to_string(), &t.token).await.map(|_| String::new())
        }
        "webhook" => {
            let text = match ev {
                Event::Sms { text, .. } if t.include_text => Some(text.as_str()),
                _ => None,
            };
            let via = match ev {
                Event::Sms { via, .. } => Some(*via),
                _ => None,
            };
            let msg = json!({
                "event": ev.key(), "from": ev.from(), "name": name, "text": text, "via": via,
                "title": title, "at": crate::cmd::now(),
            });
            curl_post(&t.url, &msg.to_string(), &t.token).await.map(|_| String::new())
        }
        "lxmf" => lxmf_send(t, &title, &body).await,
        "meshtastic" => {
            let to = if t.to.is_empty() { BROADCAST } else { parse_node(&t.to).ok_or("bad node id")? };
            let text = truncate_bytes(&format!("{title}: {body}"), TEXT_MAX);
            meshtastic_send(&t.host, &text, to, t.channel).await.map(|_| String::new())
        }
        k => Err(format!("unknown kind {k}")),
    }
}

// ---------------- LXMF via lnmsg ----------------

/// Where lnmsg lives and keeps its state; set once at startup from the config.
#[derive(Debug, Clone)]
pub struct LxmfEnv {
    /// the lnmsg binary
    pub bin: String,
    /// LNMSG_HOME: a small tmpfs (RAM only), e.g. /run/lnmsg
    pub home: std::path::PathBuf,
    /// the sender identity, kept on flash in a 0700 directory
    pub identity: std::path::PathBuf,
    /// sender name recipients see
    pub from: String,
}

static LXMF: std::sync::OnceLock<LxmfEnv> = std::sync::OnceLock::new();

pub fn set_lxmf_env(env: LxmfEnv) {
    let _ = LXMF.set(env);
}

pub fn lxmf_env() -> Option<&'static LxmfEnv> {
    LXMF.get()
}

/// One sent (or failed) notification, for the UI: the last RECENT_MAX, RAM only.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Record {
    pub at: u64,
    pub target: String,
    pub kind: String,
    pub event: &'static str,
    pub ok: bool,
    pub detail: String,
}

const RECENT_MAX: usize = 30;
static RECENT: std::sync::Mutex<std::collections::VecDeque<Record>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());

fn remember(t: &NotifyTarget, ev: &Event, r: &Result<String, String>) {
    let rec = Record {
        at: crate::cmd::now(),
        target: label(t),
        kind: t.kind.clone(),
        event: ev.key(),
        ok: r.is_ok(),
        detail: match r {
            Ok(d) => d.clone(),
            Err(e) => e.clone(),
        },
    };
    if let Ok(mut q) = RECENT.lock() {
        q.push_front(rec);
        q.truncate(RECENT_MAX);
    }
}

/// Newest first.
pub fn recent() -> Vec<Record> {
    RECENT.lock().map(|q| q.iter().cloned().collect()).unwrap_or_default()
}

/// "lxmf@80fe…" or "80fe…" (32 hex) -> lowercase address
fn parse_lxmf(s: &str) -> Option<String> {
    let a = s.trim().strip_prefix("lxmf@").unwrap_or(s.trim()).to_ascii_lowercase();
    (a.len() == 32 && a.chars().all(|c| c.is_ascii_hexdigit())).then_some(a)
}

/// The identity lives on flash (`env.identity`, directory 0700: the key file
/// itself is created 0644 by lnmsg); LNMSG_HOME/identity is a symlink to it, so
/// everything else lnmsg writes stays in RAM. lnmsg creates the identity on
/// first use, through the symlink.
fn prepare_lnmsg_home(env: &LxmfEnv) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    if let Some(dir) = env.identity.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::create_dir_all(&env.home).map_err(|e| format!("{}: {e}", env.home.display()))?;
    let link = env.home.join("identity");
    match std::fs::read_link(&link) {
        Ok(target) if target == env.identity => Ok(()),
        Ok(_) | Err(_) => {
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&env.identity, &link).map_err(|e| format!("{}: {e}", link.display()))
        }
    }
}

/// What lnmsg's event log (LEVICULUM_EVENT_LOG) says about one send.
#[derive(Debug, Default, PartialEq)]
struct LnmsgOutcome {
    /// last LNMSG_STATE, e.g. "Sending", "Delivered"
    state: String,
    /// LNMSG_VIA method + reason, e.g. "direct", "link-delivery"
    method: String,
    reason: String,
}

fn event_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace().find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
}

fn parse_lnmsg_events(log: &str) -> LnmsgOutcome {
    let mut o = LnmsgOutcome::default();
    for l in log.lines() {
        if l.starts_with("LNMSG_STATE ") {
            if let Some(v) = event_field(l, "state") {
                o.state = v.to_string();
            }
        } else if l.starts_with("LNMSG_VIA ") {
            o.method = event_field(l, "method").unwrap_or_default().to_string();
            o.reason = event_field(l, "reason").unwrap_or_default().to_string();
        }
    }
    o
}

/// Human summary of an lnmsg run: exit 0 = on its way (or delivered, if the
/// proof came back before lnmsg exited), exit 3 = left at a propagation node.
fn lnmsg_summary(code: Option<i32>, o: &LnmsgOutcome) -> Option<String> {
    let how = if o.method.is_empty() { String::new() } else { format!(" ({})", o.method) };
    match code {
        Some(0) if o.state == "Delivered" => Some(format!("delivered{how}")),
        Some(0) => Some(format!("sent{how}, delivery not confirmed yet")),
        Some(3) => Some(
            "no direct link came up: left at a propagation node, the recipient's app collects it from there".into(),
        ),
        _ => None,
    }
}

async fn lxmf_send(t: &NotifyTarget, title: &str, body: &str) -> Result<String, String> {
    let env = LXMF.get().ok_or("LXMF not set up")?;
    let to = parse_lxmf(&t.to).ok_or("bad LXMF address")?;
    prepare_lnmsg_home(env)?;
    let via = if t.via.is_empty() { "auto" } else { t.via.as_str() };
    let mut args = vec!["send", "--from", &env.from, "--title", title, "--via", via, "--timeout", "30"];
    let pn = parse_lxmf(&t.pn);
    if let Some(pn) = &pn {
        args.extend(["--pn", pn.as_str()]);
    }
    args.push(&to);
    // one event log per send (concurrent sends must not share it), in the tmpfs
    let events = env.home.join(format!("send-{:08x}.events", random_u32()));
    let mut child = tokio::process::Command::new(&env.bin)
        .args(&args)
        .env("LNMSG_HOME", &env.home)
        .env("LEVICULUM_EVENT_LOG", &events)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("{}: {e}", env.bin))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(body.as_bytes()).await.map_err(|e| format!("lnmsg stdin: {e}"))?;
    }
    let out = tokio::time::timeout(Duration::from_secs(90), child.wait_with_output())
        .await
        .map_err(|_| "lnmsg timed out".to_string())?
        .map_err(|e| format!("lnmsg: {e}"))?;
    let outcome = parse_lnmsg_events(&std::fs::read_to_string(&events).unwrap_or_default());
    let _ = std::fs::remove_file(&events);
    match lnmsg_summary(out.status.code(), &outcome) {
        Some(s) => Ok(s),
        None => Err(String::from_utf8_lossy(&out.stderr).trim().chars().take(200).collect()),
    }
}

// ---------------- HTTP via curl ----------------

/// "https://ntfy.sh/mytopic" -> ("https://ntfy.sh", "mytopic")
fn ntfy_split(url: &str) -> Option<(&str, &str)> {
    let (base, topic) = url.trim_end_matches('/').rsplit_once('/')?;
    let ok = !topic.is_empty() && topic.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    (ok && base.contains("://") && !base.ends_with(':') && !base.ends_with('/')).then_some((base, topic))
}

/// A double-quoted string in curl's config file syntax.
fn curl_quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn curl_config(url: &str, body: &str, token: &str) -> String {
    let mut c = format!("url = {}\nheader = \"Content-Type: application/json\"\n", curl_quote(url));
    if !token.is_empty() {
        c += &format!("header = {}\n", curl_quote(&format!("Authorization: Bearer {token}")));
    }
    c += &format!("data-binary = {}\n", curl_quote(body));
    c
}

async fn curl_post(url: &str, body: &str, token: &str) -> Result<(), String> {
    let mut child = tokio::process::Command::new("curl")
        .args(["-sS", "--fail", "--max-time", "20", "--proto", "=https,http", "-o", "/dev/null", "-K", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(curl_config(url, body, token).as_bytes()).await.map_err(|e| format!("curl stdin: {e}"))?;
    }
    let out = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .map_err(|_| "curl timed out".to_string())?
        .map_err(|e| format!("curl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().chars().take(200).collect())
    }
}

// ---------------- Meshtastic TCP API ----------------

const BROADCAST: u32 = 0xFFFF_FFFF;
/// Data.payload limit is 233 bytes; leave room.
const TEXT_MAX: usize = 200;
const PORT_TEXT_MESSAGE: u64 = 1;
const HOP_LIMIT: u64 = 3;

/// "!a1b2c3d4" -> 0xa1b2c3d4
fn parse_node(s: &str) -> Option<u32> {
    let h = s.strip_prefix('!')?;
    (h.len() == 8).then(|| u32::from_str_radix(h, 16).ok()).flatten()
}

fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max - 3;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

fn varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn f_varint(out: &mut Vec<u8>, field: u64, v: u64) {
    varint(field << 3, out);
    varint(v, out);
}

fn f_fixed32(out: &mut Vec<u8>, field: u64, v: u32) {
    varint((field << 3) | 5, out);
    out.extend_from_slice(&v.to_le_bytes());
}

fn f_bytes(out: &mut Vec<u8>, field: u64, b: &[u8]) {
    varint((field << 3) | 2, out);
    varint(b.len() as u64, out);
    out.extend_from_slice(b);
}

/// ToRadio { packet: MeshPacket { to, channel, decoded: Data { portnum: TEXT_MESSAGE_APP, payload }, id, hop_limit } }
fn to_radio_text(text: &str, to: u32, channel: u32, id: u32) -> Vec<u8> {
    let mut data = vec![];
    f_varint(&mut data, 1, PORT_TEXT_MESSAGE); // Data.portnum
    f_bytes(&mut data, 2, text.as_bytes()); // Data.payload
    let mut pkt = vec![];
    f_fixed32(&mut pkt, 2, to); // MeshPacket.to
    if channel > 0 {
        f_varint(&mut pkt, 3, channel as u64); // MeshPacket.channel
    }
    f_bytes(&mut pkt, 4, &data); // MeshPacket.decoded
    f_fixed32(&mut pkt, 6, id); // MeshPacket.id
    f_varint(&mut pkt, 9, HOP_LIMIT); // MeshPacket.hop_limit
    let mut out = vec![];
    f_bytes(&mut out, 1, &pkt); // ToRadio.packet
    out
}

/// ToRadio { want_config_id }
fn to_radio_want_config(nonce: u32) -> Vec<u8> {
    let mut out = vec![];
    f_varint(&mut out, 3, nonce as u64);
    out
}

/// ToRadio { disconnect: true }
fn to_radio_disconnect() -> Vec<u8> {
    let mut out = vec![];
    f_varint(&mut out, 4, 1);
    out
}

/// Stream framing: 0x94 0xC3, big-endian u16 length, protobuf.
fn frame(payload: &[u8]) -> Vec<u8> {
    let mut f = vec![0x94, 0xC3, (payload.len() >> 8) as u8, payload.len() as u8];
    f.extend_from_slice(payload);
    f
}

fn read_varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*i)?;
        *i += 1;
        v |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// FromRadio.config_complete_id (field 7), if this message is one.
fn config_complete_id(msg: &[u8]) -> Option<u32> {
    let mut i = 0;
    while i < msg.len() {
        let key = read_varint(msg, &mut i)?;
        let (field, wire) = (key >> 3, key & 7);
        match wire {
            0 => {
                let v = read_varint(msg, &mut i)?;
                if field == 7 {
                    return u32::try_from(v).ok();
                }
            }
            1 => i += 8,
            2 => {
                let n = read_varint(msg, &mut i)? as usize;
                i = i.checked_add(n)?;
            }
            5 => i += 4,
            _ => return None,
        }
    }
    None
}

/// Pull complete frames out of `buf`, dropping garbage before a frame start.
fn take_frames(buf: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut frames = vec![];
    loop {
        match buf.windows(2).position(|w| w == [0x94, 0xC3]) {
            Some(0) => {}
            Some(p) => {
                buf.drain(..p);
            }
            None => {
                let keep = usize::from(buf.last() == Some(&0x94));
                buf.drain(..buf.len() - keep);
                return frames;
            }
        }
        if buf.len() < 4 {
            return frames;
        }
        let n = ((buf[2] as usize) << 8) | buf[3] as usize;
        if buf.len() < 4 + n {
            return frames;
        }
        frames.push(buf[4..4 + n].to_vec());
        buf.drain(..4 + n);
    }
}

fn random_u32() -> u32 {
    let mut b = [0u8; 4];
    // if the OS RNG fails, a zero id still works (the node assigns its own)
    let _ = rand_core::RngCore::try_fill_bytes(&mut rand_core::OsRng, &mut b);
    u32::from_le_bytes(b)
}

/// Connect, do the want_config handshake (muxtastic and the firmware only route
/// packets from configured clients), send the text, say goodbye.
async fn meshtastic_send(host: &str, text: &str, to: u32, channel: u32) -> Result<(), String> {
    let mut s = tokio::time::timeout(Duration::from_secs(10), tokio::net::TcpStream::connect(host))
        .await
        .map_err(|_| "connect timed out".to_string())?
        .map_err(|e| format!("connect: {e}"))?;
    let nonce = random_u32() | 1;
    s.write_all(&frame(&to_radio_want_config(nonce))).await.map_err(|e| format!("write: {e}"))?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    let done = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let n = s.read(&mut chunk).await.map_err(|e| format!("read: {e}"))?;
            if n == 0 {
                return Err("connection closed during handshake".to_string());
            }
            buf.extend_from_slice(&chunk[..n]);
            if take_frames(&mut buf).iter().any(|f| config_complete_id(f) == Some(nonce)) {
                return Ok(());
            }
        }
    })
    .await;
    match done {
        Ok(r) => r?,
        Err(_) => return Err("no config_complete from the node within 30 s".into()),
    }
    s.write_all(&frame(&to_radio_text(text, to, channel, random_u32()))).await.map_err(|e| format!("write: {e}"))?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = s.write_all(&frame(&to_radio_disconnect())).await;
    let _ = s.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(kind: &str) -> NotifyTarget {
        NotifyTarget { kind: kind.into(), events: vec!["sms".into()], ..Default::default() }
    }

    #[test]
    fn validation() {
        let mut t = target("ntfy");
        t.url = "https://ntfy.sh/dongie_test-1".into();
        assert!(validate(&t).is_ok());
        t.url = "https://ntfy.sh/".into();
        assert!(validate(&t).is_err());
        t.url = "ftp://x/y".into();
        assert!(validate(&t).is_err());
        let mut m = target("meshtastic");
        m.host = "192.168.1.5:4404".into();
        assert!(validate(&m).is_ok());
        m.to = "!a1b2c3d4".into();
        assert!(validate(&m).is_ok());
        m.to = "a1b2".into();
        assert!(validate(&m).is_err());
        m.to.clear();
        m.events = vec!["sms".into(), "bogus".into()];
        assert!(validate(&m).is_err());
        assert!(validate(&target("email")).is_err());
    }

    #[test]
    fn ntfy_and_curl() {
        assert_eq!(ntfy_split("https://ntfy.sh/abc"), Some(("https://ntfy.sh", "abc")));
        assert_eq!(ntfy_split("https://n.example.org:8080/a_b/"), Some(("https://n.example.org:8080", "a_b")));
        assert_eq!(ntfy_split("https://ntfy.sh"), None);
        assert_eq!(curl_quote("a\"b\\c\nd"), r#""a\"b\\c\nd""#);
        let c = curl_config("https://x/", "{\"t\":\"hi\"}", "tk");
        assert!(c.contains("header = \"Authorization: Bearer tk\"") && c.contains(r#"data-binary = "{\"t\":\"hi\"}""#));
        assert!(!curl_config("https://x/", "{}", "").contains("Authorization"));
    }

    #[test]
    fn labels_hide_paths_and_tokens() {
        let mut t = target("ntfy");
        t.url = "https://ntfy.example.org/secret-topic".into();
        t.token = "tk_123".into();
        assert_eq!(label(&t), "ntfy ntfy.example.org");
    }

    #[test]
    fn meshtastic_encoding() {
        // hand-checked against the wire format:
        // ToRadio{1: MeshPacket{2: fixed32 to, 4: Data{1: 1, 2: "hi"}, 6: fixed32 id, 9: 3}}
        let b = to_radio_text("hi", BROADCAST, 0, 0x01020304);
        let data = [0x08, 0x01, 0x12, 0x02, b'h', b'i'];
        let mut pkt = vec![0x15, 0xff, 0xff, 0xff, 0xff, 0x22, data.len() as u8];
        pkt.extend_from_slice(&data);
        pkt.extend_from_slice(&[0x35, 0x04, 0x03, 0x02, 0x01, 0x48, 0x03]);
        let mut want = vec![0x0a, pkt.len() as u8];
        want.extend_from_slice(&pkt);
        assert_eq!(b, want);
        // channel set -> field 3 varint
        assert!(to_radio_text("hi", 1, 2, 0).windows(2).any(|w| w == [0x18, 0x02]));
        assert_eq!(to_radio_want_config(300), vec![0x18, 0xac, 0x02]);
        assert_eq!(to_radio_disconnect(), vec![0x20, 0x01]);
        assert_eq!(frame(&[1, 2, 3]), vec![0x94, 0xc3, 0, 3, 1, 2, 3]);
        assert_eq!(parse_node("!a1b2c3d4"), Some(0xa1b2c3d4));
        assert_eq!(parse_node("!a1b2"), None);
    }

    #[test]
    fn meshtastic_frames_and_handshake() {
        // FromRadio{1: id 5, 7: config_complete_id 300}
        let msg = [0x08, 0x05, 0x38, 0xac, 0x02];
        assert_eq!(config_complete_id(&msg), Some(300));
        assert_eq!(config_complete_id(&[0x08, 0x05]), None);
        let mut buf = vec![0x00, 0x13];
        buf.extend(frame(&msg));
        buf.extend(frame(&[0x08, 0x01]));
        buf.extend([0x94, 0xc3, 0x00]); // incomplete header stays
        let f = take_frames(&mut buf);
        assert_eq!(f, vec![msg.to_vec(), vec![0x08, 0x01]]);
        assert_eq!(buf, vec![0x94, 0xc3, 0x00]);
    }

    #[test]
    fn lxmf_targets() {
        let mut t = target("lxmf");
        t.to = "lxmf@0123456789ABCDEF0123456789ABCDEF".into();
        assert!(validate(&t).is_ok());
        assert_eq!(label(&t), "lxmf 01234567…");
        t.via = "propagated".into();
        assert!(validate(&t).is_ok());
        t.via = "pigeon".into();
        assert!(validate(&t).is_err());
        t.via.clear();
        t.to = "01234567".into();
        assert!(validate(&t).is_err());
    }

    #[test]
    fn lnmsg_event_log() {
        // excerpt of a real LEVICULUM_EVENT_LOG from a stick
        let log = "LNMSG_RESOLVED node=local dst=4d7b waited_ms=215 t=221\n\
                   LNMSG_ENQUEUED node=local bytes=17 dst=4d7b id=243b via=link t=440\n\
                   LNMSG_STATE node=local id=243b state=Sending t=590\n\
                   LNMSG_STATE node=local id=243b state=Delivered t=696\n\
                   LNMSG_VIA node=local method=direct reason=link-delivery t=696\n\
                   LNMSG_DONE node=local code=0 id=243b outcome=queued t=696\n";
        let o = parse_lnmsg_events(log);
        assert_eq!(
            o,
            LnmsgOutcome { state: "Delivered".into(), method: "direct".into(), reason: "link-delivery".into() }
        );
        assert_eq!(lnmsg_summary(Some(0), &o).as_deref(), Some("delivered (direct)"));
        assert_eq!(
            lnmsg_summary(Some(0), &LnmsgOutcome::default()).as_deref(),
            Some("sent, delivery not confirmed yet")
        );
        assert!(lnmsg_summary(Some(3), &o).unwrap().contains("propagation node"));
        assert_eq!(lnmsg_summary(Some(1), &o), None);
        let mut t = target("lxmf");
        t.to = "0123456789abcdef0123456789abcdef".into();
        t.pn = "nope".into();
        assert!(validate(&t).is_err());
    }

    #[test]
    fn lnmsg_home_symlink() {
        let d = std::env::temp_dir().join(format!("dongie-lnmsg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let env = LxmfEnv {
            bin: "lnmsg".into(),
            home: d.join("run"),
            identity: d.join("state/lxmf/identity"),
            from: "x".into(),
        };
        prepare_lnmsg_home(&env).unwrap();
        prepare_lnmsg_home(&env).unwrap(); // idempotent
        assert_eq!(std::fs::read_link(d.join("run/identity")).unwrap(), env.identity);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(d.join("state/lxmf")).unwrap().permissions().mode() & 0o777, 0o700);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn titles_with_contact_names() {
        let ev = Event::Sms { from: "+34600112233".into(), text: "hola".into(), via: "modem" };
        assert_eq!(ev.title(Some("Mamá")), "SMS from Mamá (+34600112233)");
        assert_eq!(ev.title(None), "SMS from +34600112233");
        assert_eq!(Event::MissedCall { from: String::new() }.title(Some("x")), "Missed call from unknown number");
    }

    #[test]
    fn text_truncation() {
        let s = "ñ".repeat(150); // 300 bytes
        let t = truncate_bytes(&s, TEXT_MAX);
        assert!(t.len() <= TEXT_MAX && t.ends_with("..."));
        assert_eq!(truncate_bytes("short", TEXT_MAX), "short");
    }
}
