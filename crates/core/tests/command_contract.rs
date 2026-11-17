/// Команды Tauri берут поля по имени аргумента.
/// Интерфейс вызывает `invoke("set_name", { name })`, а не `{ args: { name } }`.
#[test]
fn ipc_commands_match_frontend_keys() {
    let source = include_str!("../../../src-tauri/src/lib.rs");
    for signature in [
        "fn set_name(name: String",
        "fn set_paused(paused: bool",
        "fn trust(device_id: String",
        "fn reject(device_id: String",
        "fn forget(device_id: String",
        "fn create_share(path: String",
        "fn attach_share(share_id: String, path: String",
        "fn remove_share(share_id: String",
        "fn rescan(share_id: String",
        "fn versions(share_id: String",
        "fn restore(share_id: String, rel_path: String, hash: String",
    ] {
        assert!(
            source.contains(signature),
            "команда должна принимать {signature}, иначе интерфейс получит missing required key"
        );
    }
    assert!(
        !source.contains("fn set_name(args:"),
        "set_name не должен требовать ключ args"
    );
}
