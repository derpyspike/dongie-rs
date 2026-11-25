// SPDX-License-Identifier: EUPL-1.2
//! /etc/dongie/config.toml — every field optional, defaults below.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    /// Web UI + login (port 8040 by default). Bound to the Tailscale IP.
    pub web_listen: String,
    /// More addresses for the web UI (NOT the open API), e.g. the hotspot
    /// ("192.168.4.1:8040") or USB address, so the stick is reachable without
    /// Tailscale. Addresses that don't exist yet are retried in the background.
    pub web_listen_extra: Vec<String>,
    /// Unauthenticated API (Tailscale only): status/history, plus SMS when `api_allow_sms`.
    pub api_listen: String,
    /// false disables the unauthenticated API listener completely.
    pub api_enabled: bool,
    /// Let the unauthenticated API read/send SMS too (default on: that is what the port is for).
    pub api_allow_sms: bool,
    /// Show the dashboard (guest view, no SMS) without logging in.
    pub anonymous_guest: bool,
    pub users_file: PathBuf,
    pub state_dir: PathBuf,
    pub qmi_device: String,
    pub hotspot_connection: String,
    pub lte_connection: String,
    /// Seconds between status refreshes.
    pub poll_secs: u64,
    /// Max messages kept in RAM.
    pub max_messages: usize,
    /// Units shown in the services card.
    pub services: Vec<String>,
    /// dongie-ims API socket (Wi-Fi calling / IMS); "" disables the integration.
    pub ims_socket: String,
    /// USSD codes in the UI (admin only). Off by default: on LTE the modem falls
    /// back to 3G for USSD, so data (and Tailscale) stalls for ~20-30 s, and on
    /// the UZ801 the reply often never comes.
    pub ussd: bool,
    /// Notifications for incoming SMS and calls (`[[notify]]` tables), see notify.rs.
    pub notify: Vec<NotifyTarget>,
    /// Keep SMS and the call log in <state_dir> across restarts and power cuts
    /// (compressed + checksummed, written only when something changed). Off: RAM only.
    pub persist: bool,
    /// LXMF notifications: Leviculum's lnmsg binary, its state directory (a small
    /// tmpfs; the identity itself is kept in <state_dir>/lxmf/identity) and the
    /// sender name ("" = hostname).
    pub lnmsg: String,
    pub lnmsg_home: String,
    pub lxmf_name: String,
    /// Reticulum tab: the daemon's systemd unit ("" hides the tab), Leviculum's
    /// lnstatus, and a client config dir whose `config` has the daemon's rpc_key.
    pub lnsd_unit: String,
    pub lnstatus: String,
    pub reticulum_config: String,
    /// Meshtastic tab: muxtastic's web/API on loopback ("" hides the tab), its
    /// systemd unit, and the link the "open muxtastic" button uses ("" = the
    /// host the page was opened with, on muxtastic_url's port).
    pub muxtastic_url: String,
    pub muxtastic_unit: String,
    pub muxtastic_link: String,
    /// muxtastic's config file, edited from the Mesh tab (group dongie must be
    /// able to write it, see contrib/muxtastic).
    pub muxtastic_config: String,
}

/// One `[[notify]]` target.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct NotifyTarget {
    /// "ntfy" | "webhook" | "meshtastic"
    pub kind: String,
    /// Shown in logs and the UI instead of the URL.
    pub name: String,
    /// ntfy: topic URL (https://ntfy.sh/mytopic); webhook: URL that gets a JSON POST.
    pub url: String,
    /// Optional bearer token (ntfy access token, webhook secret).
    pub token: String,
    /// meshtastic: "host:port" of a node (4403) or muxtastic (4404).
    pub host: String,
    /// meshtastic: channel index 0-7.
    pub channel: u32,
    /// meshtastic: "" = broadcast on the channel, or a node id "!a1b2c3d4" (DM).
    pub to: String,
    /// lxmf: "auto" (default) | "direct" | "propagated" (through a propagation node).
    pub via: String,
    /// lxmf: propagation node to leave messages at when no direct link comes up
    /// (32 hex; empty = the one lnmsg heard most recently). Use the one the
    /// recipient's app syncs from.
    pub pn: String,
    /// Any of "sms", "call" (incoming, ringing), "missed_call".
    pub events: Vec<String>,
    /// Put the SMS text into the notification (otherwise just sender + length).
    pub include_text: bool,
}

impl Default for NotifyTarget {
    fn default() -> Self {
        Self {
            kind: String::new(),
            name: String::new(),
            url: String::new(),
            token: String::new(),
            host: String::new(),
            channel: 0,
            to: String::new(),
            via: String::new(),
            pn: String::new(),
            events: vec!["sms".into(), "call".into(), "missed_call".into()],
            include_text: true,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            web_listen: "127.0.0.1:8040".into(),
            web_listen_extra: vec![],
            api_listen: "127.0.0.1:8041".into(),
            api_enabled: true,
            api_allow_sms: true,
            anonymous_guest: true,
            users_file: "/etc/dongie/users".into(),
            state_dir: "/var/lib/dongie".into(),
            qmi_device: "/dev/wwan0qmi0".into(),
            hotspot_connection: "hotspot".into(),
            lte_connection: "lte".into(),
            poll_secs: 10,
            max_messages: 500,
            services: [
                "ModemManager",
                "NetworkManager",
                "tailscaled",
                "fix-attach-apn",
                "dongie-leds",
                "ssh",
                "systemd-zram-setup@zram0",
                "dongie-ims",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            ims_socket: "/run/dongie-ims/api.sock".into(),
            ussd: false,
            notify: vec![],
            persist: false,
            lnmsg: "/usr/local/bin/lnmsg".into(),
            lnmsg_home: "/run/lnmsg".into(),
            lxmf_name: String::new(),
            lnsd_unit: "lnsd".into(),
            lnstatus: "/usr/local/bin/lnstatus".into(),
            reticulum_config: "/etc/dongie/reticulum".into(),
            muxtastic_url: "http://127.0.0.1:8090".into(),
            muxtastic_unit: "muxtastic".into(),
            muxtastic_link: String::new(),
            muxtastic_config: "/etc/muxtastic/muxtastic.toml".into(),
        }
    }
}

impl Config {
    /// config.toml, with the web UI's <state_dir>/settings.toml on top (settings.rs).
    /// A broken or invalid settings.toml is ignored (logged), never fatal.
    pub fn load(path: &str) -> Config {
        use crate::settings;
        if !std::path::Path::new(path).exists() {
            eprintln!("config {path}: not found, using defaults");
        }
        let base = match settings::read_table(std::path::Path::new(path)) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("config {path}: {e}, using defaults");
                return Config::default();
            }
        };
        let base_cfg = match settings::to_config(base.clone()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("config {path}: parse error ({e}), using defaults");
                return Config::default();
            }
        };
        let opath = settings::override_path(&base_cfg.state_dir);
        match settings::read_table(&opath) {
            Ok(over) if over.is_empty() => base_cfg,
            Ok(over) => match settings::to_config(settings::merge(&base, &over))
                .and_then(|c| settings::validate(&c).map(|_| c))
            {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("{} ignored ({e})", opath.display());
                    base_cfg
                }
            },
            Err(e) => {
                eprintln!("{e} (ignored)");
                base_cfg
            }
        }
    }
}
