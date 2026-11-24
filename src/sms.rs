// SPDX-License-Identifier: EUPL-1.2
//! SMS via ModemManager's D-Bus API. Messages live in RAM only.
//!
//! - Incoming SMS are copied into RAM and then deleted from the modem/SIM, so the
//!   SIM never fills up. The modem often stores one SMS twice (SIM + modem memory);
//!   we de-duplicate by (number, timestamp, text).
//! - Delivery reports arrive as separate SMS (pdu type status-report) and are
//!   matched to our sent message by the message reference.

use crate::cmd;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use zbus::proxy::{Builder, CacheProperties};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, Value};
use zbus::{Connection, Proxy};

const MM: &str = "org.freedesktop.ModemManager1";
const MM_PATH: &str = "/org/freedesktop/ModemManager1";
const IF_MESSAGING: &str = "org.freedesktop.ModemManager1.Modem.Messaging";
const IF_SMS: &str = "org.freedesktop.ModemManager1.Sms";

// MMSmsPduType / MMSmsState
const PDU_DELIVER: u32 = 1;
const PDU_SUBMIT: u32 = 2;
const PDU_STATUS_REPORT: u32 = 3;
const STATE_RECEIVED: u32 = 3;
const STATE_SENT: u32 = 5;

#[derive(Debug, Clone, Serialize)]
pub struct Msg {
    pub id: u64,
    /// "in" or "out"
    pub dir: &'static str,
    pub number: String,
    pub text: String,
    /// SMSC timestamp (incoming) or creation time (outgoing), RFC3339-ish
    pub timestamp: String,
    /// unix time we stored it
    pub stored_at: u64,
    /// in: "received" | out: "sending", "sent", "delivered", "pending", "failed"
    pub state: String,
    pub msg_ref: Option<u32>,
    pub error: Option<String>,
    pub read: bool,
    /// "modem" (ModemManager) or "ims" (delivered over IMS / Wi-Fi calling)
    pub via: &'static str,
}

#[derive(Default)]
pub struct Store {
    pub msgs: Vec<Msg>,
    next_id: u64,
    max: usize,
    /// delivery reports that arrived before we knew the message reference
    pending_reports: HashMap<u32, u32>,
}

fn delivery_state(ds: u32) -> &'static str {
    match ds {
        0x00..=0x1F => "delivered",
        0x20..=0x3F => "pending",
        0x100 => "sent",
        _ => "failed",
    }
}

fn apply_report(m: &mut Msg, ds: u32) {
    m.state = delivery_state(ds).into();
    if m.state == "failed" {
        m.error = Some(format!("delivery state 0x{ds:02x}"));
    }
}

impl Store {
    pub fn new(max: usize) -> Self {
        Store { msgs: vec![], next_id: 1, max, pending_reports: HashMap::new() }
    }

    fn push(&mut self, mut m: Msg) -> u64 {
        m.id = self.next_id;
        self.next_id += 1;
        self.msgs.push(m);
        if self.msgs.len() > self.max {
            let extra = self.msgs.len() - self.max;
            self.msgs.drain(..extra);
        }
        self.next_id - 1
    }

    fn has_incoming(&self, number: &str, ts: &str, text: &str) -> bool {
        self.msgs
            .iter()
            .any(|m| m.dir == "in" && m.number == number && m.timestamp == ts && m.text == text)
    }

