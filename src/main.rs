// SPDX-License-Identifier: EUPL-1.2
//! dongie — web dashboard + API for OpenStick MSM8916 4G dongles.
//!
//!   dongie serve [-c /etc/dongie/config.toml]
//!   dongie user list
//!   dongie user add <name> [--role admin|guest]
//!   dongie user passwd <name>
//!   dongie user del <name>

#![forbid(unsafe_code)]

mod blob;
mod cmd;
mod config;
mod contacts;
mod history;
mod ims;
mod mesh;
mod muxcfg;
mod net;
mod netsel;
mod nm;
mod notify;
mod persist;
mod reticulum;
mod settings;
mod sms;
mod squeeze;
mod status;
mod usage;
mod users;
mod ussd;
mod web;
mod wgedit;
mod wifi;

use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

const DEFAULT_CONFIG: &str = "/etc/dongie/config.toml";

fn usage() -> ! {
    eprintln!(
        "dongie {}\n\
         usage:\n  dongie serve [-c CONFIG]\n  dongie user list\n  dongie user add NAME [--role admin|guest]\n  \
         dongie user passwd NAME\n  dongie user del NAME\n  dongie notify test\n  \
         dongie wg show NAME | wg apply NAME EDIT.json | wg create NAME EDIT.json\n\
         config defaults to {DEFAULT_CONFIG} (or $DONGIE_CONFIG)",
        env!("CARGO_PKG_VERSION")
    );
    std::process::exit(2)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cfg_path = std::env::var("DONGIE_CONFIG").unwrap_or_else(|_| DEFAULT_CONFIG.into());
    let mut rest = vec![];
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-c" || args[i] == "--config" {
            cfg_path = args.get(i + 1).cloned().unwrap_or_else(|| usage());
            i += 2;
        } else {
            rest.push(args[i].clone());
            i += 1;
        }
    }
    let cfg = config::Config::load(&cfg_path);
    match rest.iter().map(|s| s.as_str()).collect::<Vec<_>>().as_slice() {
        ["serve"] => serve(cfg, cfg_path),
        ["user", "list"] => user_list(&cfg),
        ["user", "add", name] => user_add(&cfg, name, "admin"),
        ["user", "add", name, "--role", role] => user_add(&cfg, name, role),
        ["user", "passwd", name] => user_passwd(&cfg, name),
        ["user", "del", name] => user_del(&cfg, name),
        ["notify", "test"] => notify_test(&cfg),
        ["wg", "show", name] => wg_cli(name, None),
        ["wg", "apply", name, file] => wg_cli(name, Some(file)),
        ["wg", "create", name, file] => wg_create_cli(name, file),
        ["version"] | ["--version"] => println!("dongie {}", env!("CARGO_PKG_VERSION")),
        _ => usage(),
    }
}

