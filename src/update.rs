// SPDX-License-Identifier: EUPL-1.2
//! Updates from the GitHub releases of dongie-rs.
//!
//! - **Check** (dongie, unprivileged): once a day and on "Check now", read
//!   `releases/latest` through `curl` (like notifications: no TLS stack in
//!   dongie). The UI shows a badge when a newer version exists.
//! - **Apply** (admin click): dongie downloads the binary, `SHA256SUMS` and
//!   `SHA256SUMS.asc` into `<state_dir>/update/`, checks them, then starts
//!   `dongie-update.service` (polkit allows exactly that unit).
//! - **Install** (`dongie update-apply`, root, run by that unit): copies the
//!   staged files somewhere only root can write, checks them **again** (the
//!   staging dir belongs to the dongie user), installs atomically, keeps
//!   `dongie.bak`, restarts dongie and puts the old binary back if the new one
//!   doesn't answer within a minute.
//!
//! A release is only installed when `SHA256SUMS.asc` is a valid signature by
//! the release key built into the *installed* binary (`deploy/release-key.gpg`,
//! checked with `gpgv`), the binary matches its line in `SHA256SUMS`, and its
//! version is higher than the installed one (no downgrades to old signed
//! releases). GitHub holds no key: the signature is made by hand after CI.

use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Public release key (primary key only signs the subkeys; minimal export).
pub const KEYRING: &[u8] = include_bytes!("../deploy/release-key.gpg");
/// Primary key fingerprint the signature must chain up to (gpgv VALIDSIG).
pub const RELEASE_FPR: &str = "6882C1DBD9D70697E99A27A0B1FEEB2BF58465DC";
pub const HELPER_UNIT: &str = "dongie-update.service";
/// Written by the root helper (RuntimeDirectory=dongie-update), read by dongie.
const RESULT_FILE: &str = "/run/dongie-update/result.json";
const WORK_DIR: &str = "/run/dongie-update/work";
const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// The only build the releases carry.
const ARCH_OK: bool = cfg!(target_arch = "aarch64");
const MAX_BIN: &str = "41943040"; // 40 MiB, curl --max-filesize

#[derive(Debug, Default, Clone, Serialize)]
pub struct Info {
    pub current: String,
    /// "0.2.2"
    pub latest: Option<String>,
    pub tag: Option<String>,
    pub newer: bool,
    /// the release has SHA256SUMS.asc (only signed releases can be installed)
    pub signed: bool,
    pub notes: Option<String>,
    pub url: Option<String>,
    /// unix time of the last successful check
    pub checked: Option<u64>,
    pub error: Option<String>,
    /// "idle" | "downloading" | "installing"
    pub state: String,
    /// the root helper's last result: {ok, from, to, at, message}
    pub result: Option<Value>,
    #[serde(skip)]
    assets: Vec<(String, String)>,
    /// when the helper was started (unix time)
    #[serde(skip)]
    started: u64,
}

impl Info {
    /// "installing" ends with a helper result newer than the start, or after
    /// 5 minutes without one (the helper failed before writing anything).
    pub fn settle(&mut self) {
        self.result = read_result();
        if self.state != "installing" {
            return;
        }
        let at = self.result.as_ref().and_then(|r| r["at"].as_u64()).unwrap_or(0);
        if at >= self.started {
            self.state = "idle".into();
        } else if now() > self.started + 300 {
            self.state = "idle".into();
            self.error = Some(format!("no result from {HELPER_UNIT} after 5 minutes: see journalctl -u {HELPER_UNIT}"));
        }
    }

    /// Guests see whether there is an update, nothing else.
    pub fn for_guest(&self) -> Value {
        json!({ "current": self.current, "latest": self.latest, "newer": self.newer })
    }
}

pub type Shared = Arc<RwLock<Info>>;

pub fn shared() -> Shared {
    Arc::new(RwLock::new(Info { current: CURRENT.into(), state: "idle".into(), ..Default::default() }))
}

/// "v1.2.3" / "1.2.3" -> (1, 2, 3). Pre-releases and build tags are not offered.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().strip_prefix('v').unwrap_or(s.trim());
    let mut it = s.split('.');
    let v = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    it.next().is_none().then_some(v)
}

