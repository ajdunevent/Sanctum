//! The GNOME runtime has no dconf GIO module, so change sets are serialised
//! here in dconf's `a{smv}` form and the user database is read directly.
//! dconf has no read method over D-Bus; clients read its file themselves.

use std::collections::HashMap;

use gtk::glib;
use gtk::glib::prelude::*;

use crate::agent::dbus;

pub const DCONF: &str = "ca.desrt.dconf";
const WRITER_PATH: &str = "/ca/desrt/dconf/Writer/user";
const WRITER: &str = "ca.desrt.dconf.Writer";

const SHOW_BANNERS: &str = "/org/gnome/desktop/notifications/show-banners";
const IDLE_DELAY: &str = "/org/gnome/desktop/session/idle-delay";

fn changeset(key: &str, value: glib::Variant) -> Vec<u8> {
    let mut changes: HashMap<String, Option<glib::Variant>> = HashMap::new();
    changes.insert(key.to_owned(), Some(value));
    changes.to_variant().data_as_bytes().to_vec()
}

async fn write(key: &str, value: glib::Variant) -> Result<(), String> {
    let session = dbus::session()
        .await
        .ok_or("the session bus is not reachable")?;
    dbus::call(
        &session,
        DCONF,
        WRITER_PATH,
        WRITER,
        "Change",
        Some((changeset(key, value),).to_variant()),
    )
    .await
    .map(drop)
    .map_err(|err| err.message().to_owned())
}

/// `--filesystem` access to the user database, which reading needs.
pub const DATABASE: &str = "xdg-config/dconf:ro";

/// Inside Flatpak the config directory is the app's, so the host's is
/// found under the home directory.
pub fn database_path() -> std::path::PathBuf {
    if super::sandbox::Policy::current().sandboxed {
        glib::home_dir().join(".config")
    } else {
        glib::user_config_dir()
    }
    .join("dconf")
    .join("user")
}

/// `None` when the key was never changed from its default, which for
/// `show-banners` means Do Not Disturb is off.
pub fn read(key: &str) -> Option<glib::Variant> {
    let data = std::fs::read(database_path()).ok()?;
    lookup(&data, key)
}

pub fn do_not_disturb() -> Option<bool> {
    if !std::fs::exists(database_path()).unwrap_or(false) {
        return None;
    }
    Some(read(SHOW_BANNERS).and_then(|value| value.get::<bool>()) == Some(false))
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

const ITEM_SIZE: usize = 24;
const NO_PARENT: u32 = u32::MAX;

/// Finds `key` in a GVDB file, the format dconf stores its database in.
/// Each item holds one path segment and its parent's index, so full keys are
/// rebuilt by walking up the parents. Little-endian files only.
fn lookup(data: &[u8], key: &str) -> Option<glib::Variant> {
    if data.get(..8)? != b"GVariant" {
        return None;
    }
    let (root_start, root_end) = (u32_at(data, 16)? as usize, u32_at(data, 20)? as usize);
    let table = data.get(root_start..root_end)?;
    let n_bloom_words = (u32_at(table, 0)? & ((1 << 27) - 1)) as usize;
    let n_buckets = u32_at(table, 4)? as usize;
    let items_offset = 8 + 4 * (n_bloom_words + n_buckets);
    let items = table.get(items_offset..)?;
    let count = items.len() / ITEM_SIZE;
    let item = |index: usize| items.get(index * ITEM_SIZE..(index + 1) * ITEM_SIZE);
    let name = |index: usize| -> Option<String> {
        let mut parts = Vec::new();
        let mut current = index;
        for _ in 0..count {
            let entry = item(current)?;
            let start = u32_at(entry, 8)? as usize;
            let size = u16::from_le_bytes(entry.get(12..14)?.try_into().ok()?) as usize;
            parts.push(std::str::from_utf8(data.get(start..start + size)?).ok()?);
            match u32_at(entry, 4)? {
                NO_PARENT => break,
                parent => current = parent as usize,
            }
        }
        parts.reverse();
        Some(parts.concat())
    };
    let index = (0..count).find(|&index| name(index).as_deref() == Some(key))?;
    let entry = item(index)?;
    if entry[14] != b'v' {
        return None;
    }
    let (start, end) = (u32_at(entry, 16)? as usize, u32_at(entry, 20)? as usize);
    let bytes = glib::Bytes::from(data.get(start..end)?);
    glib::Variant::from_bytes_with_type(&bytes, glib::VariantTy::VARIANT).as_variant()
}

/// Phosh binds its Do Not Disturb switch to this key and silences feedbackd
/// itself, so no profile change is made here.
pub async fn set_banners_hidden(on: bool) -> Result<(), String> {
    write(SHOW_BANNERS, (!on).to_variant()).await
}

pub async fn set_screen_timeout(seconds: u32) -> Result<(), String> {
    write(IDLE_DELAY, seconds.to_variant()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_database() {
        let data = include_bytes!("testdata/dconf-user");
        assert_eq!(
            lookup(data, SHOW_BANNERS).and_then(|v| v.get::<bool>()),
            Some(false)
        );
        assert_eq!(
            lookup(data, IDLE_DELAY).and_then(|v| v.get::<u32>()),
            Some(300)
        );
        assert_eq!(
            lookup(data, "/org/gnome/desktop/interface/color-scheme")
                .and_then(|v| v.get::<String>()),
            Some("prefer-dark".into())
        );
        assert!(lookup(data, "/org/gnome/desktop/missing").is_none());
        assert!(lookup(b"nonsense", SHOW_BANNERS).is_none());
    }

    #[test]
    fn serialises_changeset() {
        let bytes = changeset(SHOW_BANNERS, false.to_variant());
        let variant = glib::Variant::from_bytes_with_type(
            &glib::Bytes::from_owned(bytes),
            glib::VariantTy::new("a{smv}").unwrap(),
        );
        let changes: HashMap<String, Option<glib::Variant>> = variant.get().unwrap();
        assert_eq!(
            changes[SHOW_BANNERS].as_ref().and_then(|v| v.get::<bool>()),
            Some(false)
        );
    }
}
