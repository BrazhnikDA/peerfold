use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::identity::{self, parse_hex_32};
use crate::model::{valid_hash, FileRec, PROTO};
use crate::paths::{conflict_rel, safe_join};
use crate::protocol::{self, decode_chunk, encode_chunk, write_frame, write_msg, Msg};
use crate::scan::{mtime_of, remember, set_mtime_ns};
use crate::state::{writing_key, Inner, LivePeer, SessionOut};

struct End {
    inner: Arc<Inner>,
    device_id: String,
    writer: Arc<Mutex<TcpStream>>,
    active: bool,
}

impl Drop for End {
    fn drop(&mut self) {
        self.inner.connecting.lock().expect("connecting").remove(&self.device_id);
        if !self.active {
            return;
        }
        let mut sessions = self.inner.sessions.lock().expect("sessions");
        if sessions
            .get(&self.device_id)
            .map(|session| Arc::ptr_eq(&session.writer, &self.writer))
            .unwrap_or(false)
        {
            sessions.remove(&self.device_id);
        }
        drop(sessions);
        if let Some(peer) = self.inner.live.lock().expect("live").get_mut(&self.device_id) {
            peer.online = false;
            peer.remote_trust = false;
        }
        self.inner.touch();
    }
}

struct Job {
    share_id: String,
    entry: crate::model::WireFile,
}

struct Pending {
    share_id: String,
    entry: crate::model::WireFile,
    file: File,
    hasher: blake3::Hasher,
    got: u64,
    partial: std::path::PathBuf,
}

