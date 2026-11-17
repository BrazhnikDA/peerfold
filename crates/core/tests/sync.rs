use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use std::time::{SystemTime, UNIX_EPOCH};

use peerfold_core::{Node, NodeOpts};

fn temp_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "peerfold-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn node(data: PathBuf) -> Node {
    Node::start_with(NodeOpts {
        data_dir: data,
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        discover: false,
        auto_trust: true,
    })
    .unwrap()
}

fn set_unix(path: &std::path::Path, secs: u64) {
    fs::set_times(
        path,
        fs::FileTimes::new().set_modified(UNIX_EPOCH + std::time::Duration::from_secs(secs)),
    )
    .unwrap();
    let _ = SystemTime::now();
}

fn wait_until(mut check: impl FnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if check() {
            return;
        }
        thread::sleep(Duration::from_millis(40));
    }
    panic!("таймаут синхронизации");
}

#[test]
fn two_computers_sync_conflict_and_restore() {
    let data_a = temp_dir("data-a");
    let data_b = temp_dir("data-b");
    let folder_a = temp_dir("folder-a");
    let folder_b = temp_dir("folder-b");
    let node_a = node(data_a);
    let node_b = node(data_b);
    let share = node_a.create_share(&folder_a).unwrap();
    node_b.attach_share(&share.id, &folder_b).unwrap();
    fs::write(folder_a.join("hello.txt"), b"from-a").unwrap();
    node_a.rescan(&share.id).unwrap();
    node_b.connect(node_a.local_addr()).unwrap();

    let original_hello = node_a
        .versions(&share.id)
        .unwrap()
        .into_iter()
        .find(|version| version.rel_path == "hello.txt")
        .expect("история hello.txt")
        .hash;
    let hello_b = folder_b.join("hello.txt");
    wait_until(|| fs::read(&hello_b).ok().as_deref() == Some(b"from-a".as_slice()));

    fs::write(folder_a.join("note.txt"), b"version-a").unwrap();
    fs::write(folder_b.join("note.txt"), b"version-b").unwrap();
    set_unix(&folder_a.join("note.txt"), 1_700_000_000);
    set_unix(&folder_b.join("note.txt"), 1_800_000_000);
    node_a.rescan(&share.id).unwrap();
    node_b.rescan(&share.id).unwrap();

    wait_until(|| fs::read(folder_a.join("note.txt")).ok().as_deref() == Some(b"version-b".as_slice()));
    wait_until(|| fs::read(folder_b.join("note.txt")).ok().as_deref() == Some(b"version-b".as_slice()));
    wait_until(|| {
        fs::read_dir(&folder_a).unwrap().flatten().any(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.contains(".conflict-") && fs::read(entry.path()).ok().as_deref() == Some(b"version-a".as_slice())
        })
    });

    fs::write(folder_a.join("hello.txt"), b"changed-later").unwrap();
    node_a.rescan(&share.id).unwrap();
    wait_until(|| fs::read(&hello_b).ok().as_deref() == Some(b"changed-later".as_slice()));
    node_a.restore(&share.id, "hello.txt", &original_hello).unwrap();
    wait_until(|| fs::read(folder_a.join("hello.txt")).ok().as_deref() == Some(b"from-a".as_slice()));
    wait_until(|| fs::read(&hello_b).ok().as_deref() == Some(b"from-a".as_slice()));
}
