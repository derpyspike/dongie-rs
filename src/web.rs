// SPDX-License-Identifier: EUPL-1.2
//! HTTP: web UI + logged-in API on `web_listen`, unauthenticated API on `api_listen`.
//!
//! Access levels:
//!   admin  — everything: SMS, actions, sensitive fields (IMEI, ICCID, number, client MACs)
//!   guest  — dashboard only (logged-in guest user, or anyone if `anonymous_guest`)
//! CSRF: every state-changing request must carry the header `X-Dongie: 1`
//! (browsers can't add custom headers cross-site without a CORS preflight, which we never allow).

use crate::config::Config;
use crate::sms::{self, Sms};
use crate::status::Status;
use crate::users::{self, Role, Sessions};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;

const INDEX: &str = include_str!("../web/index.html");
const COOKIE: &str = "dongie_session";

#[derive(Clone)]
pub struct App {
    pub cfg: Arc<Config>,
    /// config.toml path (the Settings tab reads it to show what's overridden)
    pub cfg_path: String,
    /// notified to restart dongie (systemd starts it again) after a settings change
    pub restart: Arc<tokio::sync::Notify>,
    pub status: Arc<RwLock<Status>>,
    pub history: Arc<tokio::sync::Mutex<crate::history::History>>,
    pub sms: Option<Sms>,
    pub ims: crate::ims::Ims,
    pub netsel: crate::netsel::NetSel,
    pub contacts: Arc<tokio::sync::RwLock<crate::contacts::Contacts>>,
    pub update: crate::update::Shared,
    pub sessions: Arc<Sessions>,
    pub login_gate: Arc<tokio::sync::Mutex<()>>,
}

enum Access {
    Admin(String),
    Guest(Option<String>),
    Denied,
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

fn token(h: &HeaderMap) -> Option<String> {
    let c = h.get(header::COOKIE)?.to_str().ok()?;
    c.split(';').find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == COOKIE).then(|| v.to_string())
    })
}

fn access(app: &App, h: &HeaderMap) -> Access {
    if let Some(s) = token(h).and_then(|t| app.sessions.get(&t)) {
        return match s.role {
            Role::Admin => Access::Admin(s.user),
            Role::Guest => Access::Guest(Some(s.user)),
        };
    }
    if app.cfg.anonymous_guest && guest_network_ok(&app.cfg.guest_networks, peer(h)) {
        Access::Guest(None)
    } else {
        Access::Denied
    }
}

/// Set by `peer_layer` from the TCP connection; a client-sent one is dropped.
const PEER: &str = "x-dongie-peer";

fn peer(h: &HeaderMap) -> Option<std::net::IpAddr> {
    h.get(PEER)?.to_str().ok()?.parse().ok()
}

/// Puts the client's address into the request for `access` (which only sees
/// headers), after removing any client-sent copy.
pub async fn peer_layer(mut req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    req.headers_mut().remove(PEER);
    let ip =
        req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip().to_canonical());
    if let Some(v) = ip.and_then(|ip| HeaderValue::from_str(&ip.to_string()).ok()) {
        req.headers_mut().insert(PEER, v);
    }
    next.run(req).await
}

/// No list = everywhere; otherwise the client must be in one of the networks
/// (unknown address = no).
fn guest_network_ok(nets: &[String], ip: Option<std::net::IpAddr>) -> bool {
    if nets.is_empty() {
        return true;
    }
    let Some(ip) = ip else { return false };
    nets.iter().filter_map(|n| crate::wgedit::parse_cidr(n)).any(|(net, p)| in_net(ip, net, p))
}

fn in_net(ip: std::net::IpAddr, net: std::net::IpAddr, prefix: u32) -> bool {
    use std::net::IpAddr::{V4, V6};
    match (ip, net) {
        (V4(a), V4(n)) => {
            let m = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
            u32::from(a) & m == u32::from(n) & m
        }
        (V6(a), V6(n)) => {
            let m = if prefix == 0 { 0 } else { u128::MAX << (128 - prefix) };
            u128::from(a) & m == u128::from(n) & m
        }
        _ => false,
    }
}

fn csrf_ok(h: &HeaderMap) -> bool {
    h.get("x-dongie").is_some()
}

/// Admin + CSRF check for state-changing endpoints.
// Response as the error is the usual axum pattern; boxing it everywhere only adds noise.
#[allow(clippy::result_large_err)]
fn require_admin(app: &App, h: &HeaderMap, mutating: bool) -> Result<String, Response> {
    if mutating && !csrf_ok(h) {
        return Err(err(StatusCode::FORBIDDEN, "missing X-Dongie header"));
    }
    match access(app, h) {
        Access::Admin(u) => Ok(u),
        Access::Guest(_) => Err(err(StatusCode::FORBIDDEN, "admin only")),
        Access::Denied => Err(err(StatusCode::UNAUTHORIZED, "login required")),
    }
}

// ---------------- web listener ----------------

pub fn web_router(app: App) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/me", get(me))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/status", get(status))
        .route("/api/update", get(update_info))
        .route("/api/update/check", post(update_check))
        .route("/api/update/apply", post(update_apply))
        .route("/api/history", get(history))
        .route("/api/messages", get(messages).post(send))
        .route("/api/messages/read", post(mark_read))
        .route("/api/messages/{id}", delete(delete_msg))
        .route("/api/calls", get(calls))
        .route("/api/calls/{id}/reject", post(reject_call))
        .route("/api/calls/{id}/{op}", post(call_op))
        .route("/api/calls/dial", post(dial))
        .route("/api/network", get(network_info))
        .route("/api/network/select", post(network_select))
        .route("/api/ussd/{op}", post(ussd))
        .route("/api/features", get(features_get))
        .route("/api/features/{name}", post(features_set))
        .route("/api/ims/mode", post(ims_mode_set))
        .route("/api/ims/voicemail", get(vm_list))
        .route("/api/ims/voicemail/config", post(vm_config_set))
        .route("/api/ims/voicemail/{id}", get(vm_audio).delete(vm_delete))
        .route(
            "/api/ims/greeting",
            get(greeting_get)
                .post(greeting_set)
                .delete(greeting_delete)
                .layer(axum::extract::DefaultBodyLimit::max(3 << 20)),
        )
        .route("/api/firewall", get(firewall_get).post(firewall_set))
        .route("/api/firewall/confirm", post(firewall_confirm))
        .route("/api/firewall/rollback", post(firewall_rollback))
        .route("/api/sim", get(sim_get))
        .route("/api/sim/unlock", post(sim_unlock))
        .route("/api/sim/forget", post(sim_forget))
        .route("/api/settings", get(settings_get).post(settings_set))
        .route("/api/settings/confirm", post(settings_confirm))
        .route("/api/notify", get(notify_list))
        .route("/api/reticulum", get(reticulum_info))
        .route("/api/reticulum/restart", post(reticulum_restart))
        .route("/api/mesh", get(mesh_info))
        .route("/api/mesh/config", get(mesh_config_get).post(mesh_config_set))
        .route("/api/mesh/service", post(mesh_service))
        .route("/api/contacts", get(contacts_list).post(contacts_set))
        .route("/api/notify/test", post(notify_test))
        .route("/api/action/{name}", post(action))
        .route("/api/wifi/hotspot", post(wifi_hotspot))
        .route("/api/wifi/scan", post(wifi_scan))
        .route("/api/net", get(net_info))
        .route("/api/wireguard/import", post(wg_import))
        .route("/api/wireguard/action", post(wg_action))
        .route("/api/wireguard/config/{name}", get(wg_config_get).post(wg_config_set))
        .route("/api/wireguard/create", post(wg_create))
        .route("/api/nettest", post(net_test))
        .route("/api/wifi/connect", post(wifi_connect))
        .route("/api/wifi/activate", post(wifi_activate))
        .route("/api/wifi/forget", post(wifi_forget))
        .layer(axum::middleware::from_fn(crate::squeeze::layer))
        .layer(axum::middleware::from_fn(peer_layer))
        .with_state(app)
}

