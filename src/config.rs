// SPDX-License-Identifier: EUPL-1.2
//! /etc/dongie/config.toml — every field optional, defaults below.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Web UI + login (port 8040 by default). Bound to the Tailscale IP.
    pub web_listen: String,
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            web_listen: "127.0.0.1:8040".into(),
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
        }
    }
}

impl Config {
    pub fn load(path: &str) -> Config {
        match std::fs::read_to_string(path) {
            Ok(s) => match toml::from_str(&s) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("config {path}: parse error ({e}), using defaults");
                    Config::default()
                }
            },
            Err(_) => {
                eprintln!("config {path}: not found, using defaults");
                Config::default()
            }
        }
    }
}
