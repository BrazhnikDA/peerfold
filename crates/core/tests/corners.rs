use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use peerfold_core::{Node, NodeOpts};

fn temp_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "peerfold-corner-{name}-{}-{}",
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

fn wait_until(mut check: impl FnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(8) {
        if check() {
            return;
        }
        thread::sleep(Duration::from_millis(40));
    }
    panic!("таймаут");
}

#[test]
fn missing_file_and_duplicate_paths_are_rejected() {
    let node = node(temp_dir("reject"));
    let missing = temp_dir("missing-parent").join("gone");
    let err = node.create_share(&missing).unwrap_err();
    assert!(err.to_string().contains("папка не найдена"), "{err}");

    let folder = temp_dir("real");
    let file = folder.join("note.txt");
    fs::write(&file, b"not a folder").unwrap();
    let err = node.create_share(&file).unwrap_err();
    assert!(err.to_string().contains("нужна папка"), "{err}");

    let share = node.create_share(&folder).unwrap();
    let err = node.create_share(&folder).unwrap_err();
    assert!(err.to_string().contains("уже подключена"), "{err}");
    assert!(node.attach_share("bad", &folder).is_err());
    assert!(node.restore(&share.id, "note.txt", &"ab".repeat(32)).is_err());
}

#[test]
fn unavailable_folder_does_not_wipe_files() {
    let data_a = temp_dir("wipe-a");
    let data_b = temp_dir("wipe-b");
    let folder_a = temp_dir("wipe-folder-a");
    let folder_b = temp_dir("wipe-folder-b");
    let node_a = node(data_a);
    let node_b = node(data_b);
    let share = node_a.create_share(&folder_a).unwrap();
    node_b.attach_share(&share.id, &folder_b).unwrap();
    fs::write(folder_a.join("keep.txt"), b"keep-me").unwrap();
    node_a.rescan(&share.id).unwrap();
    node_b.connect(node_a.local_addr()).unwrap();
    let copied = folder_b.join("keep.txt");
    wait_until(|| fs::read(&copied).ok().as_deref() == Some(b"keep-me".as_slice()));

    fs::remove_dir_all(&folder_a).unwrap();
    node_a.rescan(&share.id).unwrap();
    thread::sleep(Duration::from_millis(400));
    assert_eq!(fs::read(&copied).unwrap(), b"keep-me");
    assert_eq!(node_a.snapshot().shares[0].file_count, 1);
}

#[test]
fn dropped_peer_goes_offline_and_can_reconnect() {
    let data_a = temp_dir("drop-a");
    let data_b = temp_dir("drop-b");
    let folder_a = temp_dir("drop-folder-a");
    let folder_b = temp_dir("drop-folder-b");
    let node_a = node(data_a);
    let node_b = node(data_b.clone());
    let share = node_a.create_share(&folder_a).unwrap();
    node_b.attach_share(&share.id, &folder_b).unwrap();
    fs::write(folder_a.join("first.txt"), b"one").unwrap();
    node_a.rescan(&share.id).unwrap();
    node_b.connect(node_a.local_addr()).unwrap();
    wait_until(|| fs::read(folder_b.join("first.txt")).ok().as_deref() == Some(b"one".as_slice()));
    let peer_id = node_b.device_id();
    wait_until(|| {
        node_a
            .snapshot()
            .peers
            .iter()
            .any(|peer| peer.device_id == peer_id && peer.online)
    });

    drop(node_b);
    wait_until(|| {
        node_a
            .snapshot()
            .peers
            .iter()
            .any(|peer| peer.device_id == peer_id && !peer.online)
    });
    assert_eq!(fs::read(folder_a.join("first.txt")).unwrap(), b"one");

    let node_b = node(data_b);
    fs::write(folder_a.join("second.txt"), b"two").unwrap();
    node_a.rescan(&share.id).unwrap();
    node_b.connect(node_a.local_addr()).unwrap();
    wait_until(|| fs::read(folder_b.join("second.txt")).ok().as_deref() == Some(b"two".as_slice()));
    assert_eq!(fs::read(folder_b.join("first.txt")).unwrap(), b"one");
}

#[test]
fn empty_file_and_deletion_propagate() {
    let data_a = temp_dir("empty-a");
    let data_b = temp_dir("empty-b");
    let folder_a = temp_dir("empty-folder-a");
    let folder_b = temp_dir("empty-folder-b");
    let node_a = node(data_a);
    let node_b = node(data_b);
    let share = node_a.create_share(&folder_a).unwrap();
    node_b.attach_share(&share.id, &folder_b).unwrap();
    fs::write(folder_a.join("empty.txt"), b"").unwrap();
    fs::write(folder_a.join("gone.txt"), b"bye").unwrap();
    node_a.rescan(&share.id).unwrap();
    node_b.connect(node_a.local_addr()).unwrap();
    wait_until(|| folder_b.join("empty.txt").is_file() && folder_b.join("gone.txt").is_file());
    assert_eq!(fs::read(folder_b.join("empty.txt")).unwrap(), b"");

    fs::remove_file(folder_a.join("gone.txt")).unwrap();
    node_a.rescan(&share.id).unwrap();
    wait_until(|| !folder_b.join("gone.txt").exists());
    assert!(folder_b.join("empty.txt").is_file());
}

#[test]
fn paused_node_does_not_publish_new_files() {
    let node = node(temp_dir("pause"));
    let folder = temp_dir("pause-folder");
    let share = node.create_share(&folder).unwrap();
    fs::write(folder.join("before.txt"), b"before").unwrap();
    node.rescan(&share.id).unwrap();
    assert_eq!(node.snapshot().shares[0].file_count, 1);

    node.set_paused(true).unwrap();
    fs::write(folder.join("after.txt"), b"after").unwrap();
    node.rescan(&share.id).unwrap();
    assert_eq!(node.snapshot().shares[0].file_count, 1);
    assert!(node.snapshot().paused);

    node.set_paused(false).unwrap();
    assert_eq!(node.snapshot().shares[0].file_count, 2);
}