    /// Store an incoming message unless we already have it. Returns true if new.
    pub fn push_incoming(&mut self, number: String, text: String, ts: String, via: &'static str) -> bool {
        if self.has_incoming(&number, &ts, &text) {
            return false;
        }
        self.push(Msg {
            id: 0,
            dir: "in",
            number,
            text,
            timestamp: ts,
            stored_at: cmd::now(),
            state: "received".into(),
            msg_ref: None,
            error: None,
            read: false,
            via,
        });
        true
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Msg> {
        self.msgs.iter_mut().find(|m| m.id == id)
    }

    pub fn delete(&mut self, id: u64) -> bool {
        let n = self.msgs.len();
        self.msgs.retain(|m| m.id != id);
        n != self.msgs.len()
    }

    pub fn unread(&self) -> usize {
        self.msgs.iter().filter(|m| m.dir == "in" && !m.read).count()
    }
}

/// "2026-11-24T13:42:31+01" -> "2026-11-24T13:42:31+01:00" (parsable by JS Date)
pub fn normalize_ts(ts: &str) -> String {
    let b = ts.as_bytes();
    if b.len() >= 3 {
        let tail = &ts[ts.len() - 3..];
        if (tail.starts_with('+') || tail.starts_with('-')) && tail[1..].chars().all(|c| c.is_ascii_digit()) {
            return format!("{ts}:00");
        }
    }
    ts.to_string()
}

pub fn valid_number(n: &str) -> bool {
    let digits = n.strip_prefix('+').unwrap_or(n);
    (3..=20).contains(&digits.len()) && digits.chars().all(|c| c.is_ascii_digit())
}

fn now_rfc3339() -> String {
    rfc3339(cmd::now())
}

/// unix time -> "2026-11-24T16:22:41+00:00"
pub fn rfc3339(t: u64) -> String {
    let (y, m, d) = cmd::ymd(t);
    let s = t % 86400;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}+00:00", s / 3600, (s / 60) % 60, s % 60)
}

#[derive(Clone)]
pub struct Sms {
    conn: Connection,
    pub store: Arc<Mutex<Store>>,
}

async fn proxy<'a>(conn: &'a Connection, path: &str, iface: &'static str) -> zbus::Result<Proxy<'a>> {
    let p = ObjectPath::try_from(path.to_string())?;
    Builder::<Proxy>::new(conn)
        .destination(MM)?
        .path(p)?
        .interface(iface)?
        .cache_properties(CacheProperties::No)
        .build()
        .await
}

impl Sms {
    pub async fn connect(max: usize) -> zbus::Result<Sms> {
        let conn = Connection::system().await?;
        Ok(Sms { conn, store: Arc::new(Mutex::new(Store::new(max))) })
    }

    async fn modem_path(&self) -> zbus::Result<String> {
        let om = zbus::fdo::ObjectManagerProxy::builder(&self.conn)
            .destination(MM)?
            .path(MM_PATH)?
            .build()
            .await?;
        let objs = om.get_managed_objects().await?;
        objs.keys()
            .map(|k| k.as_str().to_string())
            .find(|k| k.contains("/Modem/"))
            .ok_or_else(|| zbus::Error::Failure("no modem".into()))
    }