pub fn session(stream: TcpStream, inner: Arc<Inner>) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    let Ok(writer_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = stream;
    let writer = Arc::new(Mutex::new(writer_stream));
    if send_hello(&writer, &inner).is_err() {
        return;
    }
    let hello = match read_hello(&mut reader) {
        Ok(hello) => hello,
        Err(_) => return,
    };
    if hello.device_id == inner.device_id {
        return;
    }
    let mut end = End {
        inner: Arc::clone(&inner),
        device_id: hello.device_id.clone(),
        writer: Arc::clone(&writer),
        active: false,
    };
    if rejected_key(&inner, &hello) {
        inner.log(format!("Ключ {} изменился, соединение закрыто", hello.name));
        return;
    }
    {
        let mut sessions = inner.sessions.lock().expect("sessions");
        if sessions.contains_key(&hello.device_id) {
            return;
        }
        sessions.insert(
            hello.device_id.clone(),
            SessionOut {
                writer: Arc::clone(&writer),
                started: std::sync::atomic::AtomicBool::new(false),
            },
        );
        end.active = true;
    }
    remember_peer(&inner, &hello);
    if inner.auto_trust {
        let _ = inner.with_store(|store| store.set_trusted(&hello.device_id, true));
        set_remote_trust(&inner, &hello.device_id, true);
        let _ = send(&writer, &inner, &Msg::PairAccept);
        ensure_sync(&inner, &hello.device_id);
    } else if inner.local_trust(&hello.device_id) {
        let _ = send(&writer, &inner, &Msg::PairRequest);
    }
    inner.log(format!("{} на связи", hello.name));

    let mut queue = VecDeque::new();
    let mut inflight: Option<Job> = None;
    let mut pending: Option<Pending> = None;
    loop {
        if inner.shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let frame = match protocol::read_frame(&mut reader) {
            Ok(Some(frame)) => frame,
            Ok(None) => continue,
            Err(_) => break,
        };
        if !frame.verify(&hello.pubkey) {
            inner.log(format!("Плохая подпись от {}", hello.name));
            break;
        }
        if frame.kind == 2 {
            if let Some(current) = pending.as_mut() {
                match decode_chunk(&frame.payload) {
                    Ok(chunk)
                        if chunk.share_id == current.share_id && chunk.rel_path == current.entry.rel_path =>
                    {
                        if chunk.offset != current.got || chunk.total != current.entry.size {
                            inner.log(format!("Сбой передачи {}", chunk.rel_path));
                            break;
                        }
                        if current.file.write_all(chunk.data).is_err() {
                            break;
                        }
                        current.hasher.update(chunk.data);
                        current.got += chunk.data.len() as u64;
                        if current.got == current.entry.size {
                            let done = pending.take().unwrap();
                            finish_download(&inner, done);
                            inflight = None;
                            pump(&inner, &writer, &mut queue, &mut inflight);
                        }
                    }
                    _ => inner.log("Пропущен повреждённый блок"),
                }
            }
            continue;
        }
        let Ok(message) = frame.message() else {
            continue;
        };
        match message {
            Msg::Hello { .. } => {}
            Msg::PairRequest => {
                set_remote_trust(&inner, &hello.device_id, true);
                if inner.local_trust(&hello.device_id) {
                    let _ = send(&writer, &inner, &Msg::PairAccept);
                    ensure_sync(&inner, &hello.device_id);
                } else {
                    inner.log(format!("{} хочет подключиться", hello.name));
                }
            }
            Msg::PairAccept => {
                set_remote_trust(&inner, &hello.device_id, true);
                if inner.local_trust(&hello.device_id) {
                    ensure_sync(&inner, &hello.device_id);
                }
            }
            Msg::PairReject => {
                let _ = inner.with_store(|store| store.set_trusted(&hello.device_id, false));
                set_remote_trust(&inner, &hello.device_id, false);
                inner.log(format!("{} отклонил подключение", hello.name));
            }
            Msg::Shares { shares } => {
                let mut remote = inner.remote_shares.lock().expect("remote");
                for share in shares {
                    remote.insert((hello.device_id.clone(), share.id), share.name);
                }
                drop(remote);
                inner.touch();
            }
            Msg::Want { share_id } => {
                if inner.mutual(&hello.device_id) {
                    send_share_index(&inner, &hello.device_id, &share_id);
                }
            }
            Msg::Index { share_id, entries } => {
                on_index(&inner, &writer, &hello.device_id, &share_id, entries, &mut queue, &mut inflight);
            }
            Msg::Get { share_id, rel_path } => {
                serve_file(&inner, &writer, &share_id, &rel_path);
            }
            Msg::File { share_id, entry } => {
                let matches = inflight
                    .as_ref()
                    .map(|job| job.share_id == share_id && job.entry.rel_path == entry.rel_path)
                    .unwrap_or(false);
                if !matches {
                    continue;
                }
                if entry.deleted || entry.size == 0 {
                    if entry.deleted {
                        let _ = apply_remote_delete(&inner, &share_id, &entry, false);
                    } else if valid_hash(&entry.hash) {
                        let _ = write_remote_bytes(&inner, &share_id, &entry, &[]);
                    }
                    inflight = None;
                    pump(&inner, &writer, &mut queue, &mut inflight);
                    continue;
                }
                if !valid_hash(&entry.hash) {
                    inflight = None;
                    pump(&inner, &writer, &mut queue, &mut inflight);
                    continue;
                }
                match open_partial(&inner, &share_id, &entry) {
                    Ok(next) => pending = Some(next),
                    Err(err) => {
                        inner.log(err.to_string());
                        inflight = None;
                        pump(&inner, &writer, &mut queue, &mut inflight);
                    }
                }
            }
            Msg::Missing { rel_path, .. } => {
                if inflight.as_ref().map(|job| job.entry.rel_path == rel_path).unwrap_or(false) {
                    inflight = None;
                    pending = None;
                    pump(&inner, &writer, &mut queue, &mut inflight);
                }
            }
        }
    }
}

struct Hello {
    device_id: String,
    name: String,
    pubkey: [u8; 32],
    pubkey_hex: String,
}

fn send_hello(writer: &Mutex<TcpStream>, inner: &Inner) -> Result<()> {
    let identity = inner.identity();
    send(
        writer,
        inner,
        &Msg::Hello {
            device_id: identity.device_id.clone(),
            name: identity.name.clone(),
            pubkey: identity.pubkey_hex(),
            proto: PROTO,
        },
    )
}

fn read_hello(reader: &mut TcpStream) -> Result<Hello> {
    for _ in 0..20 {
        let Some(frame) = protocol::read_frame(reader)? else {
            continue;
        };
        if frame.kind != 1 {
            continue;
        }
        let Msg::Hello { device_id, name, pubkey, proto } = frame.message()? else {
            continue;
        };
        if proto != PROTO {
            return Err(Error::msg("другая версия протокола"));
        }
        let pubkey_bytes = parse_hex_32(&pubkey)?;
        if device_id != identity::device_id_from_pubkey(&pubkey_bytes) || !frame.verify(&pubkey_bytes) {
            return Err(Error::msg("чужая подпись"));
        }
        return Ok(Hello {
            device_id,
            name,
            pubkey: pubkey_bytes,
            pubkey_hex: pubkey,
        });
    }
    Err(Error::msg("нет приветствия"))
}