async fn index() -> impl IntoResponse {
    (
        [
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(
                    "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; frame-ancestors 'none'",
                ),
            ),
        ],
        Html(INDEX),
    )
}

/// GET /api/update: installed/latest version (guests: only whether there is one).
async fn update_info(State(app): State<App>, h: HeaderMap) -> Response {
    let access = access(&app, &h);
    if matches!(access, Access::Denied) {
        return err(StatusCode::UNAUTHORIZED, "login required");
    }
    let i = {
        let mut s = app.update.write().await;
        s.settle();
        s.clone()
    };
    match access {
        Access::Admin(_) => Json(i).into_response(),
        _ => Json(i.for_guest()).into_response(),
    }
}

/// POST /api/update/check (admin): ask GitHub now.
async fn update_check(State(app): State<App>, h: HeaderMap) -> Response {
    if let Err(r) = require_admin(&app, &h, true) {
        return r;
    }
    crate::update::check(&app.cfg.update_repo, &app.update).await;
    let i = {
        let mut s = app.update.write().await;
        s.settle();
        s.clone()
    };
    Json(i).into_response()
}

/// POST /api/update/apply (admin): download, verify, hand over to the root helper.
async fn update_apply(State(app): State<App>, h: HeaderMap) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    {
        let mut s = app.update.write().await;
        if s.state != "idle" {
            return err(StatusCode::CONFLICT, "an update is already running");
        }
        if !s.newer || !s.signed {
            return err(StatusCode::BAD_REQUEST, "no signed newer release to install");
        }
        s.state = "downloading".into();
        s.error = None;
        eprintln!("update: {user} installs {}", s.tag.as_deref().unwrap_or("?"));
    }
    let (dir, shared) = (app.cfg.state_dir.clone(), app.update.clone());
    tokio::spawn(async move { crate::update::apply(&dir, shared).await });
    Json(json!({ "ok": true })).into_response()
}

async fn me(State(app): State<App>, h: HeaderMap) -> Response {
    let (user, role) = match access(&app, &h) {
        Access::Admin(u) => (Some(u), "admin"),
        Access::Guest(u) => (u, "guest"),
        Access::Denied => (None, "none"),
    };
    Json(json!({
        "user": user,
        "role": role,
        // effective for this client (guest_networks may exclude it)
        "anonymous_guest": app.cfg.anonymous_guest && guest_network_ok(&app.cfg.guest_networks, peer(&h)),
        "sms_available": app.sms.is_some(),
        // fresh install: tell the login dialog how to create the first user
        "no_users": users::load(&app.cfg.users_file).is_empty(),
        "ussd": app.cfg.ussd,
        "persist": app.cfg.persist,
        "reticulum": crate::reticulum::available(&app.cfg),
        "mesh": crate::mesh::available(&app.cfg),
        "version": env!("CARGO_PKG_VERSION"),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct LoginReq {
    user: String,
    pass: String,
}

async fn login(State(app): State<App>, h: HeaderMap, Json(req): Json<LoginReq>) -> Response {
    if !csrf_ok(&h) {
        return err(StatusCode::FORBIDDEN, "missing X-Dongie header");
    }
    // one login attempt at a time + delay on failure = slow brute force
    let _gate = app.login_gate.lock().await;
    let users = users::load(&app.cfg.users_file);
    let found = users.into_iter().find(|u| u.name == req.user);
    let pass = req.pass.clone();
    let ok_role = tokio::task::spawn_blocking(move || {
        found.and_then(|u| users::verify_password(&pass, &u.hash).then_some(u.role))
    })
    .await
    .ok()
    .flatten();
    let Some(role) = ok_role else {
        eprintln!("login failed for {:?}", req.user);
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        return err(StatusCode::UNAUTHORIZED, "wrong user or password");
    };
    eprintln!("login ok: {} ({})", req.user, role.as_str());
    let t = app.sessions.create(&req.user, role);
    let cookie = format!("{COOKIE}={t}; Path=/; HttpOnly; SameSite=Strict; Max-Age=604800");
    ([(header::SET_COOKIE, cookie)], Json(json!({ "user": req.user, "role": role.as_str() }))).into_response()
}

async fn logout(State(app): State<App>, h: HeaderMap) -> Response {
    if let Some(t) = token(&h) {
        app.sessions.remove(&t);
    }
    (
        [(header::SET_COOKIE, format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"))],
        Json(json!({ "ok": true })),
    )
        .into_response()
}

async fn status(State(app): State<App>, h: HeaderMap) -> Response {
    let st = app.status.read().await;
    match access(&app, &h) {
        Access::Admin(_) => Json(st.clone()).into_response(),
        Access::Guest(_) => Json(st.for_guest()).into_response(),
        Access::Denied => err(StatusCode::UNAUTHORIZED, "login required"),
    }
}

#[derive(Deserialize)]
struct HistoryQ {
    range: Option<u64>,
    points: Option<usize>,
}

async fn history_json(app: &App, q: HistoryQ) -> Response {
    let v = app.history.lock().await.query(
        q.range.unwrap_or(3600),
        crate::cmd::now(),
        q.points.unwrap_or(240).clamp(10, 600),
    );
    Json(v).into_response()
}

async fn history(State(app): State<App>, h: HeaderMap, Query(q): Query<HistoryQ>) -> Response {
    if let Access::Denied = access(&app, &h) {
        return err(StatusCode::UNAUTHORIZED, "login required");
    }
    history_json(&app, q).await
}

async fn messages(State(app): State<App>, h: HeaderMap) -> Response {
    if let Err(r) = require_admin(&app, &h, false) {
        return r;
    }
    list_messages(&app).await
}

async fn list_messages(app: &App) -> Response {
    let Some(sms) = &app.sms else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "ModemManager not reachable");
    };
    let st = sms.store.lock().await;
    let mut msgs = st.msgs.clone();
    msgs.reverse();
    Json(json!({ "unread": st.unread(), "messages": msgs })).into_response()
}

// ---------------- mobile network selection (admin) ----------------

async fn network_info(State(app): State<App>, h: HeaderMap) -> Response {
    if let Err(r) = require_admin(&app, &h, false) {
        return r;
    }
    Json(app.netsel.info().await).into_response()
}

#[derive(Deserialize)]
struct SelectReq {
    network: String,
}

/// GET /api/settings: editable settings, effective values (tokens hidden).
async fn settings_get(State(app): State<App>, h: HeaderMap) -> Response {
    if let Err(r) = require_admin(&app, &h, false) {
        return r;
    }
    let base = crate::settings::read_table(std::path::Path::new(&app.cfg_path)).unwrap_or_default();
    Json(crate::settings::view(&app.cfg, &app.cfg.state_dir, &base)).into_response()
}

/// POST /api/settings {"changes": {key: value | null}}: validate, write
/// settings.toml, restart. Listen-address changes must be confirmed afterwards.
async fn settings_set(State(app): State<App>, h: HeaderMap, Json(req): Json<serde_json::Value>) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some(changes) = req.get("changes").and_then(|c| c.as_object()) else {
        return err(StatusCode::BAD_REQUEST, "expected {\"changes\": {...}}");
    };
    if crate::settings::pending_left(&app.cfg.state_dir).is_some() {
        return err(StatusCode::CONFLICT, "confirm or wait out the pending listen-address change first");
    }
    match crate::settings::apply(&app.cfg_path, &app.cfg, changes) {
        Ok(a) => {
            let keys: Vec<&String> = changes.keys().collect();
            eprintln!("settings: {user} changed {keys:?}{}", if a.risky { " (needs confirmation)" } else { "" });
            let restart = app.restart.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                restart.notify_one();
            });
            Json(json!({ "ok": true, "restarting": true, "confirm_within": a.risky.then_some(crate::settings::CONFIRM_SECS) }))
                .into_response()
        }
        Err(e) => err(StatusCode::BAD_REQUEST, &e),
    }
}

/// POST /api/settings/confirm: keep a pending listen-address change.
async fn settings_confirm(State(app): State<App>, h: HeaderMap) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let had = crate::settings::confirm(&app.cfg.state_dir);
    if had {
        eprintln!("settings: {user} confirmed the listen-address change");
    }
    Json(json!({ "ok": true, "was_pending": had })).into_response()
}

