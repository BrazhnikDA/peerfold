use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::net::TcpStream;

use crate::blobs::BlobStore;
use crate::error::Result;
use crate::identity::Identity;
use crate::model::{ActivitySnap, IncomingSnap, PeerSnap, ShareSnap, Snapshot};
use crate::protocol::Msg;
use crate::store::Store;

pub struct SessionOut {
    pub writer: Arc<Mutex<TcpStream>>,
    pub started: AtomicBool,
}

#[derive(Clone)]
pub struct LivePeer {
    pub name: String,
    pub pubkey_hex: String,
    pub addr: Option<SocketAddr>,
    pub online: bool,
    pub remote_trust: bool,
}

pub struct Inner {
    pub device_id: String,
    pub identity: Mutex<Identity>,
    pub store: Mutex<Store>,
    pub blobs: BlobStore,
    pub auto_trust: bool,
    pub paused: AtomicBool,
    pub shutdown: AtomicBool,
    pub port: AtomicU16,
    pub live: Mutex<HashMap<String, LivePeer>>,
    pub sessions: Mutex<HashMap<String, SessionOut>>,
    pub connecting: Mutex<HashSet<String>>,
    pub remote_shares: Mutex<HashMap<(String, String), String>>,
    pub activity: Mutex<VecDeque<(i64, String)>>,
    pub share_status: Mutex<HashMap<String, String>>,
    pub writing: Mutex<HashSet<String>>,
    pub scan_lock: Mutex<()>,
    pub on_change: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl Inner {
    pub fn identity(&self) -> Identity {
        self.identity.lock().expect("identity").clone()
    }

    pub fn log(&self, text: impl Into<String>) {
        let mut activity = self.activity.lock().expect("activity");
        activity.push_front((crate::model::now_secs(), text.into()));
        while activity.len() > 40 {
            activity.pop_back();
        }
        drop(activity);
        self.touch();
    }

    pub fn touch(&self) {
        if let Some(callback) = self.on_change.lock().expect("callback").clone() {
            callback();
        }
    }

    pub fn with_store<T>(&self, body: impl FnOnce(&mut Store) -> Result<T>) -> Result<T> {
        let mut store = self.store.lock().expect("store");
        body(&mut store)
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn local_trust(&self, device_id: &str) -> bool {
        self.auto_trust
            || self
                .with_store(|store| store.is_trusted(device_id))
                .unwrap_or(false)
    }

    pub fn mutual(&self, device_id: &str) -> bool {
        if self.auto_trust {
            return true;
        }
        let remote = self
            .live
            .lock()
            .expect("live")
            .get(device_id)
            .map(|peer| peer.remote_trust)
            .unwrap_or(false);
        remote && self.local_trust(device_id)
    }

    pub fn online(&self, device_id: &str) -> bool {
        self.live
            .lock()
            .expect("live")
            .get(device_id)
            .map(|peer| peer.online)
            .unwrap_or(false)
    }

    pub fn note_addr(&self, device_id: &str, name: &str, addr: SocketAddr) {
        let mut live = self.live.lock().expect("live");
        let peer = live.entry(device_id.to_string()).or_insert_with(|| LivePeer {
            name: name.to_string(),
            pubkey_hex: String::new(),
            addr: None,
            online: false,
            remote_trust: false,
        });
        if peer.name.is_empty() || peer.name == device_id {
            peer.name = name.to_string();
        }
        peer.addr = Some(addr);
        drop(live);
        self.touch();
    }

    pub fn snapshot(&self) -> Snapshot {
        let identity = self.identity();
        let peers_db = self.with_store(|store| store.peers()).unwrap_or_default();
        let shares = self.with_store(|store| store.shares()).unwrap_or_default();
        let live = self.live.lock().expect("live").clone();
        let statuses = self.share_status.lock().expect("status").clone();
        let mut seen = HashSet::new();
        let mut peers = Vec::new();
        for peer in &peers_db {
            seen.insert(peer.device_id.clone());
            peers.push(self.peer_snap(&identity, peer.device_id.as_str(), &peer.name, peer.trusted, live.get(&peer.device_id)));
        }
        for (device_id, peer) in &live {
            if seen.contains(device_id) {
                continue;
            }
            peers.push(self.peer_snap(&identity, device_id, &peer.name, false, Some(peer)));
        }
        peers.sort_by(|left, right| left.name.cmp(&right.name));
        let share_snaps = shares
            .iter()
            .map(|share| {
                let status = if self.paused() {
                    "paused".to_string()
                } else {
                    statuses.get(&share.id).cloned().unwrap_or_else(|| "idle".into())
                };
                ShareSnap {
                    id: share.id.clone(),
                    name: share.name.clone(),
                    path: share.path.clone(),
                    file_count: self.with_store(|store| store.file_count(&share.id)).unwrap_or(0),
                    status,
                }
            })
            .collect();
        let remote = self.remote_shares.lock().expect("remote").clone();
        let mut incoming = Vec::new();
        for ((device_id, share_id), name) in remote {
            if self.with_store(|store| store.share(&share_id)).ok().flatten().is_some() {
                continue;
            }
            let device_name = live
                .get(&device_id)
                .map(|peer| peer.name.clone())
                .or_else(|| peers_db.iter().find(|peer| peer.device_id == device_id).map(|peer| peer.name.clone()))
                .unwrap_or(device_id.clone());
            incoming.push(IncomingSnap {
                device_id,
                device_name,
                share_id,
                name,
            });
        }
        let activity = self
            .activity
            .lock()
            .expect("activity")
            .iter()
            .map(|(at, text)| ActivitySnap { at: *at, text: text.clone() })
            .collect();
        Snapshot {
            device_id: identity.device_id,
            device_name: identity.name,
            paused: self.paused(),
            port: self.port.load(Ordering::Relaxed),
            peers,
            shares: share_snaps,
            incoming,
            activity,
        }
    }

    fn peer_snap(
        &self,
        identity: &Identity,
        device_id: &str,
        name: &str,
        trusted: bool,
        live: Option<&LivePeer>,
    ) -> PeerSnap {
        let pubkey = live.map(|peer| peer.pubkey_hex.as_str()).unwrap_or("");
        let pairing_code = if pubkey.is_empty() {
            String::new()
        } else {
            crate::identity::pairing_code(&identity.pubkey_hex(), pubkey)
        };
        let online = live.map(|peer| peer.online).unwrap_or(false);
        let remote_trust = live.map(|peer| peer.remote_trust).unwrap_or(false) || self.auto_trust;
        let trusted = trusted || self.auto_trust;
        PeerSnap {
            device_id: device_id.to_string(),
            name: if name.is_empty() { device_id.to_string() } else { name.to_string() },
            online,
            trusted,
            mutual: trusted && remote_trust && (online || self.auto_trust),
            pairing_code,
            waiting: trusted && online && !remote_trust,
        }
    }

    pub fn writer(&self, device_id: &str) -> Option<Arc<Mutex<TcpStream>>> {
        self.sessions
            .lock()
            .expect("sessions")
            .get(device_id)
            .map(|session| Arc::clone(&session.writer))
    }

    pub fn send_msg(&self, device_id: &str, message: &Msg) -> Result<()> {
        let Some(writer) = self.writer(device_id) else {
            return Ok(());
        };
        let mut stream = writer.lock().expect("writer");
        crate::protocol::write_msg(&mut stream, &self.identity(), message)
    }

    pub fn broadcast_index(&self, share_id: &str, entries: &[crate::model::WireFile]) {
        if entries.is_empty() || self.paused() {
            return;
        }
        let targets: Vec<_> = self
            .sessions
            .lock()
            .expect("sessions")
            .keys()
            .cloned()
            .collect();
        for device_id in targets {
            if !self.mutual(&device_id) {
                continue;
            }
            for chunk in entries.chunks(300) {
                let _ = self.send_msg(
                    &device_id,
                    &Msg::Index {
                        share_id: share_id.to_string(),
                        entries: chunk.to_vec(),
                    },
                );
            }
        }
    }

    pub fn announce_shares(&self) {
        let shares = self.with_store(|store| store.shares()).unwrap_or_default();
        let message = Msg::Shares {
            shares: shares
                .into_iter()
                .map(|share| crate::model::ShareAnn {
                    id: share.id,
                    name: share.name,
                })
                .collect(),
        };
        let targets: Vec<_> = self.sessions.lock().expect("sessions").keys().cloned().collect();
        for device_id in targets {
            if self.mutual(&device_id) {
                let _ = self.send_msg(&device_id, &message);
            }
        }
    }

    pub fn push_full(&self, device_id: &str) {
        if !self.mutual(device_id) {
            return;
        }
        self.announce_shares();
        let shares = self.with_store(|store| store.shares()).unwrap_or_default();
        for share in shares {
            let entries = self
                .with_store(|store| {
                    Ok(store
                        .files(&share.id)?
                        .into_iter()
                        .map(|file| file.to_wire())
                        .collect::<Vec<_>>())
                })
                .unwrap_or_default();
            for chunk in entries.chunks(300) {
                let _ = self.send_msg(
                    device_id,
                    &Msg::Index {
                        share_id: share.id.clone(),
                        entries: chunk.to_vec(),
                    },
                );
            }
        }
    }

    pub fn mark_sync_started(&self, device_id: &str) -> bool {
        let sessions = self.sessions.lock().expect("sessions");
        let Some(session) = sessions.get(device_id) else {
            return true;
        };
        session.started.swap(true, Ordering::AcqRel)
    }
}

pub fn writing_key(share_id: &str, rel_path: &str) -> String {
    format!("{share_id}\n{rel_path}")
}