fn rejected_key(inner: &Inner, hello: &Hello) -> bool {
    inner
        .with_store(|store| store.peer(&hello.device_id))
        .ok()
        .flatten()
        .map(|peer| !peer.pubkey_hex.is_empty() && peer.pubkey_hex != hello.pubkey_hex)
        .unwrap_or(false)
}

fn remember_peer(inner: &Inner, hello: &Hello) {
    let existing_trust = inner.local_trust(&hello.device_id);
    let _ = inner.with_store(|store| {
        store.upsert_peer(&crate::model::PeerRow {
            device_id: hello.device_id.clone(),
            name: hello.name.clone(),
            pubkey_hex: hello.pubkey_hex.clone(),
            trusted: existing_trust,
            last_seen: Some(crate::model::now_secs()),
        })
    });
    let mut live = inner.live.lock().expect("live");
    let peer = live.entry(hello.device_id.clone()).or_insert_with(|| LivePeer {
        name: hello.name.clone(),
        pubkey_hex: hello.pubkey_hex.clone(),
        addr: None,
        online: true,
        remote_trust: false,
    });
    peer.name = hello.name.clone();
    peer.pubkey_hex = hello.pubkey_hex.clone();
    peer.online = true;
    drop(live);
    inner.touch();
}

fn set_remote_trust(inner: &Inner, device_id: &str, trusted: bool) {
    if let Some(peer) = inner.live.lock().expect("live").get_mut(device_id) {
        peer.remote_trust = trusted;
    }
    inner.touch();
}

fn ensure_sync(inner: &Inner, device_id: &str) {
    if !inner.mutual(device_id) || inner.mark_sync_started(device_id) {
        return;
    }
    inner.push_full(device_id);
}

fn send(writer: &Mutex<TcpStream>, inner: &Inner, message: &Msg) -> Result<()> {
    let mut stream = writer.lock().expect("writer");
    write_msg(&mut stream, &inner.identity(), message)
}

fn send_share_index(inner: &Inner, device_id: &str, share_id: &str) {
    let entries = inner
        .with_store(|store| {
            Ok(store
                .files(share_id)?
                .into_iter()
                .map(|file| file.to_wire())
                .collect::<Vec<_>>())
        })
        .unwrap_or_default();
    for chunk in entries.chunks(300) {
        let _ = inner.send_msg(
            device_id,
            &Msg::Index {
                share_id: share_id.to_string(),
                entries: chunk.to_vec(),
            },
        );
    }
}

fn on_index(
    inner: &Inner,
    writer: &Mutex<TcpStream>,
    device_id: &str,
    share_id: &str,
    entries: Vec<crate::model::WireFile>,
    queue: &mut VecDeque<Job>,
    inflight: &mut Option<Job>,
) {
    if !inner.mutual(device_id) || inner.paused() {
        return;
    }
    let Some(share) = inner.with_store(|store| store.share(share_id)).ok().flatten() else {
        inner
            .remote_shares
            .lock()
            .expect("remote")
            .entry((device_id.to_string(), share_id.to_string()))
            .or_insert_with(|| share_id.to_string());
        inner.touch();
        return;
    };
    let mut adopted = 0usize;
    for entry in entries {
        if safe_join(Path::new(&share.path), &entry.rel_path).is_err() {
            continue;
        }
        let local = inner
            .with_store(|store| store.file(share_id, &entry.rel_path))
            .ok()
            .flatten();
        match crate::decide::decide(local.as_ref(), &entry) {
            crate::decide::Decision::InSync => {
                let synced = if entry.deleted { "" } else { entry.hash.as_str() };
                let _ = inner.with_store(|store| store.set_synced(share_id, &entry.rel_path, synced));
            }
            crate::decide::Decision::KeepLocal => {}
            crate::decide::Decision::Adopt { conflict } => {
                if entry.deleted {
                    let _ = apply_remote_delete(inner, share_id, &entry, conflict);
                } else {
                    if conflict {
                        if let Some(local) = local.as_ref() {
                            let _ = save_conflict(inner, &share, local);
                        }
                    }
                    queue.push_back(Job { share_id: share_id.to_string(), entry });
                }
                adopted += 1;
            }
        }
    }
    if adopted == 1 {
        inner.log(format!("Обновляю файл в «{}»", share.name));
    } else if adopted > 1 {
        inner.log(format!("Обновляю {adopted} файлов в «{}»", share.name));
    }
    pump(inner, writer, queue, inflight);
}

