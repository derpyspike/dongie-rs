// SPDX-License-Identifier: EUPL-1.2
//! LTE data usage, from the modem's own QMI packet counters.
//!
//! The kernel's wwan0 counters stay at 0 on this hardware (bam-dmux), so we read
//! `qmicli --wds-get-packet-statistics`. Those counters are per data session and
//! restart at 0 after a reconnect: we add up the deltas per UTC day and persist
//! them to <state_dir>/usage.json.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    /// "YYYY-MM-DD" (UTC) -> [rx_bytes, tx_bytes]
    pub days: BTreeMap<String, [u64; 2]>,
    #[serde(default)]
    last_rx: u64,
    #[serde(default)]
    last_tx: u64,
    #[serde(skip)]
    pub session_rx: u64,
    #[serde(skip)]
    pub session_tx: u64,
    #[serde(skip)]
    dirty: bool,
    #[serde(skip)]
    path: PathBuf,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct UsageView {
    pub available: bool,
    pub session_rx: u64,
    pub session_tx: u64,
    pub today_rx: u64,
    pub today_tx: u64,
    pub month_rx: u64,
    pub month_tx: u64,
    pub since: Option<String>,
    /// last 31 days, oldest first: (date, rx, tx)
    pub history: Vec<(String, u64, u64)>,
}

fn day_key(unix: u64) -> String {
    let (y, m, d) = crate::cmd::ymd(unix);
    format!("{y:04}-{m:02}-{d:02}")
}

impl Usage {
    pub fn load(state_dir: &std::path::Path) -> Usage {
        let path = state_dir.join("usage.json");
        let mut u: Usage =
            std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        u.path = path;
        u
    }

    /// Feed the current session counters.
    pub fn update(&mut self, rx: u64, tx: u64, now: u64) {
        // counters went down => new data session, everything so far is new
        let drx = if rx >= self.last_rx { rx - self.last_rx } else { rx };
        let dtx = if tx >= self.last_tx { tx - self.last_tx } else { tx };
        self.last_rx = rx;
        self.last_tx = tx;
        self.session_rx = rx;
        self.session_tx = tx;
        if drx > 0 || dtx > 0 {
            let e = self.days.entry(day_key(now)).or_insert([0, 0]);
            e[0] += drx;
            e[1] += dtx;
            self.dirty = true;
            while self.days.len() > 400 {
                let k = self.days.keys().next().cloned().unwrap();
                self.days.remove(&k);
            }
        }
    }

    pub fn save_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        if let Ok(s) = serde_json::to_string(self) {
            let tmp = self.path.with_extension("tmp");
            if std::fs::write(&tmp, s).is_ok() && std::fs::rename(&tmp, &self.path).is_ok() {
                self.dirty = false;
            }
        }
    }

    pub fn view(&self, now: u64, available: bool) -> UsageView {
        let today = day_key(now);
        let month = &today[..7];
        let mut v = UsageView {
            available,
            session_rx: self.session_rx,
            session_tx: self.session_tx,
            since: self.days.keys().next().cloned(),
            ..Default::default()
        };
        if let Some(d) = self.days.get(&today) {
            v.today_rx = d[0];
            v.today_tx = d[1];
        }
        for (k, d) in &self.days {
            if k.starts_with(month) {
                v.month_rx += d[0];
                v.month_tx += d[1];
            }
        }
        v.history = self.days.iter().rev().take(31).map(|(k, d)| (k.clone(), d[0], d[1])).collect();
        v.history.reverse();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deltas_and_reset() {
        let mut u = Usage::default();
        let t = 1_795_518_317; // 2026-11-24
        u.update(1000, 100, t);
        u.update(1500, 150, t);
        u.update(200, 20, t); // reconnect: counters restarted
        let v = u.view(t, true);
        assert_eq!(v.today_rx, 1700);
        assert_eq!(v.today_tx, 170);
        assert_eq!(v.month_rx, 1700);
        assert_eq!(v.session_rx, 200);
    }
}
