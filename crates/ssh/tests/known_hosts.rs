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

/// 生产连接路径用的是查找，不是比对。
///
/// `establish` 只想知道「钉了什么」，所以它走 `pinned_fingerprint`。这条测试把两者的
/// 关系钉住：**查找返回的钉子必须与 `check` 在指纹相符时给出的钉子一致**，而查找本身
/// 不关心（也不接受）一个「presented」参数。
///
/// 为什么值得写：这段逻辑曾经写成 `check(host, port, "")` 再去合并 `Matches` 与
/// `Mismatch` 两个分支。那样写结果一样，但把「取值」写成了「和空串比对」，并且让
/// `Check::Mismatch` 看起来在生产路径上是活的 —— 它其实只由测试触发，真正的比对在
/// `ConnHandler::check_server_key`（指纹只存在于那个回调里）。这条测试让「查找」这个
/// 概念在类型上独立出来，退回去需要改动它就说明有人没看懂上面这段。
#[test]
fn pinned_fingerprint_is_a_lookup_not_a_comparison() {
    let mut store = KnownHostsStore::in_memory();

    // 未钉过 → 没有钉子，且与 check 的 Unknown 对齐。
    assert_eq!(store.pinned_fingerprint("api.example.com", 22), None);
    assert_eq!(
        store.check("api.example.com", 22, "SHA256:zzz"),
        Check::Unknown
    );

    store
        .register("api.example.com", 22, "SHA256:aaa")
        .expect("register");

    assert_eq!(
        store.pinned_fingerprint("api.example.com", 22),
        Some("SHA256:aaa".into())
    );

    // 与 check 在「相符」时的钉子一致。
    match store.check("api.example.com", 22, "SHA256:aaa") {
        Check::Matches { pinned } => {
            assert_eq!(
                Some(pinned),
                store.pinned_fingerprint("api.example.com", 22)
            );
        }
        other => panic!("expected Matches, got {other:?}"),
    }

    // 关键性质：查找与「服务器会出示什么」无关。指纹不符时 check 报 Mismatch，
    // 但查找仍返回同一个钉子 —— 这正是 establish 需要的行为。
    assert!(matches!(
        store.check("api.example.com", 22, "SHA256:bbb"),
        Check::Mismatch { .. }
    ));
    assert_eq!(
        store.pinned_fingerprint("api.example.com", 22),
        Some("SHA256:aaa".into())
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
