// SPDX-License-Identifier: EUPL-1.2
//! USSD codes (e.g. *138# "my number" on Vodafone ES) through ModemManager's
//! Modem3gpp.Ussd interface. Admin only: some codes change things (top-ups,
//! services), not just query them. One session at a time.

use crate::sms::{proxy, Sms};
use serde::Serialize;
use std::time::Duration;

const IF_USSD: &str = "org.freedesktop.ModemManager1.Modem.Modem3gpp.Ussd";
/// The network can take a while to answer; ModemManager itself waits ~30 s.
const TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Serialize)]
pub struct Reply {
    pub reply: String,
    /// idle | active | user-response (the network waits for an answer) | unknown
    pub state: &'static str,
}

pub enum Op<'a> {
    Initiate(&'a str),
    Respond(&'a str),
    Cancel,
}

/// A code to start a session: digits, * # +, at most 32 characters.
pub fn valid_code(c: &str) -> bool {
    !c.is_empty() && c.len() <= 32 && c.chars().all(|ch| ch.is_ascii_digit() || "*#+".contains(ch))
}

/// An answer inside a session (menu choice or text).
pub fn valid_response(t: &str) -> bool {
    !t.is_empty() && t.chars().count() <= 160 && !t.chars().any(char::is_control)
}

fn state_name(s: u32) -> &'static str {
    match s {
        1 => "idle",
        2 => "active",
        3 => "user-response",
        _ => "unknown",
    }
}

impl Sms {
    pub async fn ussd(&self, op: Op<'_>) -> Result<Reply, String> {
        let _one = self.ussd_lock.lock().await;
        let modem = self.modem_path().await.map_err(|e| e.to_string())?;
        let p = proxy(&self.conn, &modem, IF_USSD).await.map_err(|e| e.to_string())?;
        let call = async {
            match op {
                Op::Initiate(code) => p.call::<_, _, String>("Initiate", &(code,)).await,
                Op::Respond(text) => p.call::<_, _, String>("Respond", &(text,)).await,
                Op::Cancel => p.call::<_, _, ()>("Cancel", &()).await.map(|_| String::new()),
            }
        };
        let reply = match tokio::time::timeout(TIMEOUT, call).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return Err(clean_error(&e.to_string())),
            Err(_) => {
                // don't leave a half-open session behind
                let _: zbus::Result<()> = p.call("Cancel", &()).await;
                return Err("no answer from the network (timeout)".into());
            }
        };
        let state = p.get_property::<u32>("State").await.map(state_name).unwrap_or("unknown");
        Ok(Reply { reply, state })
    }
}

/// ModemManager errors come as "org.freedesktop.ModemManager1.Error.Core.Failed: text".
fn clean_error(e: &str) -> String {
    let e = e.rsplit_once("Error.").map(|(_, r)| r).unwrap_or(e);
    e.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation() {
        assert!(valid_code("*138#") && valid_code("*#100#") && valid_code("+34*1#"));
        assert!(!valid_code("") && !valid_code("*138#; rm") && !valid_code(&"1".repeat(33)));
        assert!(valid_response("1") && valid_response("Sí, 2"));
        assert!(!valid_response("") && !valid_response("a\nb") && !valid_response(&"x".repeat(161)));
        assert_eq!(state_name(3), "user-response");
        assert_eq!(clean_error("org.freedesktop.ModemManager1.Error.Core.WrongState: busy"), "Core.WrongState: busy");
    }
}
