// SPDX-License-Identifier: EUPL-1.2
//! Users (file with argon2 hashes) and in-RAM sessions.
//!
//! File format, one user per line:   name:role:argon2-hash
//! role is `admin` (everything) or `guest` (dashboard only, no SMS, no actions).

use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Guest,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "admin" => Some(Role::Admin),
            "guest" => Some(Role::Guest),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Guest => "guest",
        }
    }
}

#[derive(Debug, Clone)]
pub struct User {
    pub name: String,
    pub role: Role,
    pub hash: String,
}

/// Light argon2id parameters: the dongle only has ~300 MB RAM and a slow CPU.
fn hasher() -> Argon2<'static> {
    let params = Params::new(8 * 1024, 3, 1, None).expect("argon2 params");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

pub fn hash_password(pw: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    hasher().hash_password(pw.as_bytes(), &salt).expect("hash").to_string()
}

pub fn verify_password(pw: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(h) => hasher().verify_password(pw.as_bytes(), &h).is_ok(),
        Err(_) => false,
    }
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

pub fn load(path: &Path) -> Vec<User> {
    let Ok(s) = std::fs::read_to_string(path) else {
        return vec![];
    };
    s.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut it = l.splitn(3, ':');
            let name = it.next()?.trim().to_string();
            let role = Role::parse(it.next()?.trim())?;
            let hash = it.next()?.trim().to_string();
            Some(User { name, role, hash })
        })
        .collect()
}

pub fn save(path: &Path, users: &[User]) -> std::io::Result<()> {
    let mut s = String::from("# dongie users — name:role:argon2hash — edit with `dongie user ...`\n");
    for u in users {
        s.push_str(&format!("{}:{}:{}\n", u.name, u.role.as_str(), u.hash));
    }
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, s)?;
    // keep owner/group of the existing file (root:dongie), never world-readable
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o640))?;
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::os::unix::fs::chown(&tmp, Some(meta.uid()), Some(meta.gid()));
    }
    std::fs::rename(&tmp, path)
}

// ---------------- sessions ----------------

const SESSION_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub user: String,
    pub role: Role,
    /// unix time
    expires: u64,
}

/// Login sessions. In RAM; with a runtime directory (systemd RuntimeDirectory,
/// a tmpfs under /run) they are also kept in sessions.json there, so a restart
/// of dongie (e.g. after saving settings) doesn't log everyone out. A reboot
/// still does: /run is RAM, nothing goes to flash.
#[derive(Default)]
pub struct Sessions {
    map: Mutex<HashMap<String, Session>>,
    file: Option<std::path::PathBuf>,
}

impl Sessions {
    /// Sessions kept in `<dir>/sessions.json`; ones whose user no longer exists
    /// (or has another role now) are dropped when loading.
    pub fn with_runtime_dir(dir: &Path, users_file: &Path) -> Sessions {
        let file = dir.join("sessions.json");
        let users = load(users_file);
        let now = now();
        let mut map: HashMap<String, Session> =
            std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        map.retain(|_, s| s.expires > now && users.iter().any(|u| u.name == s.user && u.role == s.role));
        Sessions { map: Mutex::new(map), file: Some(file) }
    }

    fn save(&self, m: &HashMap<String, Session>) {
        let Some(file) = &self.file else { return };
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let tmp = file.with_extension("tmp");
        let ok = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .and_then(|mut f| f.write_all(&serde_json::to_vec(m).unwrap_or_default()))
            .and_then(|_| std::fs::rename(&tmp, file));
        if let Err(e) = ok {
            eprintln!("sessions: {}: {e}", file.display());
        }
    }

    pub fn create(&self, user: &str, role: Role) -> String {
        let mut b = [0u8; 32];
        OsRng.fill_bytes(&mut b);
        let token: String = b.iter().map(|x| format!("{x:02x}")).collect();
        let mut m = self.map.lock().unwrap();
        let now = now();
        m.retain(|_, s| s.expires > now);
        m.insert(token.clone(), Session { user: user.to_string(), role, expires: now + SESSION_TTL.as_secs() });
        self.save(&m);
        token
    }

    pub fn get(&self, token: &str) -> Option<Session> {
        let m = self.map.lock().unwrap();
        m.get(token).filter(|s| s.expires > now()).cloned()
    }

    pub fn remove(&self, token: &str) {
        let mut m = self.map.lock().unwrap();
        m.remove(token);
        self.save(&m);
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_roundtrip() {
        let h = hash_password("dongie rocks");
        assert!(verify_password("dongie rocks", &h));
        assert!(!verify_password("wrong", &h));
    }

    #[test]
    fn file_roundtrip() {
        let p = std::env::temp_dir().join(format!("dongie-users-test-{}", std::process::id()));
        let users = vec![User { name: "alice".into(), role: Role::Admin, hash: hash_password("x") }];
        save(&p, &users).unwrap();
        let back = load(&p);
        std::fs::remove_file(&p).ok();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].name, "alice");
        assert_eq!(back[0].role, Role::Admin);
        assert!(verify_password("x", &back[0].hash));
    }

    #[test]
    fn sessions() {
        let s = Sessions::default();
        let t = s.create("alice", Role::Admin);
        assert_eq!(s.get(&t).unwrap().user, "alice");
        s.remove(&t);
        assert!(s.get(&t).is_none());
        assert!(s.get("nope").is_none());
    }

    #[test]
    fn sessions_survive_restart_but_not_user_deletion() {
        let d = std::env::temp_dir().join(format!("dongie-sess-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let uf = d.join("users");
        save(&uf, &[User { name: "alice".into(), role: Role::Admin, hash: hash_password("pw123456") }]).unwrap();
        let s = Sessions::with_runtime_dir(&d, &uf);
        let (a, b) = (s.create("alice", Role::Admin), s.create("bob", Role::Admin));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(d.join("sessions.json")).unwrap().permissions().mode() & 0o777, 0o600);
        let s2 = Sessions::with_runtime_dir(&d, &uf); // "restart"
        assert_eq!(s2.get(&a).unwrap().user, "alice");
        assert!(s2.get(&b).is_none()); // bob isn't a user (anymore)
        s2.remove(&a);
        assert!(Sessions::with_runtime_dir(&d, &uf).get(&a).is_none()); // logout sticks
        let _ = std::fs::remove_dir_all(&d);
    }
}