    /// Poll forever: ingest new SMS and delivery reports.
    pub async fn run(self) {
        let mut modem: Option<String> = None;
        loop {
            if modem.is_none() {
                modem = self.modem_path().await.ok();
            }
            if let Some(m) = modem.clone() {
                if let Err(e) = self.poll_once(&m).await {
                    eprintln!("sms poll: {e}");
                    modem = None; // modem path may have changed (MM restart, modem reset)
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }

    async fn poll_once(&self, modem: &str) -> zbus::Result<()> {
        let messaging = proxy(&self.conn, modem, IF_MESSAGING).await?;
        let list: Vec<OwnedObjectPath> = messaging.call("List", &()).await?;
        for p in list {
            let sms = match proxy(&self.conn, p.as_str(), IF_SMS).await {
                Ok(s) => s,
                Err(_) => continue,
            };
            let pdu: u32 = sms.get_property("PduType").await.unwrap_or(0);
            let state: u32 = sms.get_property("State").await.unwrap_or(0);
            match pdu {
                PDU_DELIVER if state == STATE_RECEIVED => {
                    let number: String = sms.get_property("Number").await.unwrap_or_default();
                    let text: String = sms.get_property("Text").await.unwrap_or_default();
                    let ts: String = sms.get_property("Timestamp").await.unwrap_or_default();
                    let ts = normalize_ts(&ts);
                    {
                        let mut st = self.store.lock().await;
                        if !st.has_incoming(&number, &ts, &text) {
                            eprintln!("sms: received from {number} ({} chars)", text.chars().count());
                            st.push(Msg {
                                id: 0,
                                dir: "in",
                                number,
                                text,
                                timestamp: ts,
                                stored_at: cmd::now(),
                                state: "received".into(),
                                msg_ref: None,
                                error: None,
                                read: false,
                                via: "modem",
                            });
                        }
                    }
                    let _: zbus::Result<()> = messaging.call("Delete", &(ObjectPath::try_from(p.as_str())?,)).await;
                }
                PDU_STATUS_REPORT => {
                    let mref: u32 = sms.get_property("MessageReference").await.unwrap_or(u32::MAX);
                    let ds: u32 = sms.get_property("DeliveryState").await.unwrap_or(0x100);
                    {
                        let mut st = self.store.lock().await;
                        if let Some(m) = st
                            .msgs
                            .iter_mut()
                            .rev()
                            .find(|m| m.dir == "out" && m.msg_ref == Some(mref))
                        {
                            apply_report(m, ds);
                        } else {
                            // our Send() may not have returned the reference yet
                            st.pending_reports.insert(mref, ds);
                            if st.pending_reports.len() > 50 {
                                st.pending_reports.clear();
                            }
                        }
                    }
                    let _: zbus::Result<()> = messaging.call("Delete", &(ObjectPath::try_from(p.as_str())?,)).await;
                }
                // leftovers of our own sends (e.g. after a crash mid-send): clean up
                PDU_SUBMIT if state == STATE_SENT => {
                    let _: zbus::Result<()> = messaging.call("Delete", &(ObjectPath::try_from(p.as_str())?,)).await;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Queue an SMS; returns the message id immediately, sending happens in the background.
    pub async fn send(&self, number: String, text: String) -> u64 {
        let id = self.store.lock().await.push(Msg {
            id: 0,
            dir: "out",
            number: number.clone(),
            text: text.clone(),
            timestamp: now_rfc3339(),
            stored_at: cmd::now(),
            state: "sending".into(),
            msg_ref: None,
            error: None,
            read: true,
            via: "modem",
        });
        let me = self.clone();
        tokio::spawn(async move {
            let res = me.send_now(&number, &text).await;
            let mut st = me.store.lock().await;
            let early = match &res {
                Ok(mref) => st.pending_reports.remove(mref),
                Err(_) => None,
            };
            if let Some(m) = st.get_mut(id) {
                match res {
                    Ok(mref) => {
                        m.state = "sent".into();
                        m.msg_ref = Some(mref);
                        if let Some(ds) = early {
                            apply_report(m, ds);
                        }
                        eprintln!("sms: sent id {id} ref {mref}");
                    }
                    Err(e) => {
                        m.state = "failed".into();
                        m.error = Some(e.to_string());
                        eprintln!("sms: send id {id} failed: {e}");
                    }
                }
            }
        });
        id
    }

    async fn send_now(&self, number: &str, text: &str) -> zbus::Result<u32> {
        let modem = self.modem_path().await?;
        let messaging = proxy(&self.conn, &modem, IF_MESSAGING).await?;
        let mut props: HashMap<&str, Value> = HashMap::new();
        props.insert("number", Value::from(number));
        props.insert("text", Value::from(text));
        props.insert("delivery-report-request", Value::from(true));
        let path: OwnedObjectPath = messaging.call("Create", &(props,)).await?;
        let sms = proxy(&self.conn, path.as_str(), IF_SMS).await?;
        let sent: zbus::Result<()> = sms.call("Send", &()).await;
        let mref: u32 = sms.get_property("MessageReference").await.unwrap_or(0);
        let _: zbus::Result<()> = messaging.call("Delete", &(ObjectPath::try_from(path.as_str())?,)).await;
        sent.map(|_| mref)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ts_and_numbers() {
        assert_eq!(normalize_ts("2026-11-24T13:42:31+01"), "2026-11-24T13:42:31+01:00");
        assert_eq!(normalize_ts("2026-11-24T13:42:31+01:00"), "2026-11-24T13:42:31+01:00");
        assert!(valid_number("+34600000001"));
        assert!(!valid_number("+34 650"));
        assert!(!valid_number("abc"));
    }

    #[test]
    fn store_dedupe_and_cap() {
        let mut s = Store::new(2);
        for i in 0..3 {
            s.push(Msg {
                id: 0,
                dir: "in",
                number: "1".into(),
                text: format!("t{i}"),
                timestamp: "x".into(),
                stored_at: 0,
                state: "received".into(),
                msg_ref: None,
                error: None,
                read: false,
                via: "modem",
            });
        }
        assert_eq!(s.msgs.len(), 2);
        assert!(s.has_incoming("1", "x", "t2"));
        assert!(!s.has_incoming("1", "x", "t0"));
    }
}
