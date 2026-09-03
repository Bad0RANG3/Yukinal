//! known_hosts store: pure logic tests（不触网）。

use std::path::PathBuf;

use yukinal_ssh::known_hosts::{Check, KnownHostsStore};

fn temp_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "yukinal-known-hosts-{tag}-{}.txt",
        std::process::id()
    ))
}

#[test]
fn register_then_check_matches() {
    let mut store = KnownHostsStore::in_memory();
    assert_eq!(
        store.check("api.example.com", 22, "SHA256:aaa"),
        Check::Unknown
    );
    store
        .register("api.example.com", 22, "SHA256:aaa")
        .expect("register");
    assert_eq!(
        store.check("api.example.com", 22, "SHA256:aaa"),
        Check::Matches {
            pinned: "SHA256:aaa".into()
        },
    );
    // 已钉过的主机出现别的指纹 → mismatch，必须阻断。
    assert_eq!(
        store.check("api.example.com", 22, "SHA256:bbb"),
        Check::Mismatch {
            pinned: "SHA256:aaa".into(),
            presented: "SHA256:bbb".into(),
        },
    );
}

#[test]
fn separate_hosts_and_ports_are_isolated() {
    let mut store = KnownHostsStore::in_memory();
    store
        .register("api.example.com", 22, "SHA256:aaa")
        .expect("register");
    assert_eq!(
        store.check("api.example.com", 2222, "SHA256:aaa"),
        Check::Unknown
    );
    assert_eq!(
        store.check("other.example.com", 22, "SHA256:aaa"),
        Check::Unknown
    );
}

#[test]
fn save_and_load_round_trip() {
    let path = temp_path("roundtrip");
    let _ = std::fs::remove_file(&path);

    let mut store = KnownHostsStore::load(&path).expect("load missing file is fine");
    assert!(store.is_empty());
    store
        .register("api.example.com", 22, "SHA256:aaa")
        .expect("register");
    store
        .register("db.internal", 2200, "SHA256:bbb")
        .expect("register");

    let loaded = KnownHostsStore::load(&path).expect("reload");
    assert_eq!(
        loaded.check("api.example.com", 22, "SHA256:aaa"),
        Check::Matches {
            pinned: "SHA256:aaa".into()
        }
    );
    assert_eq!(
        loaded.check("db.internal", 2200, "SHA256:bbb"),
        Check::Matches {
            pinned: "SHA256:bbb".into()
        }
    );
    // 文件行格式稳定：排序输出，避免无关 diff。
    let raw = std::fs::read_to_string(&path).expect("read");
    assert!(raw.contains("v1:api.example.com:22:SHA256:aaa"));
    assert!(raw.contains("v1:db.internal:2200:SHA256:bbb"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn malformed_lines_are_rejected_loudly() {
    let path = temp_path("malformed");
    let _ = std::fs::remove_file(&path);
    std::fs::write(&path, "v1:example.com:22:not-a-fingerprint\n").expect("write");
    assert!(KnownHostsStore::load(&path).is_err());
    let _ = std::fs::remove_file(&path);
}