/// GET /api/contacts: admin only (names belong with the numbers admins see).
async fn contacts_list(State(app): State<App>, h: HeaderMap) -> Response {
    if let Err(r) = require_admin(&app, &h, false) {
        return r;
    }
    Json(json!({ "contacts": app.contacts.read().await.list })).into_response()
}

#[derive(Deserialize)]
struct ContactReq {
    number: String,
    #[serde(default)]
    name: String,
}

/// POST /api/contacts {number, name}: add or rename; empty name deletes.
async fn contacts_set(State(app): State<App>, h: HeaderMap, Json(req): Json<ContactReq>) -> Response {
    if let Err(r) = require_admin(&app, &h, true) {
        return r;
    }
    match app.contacts.write().await.set(&req.number, &req.name) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, &e),
    }
}

/// GET /api/reticulum: daemon, interfaces, tables, storage, LXMF. Guests see it
/// too, minus the recent notifications (target names and recipient addresses).
async fn reticulum_info(State(app): State<App>, h: HeaderMap) -> Response {
    let admin = match access(&app, &h) {
        Access::Admin(_) => true,
        Access::Guest(_) => false,
        Access::Denied => return err(StatusCode::UNAUTHORIZED, "login required"),
    };
    if !crate::reticulum::available(&app.cfg) {
        return err(StatusCode::NOT_FOUND, "no Reticulum daemon configured (lnsd_unit / lnstatus)");
    }
    let mut info = crate::reticulum::info(&app.cfg).await;
    if !admin {
        info["recent"] = json!([]);
        info["lxmf"]["targets"] = json!(null);
    }
    Json(info).into_response()
}

/// GET /api/mesh: muxtastic's view of the Meshtastic node. Guests get the status,
/// node list and totals; messages, positions and addresses are admin-only.
async fn mesh_info(State(app): State<App>, h: HeaderMap) -> Response {
    let admin = match access(&app, &h) {
        Access::Admin(_) => true,
        Access::Guest(_) => false,
        Access::Denied => return err(StatusCode::UNAUTHORIZED, "login required"),
    };
    if !crate::mesh::available(&app.cfg) {
        return err(StatusCode::NOT_FOUND, "no muxtastic configured (muxtastic_url / muxtastic_unit)");
    }
    Json(crate::mesh::info(&app.cfg, admin).await).into_response()
}

/// GET /api/mesh/config (admin): the muxtastic settings dongie can edit.
async fn mesh_config_get(State(app): State<App>, h: HeaderMap) -> Response {
    if let Err(r) = require_admin(&app, &h, false) {
        return r;
    }
    match crate::muxcfg::read(&app.cfg.muxtastic_config).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::NOT_FOUND, &e),
    }
}

/// systemctl <verb> on muxtastic's unit (polkit: start/stop/restart of that unit only).
async fn mux_unit(app: &App, verb: &str) -> Result<(), String> {
    let unit = format!("{}.service", app.cfg.muxtastic_unit);
    match crate::cmd::run_any_t("systemctl", &[verb, &unit], 30).await {
        Some((true, _, _)) => Ok(()),
        Some((false, _, e)) => Err(e.trim().chars().take(200).collect()),
        None => Err("systemctl timed out".into()),
    }
}

/// POST /api/mesh/config (admin): write the settings into muxtastic.toml
/// (comments kept, old file as .bak) and restart muxtastic if it runs.
async fn mesh_config_set(
    State(app): State<App>,
    h: HeaderMap,
    Json(req): Json<crate::muxcfg::MuxSettings>,
) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let view = match crate::muxcfg::write(&app.cfg.muxtastic_config, &req).await {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, &e),
    };
    eprintln!(
        "mesh: {user} set muxtastic node {}:{}, admin from {:?}, history {} d",
        req.node_host.trim(),
        req.node_port,
        req.allow_admin_from,
        req.retention_days
    );
    let active = crate::cmd::run("systemctl", &["is-active", &app.cfg.muxtastic_unit]).await.is_some();
    let restart = if active { Some(mux_unit(&app, "restart").await) } else { None };
    Json(json!({
        "config": view,
        "restarted": matches!(restart, Some(Ok(()))),
        "restart_error": restart.and_then(|r| r.err()),
        "was_running": active,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct MeshServiceReq {
    action: String,
}

/// POST /api/mesh/service (admin): {"action": "start" | "stop" | "restart"}
async fn mesh_service(State(app): State<App>, h: HeaderMap, Json(req): Json<MeshServiceReq>) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if !matches!(req.action.as_str(), "start" | "stop" | "restart") {
        return err(StatusCode::BAD_REQUEST, "action: start, stop or restart");
    }
    eprintln!("mesh: {user} {}s {}", req.action, app.cfg.muxtastic_unit);
    match mux_unit(&app, &req.action).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, &e),
    }
}

