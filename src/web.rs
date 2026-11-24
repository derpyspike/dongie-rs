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
use tokio::net::TcpListener;
use tokio::sync::RwLock;

const INDEX: &str = include_str!("../web/index.html");
const COOKIE: &str = "dongie_session";

#[derive(Clone)]
pub struct App {
    pub cfg: Arc<Config>,
    pub status: Arc<RwLock<Status>>,
    pub history: Arc<tokio::sync::Mutex<crate::history::History>>,
    pub sms: Option<Sms>,
    pub ims: crate::ims::Ims,
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
    if app.cfg.anonymous_guest {
        Access::Guest(None)
    } else {
        Access::Denied
    }
}

fn csrf_ok(h: &HeaderMap) -> bool {
    h.get("x-dongie").is_some()
}

/// Admin + CSRF check for state-changing endpoints.
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
        .route("/api/history", get(history))
        .route("/api/messages", get(messages).post(send))
        .route("/api/messages/read", post(mark_read))
        .route("/api/messages/{id}", delete(delete_msg))
        .route("/api/calls", get(calls))
        .route("/api/calls/{id}/reject", post(reject_call))
        .route("/api/calls/{id}/{op}", post(call_op))
        .route("/api/calls/dial", post(dial))
        .route("/api/action/{name}", post(action))
        .route("/api/wifi/hotspot", post(wifi_hotspot))
        .route("/api/wifi/scan", post(wifi_scan))
        .route("/api/net", get(net_info))
        .route("/api/wireguard/import", post(wg_import))
        .route("/api/wireguard/action", post(wg_action))
        .route("/api/nettest", post(net_test))
        .route("/api/wifi/connect", post(wifi_connect))
        .route("/api/wifi/activate", post(wifi_activate))
        .route("/api/wifi/forget", post(wifi_forget))
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

async fn me(State(app): State<App>, h: HeaderMap) -> Response {
    let (user, role) = match access(&app, &h) {
        Access::Admin(u) => (Some(u), "admin"),
        Access::Guest(u) => (u, "guest"),
        Access::Denied => (None, "none"),
    };
    Json(json!({
        "user": user,
        "role": role,
        "anonymous_guest": app.cfg.anonymous_guest,
        "sms_available": app.sms.is_some(),
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
    (
        [(header::SET_COOKIE, cookie)],
        Json(json!({ "user": req.user, "role": role.as_str() })),
    )
        .into_response()
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

// ---------------- calls (dongie-ims, admin) ----------------

async fn calls(State(app): State<App>, h: HeaderMap) -> Response {
    if let Err(r) = require_admin(&app, &h, false) {
        return r;
    }
    list_calls(&app).await
}

async fn list_calls(app: &App) -> Response {
    let calls = app.ims.calls.lock().await.clone();
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
    let lte = app.cfg.lte_connection.clone();
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
            (StatusCode::ACCEPTED, Json(json!({ "ok": true, "note": "reconnecting in background" })))
                .into_response()
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

async fn net_info(State(app): State<App>, h: HeaderMap) -> Response {
    match access(&app, &h) {
        Access::Admin(_) => Json(crate::net::collect().await).into_response(),
        Access::Guest(_) => {
            let mut n = crate::net::collect().await;
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
    eprintln!("wireguard: {user} imports {:?} (route_all {}, autoconnect {})", req.name, req.route_all, req.autoconnect);
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
pub async fn bind_retry(addr: &str) -> TcpListener {
    let mut warned = false;
    loop {
        match TcpListener::bind(addr).await {
            Ok(l) => {
                eprintln!("listening on {addr}");
                return l;
            }
            Err(e) => {
                if !warned {
                    eprintln!("bind {addr}: {e} — retrying every 5 s");
                    warned = true;
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    }
}