pub fn is_newer(tag: &str, than: &str) -> bool {
    matches!((parse_version(tag), parse_version(than)), (Some(a), Some(b)) if a > b)
}

pub fn asset_name(tag: &str) -> String {
    format!("dongie-{tag}-aarch64-linux-musl")
}

pub fn valid_repo(r: &str) -> bool {
    let mut p = r.split('/');
    let ok = |s: Option<&str>| {
        s.is_some_and(|s| {
            !s.is_empty() && !s.starts_with('.') && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        })
    };
    ok(p.next()) && ok(p.next()) && p.next().is_none()
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

async fn curl(args: &[&str], secs: u64) -> Result<Vec<u8>, String> {
    let mut c = tokio::process::Command::new("curl");
    c.args(["-sSfL", "--proto", "=https", "--proto-redir", "=https", "--max-time", &secs.to_string()]).args(args);
    c.kill_on_drop(true);
    let out = tokio::time::timeout(std::time::Duration::from_secs(secs + 5), c.output())
        .await
        .map_err(|_| "curl timed out".to_string())?
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(out.stdout)
}

/// The latest release, parsed from the GitHub API answer.
fn parse_release(repo: &str, j: &Value, info: &mut Info) -> Result<(), String> {
    let tag = j["tag_name"].as_str().ok_or("no tag_name in the answer")?;
    let v = parse_version(tag).ok_or_else(|| format!("tag {tag:?} is not vX.Y.Z"))?;
    info.latest = Some(format!("{}.{}.{}", v.0, v.1, v.2));
    info.tag = Some(tag.to_string());
    info.newer = is_newer(tag, CURRENT);
    info.url = j["html_url"].as_str().map(String::from);
    info.notes = j["body"].as_str().map(|s| s.chars().take(4000).collect());
    // only download URLs of this very release
    let prefix = format!("https://github.com/{repo}/releases/download/{tag}/");
    info.assets = j["assets"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some((x["name"].as_str()?.to_string(), x["browser_download_url"].as_str()?.to_string()))
                })
                .filter(|(_, u)| u.starts_with(&prefix))
                .collect()
        })
        .unwrap_or_default();
    let has = |n: &str| info.assets.iter().any(|(a, _)| a == n);
    info.signed = has("SHA256SUMS.asc") && has("SHA256SUMS") && has(&asset_name(tag));
    Ok(())
}

pub async fn check(repo: &str, shared: &Shared) {
    let r = async {
        if !valid_repo(repo) {
            return Err(format!("update_repo {repo:?} is not owner/name"));
        }
        let url = format!("https://api.github.com/repos/{repo}/releases/latest");
        let body = curl(&["-H", "Accept: application/vnd.github+json", &url], 20).await?;
        let j: Value = serde_json::from_slice(&body).map_err(|e| format!("GitHub answer: {e}"))?;
        let mut info = shared.read().await.clone();
        parse_release(repo, &j, &mut info)?;
        Ok(info)
    }
    .await;
    let mut s = shared.write().await;
    match r {
        Ok(mut i) => {
            i.checked = Some(now());
            i.error = None;
            i.state = s.state.clone();
            *s = i;
        }
        Err(e) => {
            eprintln!("update check: {e}");
            s.error = Some(e);
        }
    }
}

/// Daily check, the first a few minutes after start (not during boot).
pub async fn run(repo: String, shared: Shared) {
    tokio::time::sleep(std::time::Duration::from_secs(300)).await;
    loop {
        check(&repo, &shared).await;
        tokio::time::sleep(std::time::Duration::from_secs(24 * 3600)).await;
    }
}