/// POST /api/reticulum/restart: restart the daemon (polkit rule allows the
/// dongie user exactly this unit). Paths are relearned afterwards.
async fn reticulum_restart(State(app): State<App>, h: HeaderMap) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let unit = format!("{}.service", app.cfg.lnsd_unit);
    eprintln!("reticulum: {user} restarts {unit}");
    match crate::cmd::run_any_t("systemctl", &["restart", &unit], 30).await {
        Some((true, _, _)) => Json(json!({ "ok": true })).into_response(),
        Some((false, _, e)) => err(StatusCode::BAD_GATEWAY, e.trim()),
        None => err(StatusCode::GATEWAY_TIMEOUT, "systemctl timed out"),
    }
}

/// GET /api/notify: configured targets (labels only: no URLs, topics or tokens).
async fn notify_list(State(app): State<App>, h: HeaderMap) -> Response {
    if let Err(r) = require_admin(&app, &h, false) {
        return r;
    }
    let list: Vec<_> = app
        .cfg
        .notify
        .iter()
        .map(|t| {
            json!({
                "label": crate::notify::label(t),
                "kind": t.kind,
                "events": t.events,
                "include_text": t.include_text,
                "error": crate::notify::validate(t).err(),
            })
        })
        .collect();
    Json(json!({ "targets": list })).into_response()
}

/// POST /api/notify/test: send a test notification to every target, report each.
async fn notify_test(
    State(app): State<App>,
    h: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    eprintln!("notify: {user} sends a test notification");
    let mut results = vec![];
    // ?index=N tests one target, otherwise all
    let only: Option<usize> = q.get("index").and_then(|i| i.parse().ok());
    for (i, t) in app.cfg.notify.iter().enumerate() {
        if only.is_some_and(|o| o != i) {
            continue;
        }
        let r = match crate::notify::validate(t) {
            Ok(()) => crate::notify::send(t, &crate::notify::Event::Test, None).await,
            Err(e) => Err(e),
        };
        let (ok, detail, error) = match r {
            Ok(d) => (true, d, None),
            Err(e) => (false, String::new(), Some(e)),
        };
        results.push(json!({ "label": crate::notify::label(t), "ok": ok, "detail": detail, "error": error }));
    }
    Json(json!({ "results": results })).into_response()
}

#[derive(Deserialize)]
struct UssdReq {
    #[serde(default)]
    code: String,
    #[serde(default)]
    text: String,
}

/// POST /api/ussd/{initiate|respond|cancel}: admin only (codes can change things).
async fn ussd(State(app): State<App>, h: HeaderMap, Path(op): Path<String>, Json(req): Json<UssdReq>) -> Response {
    use crate::ussd::{valid_code, valid_response, Op};
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if !app.cfg.ussd {
        return err(StatusCode::FORBIDDEN, "USSD is off (ussd = true in config.toml)");
    }
    let Some(sms) = &app.sms else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "ModemManager not available");
    };
    let (code, text) = (req.code.trim(), req.text.trim());
    let op = match op.as_str() {
        "initiate" if valid_code(code) => Op::Initiate(code),
        "initiate" => return err(StatusCode::BAD_REQUEST, "code: digits, * # + (max 32)"),
        "respond" if valid_response(text) => Op::Respond(text),
        "respond" => return err(StatusCode::BAD_REQUEST, "text: 1-160 characters"),
        "cancel" => Op::Cancel,
        _ => return err(StatusCode::NOT_FOUND, "unknown USSD operation"),
    };
    // the code is logged, the reply not (it can contain the phone number or balance)
    match &op {
        Op::Initiate(c) => eprintln!("ussd: {user} sends {c}"),
        Op::Respond(_) => eprintln!("ussd: {user} answers"),
        Op::Cancel => eprintln!("ussd: {user} cancels"),
    }
    match sms.ussd(op).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, &e),
    }
}

/// GET /api/features: Tailscale serve + VoWiFi switches (admin).
async fn features_get(State(app): State<App>, h: HeaderMap) -> Response {
    if let Err(r) = require_admin(&app, &h, false) {
        return r;
    }
    let (serve, vowifi) =
        tokio::join!(crate::features::serve_state(), crate::features::vowifi_state(&app.cfg.state_dir));
    Json(crate::features::Features { serve, vowifi }).into_response()
}

// ---------------- firewall (admin) ----------------

/// GET /api/firewall: switch, config (the pending one while it waits for
/// confirmation), what is loaded, the zones and services to choose from.
async fn firewall_get(State(app): State<App>, h: HeaderMap) -> Response {
    if let Err(r) = require_admin(&app, &h, false) {
        return r;
    }
    use crate::firewall as fw;
    let dir = &app.cfg.state_dir;
    let left = fw::pending_left(dir);
    let config = if left.is_some() {
        std::fs::read_to_string(dir.join("firewall.pending"))
            .ok()
            .and_then(|t| toml::from_str::<fw::FwConfig>(&t).ok())
            .unwrap_or_default()
    } else {
        fw::load(dir).unwrap_or_default()
    };
    let file_error = fw::load(dir).err();
    Json(json!({
        "enabled": app.cfg.firewall,
        "config": config,
        "file": dir.join("firewall.toml").exists(),
        "file_error": file_error,
        "status": fw::read_status(),
        "pending_left": left,
        "confirm_secs": fw::CONFIRM_SECS,
        "zones": fw::ZONES,
        "services": fw::SERVICES.iter().map(|(n, d)| json!({"name": n, "desc": d})).collect::<Vec<_>>(),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct FirewallReq {
    config: crate::firewall::FwConfig,
}

/// POST /api/firewall {config}: apply provisionally; confirm within CONFIRM_SECS.
async fn firewall_set(State(app): State<App>, h: HeaderMap, Json(req): Json<FirewallReq>) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if !app.cfg.firewall {
        return err(StatusCode::CONFLICT, "the firewall is off (Settings: firewall)");
    }
    eprintln!("firewall: {user} tries new rules");
    match crate::firewall::try_config(&app.cfg.state_dir, &req.config).await {
        Ok(secs) => Json(json!({ "ok": true, "confirm_secs": secs })).into_response(),
        Err(e) => {
            eprintln!("firewall: rejected: {e}");
            err(StatusCode::BAD_REQUEST, &e)
        }
    }
}

async fn firewall_confirm(State(app): State<App>, h: HeaderMap) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    match crate::firewall::confirm(&app.cfg.state_dir).await {
        Ok(()) => {
            eprintln!("firewall: {user} confirmed the new rules");
            Json(json!({ "ok": true })).into_response()
        }
        Err(e) => err(StatusCode::CONFLICT, &e),
    }
}

async fn firewall_rollback(State(app): State<App>, h: HeaderMap) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    eprintln!("firewall: {user} rolls back");
    match crate::firewall::rollback(&app.cfg.state_dir).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

// ---------------- voicemail (admin) ----------------

const VM_CONFIG: &str = "ims-voicemail.json";
const GREETING: &str = "ims-greeting.wav";

fn valid_vm_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 40 && id.bytes().all(|b| b.is_ascii_digit() || b == b'-')
}

/// GET /api/ims/voicemail: recordings + settings (from dongie-ims) + greeting info.
async fn vm_list(State(app): State<App>, h: HeaderMap) -> Response {
    if let Err(r) = require_admin(&app, &h, false) {
        return r;
    }
    let mut v = match crate::ims::request(&app.cfg.ims_socket, "GET", "/v1/voicemail").await {
        Some((200, body)) => serde_json::from_str::<serde_json::Value>(&body).unwrap_or_else(|_| json!({})),
        _ => json!({ "voicemail": [], "unavailable": true }),
    };
    let g = std::fs::metadata(app.cfg.state_dir.join(GREETING)).ok();
    v["greeting_file"] = json!(g.map(|m| json!({ "bytes": m.len(), "secs": m.len().saturating_sub(44) / 32000 })));
    v["persist"] = json!(app.cfg.persist);
    if v.get("config").is_none() {
        // dongie-ims not running: show what is saved
        v["config"] = std::fs::read_to_string(app.cfg.state_dir.join(VM_CONFIG))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| json!({ "mode": "off", "delay_secs": 20, "greeting": "beep", "max_secs": 120 }));
    }
    Json(v).into_response()
}