/// Send a test notification to every [[notify]] target and report each result.
fn notify_test(cfg: &config::Config) {
    init_lxmf(cfg);
    if cfg.notify.is_empty() {
        eprintln!("no [[notify]] targets in the config");
        std::process::exit(1);
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
    let mut failed = false;
    rt.block_on(async {
        for t in &cfg.notify {
            let r = match notify::validate(t) {
                Ok(()) => notify::send(t, &notify::Event::Test, None).await,
                Err(e) => Err(e),
            };
            match r {
                Ok(d) if d.is_empty() => println!("ok      {}", notify::label(t)),
                Ok(d) => println!("ok      {}: {d}", notify::label(t)),
                Err(e) => {
                    failed = true;
                    println!("FAILED  {}: {e}", notify::label(t));
                }
            }
        }
    });
    std::process::exit(i32::from(failed));
}

/// WireGuard editing from the shell (same code as the web UI; secrets are never printed).
fn wg_cli(name: &str, file: Option<&str>) {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
    let r = rt.block_on(async {
        match file {
            None => wgedit::view(name).await,
            Some(f) => {
                let text = std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?;
                let edit: wgedit::WgEdit = serde_json::from_str(&text).map_err(|e| format!("{f}: {e}"))?;
                wgedit::apply(name, &edit).await
            }
        }
    });
    match r {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default()),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

fn wg_create_cli(name: &str, file: &str) {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
    let r = rt.block_on(async {
        let text = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
        let mut edit: wgedit::WgEdit = serde_json::from_str(&text).map_err(|e| format!("{file}: {e}"))?;
        if edit.private_key.is_none() {
            edit.private_key = Some("generate".into());
        }
        wgedit::validate(&edit)?;
        wgedit::create(name).await?;
        match wgedit::apply(name, &edit).await {
            Ok(v) => Ok(v),
            Err(e) => {
                let _ = cmd::run_any("nmcli", &["connection", "delete", name]).await;
                Err(e)
            }
        }
    });
    match r {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default()),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

fn read_new_password() -> String {
    let a = rpassword::prompt_password("New password: ").unwrap_or_default();
    if a.chars().count() < 8 {
        eprintln!("password must be at least 8 characters");
        std::process::exit(1);
    }
    let b = rpassword::prompt_password("Repeat password: ").unwrap_or_default();
    if a != b {
        eprintln!("passwords don't match");
        std::process::exit(1);
    }
    a
}

fn user_list(cfg: &config::Config) {
    let users = users::load(&cfg.users_file);
    if users.is_empty() {
        println!("no users in {}", cfg.users_file.display());
    }
    for u in users {
        println!("{}\t{}", u.name, u.role.as_str());
    }
}

fn user_add(cfg: &config::Config, name: &str, role: &str) {
    let Some(role) = users::Role::parse(role) else {
        eprintln!("role must be admin or guest");
        std::process::exit(2)
    };
    if !users::valid_name(name) {
        eprintln!("invalid name (a-z A-Z 0-9 - _ . , max 32)");
        std::process::exit(2)
    }
    let mut list = users::load(&cfg.users_file);
    if list.iter().any(|u| u.name == name) {
        eprintln!("user {name} exists — use `dongie user passwd {name}`");
        std::process::exit(1)
    }
    let pw = read_new_password();
    list.push(users::User { name: name.into(), role, hash: users::hash_password(&pw) });
    save_or_die(cfg, &list);
    println!("added {name} ({})", role.as_str());
}

fn user_passwd(cfg: &config::Config, name: &str) {
    let mut list = users::load(&cfg.users_file);
    let Some(u) = list.iter_mut().find(|u| u.name == name) else {
        eprintln!("no user {name}");
        std::process::exit(1)
    };
    let pw = read_new_password();
    u.hash = users::hash_password(&pw);
    save_or_die(cfg, &list);
    println!("password changed for {name} (existing sessions end when dongie restarts)");
}

fn user_del(cfg: &config::Config, name: &str) {
    let mut list = users::load(&cfg.users_file);
    let n = list.len();
    list.retain(|u| u.name != name);
    if list.len() == n {
        eprintln!("no user {name}");
        std::process::exit(1)
    }
    save_or_die(cfg, &list);
    println!("deleted {name} (existing sessions end when dongie restarts)");
}

fn save_or_die(cfg: &config::Config, list: &[users::User]) {
    if let Err(e) = users::save(&cfg.users_file, list) {
        eprintln!("can't write {}: {e} (run as root?)", cfg.users_file.display());
        std::process::exit(1)
    }
}

/// LXMF sender setup for notify.rs (lnmsg paths, identity on flash, sender name).
fn init_lxmf(cfg: &config::Config) {
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default().trim().to_string();
    notify::set_lxmf_env(notify::LxmfEnv {
        bin: cfg.lnmsg.clone(),
        home: cfg.lnmsg_home.clone().into(),
        identity: cfg.state_dir.join("lxmf").join("identity"),
        from: if cfg.lxmf_name.is_empty() {
            if host.is_empty() {
                "dongie".into()
            } else {
                host
            }
        } else {
            cfg.lxmf_name.clone()
        },
    });
}

fn serve(cfg: config::Config, cfg_path: String) {
    init_lxmf(&cfg);
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio runtime");
    rt.block_on(async move {
        let cfg = Arc::new(cfg);
        eprintln!("dongie {} starting", env!("CARGO_PKG_VERSION"));

        let status = Arc::new(RwLock::new(status::Status::default()));
        let usage = Arc::new(Mutex::new(usage::Usage::load(&cfg.state_dir)));
        let history = Arc::new(Mutex::new(history::History::new(cfg.poll_secs.max(3))));
        tokio::spawn(
            status::Collector {
                cfg: (*cfg).clone(),
                status: status.clone(),
                usage: usage.clone(),
                history: history.clone(),
            }
            .run(),
        );

        if let Err(e) = std::fs::create_dir_all(&cfg.state_dir) {
            eprintln!("state dir {}: {e}", cfg.state_dir.display());
        }
        let contacts = Arc::new(RwLock::new(contacts::Contacts::load(&cfg.state_dir)));
        let events = notify::start(&cfg.notify, contacts.clone());
        let sms = match sms::Sms::connect(cfg.max_messages).await {
            Ok(s) => {
                s.store.lock().await.events = events.clone();
                Some(s)
            }
            Err(e) => {
                eprintln!("SMS disabled: can't reach ModemManager on D-Bus: {e}");
                None
            }
        };
        let ims = ims::Ims::new(cfg.ims_socket.clone());

        // restore saved SMS/calls before anything new comes in
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let persist_task = if cfg.persist {
            let mut p = persist::Persist::new(&cfg.state_dir, sms.clone(), ims.clone());
            p.restore().await;
            Some(tokio::spawn(p.run(stop_rx)))
        } else {
            None
        };
        if let Some(s) = &sms {
            tokio::spawn(s.clone().run());
        }
        tokio::spawn(ims.clone().run(sms.clone(), events));

        let netsel = netsel::NetSel::new(cfg.lte_connection.clone());
        tokio::spawn(netsel.clone().watchdog());

        // Settings tab: restart on save; undo an unconfirmed listen-address change
        let restart = Arc::new(tokio::sync::Notify::new());
        if let Some(left) = settings::pending_left(&cfg.state_dir) {
            eprintln!("settings: listen change waits for confirmation ({left} s left)");
            let (dir, restart) = (cfg.state_dir.clone(), restart.clone());
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(left)).await;
                if settings::pending_left(&dir).is_some() {
                    eprintln!("settings: not confirmed in time, restoring the previous settings");
                    if let Err(e) = settings::rollback(&dir) {
                        eprintln!("settings rollback: {e}");
                    }
                    restart.notify_one();
                }
            });
        }

        let app = web::App {
            cfg: cfg.clone(),
            cfg_path: cfg_path.clone(),
            restart: restart.clone(),
            status,
            history,
            sms,
            ims,
            netsel,
            contacts,
            // systemd sets $RUNTIME_DIRECTORY (/run/dongie) via RuntimeDirectory=
            sessions: Arc::new(match std::env::var_os("RUNTIME_DIRECTORY") {
                Some(d) => users::Sessions::with_runtime_dir(std::path::Path::new(&d), &cfg.users_file),
                None => users::Sessions::default(),
            }),
            login_gate: Arc::new(tokio::sync::Mutex::new(())),
        };

        if users::load(&cfg.users_file).is_empty() {
            eprintln!("no users yet — create one with: sudo dongie user add <name>");
        }

        let web_app = app.clone();
        for addr in cfg.web_listen_extra.clone() {
            let extra_app = app.clone();
            tokio::spawn(async move {
                let l = web::bind_retry(&addr).await;
                if let Err(e) = axum::serve(l, web::web_router(extra_app)).await {
                    eprintln!("web server {addr}: {e}");
                }
            });
        }
        let web_addr = cfg.web_listen.clone();
        let web = tokio::spawn(async move {
            let l = web::bind_retry(&web_addr).await;
            axum::serve(l, web::web_router(web_app)).await
        });
        if cfg.api_enabled {
            let api_addr = cfg.api_listen.clone();
            let api_app = app.clone();
            tokio::spawn(async move {
                let l = web::bind_retry(&api_addr).await;
                if let Err(e) = axum::serve(l, web::api_router(api_app)).await {
                    eprintln!("api server: {e}");
                }
            });
        }

        let usage_on_exit = usage.clone();
        tokio::select! {
            r = web => eprintln!("web server stopped: {r:?}"),
            _ = tokio::signal::ctrl_c() => eprintln!("interrupted"),
            _ = sigterm() => eprintln!("terminated"),
            _ = restart.notified() => eprintln!("restarting for new settings"),
        }
        usage_on_exit.lock().await.save_if_dirty();
        if let Some(t) = persist_task {
            let _ = stop_tx.send(true);
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), t).await;
        }
    });
}

async fn sigterm() {
    let mut s = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("sigterm");
    s.recv().await;
}
