// SPDX-License-Identifier: EUPL-1.2
//! Small state files on flash that survive power cuts.
//!
//! File layout (little endian):
//!   b"DGS1" | version u8 | crc32 u32 (of the compressed data) | length u32 | deflate(JSON)
//!
//! Writing: `<file>.tmp` + fsync, the current file becomes `<file>.bak`, the tmp
//! file is renamed into place, the directory is fsynced. A power cut at any point
//! leaves either the new file, the old one, or the `.bak` intact.
//! Reading: the file, else the `.bak` (when the file is missing, truncated or its
//! CRC/length doesn't match), else nothing.

use serde::{de::DeserializeOwned, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"DGS1";
const VERSION: u8 = 1;
const HEADER: usize = 4 + 1 + 4 + 4;
/// Refuse to inflate more than this (corrupt length fields, zip bombs).
const MAX_JSON: usize = 16 * 1024 * 1024;

/// CRC-32 (IEEE 802.3, as in zip/png), bitwise; these files are small.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    let packed = miniz_oxide::deflate::compress_to_vec(&json, 6);
    let mut out = Vec::with_capacity(HEADER + packed.len());
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&crc32(&packed).to_le_bytes());
    out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
    out.extend_from_slice(&packed);
    Ok(out)
}

pub fn decode<T: DeserializeOwned>(b: &[u8]) -> Result<T, String> {
    if b.len() < HEADER || &b[..4] != MAGIC {
        return Err("not a dongie state file (or truncated)".into());
    }
    if b[4] != VERSION {
        return Err(format!("unknown version {}", b[4]));
    }
    let crc = u32::from_le_bytes([b[5], b[6], b[7], b[8]]);
    let len = u32::from_le_bytes([b[9], b[10], b[11], b[12]]) as usize;
    let packed = &b[HEADER..];
    if packed.len() != len {
        return Err(format!("length {} != {len} (truncated?)", packed.len()));
    }
    if crc32(packed) != crc {
        return Err("checksum mismatch".into());
    }
    let json =
        miniz_oxide::inflate::decompress_to_vec_with_limit(packed, MAX_JSON).map_err(|e| format!("inflate: {e:?}"))?;
    serde_json::from_slice(&json).map_err(|e| format!("json: {e}"))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

pub fn save<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let data = encode(value)?;
    let tmp = with_suffix(path, ".tmp");
    let io = |e: std::io::Error| format!("{}: {e}", path.display());
    {
        let mut f = fs::File::create(&tmp).map_err(io)?;
        f.write_all(&data).map_err(io)?;
        f.sync_all().map_err(io)?;
    }
    if path.exists() {
        fs::rename(path, with_suffix(path, ".bak")).map_err(io)?;
    }
    fs::rename(&tmp, path).map_err(io)?;
    if let Some(dir) = path.parent() {
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// The newest intact copy, or None (with a log line when something was damaged).
pub fn load<T: DeserializeOwned>(path: &Path) -> Option<T> {
    for p in [path.to_path_buf(), with_suffix(path, ".bak")] {
        match fs::read(&p) {
            Ok(b) => match decode(&b) {
                Ok(v) => {
                    if p != path {
                        eprintln!("state: {} damaged or missing, using {}", path.display(), p.display());
                    }
                    return Some(v);
                }
                Err(e) => eprintln!("state: {} unreadable ({e})", p.display()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("state: {}: {e}", p.display()),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dongie-blob-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn crc_known_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926); // the standard check value
    }

    #[test]
    fn roundtrip_and_compresses() {
        let v: Vec<String> = (0..300).map(|i| format!("message {i} hola hola hola")).collect();
        let b = encode(&v).unwrap();
        assert!(b.len() < serde_json::to_vec(&v).unwrap().len() / 3);
        assert_eq!(decode::<Vec<String>>(&b).unwrap(), v);
    }

    #[test]
    fn damage_is_detected() {
        let b = encode(&vec![1u32, 2, 3]).unwrap();
        let mut flipped = b.clone();
        let last = flipped.len() - 1;
        flipped[last] ^= 0x40;
        assert!(decode::<Vec<u32>>(&flipped).unwrap_err().contains("checksum"));
        assert!(decode::<Vec<u32>>(&b[..b.len() - 2]).unwrap_err().contains("truncated"));
        assert!(decode::<Vec<u32>>(&b[..6]).is_err());
        assert!(decode::<Vec<u32>>(b"").is_err());
        assert!(decode::<Vec<u32>>(b"{\"plain\":\"json\"}").is_err());
    }

    #[test]
    fn save_keeps_backup_and_falls_back() {
        let d = dir("fallback");
        let p = d.join("sms.bin");
        assert_eq!(load::<Vec<u32>>(&p), None);
        save(&p, &vec![1u32]).unwrap();
        save(&p, &vec![1u32, 2]).unwrap();
        assert_eq!(load::<Vec<u32>>(&p), Some(vec![1, 2]));
        assert_eq!(load::<Vec<u32>>(&with_suffix(&p, ".bak")), Some(vec![1]));
        // power cut mid-write of the main file: truncated -> previous version
        let b = fs::read(&p).unwrap();
        fs::write(&p, &b[..b.len() / 2]).unwrap();
        assert_eq!(load::<Vec<u32>>(&p), Some(vec![1]));
        // cut between the two renames: main file missing -> .bak
        fs::remove_file(&p).unwrap();
        assert_eq!(load::<Vec<u32>>(&p), Some(vec![1]));
        assert!(!with_suffix(&p, ".tmp").exists());
        let _ = fs::remove_dir_all(&d);
    }
}