#[derive(Deserialize)]
struct VmConfigReq {
    mode: String,
    delay_secs: u32,
    greeting: String,
    max_secs: u32,
}

/// POST /api/ims/voicemail/config: write the settings dongie-ims reads per call.
async fn vm_config_set(State(app): State<App>, h: HeaderMap, Json(r): Json<VmConfigReq>) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if !["off", "delay", "now"].contains(&r.mode.as_str()) || !["none", "beep", "file"].contains(&r.greeting.as_str()) {
        return err(StatusCode::BAD_REQUEST, "mode: off|delay|now, greeting: none|beep|file");
    }
    if !(1..=120).contains(&r.delay_secs) || !(10..=300).contains(&r.max_secs) {
        return err(StatusCode::BAD_REQUEST, "delay 1-120 s, max length 10-300 s");
    }
    if r.greeting == "file" && !app.cfg.state_dir.join(GREETING).exists() {
        return err(StatusCode::BAD_REQUEST, "no greeting file yet: upload or record one first");
    }
    let body = json!({ "mode": r.mode, "delay_secs": r.delay_secs, "greeting": r.greeting, "max_secs": r.max_secs, "persist": app.cfg.persist });
    let p = app.cfg.state_dir.join(VM_CONFIG);
    let tmp = p.with_extension("tmp");
    if let Err(e) = std::fs::write(&tmp, body.to_string()).and_then(|_| std::fs::rename(&tmp, &p)) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
    }
    eprintln!(
        "voicemail: {user} sets mode {} (delay {} s, greeting {}, max {} s)",
        r.mode, r.delay_secs, r.greeting, r.max_secs
    );
    Json(json!({ "ok": true, "config": body })).into_response()
}

/// GET /api/ims/voicemail/{id}: the recording as WAV.
async fn vm_audio(State(app): State<App>, h: HeaderMap, Path(id): Path<String>) -> Response {
    if let Err(r) = require_admin(&app, &h, false) {
        return r;
    }
    let id = id.trim_end_matches(".wav");
    if !valid_vm_id(id) {
        return err(StatusCode::BAD_REQUEST, "bad id");
    }
    match crate::ims::request_raw(&app.cfg.ims_socket, &format!("/v1/voicemail/{id}")).await {
        Some((200, bytes)) => {
            ([(header::CONTENT_TYPE, "audio/wav"), (header::CACHE_CONTROL, "private, max-age=3600")], bytes)
                .into_response()
        }
        Some((404, _)) => err(StatusCode::NOT_FOUND, "no such voicemail"),
        _ => err(StatusCode::BAD_GATEWAY, "dongie-ims not answering"),
    }
}

async fn vm_delete(State(app): State<App>, h: HeaderMap, Path(id): Path<String>) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if !valid_vm_id(&id) {
        return err(StatusCode::BAD_REQUEST, "bad id");
    }
    match crate::ims::request(&app.cfg.ims_socket, "DELETE", &format!("/v1/voicemail/{id}")).await {
        Some((200, _)) => {
            eprintln!("voicemail: {user} deleted {id}");
            Json(json!({ "ok": true })).into_response()
        }
        Some((404, _)) => err(StatusCode::NOT_FOUND, "no such voicemail"),
        _ => err(StatusCode::BAD_GATEWAY, "dongie-ims not answering"),
    }
}

/// A greeting must be what dongie-ims plays: WAV, PCM 16-bit, mono, 16 kHz,
/// at most 60 s (the page converts uploads and recordings to that).
fn check_greeting(b: &[u8]) -> Result<f64, String> {
    if b.len() < 44 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    let (mut off, mut fmt_ok, mut data) = (12usize, false, 0usize);
    while off + 8 <= b.len() {
        let id = &b[off..off + 4];
        let size = u32::from_le_bytes([b[off + 4], b[off + 5], b[off + 6], b[off + 7]]) as usize;
        let body = &b[off + 8..(off + 8 + size).min(b.len())];
        if id == b"fmt " && body.len() >= 16 {
            let u16le = |i: usize| u16::from_le_bytes([body[i], body[i + 1]]);
            let rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
            fmt_ok = u16le(0) == 1 && u16le(2) == 1 && rate == 16000 && u16le(14) == 16;
        } else if id == b"data" {
            data = body.len();
        }
        off += 8 + size + size % 2;
    }
    if !fmt_ok {
        return Err("need PCM 16-bit mono 16 kHz".into());
    }
    let secs = data as f64 / 32000.0;
    if !(0.3..=60.5).contains(&secs) {
        return Err(format!("{secs:.1} s: a greeting is 0.3 to 60 s"));
    }
    Ok(secs)
}

async fn greeting_set(State(app): State<App>, h: HeaderMap, body: axum::body::Bytes) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let secs = match check_greeting(&body) {
        Ok(s) => s,
        Err(e) => return err(StatusCode::BAD_REQUEST, &e),
    };
    let p = app.cfg.state_dir.join(GREETING);
    let tmp = p.with_extension("tmp");
    if let Err(e) = std::fs::write(&tmp, &body).and_then(|_| std::fs::rename(&tmp, &p)) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
    }
    eprintln!("voicemail: {user} saved a {secs:.1} s greeting");
    Json(json!({ "ok": true, "secs": secs })).into_response()
}

async fn greeting_get(State(app): State<App>, h: HeaderMap) -> Response {
    if let Err(r) = require_admin(&app, &h, false) {
        return r;
    }
    match std::fs::read(app.cfg.state_dir.join(GREETING)) {
        Ok(b) => ([(header::CONTENT_TYPE, "audio/wav"), (header::CACHE_CONTROL, "no-store")], b).into_response(),
        Err(_) => err(StatusCode::NOT_FOUND, "no greeting file"),
    }
}

