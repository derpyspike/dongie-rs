// SPDX-License-Identifier: EUPL-1.2
//! SIM PIN: unlock a PIN-locked SIM from the web UI (admin only), and
//! optionally remember the PIN so the stick unlocks the SIM by itself after a
//! reboot.
//!
//! The saved PIN is bound to the SIM's ICCID (a different SIM is never tried
//! with it), lives in <state_dir>/sim-pin.json with mode 0600, is never sent
//! back to the browser and never logged. The automatic unlock only tries while
//! at least 2 PIN attempts are left, and a saved PIN that the SIM rejects is
//! deleted at once, so it can cost at most one attempt.

use crate::sms::{proxy, Sms};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;
use zbus::zvariant::OwnedObjectPath;

const IF_MODEM: &str = "org.freedesktop.ModemManager1.Modem";
const IF_SIM: &str = "org.freedesktop.ModemManager1.Sim";

// MMModemLock
const LOCK_NONE: u32 = 1;
const LOCK_SIM_PIN: u32 = 2;
const LOCK_SIM_PUK: u32 = 4;

/// What the auto-unlock keeps of a saved PIN never needs more than this.
const AUTO_MIN_RETRIES: u32 = 2;

#[derive(Debug, Serialize, Default)]
pub struct SimState {
    pub modem: bool,
    /// none | sim-pin | sim-puk | other lock names | unknown
    pub lock: String,
    pub pin_retries: Option<u32>,
    pub puk_retries: Option<u32>,
    pub iccid: Option<String>,
    /// a PIN is saved for this SIM (the PIN itself is never sent)
    pub saved: bool,
    /// what the automatic unlock did last (no PIN in it)
    pub auto: Option<String>,
}

pub fn lock_name(l: u32) -> &'static str {
    match l {
        0 => "unknown",
        LOCK_NONE => "none",
        LOCK_SIM_PIN => "sim-pin",
        3 => "sim-pin2",
        LOCK_SIM_PUK => "sim-puk",
        5 => "sim-puk2",
        _ => "other",
    }
}

pub fn valid_pin(p: &str) -> bool {
    (4..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit())
}

pub fn valid_puk(p: &str) -> bool {
    p.len() == 8 && p.bytes().all(|b| b.is_ascii_digit())
}

/// ICCID -> PIN
#[derive(Serialize, Deserialize, Default)]
struct Saved {
    pins: BTreeMap<String, String>,
}

fn saved_path(state_dir: &Path) -> PathBuf {
    state_dir.join("sim-pin.json")
}

