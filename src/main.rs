// SPDX-License-Identifier: EUPL-1.2
//! dongie — web dashboard + API for OpenStick MSM8916 4G dongles.
//!
//!   dongie serve [-c /etc/dongie/config.toml]
//!   dongie user list
//!   dongie user add <name> [--role admin|guest]
//!   dongie user passwd <name>
//!   dongie user del <name>

mod cmd;
mod config;
mod history;
mod net;
mod ims;
mod sms;
mod status;
mod usage;
mod users;
mod web;
mod wifi;

use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

const DEFAULT_CONFIG: &str = "/etc/dongie/config.toml";

fn usage() -> ! {
    eprintln!(
        "dongie {}\n\
         usage:\n  dongie serve [-c CONFIG]\n  dongie user list\n  dongie user add NAME [--role admin|guest]\n  \
         dongie user passwd NAME\n  dongie user del NAME\n\
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
        ["serve"] => serve(cfg),
        ["user", "list"] => user_list(&cfg),
        ["user", "add", name] => user_add(&cfg, name, "admin"),
        ["user", "add", name, "--role", role] => user_add(&cfg, name, role),
        ["user", "passwd", name] => user_passwd(&cfg, name),
        ["user", "del", name] => user_del(&cfg, name),
        ["version"] | ["--version"] => println!("dongie {}", env!("CARGO_PKG_VERSION")),
        _ => usage(),
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

fn serve(cfg: config::Config) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
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

        let sms = match sms::Sms::connect(cfg.max_messages).await {
            Ok(s) => {
                tokio::spawn(s.clone().run());
                Some(s)
            }
            Err(e) => {
                eprintln!("SMS disabled: can't reach ModemManager on D-Bus: {e}");
                None
            }
        };

        let ims = ims::Ims::new(cfg.ims_socket.clone());
        tokio::spawn(ims.clone().run(sms.clone()));

        let app = web::App {
            cfg: cfg.clone(),
            status,
            history,
            sms,
            ims,
            sessions: Arc::new(users::Sessions::default()),
            login_gate: Arc::new(tokio::sync::Mutex::new(())),
        };

        if users::load(&cfg.users_file).is_empty() {
            eprintln!("no users yet — create one with: sudo dongie user add <name>");
        }

        let web_app = app.clone();
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
        }
        usage_on_exit.lock().await.save_if_dirty();
    });
}

async fn sigterm() {
    let mut s = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("sigterm");
    s.recv().await;
}