async fn greeting_delete(State(app): State<App>, h: HeaderMap) -> Response {
    if let Err(r) = require_admin(&app, &h, true) {
        return r;
    }
    let _ = std::fs::remove_file(app.cfg.state_dir.join(GREETING));
    Json(json!({ "ok": true })).into_response()
}

#[derive(Deserialize)]
struct ModeReq {
    mode: String,
}

/// POST /api/ims/mode {"mode": "off" | "vowifi" | "volte" | "volte+vowifi" | "vowifi+volte"}: admin only.
async fn ims_mode_set(State(app): State<App>, h: HeaderMap, Json(req): Json<ModeReq>) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    eprintln!("ims: {user} sets calling mode {}", req.mode);
    match crate::features::ims_mode_set(&app.cfg.state_dir, &app.cfg.ims_socket, req.mode.trim()).await {
        Ok(v) => Json(json!({ "vowifi": v })).into_response(),
        Err(e) => {
            eprintln!("ims: mode {}: {e}", req.mode);
            err(StatusCode::BAD_REQUEST, &e)
        }
    }
}

#[derive(Deserialize)]
struct OnReq {
    on: bool,
}

/// POST /api/features/{serve|vowifi} {"on": bool}: admin only.
async fn features_set(
    State(app): State<App>,
    h: HeaderMap,
    Path(name): Path<String>,
    Json(req): Json<OnReq>,
) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    eprintln!("features: {user} turns {name} {}", if req.on { "on" } else { "off" });
    let r = match name.as_str() {
        "serve" => crate::features::serve_set(&app.cfg, req.on).await.map(|s| json!({ "serve": s })),
        "vowifi" => crate::features::vowifi_set(&app.cfg.state_dir, req.on).await.map(|v| json!({ "vowifi": v })),
        _ => return err(StatusCode::NOT_FOUND, "unknown feature"),
    };
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => {
            eprintln!("features: {name}: {e}");
            err(StatusCode::BAD_REQUEST, &e)
        }
    }
}

#[derive(Deserialize)]
struct SimUnlockReq {
    pin: String,
    #[serde(default)]
    puk: String,
    #[serde(default)]
    save: bool,
    #[serde(default)]
    last_try: bool,
}

/// GET /api/sim: lock state + attempts left + whether a PIN is saved (admin).
async fn sim_get(State(app): State<App>, h: HeaderMap) -> Response {
    if let Err(r) = require_admin(&app, &h, false) {
        return r;
    }
    let Some(sms) = &app.sms else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "ModemManager not available");
    };
    Json(sms.sim_state(&app.cfg.state_dir).await).into_response()
}

/// POST /api/sim/unlock {pin, puk?, save, last_try}: admin only. The PIN is never logged.
async fn sim_unlock(State(app): State<App>, h: HeaderMap, Json(req): Json<SimUnlockReq>) -> Response {
    use crate::sim::{valid_pin, valid_puk, Unlock};
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some(sms) = &app.sms else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "ModemManager not available");
    };
    let (pin, puk) = (req.pin.trim(), req.puk.trim());
    if !valid_pin(pin) {
        return err(StatusCode::BAD_REQUEST, "PIN: 4-8 digits");
    }
    let op = if puk.is_empty() {
        Unlock::Pin(pin)
    } else if valid_puk(puk) {
        Unlock::Puk(puk, pin)
    } else {
        return err(StatusCode::BAD_REQUEST, "PUK: 8 digits");
    };
    let what = if puk.is_empty() { "PIN" } else { "PUK" };
    eprintln!("sim: {user} sends the {what}{}", if req.save { " (and saves the PIN)" } else { "" });
    match sms.sim_unlock(&app.cfg.state_dir, op, req.save, req.last_try).await {
        Ok(()) => {
            eprintln!("sim: unlocked by {user}");
            Json(sms.sim_state(&app.cfg.state_dir).await).into_response()
        }
        Err(e) => {
            eprintln!("sim: {what} from {user} failed: {e}");
            err(StatusCode::BAD_REQUEST, &e)
        }
    }
}

/// POST /api/sim/forget: delete the saved PIN of the SIM in the modem.
async fn sim_forget(State(app): State<App>, h: HeaderMap) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some(sms) = &app.sms else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "ModemManager not available");
    };
    match sms.sim_forget(&app.cfg.state_dir).await {
        Ok(()) => {
            eprintln!("sim: {user} deleted the saved PIN");
            Json(sms.sim_state(&app.cfg.state_dir).await).into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

async fn network_select(State(app): State<App>, h: HeaderMap, Json(req): Json<SelectReq>) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let net = req.network.trim().to_string();
    eprintln!("netsel: {user} selects {}", if net.is_empty() { "automatic" } else { &net });
    match app.netsel.select(net).await {
        Ok(p) => (StatusCode::ACCEPTED, Json(json!({ "ok": true, "pending": p }))).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, &e),
    }
}

// ---------------- calls (dongie-ims, admin) ----------------

async fn calls(State(app): State<App>, h: HeaderMap) -> Response {
    if let Err(r) = require_admin(&app, &h, false) {
        return r;
    }
    list_calls(&app).await
}

async fn list_calls(app: &App) -> Response {
    let live = app.ims.calls.lock().await.clone();
    let calls = crate::ims::merged(&live, &app.ims.archive.lock().await);
    Json(json!({ "calls": calls })).into_response()
}

async fn reject_call(State(app): State<App>, h: HeaderMap, Path(id): Path<u64>) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    match app.ims.reject(id).await {
        Ok(()) => {
            eprintln!("calls: {user} rejected call {id}");
            Json(json!({ "ok": true })).into_response()
        }
        Err(e) => err(StatusCode::BAD_GATEWAY, &e),
    }
}

async fn ims_reply(code: u16, v: serde_json::Value) -> Response {
    let code = StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY);
    (code, Json(v)).into_response()
}

/// answer / hangup / dtmf for one call, passed through to dongie-ims.
async fn call_op(
    State(app): State<App>,
    h: HeaderMap,
    Path((id, op)): Path<(u64, String)>,
    body: Option<Json<serde_json::Value>>,
) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if !matches!(op.as_str(), "answer" | "hangup" | "dtmf") {
        return err(StatusCode::NOT_FOUND, "unknown call operation");
    }
    let body = body.map(|Json(v)| v).unwrap_or(json!({}));
    let (code, v) = app.ims.forward(&format!("/v1/calls/{id}/{op}"), &body).await;
    if op != "dtmf" {
        eprintln!("calls: {user} {op} call {id} -> {code}");
    }
    ims_reply(code, v).await
}

async fn dial(State(app): State<App>, h: HeaderMap, Json(body): Json<serde_json::Value>) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let (code, v) = app.ims.forward("/v1/dial", &body).await;
    eprintln!("calls: {user} dial -> {code}");
    ims_reply(code, v).await
}

#[derive(Deserialize)]
struct SendReq {
    to: String,
    text: String,
}

