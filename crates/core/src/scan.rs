use std::collections::{HashMap, HashSet};
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use walkdir::WalkDir;

use crate::blobs::BlobStore;
use crate::error::Result;
use crate::model::{now_ns, now_secs, FileRec, ShareRow, VersionInfo, WireFile};
use crate::paths::{is_transient, is_user_ignored};
use crate::store::Store;

pub fn set_mtime_ns(path: &Path, mtime_ns: i64) -> Result<()> {
    if mtime_ns < 0 {
        return Err(crate::error::Error::msg("отрицательное время файла"));
    }
    let secs = mtime_ns.div_euclid(1_000_000_000) as u64;
    let nanos = u32::try_from(mtime_ns.rem_euclid(1_000_000_000)).unwrap_or(0);
    fs::set_times(
        path,
        fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::new(secs, nanos)),
    )?;
    Ok(())
}

pub fn mtime_of(meta: &Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

pub fn remember(store: &mut Store, blobs: &BlobStore, file: &FileRec) -> Result<()> {
    if file.deleted || file.hash.is_empty() {
        return Ok(());
    }
    let removed = store.add_version(
        &VersionInfo {
            rel_path: file.rel_path.clone(),
            hash: file.hash.clone(),
            size: file.size,
            mtime_ns: file.mtime_ns,
            device_id: file.device_id.clone(),
            created_at: now_secs(),
        },
        &file.share_id,
    )?;
    for hash in removed {
        if !store.hash_in_use(&hash)? {
            blobs.remove_if_unused(&hash)?;
        }
    }
    Ok(())
}

pub fn scan_share(
    share: &ShareRow,
    store: &mut Store,
    blobs: &BlobStore,
    our_id: &str,
    writing: &HashSet<String>,
) -> Result<Vec<WireFile>> {
    let root = PathBuf::from(&share.path);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut seen = HashSet::new();
    let mut changed = Vec::new();
    let old_map: HashMap<String, FileRec> = store
        .files(&share.id)?
        .into_iter()
        .map(|file| (file.rel_path.clone(), file))
        .collect();

    for entry in WalkDir::new(&root).follow_links(false) {
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel_path) = entry.path().strip_prefix(&root) else {
            continue;
        };
        let rel = rel_path
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if rel.is_empty() || is_transient(&rel) || is_user_ignored(&rel) || writing.contains(&rel) {
            if writing.contains(&rel) {
                seen.insert(rel);
            }
            continue;
        }
        seen.insert(rel.clone());
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        let mtime = mtime_of(&meta);
        let previous = old_map.get(&rel);
        if let Some(previous) = previous {
            if !previous.deleted && previous.size == meta.len() && previous.mtime_ns == mtime {
                continue;
            }
        }
        let (hash, size) = match blobs.ingest_file(entry.path()) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if let Some(previous) = previous {
            if !previous.deleted && previous.hash == hash {
                if previous.mtime_ns != mtime || previous.size != size {
                    let mut updated = previous.clone();
                    updated.mtime_ns = mtime;
                    updated.size = size;
                    store.upsert_file(&updated)?;
                }
                continue;
            }
        }
        let record = FileRec {
            share_id: share.id.clone(),
            rel_path: rel,
            hash,
            size,
            mtime_ns: mtime,
            deleted: false,
            device_id: our_id.to_string(),
            synced_hash: previous.and_then(|file| file.synced_hash.clone()),
            };
        store.upsert_file(&record)?;
        remember(store, blobs, &record)?;
        changed.push(record.to_wire());
    }

    for (rel, previous) in old_map {
        if seen.contains(&rel) || previous.deleted || writing.contains(&rel) {
            continue;
        }
        if !root.join(&rel).exists() {
            let mut record = previous;
            record.hash.clear();
            record.size = 0;
            record.deleted = true;
            record.mtime_ns = now_ns();
            record.device_id = our_id.to_string();
            store.upsert_file(&record)?;
            changed.push(record.to_wire());
        }
    }
    let _ = fs::metadata(&root);
    Ok(changed)
}
