use std::fs;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use crate::blobs::BlobStore;
use crate::error::{Error, Result};
use crate::identity::{self, Identity};
use crate::model::{now_ns, FileRec, ShareRow, Snapshot, VersionInfo};
use crate::paths::{friendly_path, safe_join};
use crate::protocol::Msg;
use crate::scan::{remember, scan_share, set_mtime_ns};
use crate::state::{writing_key, Inner};
use crate::store::Store;

pub struct NodeOpts {
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
    pub discover: bool,
    pub auto_trust: bool,
}

enum WatchCmd {
    Add(PathBuf),
    Remove(PathBuf),
    Fs,
    Shutdown,
}

pub struct Node {
    inner: Arc<Inner>,
    watch_tx: Sender<WatchCmd>,
    local_addr: SocketAddr,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl Node {
    pub fn start(data_dir: impl Into<PathBuf>) -> Result<Self> {
        Self::start_with(NodeOpts {
            data_dir: data_dir.into(),
            listen: SocketAddr::from(([0, 0, 0, 0], 47_332)),
            discover: true,
            auto_trust: false,
        })
    }

    pub fn start_with(opts: NodeOpts) -> Result<Self> {
        fs::create_dir_all(&opts.data_dir)?;
        let mut store = Store::open(&opts.data_dir.join("peerfold.db"))?;
        let identity = load_identity(&mut store)?;
        let paused = store.meta("paused")?.as_deref() == Some("1");
        let shares = store.shares()?;
        let device_id = identity.device_id.clone();
        let blobs = BlobStore::new(opts.data_dir.join("blobs"))?;
        let listener = bind_listener(opts.listen)?;
        let local_addr = listener.local_addr()?;
        let inner = Arc::new(Inner {
            device_id,
            identity: Mutex::new(identity),
            store: Mutex::new(store),
            blobs,
            auto_trust: opts.auto_trust,
            paused: std::sync::atomic::AtomicBool::new(paused),
            shutdown: std::sync::atomic::AtomicBool::new(false),
            port: std::sync::atomic::AtomicU16::new(local_addr.port()),
            live: Mutex::new(std::collections::HashMap::new()),
            sessions: Mutex::new(std::collections::HashMap::new()),
            connecting: Mutex::new(std::collections::HashSet::new()),
            remote_shares: Mutex::new(std::collections::HashMap::new()),
            activity: Mutex::new(std::collections::VecDeque::new()),
            share_status: Mutex::new(std::collections::HashMap::new()),
            writing: Mutex::new(std::collections::HashSet::new()),
            scan_lock: Mutex::new(()),
            on_change: Mutex::new(None),
        });
        let (watch_tx, watch_rx) = mpsc::channel();
        let mut threads = Vec::new();
        let accept_inner = Arc::clone(&inner);
        threads.push(thread::spawn(move || accept_loop(listener, accept_inner)));
        let watch_inner = Arc::clone(&inner);
        let watch_tx_thread = watch_tx.clone();
        threads.push(thread::spawn(move || watch_loop(watch_rx, watch_tx_thread, watch_inner)));
        if opts.discover {
            threads.push(crate::discover::spawn(Arc::clone(&inner)));
        }
        let node = Self {
            inner,
            watch_tx,
            local_addr,
            threads: Mutex::new(threads),
        };
        for share in shares {
            let path = PathBuf::from(&share.path);
            let _ = node.watch_tx.send(WatchCmd::Add(path));
            node.scan_and_publish(&share.id)?;
        }
        Ok(node)
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn device_id(&self) -> String {
        self.inner.device_id.clone()
    }

    pub fn snapshot(&self) -> Snapshot {
        self.inner.snapshot()
    }

    pub fn set_on_change<F>(&self, callback: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        *self.inner.on_change.lock().expect("callback") = Some(Arc::new(callback));
    }

    pub fn set_paused(&self, paused: bool) -> Result<()> {
        self.inner.paused.store(paused, Ordering::Relaxed);
        self.inner
            .with_store(|store| store.set_meta("paused", if paused { "1" } else { "0" }))?;
        if !paused {
            let shares = self.inner.with_store(|store| store.shares())?;
            for share in shares {
                self.scan_and_publish(&share.id)?;
            }
        }
        self.inner.touch();
        Ok(())
    }

    pub fn set_name(&self, name: &str) -> Result<()> {
        let name = name.trim();
        let chars = name.chars().count();
        if chars == 0 || chars > 64 {
            return Err(Error::msg("имя должно быть от 1 до 64 символов"));
        }
        self.inner.with_store(|store| store.set_name(name))?;
        self.inner.identity.lock().expect("identity").name = name.to_string();
        self.inner.touch();
        Ok(())
    }

    pub fn trust(&self, device_id: &str) -> Result<()> {
        self.inner.with_store(|store| store.set_trusted(device_id, true))?;
        let remote_trust = self
            .inner
            .live
            .lock()
            .expect("live")
            .get(device_id)
            .map(|peer| peer.remote_trust)
            .unwrap_or(false);
        if remote_trust {
            let _ = self.inner.send_msg(device_id, &Msg::PairAccept);
            if !self.inner.mark_sync_started(device_id) {
                self.inner.push_full(device_id);
            }
            self.inner.log("Компьютер подтверждён");
        } else {
            let _ = self.inner.send_msg(device_id, &Msg::PairRequest);
            self.inner.log("Ждём подтверждение на другом компьютере");
        }
        Ok(())
    }

    pub fn reject(&self, device_id: &str) -> Result<()> {
        self.inner.with_store(|store| store.set_trusted(device_id, false))?;
        if let Some(peer) = self.inner.live.lock().expect("live").get_mut(device_id) {
            peer.remote_trust = false;
        }
        let _ = self.inner.send_msg(device_id, &Msg::PairReject);
        self.inner.touch();
        Ok(())
    }

    pub fn forget(&self, device_id: &str) -> Result<()> {
        self.inner.with_store(|store| store.forget_peer(device_id))?;
        self.inner.live.lock().expect("live").remove(device_id);
        if let Some(session) = self.inner.sessions.lock().expect("sessions").remove(device_id) {
            let _ = session.writer.lock().expect("writer").shutdown(std::net::Shutdown::Both);
        }
        self.inner.touch();
        Ok(())
    }

    pub fn create_share(&self, path: &Path) -> Result<ShareRow> {
        self.add_share(None, path)
    }

    pub fn attach_share(&self, share_id: &str, path: &Path) -> Result<ShareRow> {
        if !valid_share_id(share_id) {
            return Err(Error::msg("плохой идентификатор папки"));
        }
        self.add_share(Some(share_id.to_string()), path)
    }

    pub fn remove_share(&self, share_id: &str) -> Result<()> {
        let Some(share) = self.inner.with_store(|store| store.share(share_id))? else {
            return Ok(());
        };
        let _ = self.watch_tx.send(WatchCmd::Remove(PathBuf::from(&share.path)));
        let hashes = self.inner.with_store(|store| store.remove_share(share_id))?;
        for hash in hashes {
            if !self.inner.with_store(|store| store.hash_in_use(&hash))? {
                self.inner.blobs.remove_if_unused(&hash)?;
            }
        }
        self.inner.announce_shares();
        self.inner.touch();
        Ok(())
    }

    pub fn rescan(&self, share_id: &str) -> Result<()> {
        self.scan_and_publish(share_id)
    }

    pub fn versions(&self, share_id: &str) -> Result<Vec<VersionInfo>> {
        self.inner.with_store(|store| store.recent_versions(share_id, 50))
    }

    pub fn restore(&self, share_id: &str, rel_path: &str, hash: &str) -> Result<()> {
        let share = self
            .inner
            .with_store(|store| store.share(share_id))?
            .ok_or_else(|| Error::msg("папка не найдена"))?;
        let version = self
            .inner
            .with_store(|store| store.find_version(share_id, rel_path, hash))?
            .ok_or_else(|| Error::msg("версия не найдена"))?;
        let blob = self.inner.blobs.path(hash)?;
        if !blob.is_file() {
            return Err(Error::msg("содержимое версии уже удалено"));
        }
        let dest = safe_join(Path::new(&share.path), rel_path)?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let key = writing_key(share_id, rel_path);
        self.inner.writing.lock().expect("writing").insert(key.clone());
        fs::copy(&blob, &dest)?;
        let mtime = now_ns();
        set_mtime_ns(&dest, mtime)?;
        let previous = self.inner.with_store(|store| store.file(share_id, rel_path))?;
        let record = FileRec {
            share_id: share_id.to_string(),
            rel_path: rel_path.to_string(),
            hash: hash.to_string(),
            size: version.size,
            mtime_ns: mtime,
            deleted: false,
            device_id: self.inner.device_id.clone(),
            synced_hash: previous.and_then(|file| file.synced_hash),
        };
        self.inner.with_store(|store| {
            store.upsert_file(&record)?;
            remember(store, &self.inner.blobs, &record)
        })?;
        self.inner.writing.lock().expect("writing").remove(&key);
        self.inner.broadcast_index(share_id, &[record.to_wire()]);
        self.inner.log(format!("Возвращена версия {rel_path}"));
        Ok(())
    }

    pub fn connect(&self, addr: SocketAddr) -> Result<()> {
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(4))?;
        let inner = Arc::clone(&self.inner);
        thread::spawn(move || crate::session::session(stream, inner));
        Ok(())
    }

    fn add_share(&self, id: Option<String>, path: &Path) -> Result<ShareRow> {
        let path = friendly_path(&path.canonicalize().map_err(|_| Error::msg("папка не найдена"))?);
        if !path.is_dir() {
            return Err(Error::msg("нужна папка, не файл"));
        }
        let path_text = path
            .to_str()
            .ok_or_else(|| Error::msg("путь не в UTF-8"))?
            .to_string();
        if self
            .inner
            .with_store(|store| store.shares())?
            .iter()
            .any(|share| share.path == path_text)
        {
            return Err(Error::msg("эта папка уже подключена"));
        }
        let id = id.unwrap_or_else(random_id);
        if self.inner.with_store(|store| store.share(&id))?.is_some() {
            return Err(Error::msg("такая папка уже есть на этом компьютере"));
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "папка".into());
        let share = ShareRow {
            id,
            name,
            path: path_text,
        };
        self.inner.with_store(|store| store.insert_share(&share))?;
        self.inner
            .share_status
            .lock()
            .expect("status")
            .insert(share.id.clone(), "syncing".into());
        let _ = self.watch_tx.send(WatchCmd::Add(path));
        let inner = Arc::clone(&self.inner);
        let share_id = share.id.clone();
        thread::spawn(move || {
            let _ = scan_publish(&inner, &share_id);
        });
        self.inner.announce_shares();
        let targets: Vec<_> = self
            .inner
            .sessions
            .lock()
            .expect("sessions")
            .keys()
            .cloned()
            .collect();
        for device_id in targets {
            if self.inner.mutual(&device_id) {
                let _ = self.inner.send_msg(&device_id, &Msg::Want { share_id: share.id.clone() });
            }
        }
        self.inner.log(format!("Папка «{}» в общем доступе", share.name));
        Ok(share)
    }

    fn scan_and_publish(&self, share_id: &str) -> Result<()> {
        if self.inner.paused() {
            return Ok(());
        }
        let _guard = self.inner.scan_lock.lock().expect("scan");
        self.inner
            .share_status
            .lock()
            .expect("status")
            .insert(share_id.to_string(), "syncing".into());
        let Some(share) = self.inner.with_store(|store| store.share(share_id))? else {
            return Ok(());
        };
        let prefix = format!("{share_id}\n");
        let writing = self
            .inner
            .writing
            .lock()
            .expect("writing")
            .iter()
            .filter_map(|key| key.strip_prefix(&prefix).map(str::to_string))
            .collect();
        let changed = {
            let mut store = self.inner.store.lock().expect("store");
            scan_share(&share, &mut store, &self.inner.blobs, &self.inner.device_id, &writing)?
        };
        self.inner
            .share_status
            .lock()
            .expect("status")
            .insert(share_id.to_string(), "idle".into());
        self.inner.broadcast_index(share_id, &changed);
        self.inner.touch();
        Ok(())
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.inner.shutdown.store(true, Ordering::Relaxed);
        let _ = self.watch_tx.send(WatchCmd::Shutdown);
        let _ = TcpStream::connect_timeout(&self.local_addr, Duration::from_millis(300));
        for handle in self.threads.lock().expect("threads").drain(..) {
            let _ = handle.join();
        }
    }
}

fn load_identity(store: &mut Store) -> Result<Identity> {
    if let Some((device_id, name, secret_hex, pubkey_hex)) = store.load_identity()? {
        let secret = identity::parse_hex_32(&secret_hex)?;
        let identity = Identity::from_secret(name, secret);
        if identity.device_id != device_id || identity.pubkey_hex() != pubkey_hex {
            return Err(Error::msg("повреждён ключ устройства"));
        }
        return Ok(identity);
    }
    let identity = Identity::generate(machine_name())?;
    store.save_identity(
        &identity.device_id,
        &identity.name,
        &identity.secret_hex(),
        &identity.pubkey_hex(),
    )?;
    Ok(identity)
}

fn machine_name() -> String {
    if let Ok(name) = std::env::var("COMPUTERNAME") {
        let name = name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }
    if let Ok(output) = std::process::Command::new("hostname").output() {
        if let Ok(text) = String::from_utf8(output.stdout) {
            let text = text.trim();
            if !text.is_empty() {
                return text.to_string();
            }
        }
    }
    "computer".into()
}

fn random_id() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    hex::encode(bytes)
}