async fn send(State(app): State<App>, h: HeaderMap, Json(req): Json<SendReq>) -> Response {
    if let Err(r) = require_admin(&app, &h, true) {
        return r;
    }
    do_send(&app, req).await
}

async fn do_send(app: &App, req: SendReq) -> Response {
    let Some(sms) = &app.sms else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "ModemManager not reachable");
    };
    let to: String = req.to.chars().filter(|c| !c.is_whitespace()).collect();
    if !sms::valid_number(&to) {
        return err(StatusCode::BAD_REQUEST, "invalid number");
    }
    let n = req.text.chars().count();
    if n == 0 || n > 1000 {
        return err(StatusCode::BAD_REQUEST, "text must be 1-1000 characters");
    }
    let id = sms.send(to, req.text).await;
    (StatusCode::ACCEPTED, Json(json!({ "id": id, "state": "sending" }))).into_response()
}

#[derive(Deserialize)]
struct ReadReq {
    number: String,
}

async fn mark_read(State(app): State<App>, h: HeaderMap, Json(req): Json<ReadReq>) -> Response {
    if let Err(r) = require_admin(&app, &h, true) {
        return r;
    }
    let Some(sms) = &app.sms else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "ModemManager not reachable");
    };
    let mut st = sms.store.lock().await;
    for m in st.msgs.iter_mut().filter(|m| m.number == req.number) {
        m.read = true;
    }
    Json(json!({ "ok": true })).into_response()
}

async fn delete_msg(State(app): State<App>, h: HeaderMap, Path(id): Path<u64>) -> Response {
    if let Err(r) = require_admin(&app, &h, true) {
        return r;
    }
    let Some(sms) = &app.sms else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "ModemManager not reachable");
    };
    if sms.store.lock().await.delete(id) {
        Json(json!({ "ok": true })).into_response()
    } else {
        err(StatusCode::NOT_FOUND, "no such message")
    }
}

async fn action(State(app): State<App>, h: HeaderMap, Path(name): Path<String>) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let hs = app.cfg.hotspot_connection.clone();
    let lte = crate::cmd::active_gsm_connection().await.unwrap_or_else(|| app.cfg.lte_connection.clone());
    eprintln!("action {name} by {user}");
    match name.as_str() {
        "hotspot-on" | "hotspot-off" => {
            let verb = if name == "hotspot-on" { "up" } else { "down" };
            match crate::cmd::run_any("nmcli", &["con", verb, &hs]).await {
                Some((true, _, _)) => Json(json!({ "ok": true })).into_response(),
                Some((false, _, e)) => err(StatusCode::INTERNAL_SERVER_ERROR, e.trim()),
                None => err(StatusCode::GATEWAY_TIMEOUT, "nmcli timed out"),
            }
        }
        "lte-reconnect" => {
            // may take a while and may cut our own Tailscale path: run in background
            tokio::spawn(async move {
                let _ = crate::cmd::run_any("nmcli", &["con", "down", &lte]).await;
                let r = crate::cmd::run_any("nmcli", &["con", "up", &lte]).await;
                eprintln!("lte-reconnect: {:?}", r.map(|x| x.0));
            });
            (StatusCode::ACCEPTED, Json(json!({ "ok": true, "note": "reconnecting in background" }))).into_response()
        }
        "reboot" => {
            tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let r = crate::cmd::run_any("systemctl", &["reboot"]).await;
                eprintln!("reboot: {:?}", r);
            });
            (StatusCode::ACCEPTED, Json(json!({ "ok": true, "note": "rebooting in 2 s" }))).into_response()
        }
        _ => err(StatusCode::NOT_FOUND, "unknown action"),
    }
}

// ---------------- Wi-Fi (admin) ----------------

/// GET /api/wireguard/config/{name}: interface + peers, secrets only as "set".
async fn wg_config_get(State(app): State<App>, h: HeaderMap, Path(name): Path<String>) -> Response {
    if let Err(r) = require_admin(&app, &h, false) {
        return r;
    }
    match crate::wgedit::view(&name).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, &e),
    }
}

/// POST /api/wireguard/config/{name} {WgEdit}: save; an active tunnel is re-activated.
async fn wg_config_set(
    State(app): State<App>,
    h: HeaderMap,
    Path(name): Path<String>,
    Json(req): Json<crate::wgedit::WgEdit>,
) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    eprintln!(
        "wireguard: {user} edits {name:?} ({} peers{})",
        req.peers.len(),
        match req.private_key.as_deref() {
            Some("generate") => ", new key pair",
            Some(_) => ", new private key",
            None => "",
        }
    );
    match crate::wgedit::apply(&name, &req).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, &e),
    }
}

#[derive(Deserialize)]
struct WgCreateReq {
    name: String,
    #[serde(flatten)]
    edit: crate::wgedit::WgEdit,
}

/// POST /api/wireguard/create {name, …WgEdit}: new tunnel (own key pair unless
/// one is given), not started, not at boot.
async fn wg_create(State(app): State<App>, h: HeaderMap, Json(mut req): Json<WgCreateReq>) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if req.edit.private_key.is_none() {
        req.edit.private_key = Some("generate".into());
    }
    if let Err(e) = crate::wgedit::validate(&req.edit) {
        return err(StatusCode::BAD_REQUEST, &e);
    }
    eprintln!("wireguard: {user} creates {:?}", req.name);
    if let Err(e) = crate::wgedit::create(&req.name).await {
        return err(StatusCode::BAD_REQUEST, &e);
    }
    match crate::wgedit::apply(&req.name, &req.edit).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => {
            // don't leave a half-made tunnel behind
            let _ = crate::cmd::run_any("nmcli", &["connection", "delete", &req.name]).await;
            err(StatusCode::BAD_REQUEST, &e)
        }
    }
}

fn wifi_result(r: Result<String, String>) -> Response {
    match r {
        Ok(m) => Json(json!({ "ok": true, "message": m })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, &e),
    }
}

#[derive(Deserialize)]
struct HotspotReq {
    ssid: Option<String>,
    password: Option<String>,
    channel: Option<u32>,
}

async fn wifi_hotspot(State(app): State<App>, h: HeaderMap, Json(req): Json<HotspotReq>) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    eprintln!(
        "wifi: {user} requests hotspot change (ssid {:?}, new password {}, channel {:?})",
        req.ssid,
        if req.password.as_deref().is_some_and(|p| !p.is_empty()) { "given" } else { "no" },
        req.channel
    );
    wifi_result(crate::wifi::set_hotspot(&app.cfg.hotspot_connection, req.ssid, req.password, req.channel).await)
}

#[derive(Deserialize)]
struct ScanReq {
    #[serde(default)]
    pause_hotspot: bool,
}

async fn wifi_scan(State(app): State<App>, h: HeaderMap, Json(req): Json<ScanReq>) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    eprintln!("wifi: {user} scans (pause hotspot: {})", req.pause_hotspot);
    match crate::wifi::scan(&app.cfg.hotspot_connection, req.pause_hotspot).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, &e),
    }
}

