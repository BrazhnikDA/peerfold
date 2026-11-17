use serde::{Deserialize, Serialize};

pub const PROTO: u32 = 1;
pub const HISTORY_KEEP: usize = 20;
pub const SERVICE_TYPE: &str = "_peerfold._tcp.local.";
pub const CHUNK: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WireFile {
    pub rel_path: String,
    pub hash: String,
    pub size: u64,
    pub mtime_ns: i64,
    pub deleted: bool,
    pub device_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRec {
    pub share_id: String,
    pub rel_path: String,
    pub hash: String,
    pub size: u64,
    pub mtime_ns: i64,
    pub deleted: bool,
    pub device_id: String,
    pub synced_hash: Option<String>,
}

impl FileRec {
    pub fn to_wire(&self) -> WireFile {
        WireFile {
            rel_path: self.rel_path.clone(),
            hash: self.hash.clone(),
            size: self.size,
            mtime_ns: self.mtime_ns,
            deleted: self.deleted,
            device_id: self.device_id.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareAnn {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct ShareRow {
    pub id: String,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct PeerRow {
    pub device_id: String,
    pub name: String,
    pub pubkey_hex: String,
    pub trusted: bool,
    pub last_seen: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    pub rel_path: String,
    pub hash: String,
    pub size: u64,
    pub mtime_ns: i64,
    pub device_id: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerSnap {
    pub device_id: String,
    pub name: String,
    pub online: bool,
    pub trusted: bool,
    pub mutual: bool,
    pub pairing_code: String,
    pub waiting: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareSnap {
    pub id: String,
    pub name: String,
    pub path: String,
    pub file_count: u64,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingSnap {
    pub device_id: String,
    pub device_name: String,
    pub share_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivitySnap {
    pub at: i64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub device_id: String,
    pub device_name: String,
    pub paused: bool,
    pub port: u16,
    pub peers: Vec<PeerSnap>,
    pub shares: Vec<ShareSnap>,
    pub incoming: Vec<IncomingSnap>,
    pub activity: Vec<ActivitySnap>,
}

pub fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

pub fn now_secs() -> i64 {
    now_ns().div_euclid(1_000_000_000)
}

pub fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}