fn valid_share_id(id: &str) -> bool {
    (8..=64).contains(&id.len())
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn bind_listener(addr: SocketAddr) -> Result<TcpListener> {
    if addr.port() != 0 {
        if let Ok(listener) = TcpListener::bind(addr) {
            return Ok(listener);
        }
    }
    if addr.ip().is_loopback() {
        return Ok(TcpListener::bind(SocketAddr::new(addr.ip(), 0))?);
    }
    for port in 47_332..47_348 {
        if let Ok(listener) = TcpListener::bind(SocketAddr::new(addr.ip(), port)) {
            return Ok(listener);
        }
    }
    Ok(TcpListener::bind(SocketAddr::new(addr.ip(), 0))?)
}

fn accept_loop(listener: TcpListener, inner: Arc<Inner>) {
    while !inner.shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let inner = Arc::clone(&inner);
                thread::spawn(move || crate::session::session(stream, inner));
            }
            Err(_) => {
                if inner.shutdown.load(Ordering::Relaxed) {
                    break;
                }
                thread::sleep(Duration::from_millis(40));
            }
        }
    }
}

fn watch_loop(rx: Receiver<WatchCmd>, tx: Sender<WatchCmd>, inner: Arc<Inner>) {
    let tx_event = tx.clone();
    let mut watcher: Option<RecommendedWatcher> = notify::recommended_watcher(move |result: std::result::Result<Event, notify::Error>| {
        if result.is_ok() {
            let _ = tx_event.send(WatchCmd::Fs);
        }
    })
    .ok();
    let mut dirty = false;
    let mut last = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(400)) {
            Ok(WatchCmd::Shutdown) => break,
            Ok(WatchCmd::Add(path)) => {
                if let Some(watcher) = watcher.as_mut() {
                    let _ = watcher.watch(&path, RecursiveMode::Recursive);
                }
            }
            Ok(WatchCmd::Remove(path)) => {
                if let Some(watcher) = watcher.as_mut() {
                    let _ = watcher.unwatch(&path);
                }
            }
            Ok(WatchCmd::Fs) => {
                dirty = true;
                last = Instant::now();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if dirty && last.elapsed() >= Duration::from_millis(400) && !inner.paused() {
                    dirty = false;
                    if let Ok(shares) = inner.with_store(|store| store.shares()) {
                        for share in shares {
                            let _ = scan_publish(&inner, &share.id);
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn scan_publish(inner: &Inner, share_id: &str) -> Result<()> {
    if inner.paused() {
        return Ok(());
    }
    let _guard = inner.scan_lock.lock().expect("scan");
    let Some(share) = inner.with_store(|store| store.share(share_id))? else {
        return Ok(());
    };
    let prefix = format!("{share_id}\n");
    let writing = inner
        .writing
        .lock()
        .expect("writing")
        .iter()
        .filter_map(|key| key.strip_prefix(&prefix).map(str::to_string))
        .collect();
    let changed = {
        let mut store = inner.store.lock().expect("store");
        scan_share(&share, &mut store, &inner.blobs, &inner.device_id, &writing)?
    };
    inner.broadcast_index(share_id, &changed);
    inner.touch();
    Ok(())
}
