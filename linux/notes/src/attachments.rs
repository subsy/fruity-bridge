//! Attachment bytes: a disk cache under `~/.cache/fruity-bridge/attachments`, streamed
//! downloads, pruning, and images decoded off the main thread into an in-memory texture cache.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use fruity_bridge_client::notes::Attachment;
use fruity_bridge_client::{Client, Error, cache};
use gtk::gdk;
use gtk::gdk_pixbuf::Pixbuf;
use gtk::prelude::*;

/// Files not used for this long are removed at startup.
const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// Beyond this total, the least recently used files go.
const MAX_BYTES: u64 = 500 * 1024 * 1024;
/// Images wider than this are scaled down when decoded: about twice the reading column.
pub const IMAGE_WIDTH: i32 = 1440;
/// Decoded images kept in memory, so live-update redraws don't decode again.
const TEXTURES: usize = 48;

fn dir() -> Option<PathBuf> {
    Some(cache::dir()?.join("attachments"))
}

/// Identifies a version of an attachment's bytes: its id, plus `modifiedAt` when known.
pub fn version_key(att: &Attachment) -> String {
    match &att.modified_at {
        Some(m) => format!("{}\n{m}", att.id),
        None => att.id.clone(),
    }
}

/// Cached file path for an attachment: `attachments/<hash>/<filename>`, so a file opened in
/// another app keeps its real name.
pub fn path(att: &Attachment) -> Option<PathBuf> {
    let name: String = att
        .filename
        .as_deref()
        .unwrap_or("attachment")
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let name = if name.trim_matches('.').is_empty() {
        "attachment".to_string()
    } else {
        name
    };
    Some(dir()?.join(cache::short_hash(&version_key(att))).join(name))
}

/// The attachment's cached file, downloading it first if needed. Blocking.
pub fn fetch(client: &Client, att: &Attachment) -> Result<PathBuf, Error> {
    let path = path(att).ok_or_else(|| Error::Decode("no cache directory".into()))?;
    if fs::metadata(&path).is_ok_and(|m| m.is_file()) {
        mark_used(&path);
        return Ok(path);
    }
    let url = att
        .url
        .as_deref()
        .ok_or_else(|| Error::Decode("attachment has no URL".into()))?;
    let local = |e: std::io::Error| Error::Decode(format!("couldn't write the cache: {e}"));
    let dir = path.parent().expect("cache path has a parent");
    fs::create_dir_all(dir).map_err(local)?;
    // A name per download, so two at once can't write into the same file.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let part = dir.join(format!(
        ".{}-{}.part",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = fs::File::create(&part)
        .map_err(local)
        .and_then(|mut file| client.download_attachment(url, &mut file))
        .and_then(|_| fs::rename(&part, &path).map_err(local));
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result.map(|_| path)
}

/// Records a cache hit for pruning (access times aren't reliable on every mount).
fn mark_used(path: &Path) {
    if let Ok(f) = fs::File::options().append(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

fn last_used(meta: &fs::Metadata) -> SystemTime {
    let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    meta.accessed().map_or(modified, |a| a.max(modified))
}

/// Removes cached attachments unused for 30 days, then the least recently used beyond 500 MB,
/// plus leftover partial downloads. Blocking; run it off the main thread.
pub fn prune() {
    if let Some(dir) = dir() {
        prune_dir(&dir, SystemTime::now(), MAX_AGE, MAX_BYTES);
    }
}

fn prune_dir(root: &Path, now: SystemTime, max_age: Duration, max_bytes: u64) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut files: Vec<(PathBuf, SystemTime, u64)> = Vec::new();
    let mut dirs = Vec::new();
    for sub in entries.flatten() {
        let sub = sub.path();
        let Ok(inner) = fs::read_dir(&sub) else {
            continue;
        };
        for f in inner.flatten() {
            let Ok(meta) = f.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let used = last_used(&meta);
            let stale_part = f.file_name().to_string_lossy().ends_with(".part")
                && now.duration_since(used).unwrap_or_default() > Duration::from_secs(24 * 3600);
            if stale_part || now.duration_since(used).unwrap_or_default() > max_age {
                let _ = fs::remove_file(f.path());
            } else {
                files.push((f.path(), used, meta.len()));
            }
        }
        dirs.push(sub);
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.1));
    let mut total = 0u64;
    for (path, _, len) in files {
        total += len;
        if total > max_bytes {
            let _ = fs::remove_file(path);
        }
    }
    for d in dirs {
        // Only succeeds when empty.
        let _ = fs::remove_dir(d);
    }
}

