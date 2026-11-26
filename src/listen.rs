// SPDX-License-Identifier: EUPL-1.2
//! Listen addresses: `ip:port`, or `tailscale:PORT` = this stick's Tailscale
//! IPv4 and IPv6 address, whatever they are. dongie waits until Tailscale has
//! an address, and moves the listener when it changes (re-login to another
//! tailnet) or goes away (logout), checking every 30 s.

use axum::Router;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use tokio::net::TcpListener;

const RETRY: Duration = Duration::from_secs(5);
const RECHECK: Duration = Duration::from_secs(30);
const TS_IFACE: &str = "tailscale0";

#[derive(Debug, Clone, PartialEq)]
pub enum Spec {
    Fixed(SocketAddr),
    Tailscale { v6: bool, port: u16 },
}

impl std::fmt::Display for Spec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Spec::Fixed(a) => write!(f, "{a}"),
            Spec::Tailscale { v6, port } => write!(f, "tailscale{}:{port}", if *v6 { " IPv6" } else { " IPv4" }),
        }
    }
}

/// "ip:port" or "tailscale:PORT" (both IPv4 and IPv6). None = invalid.
pub fn parse(a: &str) -> Option<Vec<Spec>> {
    let a = a.trim();
    if let Some(p) = a.strip_prefix("tailscale:") {
        let port: u16 = p.parse().ok().filter(|p| *p != 0)?;
        return Some(vec![Spec::Tailscale { v6: false, port }, Spec::Tailscale { v6: true, port }]);
    }
    a.parse().ok().map(|s| vec![Spec::Fixed(s)])
}

pub fn valid(a: &str) -> bool {
    parse(a).is_some()
}

/// Tailscale's own address from `ip -j addr show dev tailscale0` output:
/// IPv4 in 100.64.0.0/10, IPv6 in fd7a:115c:a1e0::/48.
pub fn ts_addr_from(json: &str, v6: bool) -> Option<IpAddr> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    v.as_array()?
        .iter()
        .flat_map(|i| i["addr_info"].as_array().into_iter().flatten())
        .filter_map(|a| a["local"].as_str()?.parse::<IpAddr>().ok())
        .find(|ip| match ip {
            IpAddr::V4(x) if !v6 => x.octets()[0] == 100 && (x.octets()[1] & 0xc0) == 64,
            IpAddr::V6(x) if v6 => x.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
            _ => false,
        })
}

pub async fn resolve(spec: &Spec) -> Option<SocketAddr> {
    match spec {
        Spec::Fixed(a) => Some(*a),
        Spec::Tailscale { v6, port } => {
            let fam = if *v6 { "-6" } else { "-4" };
            let out = crate::cmd::run("ip", &["-j", fam, "addr", "show", "dev", TS_IFACE]).await?;
            ts_addr_from(&out, *v6).map(|ip| SocketAddr::new(ip, *port))
        }
    }
}

/// Serve `router` on every address `addr` stands for. Runs forever.
pub async fn serve<F>(addr: String, router: F)
where
    F: Fn() -> Router + Clone + Send + Sync + 'static,
{
    let Some(specs) = parse(&addr) else {
        eprintln!("listen {addr:?}: not ip:port or tailscale:PORT, ignored");
        return std::future::pending().await;
    };
    let mut set = tokio::task::JoinSet::new();
    for s in specs {
        set.spawn(serve_one(s, router.clone()));
    }
    while set.join_next().await.is_some() {}
}

async fn serve_one<F>(spec: Spec, router: F)
where
    F: Fn() -> Router,
{
    let mut warned = String::new();
    let warn = |warned: &mut String, msg: String| {
        if msg != *warned {
            eprintln!("{msg} (retrying every {} s)", RETRY.as_secs());
            *warned = msg;
        }
    };
    loop {
        let Some(sa) = resolve(&spec).await else {
            warn(&mut warned, format!("listen {spec}: no address yet"));
            tokio::time::sleep(RETRY).await;
            continue;
        };
        let l = match TcpListener::bind(sa).await {
            Ok(l) => l,
            Err(e) => {
                warn(&mut warned, format!("bind {sa}: {e}"));
                tokio::time::sleep(RETRY).await;
                continue;
            }
        };
        warned.clear();
        match &spec {
            Spec::Fixed(_) => eprintln!("listening on {sa}"),
            _ => eprintln!("listening on {sa} ({spec})"),
        }
        let moved = {
            let spec = spec.clone();
            async move {
                if matches!(spec, Spec::Fixed(_)) {
                    return std::future::pending().await;
                }
                loop {
                    tokio::time::sleep(RECHECK).await;
                    if resolve(&spec).await != Some(sa) {
                        eprintln!("{spec}: address {sa} changed or gone, moving the listener");
                        return;
                    }
                }
            }
        };
        let svc = router().into_make_service_with_connect_info::<SocketAddr>();
        if let Err(e) = axum::serve(l, svc).with_graceful_shutdown(moved).await {
            eprintln!("server {sa}: {e}");
            tokio::time::sleep(RETRY).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        assert_eq!(parse("127.0.0.1:8040"), Some(vec![Spec::Fixed("127.0.0.1:8040".parse().unwrap())]));
        assert_eq!(parse("[::1]:8040").map(|v| v.len()), Some(1));
        assert_eq!(
            parse("tailscale:8040"),
            Some(vec![Spec::Tailscale { v6: false, port: 8040 }, Spec::Tailscale { v6: true, port: 8040 }])
        );
        assert!(parse("tailscale:0").is_none() && parse("tailscale:").is_none() && parse("tailscale").is_none());
        assert!(parse("localhost:8040").is_none() && parse("8040").is_none());
    }

    #[test]
    fn tailscale_address_from_ip_json() {
        // `ip -j addr show dev tailscale0` on a UZ801 (2026-11-26), trimmed, addresses made up
        let j = r#"[{"ifindex":7,"ifname":"tailscale0","flags":["POINTOPOINT","MULTICAST","NOARP","UP","LOWER_UP"],"mtu":1280,"addr_info":[
            {"family":"inet","local":"100.64.56.78","prefixlen":32,"scope":"global"},
            {"family":"inet6","local":"fd7a:115c:a1e0::1234:5678","prefixlen":128,"scope":"global"},
            {"family":"inet6","local":"fe80::cdd8:d144:174d:f629","prefixlen":64,"scope":"link"}]}]"#;
        assert_eq!(ts_addr_from(j, false), Some("100.64.56.78".parse().unwrap()));
        assert_eq!(ts_addr_from(j, true), Some("fd7a:115c:a1e0::1234:5678".parse().unwrap()));
        assert_eq!(ts_addr_from("[]", false), None);
        // logged out: interface up, no tailnet address
        let out = r#"[{"ifname":"tailscale0","addr_info":[{"family":"inet6","local":"fe80::1","prefixlen":64}]}]"#;
        assert_eq!(ts_addr_from(out, true), None);
        assert_eq!(ts_addr_from(r#"[{"addr_info":[{"local":"100.128.0.1"}]}]"#, false), None);
    }
}