/// The helper's last result, if any.
pub fn read_result() -> Option<Value> {
    std::fs::read(RESULT_FILE).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

/// Signature (by the built-in release key) + checksum of the staged binary.
/// Used by dongie before starting the helper and by the helper itself.
pub async fn verify(dir: &Path, tag: &str) -> Result<(), String> {
    let home = dir.join("gnupg");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).map_err(|e| format!("{}: {e}", home.display()))?;
    set_mode(&home, 0o700);
    let keyring = home.join("release-key.gpg");
    std::fs::write(&keyring, KEYRING).map_err(|e| format!("{}: {e}", keyring.display()))?;
    let out = tokio::process::Command::new("gpgv")
        .arg("--homedir")
        .arg(&home)
        .args(["--status-fd", "1", "--keyring"])
        .arg(&keyring)
        .arg(dir.join("SHA256SUMS.asc"))
        .arg(dir.join("SHA256SUMS"))
        .output()
        .await
        .map_err(|e| format!("gpgv: {e}"))?;
    let _ = std::fs::remove_dir_all(&home);
    let status = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || !good_signature(&status) {
        return Err("SHA256SUMS is not signed by the release key".into());
    }
    let sums = std::fs::read_to_string(dir.join("SHA256SUMS")).map_err(|e| format!("SHA256SUMS: {e}"))?;
    let want = expected_sum(&sums, &asset_name(tag)).ok_or_else(|| format!("{} not in SHA256SUMS", asset_name(tag)))?;
    let got = tokio::process::Command::new("sha256sum")
        .arg(dir.join("dongie"))
        .output()
        .await
        .map_err(|e| format!("sha256sum: {e}"))?;
    let got = String::from_utf8_lossy(&got.stdout).split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    if got != want {
        return Err("the binary doesn't match SHA256SUMS".into());
    }
    Ok(())
}

/// gpgv's `[GNUPG:] VALIDSIG <subkey fpr> … <primary fpr>` for our key.
fn good_signature(status: &str) -> bool {
    status
        .lines()
        .filter_map(|l| l.strip_prefix("[GNUPG:] VALIDSIG "))
        .any(|l| l.split_whitespace().last() == Some(RELEASE_FPR))
}

/// The hash for `name` in a `sha256sum` listing ("<hex>  ./name" or "<hex> *name").
fn expected_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (h, f) = l.split_once(char::is_whitespace)?;
        let f = f.trim_start().trim_start_matches('*').trim_start_matches("./");
        (f == name && h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())).then(|| h.to_ascii_lowercase())
    })
}

fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode));
}

/// Admin clicked "Update": download, check, hand over to the root helper.
pub async fn apply(state_dir: &Path, shared: Shared) {
    let r = async {
        let (tag, assets) = {
            let s = shared.read().await;
            if !ARCH_OK {
                return Err("releases are built for aarch64 only".to_string());
            }
            if !s.newer {
                return Err("no newer version known: check first".into());
            }
            if !s.signed {
                return Err("this release isn't signed (yet): it can't be installed".into());
            }
            (s.tag.clone().unwrap_or_default(), s.assets.clone())
        };
        let url = |n: &str| assets.iter().find(|(a, _)| a == n).map(|(_, u)| u.clone());
        let dir = state_dir.join("update");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        set_mode(&dir, 0o700);
        for (name, file, max) in [
            (asset_name(&tag), "dongie", MAX_BIN),
            ("SHA256SUMS".into(), "SHA256SUMS", "65536"),
            ("SHA256SUMS.asc".into(), "SHA256SUMS.asc", "65536"),
        ] {
            let u = url(&name).ok_or_else(|| format!("{name} missing in the release"))?;
            let to = dir.join(file);
            curl(&["--max-filesize", max, "-o", &to.to_string_lossy(), &u], 300).await?;
        }
        std::fs::write(dir.join("TAG"), &tag).map_err(|e| format!("TAG: {e}"))?;
        verify(&dir, &tag).await?;
        eprintln!("update: {tag} downloaded and verified, starting {HELPER_UNIT}");
        {
            let mut s = shared.write().await;
            s.state = "installing".into();
            s.started = now();
        }
        match crate::cmd::run_any_t("systemctl", &["start", "--no-block", HELPER_UNIT], 30).await {
            Some((true, _, _)) => Ok(()),
            Some((false, _, e)) => Err(format!("systemctl start {HELPER_UNIT}: {}", e.trim())),
            None => Err("systemctl timed out".into()),
        }
    }
    .await;
    if let Err(e) = r {
        eprintln!("update: {e}");
        let mut s = shared.write().await;
        s.state = "idle".into();
        s.error = Some(e);
    }
}

// ---------------- root helper ----------------