fn pump(inner: &Inner, writer: &Mutex<TcpStream>, queue: &mut VecDeque<Job>, inflight: &mut Option<Job>) {
    if inflight.is_some() || inner.paused() {
        return;
    }
    let Some(job) = queue.pop_front() else {
        return;
    };
    let message = Msg::Get {
        share_id: job.share_id.clone(),
        rel_path: job.entry.rel_path.clone(),
    };
    if send(writer, inner, &message).is_ok() {
        *inflight = Some(job);
    }
}

fn serve_file(inner: &Inner, writer: &Mutex<TcpStream>, share_id: &str, rel_path: &str) {
    if inner.paused() {
        let _ = send(writer, inner, &Msg::Missing { share_id: share_id.into(), rel_path: rel_path.into() });
        return;
    }
    let share = inner.with_store(|store| store.share(share_id)).ok().flatten();
    let record = inner.with_store(|store| store.file(share_id, rel_path)).ok().flatten();
    let Some(share) = share else {
        let _ = send(writer, inner, &Msg::Missing { share_id: share_id.into(), rel_path: rel_path.into() });
        return;
    };
    let Some(record) = record else {
        let _ = send(writer, inner, &Msg::Missing { share_id: share_id.into(), rel_path: rel_path.into() });
        return;
    };
    if record.deleted {
        let _ = send(writer, inner, &Msg::File { share_id: share_id.into(), entry: record.to_wire() });
        return;
    }
    let Ok(path) = safe_join(Path::new(&share.path), rel_path) else {
        return;
    };
    let Ok(mut file) = File::open(&path) else {
        let _ = send(writer, inner, &Msg::Missing { share_id: share_id.into(), rel_path: rel_path.into() });
        return;
    };
    let entry = record.to_wire();
    if send(writer, inner, &Msg::File { share_id: share_id.into(), entry: entry.clone() }).is_err() {
        return;
    }
    if entry.size == 0 {
        return;
    }
    let mut offset = 0u64;
    let mut buf = vec![0u8; crate::model::CHUNK];
    loop {
        let Ok(read) = file.read(&mut buf) else { return };
        if read == 0 {
            break;
        }
        let Ok(payload) = encode_chunk(share_id, rel_path, offset, entry.size, &buf[..read]) else {
            return;
        };
        let identity = inner.identity();
        let mut stream = writer.lock().expect("writer");
        if write_frame(&mut stream, &identity, 2, &payload).is_err() {
            return;
        }
        offset += read as u64;
    }
}

fn open_partial(inner: &Inner, share_id: &str, entry: &crate::model::WireFile) -> Result<Pending> {
    let share = inner
        .with_store(|store| store.share(share_id))?
        .ok_or_else(|| Error::msg("папка не подключена"))?;
    let partial_rel = format!("{}.peerfold-partial", entry.rel_path);
    let partial = safe_join(Path::new(&share.path), &partial_rel)?;
    if let Some(parent) = partial.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(&partial)?;
    Ok(Pending {
        share_id: share_id.to_string(),
        entry: entry.clone(),
        file,
        hasher: blake3::Hasher::new(),
        got: 0,
        partial,
    })
}

fn finish_download(inner: &Inner, pending: Pending) {
    let hash = pending.hasher.finalize().to_hex().to_string();
    if hash != pending.entry.hash {
        let _ = fs::remove_file(&pending.partial);
        inner.log(format!("Хеш не совпал: {}", pending.entry.rel_path));
        return;
    }
    if write_remote_file(inner, &pending.share_id, &pending.entry, &pending.partial).is_err() {
        let _ = fs::remove_file(&pending.partial);
    }
}

fn write_remote_bytes(inner: &Inner, share_id: &str, entry: &crate::model::WireFile, bytes: &[u8]) -> Result<()> {
    let share = inner
        .with_store(|store| store.share(share_id))?
        .ok_or_else(|| Error::msg("папка не подключена"))?;
    let partial_rel = format!("{}.peerfold-partial", entry.rel_path);
    let partial = safe_join(Path::new(&share.path), &partial_rel)?;
    if let Some(parent) = partial.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&partial, bytes)?;
    write_remote_file(inner, share_id, entry, &partial)
}

