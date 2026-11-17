use crate::model::{FileRec, WireFile};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    InSync,
    KeepLocal,
    Adopt { conflict: bool },
}

fn same_state(local: &FileRec, remote: &WireFile) -> bool {
    local.deleted == remote.deleted && local.hash == remote.hash
}

fn local_matches_base(local: &FileRec) -> bool {
    match &local.synced_hash {
        Some(base) => {
            if local.deleted {
                base.is_empty()
            } else {
                base == &local.hash
            }
        }
        None => false,
    }
}

fn remote_matches_base(local: &FileRec, remote: &WireFile) -> bool {
    match &local.synced_hash {
        Some(base) => {
            if remote.deleted {
                base.is_empty()
            } else {
                base == &remote.hash
            }
        }
        None => false,
    }
}

fn remote_wins(local: &FileRec, remote: &WireFile) -> bool {
    if remote.mtime_ns != local.mtime_ns {
        return remote.mtime_ns > local.mtime_ns;
    }
    remote.device_id > local.device_id
}

pub fn decide(local: Option<&FileRec>, remote: &WireFile) -> Decision {
    let Some(local) = local else {
        return if remote.deleted {
            Decision::InSync
        } else {
            Decision::Adopt { conflict: false }
        };
    };
    if same_state(local, remote) {
        return Decision::InSync;
    }
    let local_changed = !local_matches_base(local);
    let remote_changed = !remote_matches_base(local, remote);
    let conflict = local_changed && remote_changed;
    if conflict || local.synced_hash.is_none() {
        if remote_wins(local, remote) {
            Decision::Adopt { conflict }
        } else {
            Decision::KeepLocal
        }
    } else if remote_changed {
        Decision::Adopt { conflict: false }
    } else {
        Decision::KeepLocal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_local(hash: &str, mtime: i64, device: &str, synced: Option<&str>) -> FileRec {
        FileRec {
            share_id: "s".into(),
            rel_path: "a.txt".into(),
            hash: hash.into(),
            size: hash.len() as u64,
            mtime_ns: mtime,
            deleted: hash.is_empty(),
            device_id: device.into(),
            synced_hash: synced.map(str::to_string),
        }
    }

    fn sample_remote(hash: &str, mtime: i64, device: &str) -> WireFile {
        WireFile {
            rel_path: "a.txt".into(),
            hash: hash.into(),
            size: hash.len() as u64,
            mtime_ns: mtime,
            deleted: hash.is_empty(),
            device_id: device.into(),
        }
    }

    #[test]
    fn missing_local_adopts_sample_remote() {
        let remote = sample_remote("abc", 10, "b");
        assert_eq!(decide(None, &remote), Decision::Adopt { conflict: false });
    }

    #[test]
    fn equal_hash_is_in_sync() {
        let local = sample_local("abc", 1, "a", Some("old"));
        let remote = sample_remote("abc", 9, "b");
        assert_eq!(decide(Some(&local), &remote), Decision::InSync);
    }

    #[test]
    fn only_remote_changed_adopts_without_conflict() {
        let local = sample_local("base", 1, "a", Some("base"));
        let remote = sample_remote("next", 2, "b");
        assert_eq!(
            decide(Some(&local), &remote),
            Decision::Adopt { conflict: false }
        );
    }

    #[test]
    fn only_local_changed_keeps_local_even_if_remote_mtime_is_newer() {
        let local = sample_local("mine", 5, "a", Some("base"));
        let remote = sample_remote("base", 50, "b");
        assert_eq!(decide(Some(&local), &remote), Decision::KeepLocal);
    }

    #[test]
    fn both_changed_newer_remote_adopts_with_conflict() {
        let local = sample_local("mine", 5, "a", Some("base"));
        let remote = sample_remote("theirs", 8, "b");
        assert_eq!(
            decide(Some(&local), &remote),
            Decision::Adopt { conflict: true }
        );
    }

    #[test]
    fn both_changed_newer_local_keeps_sample_local() {
        let local = sample_local("mine", 9, "a", Some("base"));
        let remote = sample_remote("theirs", 8, "b");
        assert_eq!(decide(Some(&local), &remote), Decision::KeepLocal);
    }

    #[test]
    fn tie_uses_device_id() {
        let local = sample_local("mine", 8, "a", None);
        let remote = sample_remote("theirs", 8, "b");
        assert_eq!(
            decide(Some(&local), &remote),
            Decision::Adopt { conflict: true }
        );
        let older_device = sample_remote("theirs", 8, "a");
        let newer_device = sample_local("mine", 8, "b", None);
        assert_eq!(decide(Some(&newer_device), &older_device), Decision::KeepLocal);
    }
}