fn write_result(ok: bool, to: &str, message: &str) {
    let r = json!({ "ok": ok, "from": CURRENT, "to": to, "at": now(), "message": message });
    let tmp = Path::new(RESULT_FILE).with_extension("tmp");
    if std::fs::write(&tmp, r.to_string()).is_ok() {
        set_mode(&tmp, 0o644);
        let _ = std::fs::rename(&tmp, RESULT_FILE);
    }
    eprintln!("update-apply: {message}");
}

/// Copy one staged file into the root-only work dir; staged symlinks are refused.
fn take(from: &Path, to: &Path) -> Result<(), String> {
    let m = std::fs::symlink_metadata(from).map_err(|e| format!("{}: {e}", from.display()))?;
    if !m.file_type().is_file() {
        return Err(format!("{} is not a plain file", from.display()));
    }
    std::fs::copy(from, to).map(|_| ()).map_err(|e| format!("{}: {e}", from.display()))
}

/// Where to ask the restarted dongie for its version.
fn health_url(web_listen: &str) -> String {
    let a = web_listen.replace("0.0.0.0:", "127.0.0.1:").replace("[::]:", "[::1]:");
    format!("http://{a}/api/me")
}

async fn version_answers(url: &str, want: &str) -> bool {
    let out = tokio::process::Command::new("curl").args(["-s", "--max-time", "3", url]).output().await;
    out.ok()
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .is_some_and(|j| j["version"].as_str() == Some(want))
}