fn load_saved(state_dir: &Path) -> Saved {
    std::fs::read(saved_path(state_dir)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn store_saved(state_dir: &Path, s: &Saved) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = saved_path(state_dir);
    if s.pins.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        };
    }
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let body = serde_json::to_vec(s).map_err(|e| e.to_string())?;
    let mut f =
        std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).map_err(|e| e.to_string())?;
    f.write_all(&body).and_then(|_| f.sync_all()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

fn set_saved(state_dir: &Path, iccid: &str, pin: Option<&str>) -> Result<(), String> {
    let mut s = load_saved(state_dir);
    let changed = match pin {
        Some(p) => s.pins.insert(iccid.to_string(), p.to_string()).as_deref() != Some(p),
        None => s.pins.remove(iccid).is_some(),
    };
    if changed {
        store_saved(state_dir, &s)
    } else {
        Ok(())
    }
}

/// Everything the SIM endpoints and the auto-unlock need from ModemManager.
struct Raw {
    lock: u32,
    retries: HashMap<u32, u32>,
    sim: Option<String>,
    iccid: Option<String>,
}

pub enum Unlock<'a> {
    Pin(&'a str),
    /// PUK + the new PIN
    Puk(&'a str, &'a str),
}

impl Sms {
    async fn sim_raw(&self) -> zbus::Result<Raw> {
        let modem = self.modem_path().await?;
        let m = proxy(&self.conn, &modem, IF_MODEM).await?;
        let lock: u32 = m.get_property("UnlockRequired").await.unwrap_or(0);
        let retries: HashMap<u32, u32> = m.get_property("UnlockRetries").await.unwrap_or_default();
        let sim: Option<String> =
            m.get_property::<OwnedObjectPath>("Sim").await.ok().map(|p| p.as_str().to_string()).filter(|p| p != "/");
        let mut iccid = None;
        if let Some(s) = &sim {
            if let Ok(p) = proxy(&self.conn, s, IF_SIM).await {
                iccid = p.get_property::<String>("SimIdentifier").await.ok().filter(|x| !x.is_empty());
            }
        }
        Ok(Raw { lock, retries, sim, iccid })
    }

    pub async fn sim_state(&self, state_dir: &Path) -> SimState {
        let Ok(r) = self.sim_raw().await else {
            return SimState { lock: "unknown".into(), ..Default::default() };
        };
        let saved = r.iccid.as_ref().is_some_and(|i| load_saved(state_dir).pins.contains_key(i));
        SimState {
            modem: true,
            lock: lock_name(r.lock).into(),
            pin_retries: r.retries.get(&LOCK_SIM_PIN).copied(),
            puk_retries: r.retries.get(&LOCK_SIM_PUK).copied(),
            iccid: r.iccid,
            saved,
            auto: self.sim_auto.lock().await.clone(),
        }
    }

    /// Unlock with a PIN (or PUK + new PIN). `save`: remember the PIN for this
    /// SIM; when false a PIN saved earlier for it is forgotten.
    /// `last_try` must be set when only one attempt is left.
    pub async fn sim_unlock(&self, state_dir: &Path, op: Unlock<'_>, save: bool, last_try: bool) -> Result<(), String> {
        let _one = self.sim_lock.lock().await;
        let r = self.sim_raw().await.map_err(|e| crate::ussd::clean_error(&e.to_string()))?;
        let sim = r.sim.clone().ok_or("no SIM in the modem")?;
        let (want, pin) = match op {
            Unlock::Pin(p) => (LOCK_SIM_PIN, p),
            Unlock::Puk(_, p) => (LOCK_SIM_PUK, p),
        };
        if r.lock != want {
            return Err(format!("the SIM asks for {}, not {}", lock_name(r.lock), lock_name(want)));
        }
        if r.retries.get(&want) == Some(&1) && !last_try {
            return Err("last attempt: confirm it first".into());
        }
        let p = proxy(&self.conn, &sim, IF_SIM).await.map_err(|e| e.to_string())?;
        let res = match op {
            Unlock::Pin(pin) => p.call::<_, _, ()>("SendPin", &(pin,)).await,
            Unlock::Puk(puk, pin) => p.call::<_, _, ()>("SendPuk", &(puk, pin)).await,
        };
        if let Err(e) = res {
            let left = self.sim_raw().await.ok().and_then(|r| r.retries.get(&want).copied());
            let mut msg = crate::ussd::clean_error(&e.to_string());
            if let Some(n) = left {
                msg += &format!(" ({n} attempt{} left)", if n == 1 { "" } else { "s" });
            }
            return Err(msg);
        }
        *self.sim_auto.lock().await = None;
        tokio::spawn(self.clone().refresh_after_unlock());
        if let Some(iccid) = &r.iccid {
            if let Err(e) = set_saved(state_dir, iccid, save.then_some(pin)) {
                return Err(format!("SIM unlocked, but saving the PIN failed: {e}"));
            }
        } else if save {
            return Err("SIM unlocked, but it has no ICCID to save the PIN under".into());
        }
        Ok(())
    }

    /// Forget the saved PIN of the SIM that is in the modem now (or all, when
    /// no SIM can be read).
    pub async fn sim_forget(&self, state_dir: &Path) -> Result<(), String> {
        match self.sim_raw().await.ok().and_then(|r| r.iccid) {
            Some(i) => set_saved(state_dir, &i, None),
            None => store_saved(state_dir, &Saved::default()),
        }
    }

    /// The SIM's MCC+MNC as ModemManager knows it ("21403"), None while unknown.
    async fn sim_operator_code(&self) -> Option<String> {
        let sim = self.sim_raw().await.ok()?.sim?;
        let p = proxy(&self.conn, &sim, IF_SIM).await.ok()?;
        p.get_property::<String>("OperatorIdentifier").await.ok().filter(|c| c.len() >= 5)
    }

    /// ModemManager reads the SIM's details while it is still PIN-locked and
    /// doesn't read them again after the unlock: the operator code stays empty,
    /// and fix-attach-apn and dongie-ims wait for it forever (seen on a UZ801,
    /// 2026-11-26). If it hasn't appeared 30 s after an unlock, restart
    /// ModemManager once (the SIM stays unlocked; LTE drops for a few seconds)
    /// and re-run fix-attach-apn.
    pub async fn refresh_after_unlock(self) {
        for _ in 0..6 {
            tokio::time::sleep(Duration::from_secs(5)).await;
            if self.sim_operator_code().await.is_some() {
                return;
            }
        }
        eprintln!("sim: ModemManager has no operator code for the unlocked SIM, restarting it once");
        match crate::cmd::run_any_t("systemctl", &["restart", "ModemManager.service"], 60).await {
            Some((true, _, _)) => {}
            other => {
                eprintln!("sim: restarting ModemManager failed: {:?}", other.map(|x| x.2));
                return;
            }
        }
        let mut code = None;
        for _ in 0..24 {
            tokio::time::sleep(Duration::from_secs(5)).await;
            code = self.sim_operator_code().await;
            if code.is_some() {
                break;
            }
        }
        let Some(code) = code else {
            eprintln!("sim: still no operator code after the ModemManager restart");
            return;
        };
        eprintln!("sim: operator code {code} readable now");
        let loaded =
            crate::cmd::run("systemctl", &["show", "-p", "LoadState", "--value", "fix-attach-apn.service"]).await;
        if loaded.as_deref().map(str::trim) == Some("loaded") {
            let r = crate::cmd::run_any("systemctl", &["--no-block", "restart", "fix-attach-apn.service"]).await;
            eprintln!(
                "sim: re-running fix-attach-apn: {}",
                if matches!(r, Some((true, _, _))) { "started" } else { "failed" }
            );
        }
    }

    /// Background: when the SIM wants its PIN and one is saved for it, send it.
    pub async fn sim_auto_unlock(self, state_dir: PathBuf) {
        // ICCIDs whose saved PIN failed with a non-PIN error; retried later
        let mut backoff: HashMap<String, u32> = HashMap::new();
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            let Ok(r) = self.sim_raw().await else { continue };
            if r.lock != LOCK_SIM_PIN {
                continue;
            }
            let (Some(iccid), Some(sim)) = (r.iccid.clone(), r.sim.clone()) else { continue };
            let Some(pin) = load_saved(&state_dir).pins.get(&iccid).cloned() else { continue };
            if let Some(n) = backoff.get_mut(&iccid) {
                if *n > 0 {
                    *n -= 1;
                    continue;
                }
            }
            let _one = self.sim_lock.lock().await;
            let before = r.retries.get(&LOCK_SIM_PIN).copied();
            if before.is_none_or(|n| n < AUTO_MIN_RETRIES) {
                let note = format!(
                    "saved PIN not tried: only {} attempt(s) left",
                    before.map_or("?".into(), |n| n.to_string())
                );
                let mut a = self.sim_auto.lock().await;
                if a.as_deref() != Some(&note) {
                    eprintln!("sim: {note}");
                    *a = Some(note);
                }
                continue;
            }
            let Ok(p) = proxy(&self.conn, &sim, IF_SIM).await else { continue };
            let note = match p.call::<_, _, ()>("SendPin", &(pin.as_str(),)).await {
                Ok(()) => {
                    backoff.remove(&iccid);
                    tokio::spawn(self.clone().refresh_after_unlock());
                    "SIM unlocked with the saved PIN".to_string()
                }
                Err(e) => {
                    let after = self.sim_raw().await.ok().and_then(|r| r.retries.get(&LOCK_SIM_PIN).copied());
                    let wrong = e.to_string().contains("IncorrectPassword")
                        || matches!((before, after), (Some(b), Some(a)) if a < b);
                    if wrong {
                        let _ = set_saved(&state_dir, &iccid, None);
                        format!(
                            "saved PIN was rejected and has been deleted ({} attempts left)",
                            after.map_or("?".into(), |n| n.to_string())
                        )
                    } else {
                        // no attempt used (modem busy etc.): try again in ~1 min
                        backoff.insert(iccid.clone(), 6);
                        format!("saved PIN not accepted yet: {}", crate::ussd::clean_error(&e.to_string()))
                    }
                }
            };
            eprintln!("sim: {note}");
            *self.sim_auto.lock().await = Some(note);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation() {
        assert!(valid_pin("1234") && valid_pin("12345678"));
        assert!(!valid_pin("123") && !valid_pin("123456789") && !valid_pin("12a4") && !valid_pin(" 1234"));
        assert!(valid_puk("12345678") && !valid_puk("1234567") && !valid_puk("1234567x"));
        assert_eq!(lock_name(2), "sim-pin");
        assert_eq!(lock_name(4), "sim-puk");
        assert_eq!(lock_name(1), "none");
    }

    #[test]
    fn saved_pins_are_private_and_per_sim() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("dongie-sim-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        set_saved(&dir, "8934A", Some("1234")).unwrap();
        set_saved(&dir, "8934B", Some("9999")).unwrap();
        let mode = std::fs::metadata(saved_path(&dir)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(load_saved(&dir).pins.get("8934A").map(String::as_str), Some("1234"));
        set_saved(&dir, "8934A", None).unwrap();
        assert!(!load_saved(&dir).pins.contains_key("8934A"));
        set_saved(&dir, "8934B", None).unwrap();
        assert!(!saved_path(&dir).exists(), "empty store removes the file");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
