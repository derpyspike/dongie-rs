// SPDX-License-Identifier: EUPL-1.2
//! `persist = true`: keep SMS and the call log across restarts and power cuts.
//!
//! Every SAVE_SECS the stores are serialized; a file is only written when its
//! content changed (so an idle stick writes nothing). Files go through blob.rs
//! (compressed, CRC-checked, atomic, with a .bak). Also saved on shutdown.

use crate::blob;
use crate::ims::{Call, Ims};
use crate::sms::{SavedStore, Sms};
use std::path::PathBuf;
use std::time::Duration;

const SAVE_SECS: u64 = 15;

pub struct Persist {
    sms: Option<Sms>,
    ims: Ims,
    sms_path: PathBuf,
    calls_path: PathBuf,
    last_sms: Vec<u8>,
    last_calls: Vec<u8>,
}

impl Persist {
    pub fn new(state_dir: &std::path::Path, sms: Option<Sms>, ims: Ims) -> Persist {
        Persist {
            sms,
            ims,
            sms_path: state_dir.join("sms.bin"),
            calls_path: state_dir.join("calls.bin"),
            last_sms: vec![],
            last_calls: vec![],
        }
    }

    /// Load what was saved before (call once at startup).
    pub async fn restore(&mut self) {
        if let Some(sms) = &self.sms {
            if let Some(saved) = blob::load::<SavedStore>(&self.sms_path) {
                let n = saved.msgs.len();
                sms.store.lock().await.restore(saved);
                eprintln!("persist: {n} SMS restored");
            }
            self.last_sms = blob::encode(&sms.store.lock().await.snapshot()).unwrap_or_default();
        }
        if let Some(calls) = blob::load::<Vec<Call>>(&self.calls_path) {
            eprintln!("persist: {} calls restored", calls.len());
            *self.ims.archive.lock().await = calls;
        }
        self.last_calls = blob::encode(&*self.ims.archive.lock().await).unwrap_or_default();
    }

    /// Write whatever changed since the last save.
    pub async fn save_changed(&mut self) {
        if let Some(sms) = &self.sms {
            let snap = sms.store.lock().await.snapshot();
            write_if_changed(&self.sms_path, &snap, &mut self.last_sms);
        }
        let calls = self.ims.archive.lock().await.clone();
        write_if_changed(&self.calls_path, &calls, &mut self.last_calls);
    }

    pub async fn run(mut self, stop: tokio::sync::watch::Receiver<bool>) {
        let mut stop = stop;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(SAVE_SECS)) => self.save_changed().await,
                _ = stop.changed() => {
                    self.save_changed().await;
                    return;
                }
            }
        }
    }
}

fn write_if_changed<T: serde::Serialize>(path: &std::path::Path, value: &T, last: &mut Vec<u8>) {
    let Ok(bytes) = blob::encode(value) else { return };
    if bytes == *last {
        return;
    }
    match blob::save(path, value) {
        Ok(()) => *last = bytes,
        Err(e) => eprintln!("persist: {e}"),
    }
}