fn install(src: &Path, exe: &Path) -> Result<(), String> {
    let tmp = exe.with_file_name(".dongie.new");
    std::fs::copy(src, &tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    set_mode(&tmp, 0o755);
    std::fs::File::open(&tmp).and_then(|f| f.sync_all()).map_err(|e| format!("sync: {e}"))?;
    std::fs::rename(&tmp, exe).map_err(|e| format!("{}: {e}", exe.display()))
}

async fn restart_dongie() {
    let _ = crate::cmd::run_any_t("systemctl", &["restart", "dongie.service"], 60).await;
}

/// `dongie update-apply`, run as root by dongie-update.service.
pub async fn helper(cfg: &crate::config::Config) -> Result<(), String> {
    let staged = cfg.state_dir.join("update");
    let work = PathBuf::from(WORK_DIR);
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("{WORK_DIR}: {e}"))?;
    set_mode(&work, 0o700);
    let mut to = String::new();
    let r = async {
        for f in ["TAG", "dongie", "SHA256SUMS", "SHA256SUMS.asc"] {
            take(&staged.join(f), &work.join(f))?;
        }
        let tag = std::fs::read_to_string(work.join("TAG")).map_err(|e| format!("TAG: {e}"))?.trim().to_string();
        let v = parse_version(&tag).ok_or_else(|| format!("bad tag {tag:?}"))?;
        to = format!("{}.{}.{}", v.0, v.1, v.2);
        if !is_newer(&tag, CURRENT) {
            return Err(format!("{tag} is not newer than the installed {CURRENT}"));
        }
        verify(&work, &tag).await?;
        let new = work.join("dongie");
        set_mode(&new, 0o755);
        let said = tokio::process::Command::new(&new).arg("--version").output().await.map_err(|e| format!("{e}"))?;
        if String::from_utf8_lossy(&said.stdout).trim() != format!("dongie {to}") {
            return Err(format!("the new binary says {:?}", String::from_utf8_lossy(&said.stdout).trim()));
        }
        let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
        let bak = exe.with_file_name("dongie.bak");
        std::fs::copy(&exe, &bak).map_err(|e| format!("{}: {e}", bak.display()))?;
        install(&new, &exe)?;
        restart_dongie().await;
        let url = health_url(&cfg.web_listen);
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            if version_answers(&url, &to).await {
                return Ok(());
            }
        }
        install(&bak, &exe)?;
        restart_dongie().await;
        Err(format!("{to} didn't answer within 60 s: rolled back to {CURRENT}"))
    }
    .await;
    let _ = std::fs::remove_dir_all(&work);
    match &r {
        Ok(()) => write_result(true, &to, &format!("updated {CURRENT} -> {to}")),
        Err(e) => write_result(false, &to, e),
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.2.1"), Some((0, 2, 1)));
        assert_eq!(parse_version("0.10.0"), Some((0, 10, 0)));
        assert_eq!(parse_version("v0.3.0-rc1"), None);
        assert_eq!(parse_version("v1.2"), None);
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("v0.2.2", "0.2.1"));
        assert!(!is_newer("v0.2.1", "0.2.1"));
        assert!(!is_newer("v0.2.0", "0.2.1"));
        assert!(!is_newer("garbage", "0.2.1"));
    }

    #[test]
    fn repo_names() {
        assert!(valid_repo("derpyspike/dongie-rs"));
        assert!(!valid_repo("derpyspike/dongie-rs/x"));
        assert!(!valid_repo("../etc"));
        assert!(!valid_repo("a b/c"));
    }

    #[test]
    fn release_parsing_keeps_only_this_releases_assets() {
        let j = json!({
            "tag_name": "v99.0.0", "html_url": "https://github.com/derpyspike/dongie-rs/releases/tag/v99.0.0", "body": "notes",
            "assets": [
                {"name": "dongie-v99.0.0-aarch64-linux-musl", "browser_download_url": "https://github.com/derpyspike/dongie-rs/releases/download/v99.0.0/dongie-v99.0.0-aarch64-linux-musl"},
                {"name": "SHA256SUMS", "browser_download_url": "https://github.com/derpyspike/dongie-rs/releases/download/v99.0.0/SHA256SUMS"},
                {"name": "SHA256SUMS.asc", "browser_download_url": "https://evil.example/SHA256SUMS.asc"}
            ]
        });
        let mut i = Info::default();
        parse_release("derpyspike/dongie-rs", &j, &mut i).unwrap();
        assert_eq!(i.latest.as_deref(), Some("99.0.0"));
        assert!(i.newer);
        assert!(!i.signed, "a signature from elsewhere doesn't count");
        assert_eq!(i.assets.len(), 2);
    }

    #[test]
    fn sums_and_status() {
        let h = "a".repeat(64);
        let sums = format!("{h}  ./dongie-v0.2.2-aarch64-linux-musl\n{}  ./dongie-rs-v0.2.2.tar.xz\n", "b".repeat(64));
        assert_eq!(expected_sum(&sums, "dongie-v0.2.2-aarch64-linux-musl"), Some(h.clone()));
        assert_eq!(
            expected_sum(&format!("{h} *dongie-v0.2.2-aarch64-linux-musl"), "dongie-v0.2.2-aarch64-linux-musl"),
            Some(h)
        );
        assert_eq!(expected_sum(&sums, "dongie-v0.2.3-aarch64-linux-musl"), None);
        let ok = format!("[GNUPG:] NEWSIG\n[GNUPG:] VALIDSIG 85BC32F7D2D89999CE764D95E4721956CDC07CFC 2026-11-26 1795677183 0 4 0 1 10 00 {RELEASE_FPR}\n");
        assert!(good_signature(&ok));
        assert!(!good_signature(&ok.replace(RELEASE_FPR, "0000000000000000000000000000000000000000")));
        assert!(!good_signature("[GNUPG:] BADSIG E4721956CDC07CFC x"));
        assert_eq!(health_url("0.0.0.0:8040"), "http://127.0.0.1:8040/api/me");
        assert_eq!(health_url("192.168.100.1:8040"), "http://192.168.100.1:8040/api/me");
    }

    /// The built-in key verifies a real signature: `cargo test -- --ignored release_key`
    /// (needs gpgv and a signed git tag in this checkout).
    #[tokio::test]
    #[ignore]
    async fn release_key_live() {
        let tag = std::process::Command::new("git").args(["cat-file", "-p", "v0.2.1"]).output().unwrap().stdout;
        let i = tag.windows(14).position(|w| w == b"-----BEGIN PGP").unwrap();
        let d = std::env::temp_dir().join(format!("dongie-keytest-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("SHA256SUMS"), &tag[..i]).unwrap();
        std::fs::write(d.join("SHA256SUMS.asc"), &tag[i..]).unwrap();
        // signature is good; the checksum step then fails because there is no binary
        let e = verify(&d, "v0.2.1").await.unwrap_err();
        assert!(e.contains("not in SHA256SUMS"), "{e}");
        std::fs::write(d.join("SHA256SUMS"), [&tag[..i], b"x"].concat()).unwrap();
        assert!(verify(&d, "v0.2.1").await.unwrap_err().contains("not signed"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