// ---------------- network extras ----------------

#[derive(Deserialize)]
struct FreshQ {
    fresh: Option<u8>,
}

async fn net_info(State(app): State<App>, h: HeaderMap, Query(q): Query<FreshQ>) -> Response {
    match access(&app, &h) {
        // `?fresh=1` (after a change) skips the shared cache; only admins can change things
        Access::Admin(_) => Json(crate::net::collect_cached(q.fresh == Some(1)).await).into_response(),
        Access::Guest(_) => {
            let mut n = crate::net::collect_cached(false).await;
            n.for_guest();
            Json(n).into_response()
        }
        Access::Denied => err(StatusCode::UNAUTHORIZED, "login required"),
    }
}

#[derive(Deserialize)]
struct WgImportReq {
    name: String,
    config: String,
    #[serde(default)]
    route_all: bool,
    #[serde(default)]
    autoconnect: bool,
}

async fn wg_import(State(app): State<App>, h: HeaderMap, Json(req): Json<WgImportReq>) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    eprintln!(
        "wireguard: {user} imports {:?} (route_all {}, autoconnect {})",
        req.name, req.route_all, req.autoconnect
    );
    wifi_result(crate::net::wg_import(&req.name, &req.config, req.route_all, req.autoconnect).await)
}

#[derive(Deserialize)]
struct WgActionReq {
    name: String,
    action: String,
}

async fn wg_action(State(app): State<App>, h: HeaderMap, Json(req): Json<WgActionReq>) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    eprintln!("wireguard: {user} {} {:?}", req.action, req.name);
    wifi_result(crate::net::wg_action(&req.name, &req.action).await)
}

async fn net_test(State(app): State<App>, h: HeaderMap) -> Response {
    if let Err(r) = require_admin(&app, &h, true) {
        return r;
    }
    Json(json!({ "results": crate::net::connectivity_test().await })).into_response()
}

#[derive(Deserialize)]
struct ConnectReq {
    ssid: String,
    password: Option<String>,
}

async fn wifi_connect(State(app): State<App>, h: HeaderMap, Json(req): Json<ConnectReq>) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    eprintln!("wifi: {user} joins {:?}", req.ssid);
    wifi_result(crate::wifi::connect_new(&req.ssid, req.password.as_deref(), &app.cfg.hotspot_connection).await)
}

#[derive(Deserialize)]
struct NameReq {
    name: String,
}

async fn wifi_activate(State(app): State<App>, h: HeaderMap, Json(req): Json<NameReq>) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    eprintln!("wifi: {user} activates {:?}", req.name);
    wifi_result(crate::wifi::activate(&req.name, &app.cfg.hotspot_connection).await)
}

async fn wifi_forget(State(app): State<App>, h: HeaderMap, Json(req): Json<NameReq>) -> Response {
    let user = match require_admin(&app, &h, true) {
        Ok(u) => u,
        Err(r) => return r,
    };
    eprintln!("wifi: {user} forgets {:?}", req.name);
    wifi_result(crate::wifi::forget(&req.name, &app.cfg.hotspot_connection).await)
}

// ---------------- unauthenticated API listener ----------------

pub fn api_router(app: App) -> Router {
    let mut r = Router::new()
        .route("/", get(api_index))
        .route("/api/status", get(api_status))
        .route("/api/history", get(api_history));
    if app.cfg.api_allow_sms {
        r = r.route("/api/messages", get(api_messages).post(api_send)).route("/api/calls", get(api_calls));
    }
    r.with_state(app)
}

async fn api_index(State(app): State<App>) -> Response {
    let mut eps = vec!["GET /api/status", "GET /api/history?range=SECS&points=N"];
    if app.cfg.api_allow_sms {
        eps.push("GET /api/messages");
        eps.push("POST /api/messages {\"to\":\"+34...\",\"text\":\"...\"}");
        eps.push("GET /api/calls");
    }
    Json(json!({ "name": "dongie", "version": env!("CARGO_PKG_VERSION"), "endpoints": eps })).into_response()
}

async fn api_status(State(app): State<App>) -> Response {
    Json(app.status.read().await.for_guest()).into_response()
}

async fn api_history(State(app): State<App>, Query(q): Query<HistoryQ>) -> Response {
    history_json(&app, q).await
}

async fn api_messages(State(app): State<App>) -> Response {
    list_messages(&app).await
}

async fn api_calls(State(app): State<App>) -> Response {
    list_calls(&app).await
}

async fn api_send(State(app): State<App>, Json(req): Json<SendReq>) -> Response {
    do_send(&app, req).await
}

// ---------------- binding ----------------

/// Bind, retrying: at boot the Tailscale IP may not exist yet.
#[cfg(test)]
mod vm_tests {
    #[test]
    fn greeting_wav_check() {
        let wav = |rate: u32, ch: u16, n: usize| {
            let mut b = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0".to_vec();
            b.extend(ch.to_le_bytes());
            b.extend(rate.to_le_bytes());
            b.extend((rate * 2).to_le_bytes());
            b.extend([2, 0, 16, 0]);
            b.extend(b"data");
            b.extend(((n * 2) as u32).to_le_bytes());
            b.extend(vec![0u8; n * 2]);
            b
        };
        assert!(super::check_greeting(&wav(16000, 1, 16000)).is_ok());
        assert!(super::check_greeting(&wav(44100, 1, 44100)).is_err());
        assert!(super::check_greeting(&wav(16000, 2, 16000)).is_err());
        assert!(super::check_greeting(&wav(16000, 1, 16000 * 61)).is_err());
        assert!(super::check_greeting(b"not a wav").is_err());
        assert!(super::valid_vm_id("1795712345-3") && !super::valid_vm_id("../x") && !super::valid_vm_id(""));
    }
}

#[cfg(test)]
mod tests {
    use super::{guest_network_ok, in_net};

    #[test]
    fn guest_networks() {
        let ip = |s: &str| Some(s.parse().unwrap());
        let nets = vec!["192.168.100.0/24".to_string(), "100.64.0.0/10".into(), "fdde:ad:beef::/64".into()];
        assert!(guest_network_ok(&nets, ip("192.168.100.42")));
        assert!(guest_network_ok(&nets, ip("100.101.102.103")));
        assert!(guest_network_ok(&nets, ip("fdde:ad:beef::1234")));
        assert!(!guest_network_ok(&nets, ip("192.168.1.20"))); // upstream Wi-Fi
        assert!(!guest_network_ok(&nets, ip("10.0.0.1"))); // LTE side
        assert!(!guest_network_ok(&nets, ip("2001:db8::1")));
        assert!(!guest_network_ok(&nets, None));
        assert!(guest_network_ok(&[], None)); // no list = everywhere
        assert!(in_net("1.2.3.4".parse().unwrap(), "0.0.0.0".parse().unwrap(), 0));
        assert!(!in_net("::ffff:1.2.3.4".parse().unwrap(), "1.2.3.0".parse().unwrap(), 24));
        // canonicalized before
    }
}
