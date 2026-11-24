// SPDX-License-Identifier: EUPL-1.2
//! Small helpers for running external tools and parsing their output.

use std::collections::HashMap;
use std::time::Duration;
use tokio::process::Command;

const TIMEOUT: Duration = Duration::from_secs(20);

/// Run a command; return stdout only if it exited successfully.
pub async fn run(prog: &str, args: &[&str]) -> Option<String> {
    match run_any(prog, args).await {
        Some((true, out, _)) => Some(out),
        _ => None,
    }
}

/// Run a command; return (success, stdout, stderr) regardless of exit code.
pub async fn run_any(prog: &str, args: &[&str]) -> Option<(bool, String, String)> {
    run_any_t(prog, args, TIMEOUT.as_secs()).await
}

/// Like `run_any` with a custom timeout in seconds.
pub async fn run_any_t(prog: &str, args: &[&str], secs: u64) -> Option<(bool, String, String)> {
    let fut = Command::new(prog).args(args).kill_on_drop(true).output();
    match tokio::time::timeout(Duration::from_secs(secs), fut).await {
        Ok(Ok(o)) => Some((
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )),
        _ => None,
    }
}

/// `sudo -n <args>` (only the exact commands allowed in /etc/sudoers.d/dongie work).
pub async fn sudo(args: &[&str]) -> Option<String> {
    let mut v = vec!["-n"];
    v.extend_from_slice(args);
    run("sudo", &v).await
}

/// Parse `mmcli -K` style "key   : value" lines. Values of "--" are dropped.
pub fn kv(text: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for line in text.lines() {
        if let Some(i) = line.find(": ") {
            let key = line[..i].trim();
            let val = line[i + 2..].trim();
            if !key.is_empty() && val != "--" && !val.is_empty() {
                m.insert(key.to_string(), val.to_string());
            }
        }
    }
    m
}

/// qmicli prints `Label: 'value'`; return the quoted value of the first line containing `label`.
pub fn qv(text: &str, label: &str) -> Option<String> {
    for line in text.lines() {
        if line.contains(label) {
            let a = line.find('\'')?;
            let rest = &line[a + 1..];
            let b = rest.find('\'')?;
            return Some(rest[..b].to_string());
        }
    }
    None
}

/// Like `qv` but only searches inside the section that starts with `section` (e.g. "LTE:").
pub fn qv_in(text: &str, section: &str, label: &str) -> Option<String> {
    let start = text.find(section)?;
    let body = &text[start + section.len()..];
    // section ends at the next line that does not start with a tab
    let mut out = String::new();
    for line in body.lines().skip(1) {
        if !line.starts_with('\t') {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    qv(&out, label)
}

/// First number (int or float, may be negative) in a string.
pub fn num(s: &str) -> Option<f64> {
    let mut started = false;
    let mut buf = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() || c == '.' || (c == '-' && !started) {
            started = true;
            buf.push(c);
        } else if started {
            break;
        }
    }
    buf.parse().ok()
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// (year, month, day) in UTC for a unix timestamp (Howard Hinnant's civil_from_days).
pub fn ymd(unix: u64) -> (i64, u32, u32) {
    let z = (unix / 86400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kv_parses_mmcli() {
        let t = "modem.generic.state                             : connected\n\
                 modem.generic.state-failed-reason               : --\n\
                 modem.generic.current-modes                     : allowed: 3g, 4g; preferred: 3g\n";
        let m = kv(t);
        assert_eq!(m["modem.generic.state"], "connected");
        assert!(!m.contains_key("modem.generic.state-failed-reason"));
        assert_eq!(m["modem.generic.current-modes"], "allowed: 3g, 4g; preferred: 3g");
    }

    #[test]
    fn qv_sections() {
        let t = "[x] ok\nLTE:\n\tRSSI: '-76 dBm'\n\tRSRP: '-116 dBm'\nWCDMA:\n\tRSSI: '-112 dBm'\n";
        assert_eq!(qv_in(t, "LTE:", "RSSI").as_deref(), Some("-76 dBm"));
        assert_eq!(qv_in(t, "WCDMA:", "RSSI").as_deref(), Some("-112 dBm"));
        assert_eq!(num("-116 dBm"), Some(-116.0));
        assert_eq!(num("-2.6 dB"), Some(-2.6));
    }

    #[test]
    fn dates() {
        assert_eq!(ymd(0), (1970, 1, 1));
        assert_eq!(ymd(1_795_518_317), (2026, 11, 24));
        assert_eq!(ymd(951_782_400), (2000, 2, 29));
    }
}
