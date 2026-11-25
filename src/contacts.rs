// SPDX-License-Identifier: EUPL-1.2
//! Contact names for numbers in SMS, calls and notifications.
//! Stored in <state_dir>/contacts.bin (blob.rs: compressed, checksummed, with
//! a .bak), always: contacts are entered on purpose and rarely change.

use crate::blob;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const MAX: usize = 500;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Contact {
    pub number: String,
    pub name: String,
}

#[derive(Debug, Default)]
pub struct Contacts {
    pub list: Vec<Contact>,
    path: PathBuf,
}

fn digits(n: &str) -> String {
    n.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// Same number, allowing for a missing country prefix: "+34 600 11 22 33" == "600112233".
pub fn same_number(a: &str, b: &str) -> bool {
    let (a, b) = (digits(a), digits(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let (short, long) = if a.len() <= b.len() { (&a, &b) } else { (&b, &a) };
    short == long || (short.len() >= 9 && long.ends_with(short.as_str()))
}

pub fn valid_name(n: &str) -> bool {
    let n = n.trim();
    !n.is_empty() && n.chars().count() <= 40 && !n.chars().any(char::is_control)
}

impl Contacts {
    pub fn load(state_dir: &Path) -> Contacts {
        let path = state_dir.join("contacts.bin");
        let list = blob::load(&path).unwrap_or_default();
        Contacts { list, path }
    }

    pub fn name_for(&self, number: &str) -> Option<String> {
        self.list.iter().find(|c| same_number(&c.number, number)).map(|c| c.name.clone())
    }

    /// Add, rename (same number) or, with an empty name, delete. Saved right away.
    pub fn set(&mut self, number: &str, name: &str) -> Result<(), String> {
        let number = number.trim();
        if !crate::sms::valid_number(number) {
            return Err("number: digits, optional leading +".into());
        }
        let name = name.trim();
        let pos = self.list.iter().position(|c| same_number(&c.number, number));
        match (name.is_empty(), pos) {
            (true, Some(i)) => {
                self.list.remove(i);
            }
            (true, None) => return Ok(()),
            (false, _) if !valid_name(name) => return Err("name: 1-40 characters".into()),
            (false, Some(i)) => {
                // keep the more complete form of the number (+34… over 6…)
                if digits(number).len() > digits(&self.list[i].number).len() {
                    self.list[i].number = number.into();
                }
                self.list[i].name = name.into();
            }
            (false, None) if self.list.len() >= MAX => return Err(format!("at most {MAX} contacts")),
            (false, None) => self.list.push(Contact { number: number.into(), name: name.into() }),
        }
        self.list.sort_by_key(|c| c.name.to_lowercase());
        blob::save(&self.path, &self.list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_matching() {
        assert!(same_number("+34 600 11 22 33", "600112233"));
        assert!(same_number("+34600112233", "0034600112233"));
        assert!(!same_number("600112233", "600112234"));
        assert!(!same_number("1234", "+491234")); // short codes need an exact match
        assert!(same_number("22123", "22123"));
        assert!(!same_number("", ""));
    }

    #[test]
    fn set_rename_delete_persist() {
        let d = std::env::temp_dir().join(format!("dongie-contacts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut c = Contacts::load(&d);
        c.set("+34600112233", "Mamá").unwrap();
        c.set("+491701234567", "Anna").unwrap();
        assert_eq!(c.name_for("600 11 22 33").as_deref(), Some("Mamá"));
        c.set("600112233", "Mami").unwrap(); // same number -> rename, keeps +34
        assert_eq!(c.list.len(), 2);
        assert!(c.list.iter().any(|x| x.number == "+34600112233" && x.name == "Mami"));
        assert!(c.set("abc", "x").is_err() && c.set("+34600112233", "\u{7}").is_err());
        assert_eq!(Contacts::load(&d).name_for("+34600112233").as_deref(), Some("Mami"));
        c.set("+34600112233", "").unwrap(); // delete
        assert_eq!(Contacts::load(&d).list, vec![Contact { number: "+491701234567".into(), name: "Anna".into() }]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