fn write_remote_file(inner: &Inner, share_id: &str, entry: &crate::model::WireFile, partial: &Path) -> Result<()> {
    let share = inner
        .with_store(|store| store.share(share_id))?
        .ok_or_else(|| Error::msg("папка не подключена"))?;
    let dest = safe_join(Path::new(&share.path), &entry.rel_path)?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let key = writing_key(share_id, &entry.rel_path);
    inner.writing.lock().expect("writing").insert(key.clone());
    let _ = inner.blobs.ingest_file(partial);
    if dest.exists() {
        let _ = fs::remove_file(&dest);
    }
    fs::rename(partial, &dest)?;
    set_mtime_ns(&dest, entry.mtime_ns)?;
    let record = FileRec {
        share_id: share_id.to_string(),
        rel_path: entry.rel_path.clone(),
        hash: entry.hash.clone(),
        size: entry.size,
        mtime_ns: entry.mtime_ns,
        deleted: false,
        device_id: entry.device_id.clone(),
        synced_hash: Some(entry.hash.clone()),
    };
    inner.with_store(|store| {
        store.upsert_file(&record)?;
        remember(store, &inner.blobs, &record)
    })?;
    inner.writing.lock().expect("writing").remove(&key);
    inner.broadcast_index(share_id, &[record.to_wire()]);
    inner.touch();
    Ok(())
}

fn apply_remote_delete(inner: &Inner, share_id: &str, entry: &crate::model::WireFile, conflict: bool) -> Result<()> {
    let share = inner
        .with_store(|store| store.share(share_id))?
        .ok_or_else(|| Error::msg("папка не подключена"))?;
    if conflict {
        if let Some(local) = inner.with_store(|store| store.file(share_id, &entry.rel_path))? {
            save_conflict(inner, &share, &local)?;
        }
    }
    let path = safe_join(Path::new(&share.path), &entry.rel_path)?;
    let key = writing_key(share_id, &entry.rel_path);
    inner.writing.lock().expect("writing").insert(key.clone());
    if path.exists() {
        let _ = fs::remove_file(&path);
    }
    let record = FileRec {
        share_id: share_id.to_string(),
        rel_path: entry.rel_path.clone(),
        hash: String::new(),
        size: 0,
        mtime_ns: entry.mtime_ns,
        deleted: true,
        device_id: entry.device_id.clone(),
        synced_hash: Some(String::new()),
    };
    inner.with_store(|store| store.upsert_file(&record))?;
    inner.writing.lock().expect("writing").remove(&key);
    inner.broadcast_index(share_id, &[record.to_wire()]);
    inner.touch();
    Ok(())
}

fn save_conflict(inner: &Inner, share: &crate::model::ShareRow, local: &FileRec) -> Result<()> {
    if local.deleted || local.hash.is_empty() {
        return Ok(());
    }
    let source = safe_join(Path::new(&share.path), &local.rel_path)?;
    if !source.is_file() {
        return Ok(());
    }
    let mut rel = String::new();
    for attempt in 0..50 {
        let candidate = conflict_rel(&local.rel_path, &inner.device_id, crate::model::now_secs(), attempt);
        let dest = safe_join(Path::new(&share.path), &candidate)?;
        if dest.exists() {
            continue;
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&source, &dest)?;
        rel = candidate;
        break;
    }
    if rel.is_empty() {
        return Err(Error::msg("не удалось сохранить конфликтную копию"));
    }
    let dest = safe_join(Path::new(&share.path), &rel)?;
    let meta = fs::metadata(&dest)?;
    let record = FileRec {
        share_id: share.id.clone(),
        rel_path: rel,
        hash: local.hash.clone(),
        size: local.size,
        mtime_ns: mtime_of(&meta),
        deleted: false,
        device_id: inner.device_id.clone(),
        synced_hash: None,
    };
    inner.with_store(|store| {
        store.upsert_file(&record)?;
        remember(store, &inner.blobs, &record)
    })?;
    inner.log(format!("Конфликт: {} — сохранена копия", local.rel_path));
    inner.broadcast_index(&share.id, &[record.to_wire()]);
    Ok(())
}

pub fn dial(inner: Arc<Inner>, device_id: String, addr: std::net::SocketAddr) {
    {
        let mut connecting = inner.connecting.lock().expect("connecting");
        if inner.online(&device_id) || !connecting.insert(device_id.clone()) {
            return;
        }
    }
    let clear = Arc::clone(&inner);
    std::thread::spawn(move || {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(4)) {
            Ok(stream) => session(stream, inner),
            Err(_) => {}
        }
        clear.connecting.lock().expect("connecting").remove(&device_id);
    });
}