/// Decodes an image file, scaled down to at most `max_width` pixels wide. Blocking.
pub fn decode(path: &Path, max_width: i32) -> Option<gdk::Texture> {
    let scaled = || -> Option<gdk::Texture> {
        let (_, width, _) = Pixbuf::file_info(path)?;
        let pixbuf = if width > max_width {
            Pixbuf::from_file_at_scale(path, max_width, -1, true).ok()?
        } else {
            Pixbuf::from_file(path).ok()?
        };
        let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
        let format = if pixbuf.has_alpha() {
            gdk::MemoryFormat::R8g8b8a8
        } else {
            gdk::MemoryFormat::R8g8b8
        };
        Some(
            gdk::MemoryTexture::new(
                pixbuf.width(),
                pixbuf.height(),
                format,
                &pixbuf.read_pixel_bytes(),
                pixbuf.rowstride() as usize,
            )
            .upcast(),
        )
    };
    // GTK's own loaders cover a few formats gdk-pixbuf may lack.
    scaled().or_else(|| gdk::Texture::from_filename(path).ok())
}

/// Recently decoded images by [`version_key`]. Main thread only.
#[derive(Default)]
pub struct Textures {
    map: HashMap<String, gdk::Texture>,
    order: VecDeque<String>,
}

impl Textures {
    pub fn get(&mut self, key: &str) -> Option<gdk::Texture> {
        let tex = self.map.get(key)?.clone();
        self.order.retain(|k| k != key);
        self.order.push_back(key.to_string());
        Some(tex)
    }

    pub fn insert(&mut self, key: String, texture: gdk::Texture) {
        if self.map.insert(key.clone(), texture).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > TEXTURES {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn att(id: &str, modified: Option<&str>, filename: Option<&str>) -> Attachment {
        Attachment {
            id: id.into(),
            kind: "image".into(),
            filename: filename.map(Into::into),
            modified_at: modified.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn paths_change_with_modified_at_and_keep_names() {
        let a = path(&att("x://a/b", None, Some("IMG 1.jpeg"))).unwrap();
        let b = path(&att("x://a/b", Some("2026-11-13T09:30:00Z"), Some("IMG 1.jpeg"))).unwrap();
        assert_ne!(a, b);
        assert_eq!(a.file_name().unwrap(), "IMG 1.jpeg");
        let odd = path(&att("y", None, Some("../.."))).unwrap();
        assert_eq!(odd.file_name().unwrap(), ".._..");
        let none = path(&att("y", None, Some(".."))).unwrap();
        assert_eq!(none.file_name().unwrap(), "attachment");
    }

    #[test]
    fn prunes_old_and_oversized() {
        let root = std::env::temp_dir().join(format!("notes-prune-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let now = SystemTime::now();
        let day = Duration::from_secs(24 * 3600);
        let put = |sub: &str, name: &str, len: usize, age: Duration| {
            let d = root.join(sub);
            fs::create_dir_all(&d).unwrap();
            let p = d.join(name);
            fs::write(&p, vec![0u8; len]).unwrap();
            let t = now - age;
            let times = fs::FileTimes::new().set_accessed(t).set_modified(t);
            fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_times(times)
                .unwrap();
            p
        };
        let old = put("a", "old.png", 10, day * 40);
        let fresh = put("b", "fresh.png", 60, day);
        let older = put("c", "older.png", 60, day * 2);
        let part = put("d", ".1-1.part", 5, day * 2);
        prune_dir(&root, now, day * 30, 100);
        assert!(!old.exists(), "unused for 40 days");
        assert!(fresh.exists());
        assert!(!older.exists(), "over the size limit, least recently used");
        assert!(!part.exists(), "stale partial download");
        assert!(!root.join("a").exists(), "empty directory removed");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn texture_cache_is_bounded() {
        let mut t = Textures::default();
        let bytes = gtk::glib::Bytes::from_owned(vec![0u8; 4]);
        let tex: gdk::Texture =
            gdk::MemoryTexture::new(1, 1, gdk::MemoryFormat::R8g8b8a8, &bytes, 4).upcast();
        for i in 0..TEXTURES + 5 {
            t.insert(i.to_string(), tex.clone());
        }
        assert_eq!(t.map.len(), TEXTURES);
        assert!(t.get("0").is_none());
        assert!(t.get(&(TEXTURES + 4).to_string()).is_some());
    }
}
