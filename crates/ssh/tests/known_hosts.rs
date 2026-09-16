//! known_hosts store: pure logic tests（不触网）。

use std::path::PathBuf;

use yukinal_ssh::known_hosts::{
    decide_trust, Check, Comparison, ForgetOutcome, KnownHostsStore, TrustDecision,
};

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

/* ── 人工核验（ADR 0012）：钉住 / 遗忘 ──────────────────────────────────────── */

/// `Check` 的第三种读法：三种关系，穷尽。
///
/// 命令层回答 `comparison: unpinned|matches|mismatch` 用的就是它，所以这三个词在
/// 这里被钉一次；拼错其中一个不会有编译错误，只会在界面上表现成「状态不明」。
#[test]
fn a_comparison_is_one_of_three_words() {
    assert_eq!(Check::Unknown.comparison(), Comparison::Unpinned);
    assert_eq!(
        Check::Matches {
            pinned: "SHA256:a".into()
        }
        .comparison(),
        Comparison::Matches
    );
    assert_eq!(
        Check::Mismatch {
            pinned: "SHA256:a".into(),
            presented: "SHA256:b".into()
        }
        .comparison(),
        Comparison::Mismatch
    );

    assert_eq!(Comparison::Unpinned.as_str(), "unpinned");
    assert_eq!(Comparison::Matches.as_str(), "matches");
    assert_eq!(Comparison::Mismatch.as_str(), "mismatch");

    // `pinned()` 与 `comparison()` 必须对同一件事给出一致的答案：未钉过 = 没有钉子。
    assert_eq!(Check::Unknown.pinned(), None);
    assert_eq!(
        Check::Matches {
            pinned: "SHA256:a".into()
        }
        .pinned(),
        Some("SHA256:a")
    );
    assert_eq!(
        Check::Mismatch {
            pinned: "SHA256:a".into(),
            presented: "SHA256:b".into()
        }
        .pinned(),
        Some("SHA256:a"),
        "不一致时钉子仍然是那个钉子 —— 界面要并列显示它",
    );
}

/// `trust` 的决策是纯函数，所以三种情形（含必须拒绝的那一种）都能直接穷举。
#[test]
fn decide_trust_is_a_pure_decision() {
    assert_eq!(
        decide_trust(None, "SHA256:new"),
        TrustDecision::Pin {
            fingerprint: "SHA256:new".into()
        }
    );
    assert_eq!(
        decide_trust(Some("SHA256:same"), "SHA256:same"),
        TrustDecision::AlreadyPinned {
            fingerprint: "SHA256:same".into()
        }
    );
    assert_eq!(
        decide_trust(Some("SHA256:old"), "SHA256:new"),
        TrustDecision::RefusedDifferentPin {
            pinned: "SHA256:old".into(),
            confirmed: "SHA256:new".into(),
        },
        "ADR 0012 第 5 条：单次动作不能接受一把变了的 key",
    );
}

/// **`trust` 拒绝与已钉指纹不同的指纹**，且拒绝时什么都不写。
///
/// 这是 ADR 0012 第 5 条在 IPC 路径上的最后一道闸：界面上的「信任此指纹」按钮只会
/// 传用户确认过的那个指纹，但如果它传错了（或者有人直接调命令），钉子**不能**被改掉。
/// 变更一个钉子必须先 `forget`，再重新探针确认 —— 两次显式动作。
#[test]
fn trust_refuses_a_different_fingerprint_and_writes_nothing() {
    let path = temp_path("trust-refused");
    let _ = std::fs::remove_file(&path);

    let mut store = KnownHostsStore::load(&path).expect("load");
    assert_eq!(
        store
            .trust("api.example.com", 22, "SHA256:old")
            .expect("first pin"),
        TrustDecision::Pin {
            fingerprint: "SHA256:old".into()
        }
    );

    let refusal = store
        .trust("api.example.com", 22, "SHA256:new")
        .expect("a refusal is not an IO failure");
    assert_eq!(
        refusal,
        TrustDecision::RefusedDifferentPin {
            pinned: "SHA256:old".into(),
            confirmed: "SHA256:new".into(),
        }
    );

    // 内存里没变……
    assert_eq!(
        store.pinned_fingerprint("api.example.com", 22),
        Some("SHA256:old".into())
    );
    // ……文件里也没变。只看内存的话，「拒绝」与「悄悄写进去了」在测试里长得一样。
    let raw = std::fs::read_to_string(&path).expect("read store");
    assert!(raw.contains("v1:api.example.com:22:SHA256:old"), "{raw}");
    assert!(!raw.contains("SHA256:new"), "{raw}");

    // 同一个指纹再钉一次：无事可做，但不是错误。
    assert_eq!(
        store
            .trust("api.example.com", 22, "SHA256:old")
            .expect("idempotent"),
        TrustDecision::AlreadyPinned {
            fingerprint: "SHA256:old".into()
        }
    );

    let _ = std::fs::remove_file(&path);
}

/// **`forget` 删掉钉子并落盘**；本来就没有钉子时报「没有」，而不是报错。
#[test]
fn forget_removes_the_pin_and_reports_when_there_was_nothing() {
    let path = temp_path("forget");
    let _ = std::fs::remove_file(&path);

    let mut store = KnownHostsStore::load(&path).expect("load");

    // 空手而归是幂等的，不是错误：用户点两次「遗忘」不该看到一次失败。
    assert_eq!(
        store
            .forget("api.example.com", 22)
            .expect("nothing to forget"),
        ForgetOutcome::NothingPinned
    );

    store
        .trust("api.example.com", 22, "SHA256:aaa")
        .expect("pin");
    store.trust("db.internal", 2200, "SHA256:bbb").expect("pin");

    assert_eq!(
        store.forget("api.example.com", 22).expect("forget"),
        ForgetOutcome::Removed {
            fingerprint: "SHA256:aaa".into()
        }
    );

    // 查不到 → 回到 TOFU 的起点。
    assert_eq!(store.pinned_fingerprint("api.example.com", 22), None);
    assert_eq!(
        store.check("api.example.com", 22, "SHA256:aaa"),
        Check::Unknown
    );

    // 落盘了：别的钉子留着，忘掉的那条不在文件里。
    let raw = std::fs::read_to_string(&path).expect("read store");
    assert!(!raw.contains("api.example.com"), "{raw}");
    assert!(raw.contains("v1:db.internal:2200:SHA256:bbb"), "{raw}");

    // 重新加载：遗忘不是「这次进程记得、下次又回来了」。
    let reloaded = KnownHostsStore::load(&path).expect("reload");
    assert_eq!(reloaded.pinned_fingerprint("api.example.com", 22), None);

    // 别的 host:port 不受影响。
    assert_eq!(
        store.forget("db.internal", 2200).expect("forget"),
        ForgetOutcome::Removed {
            fingerprint: "SHA256:bbb".into()
        }
    );

    let _ = std::fs::remove_file(&path);
}

/// 拒绝之后再遗忘再确认：这条是 ADR 0012 给用户留的**唯一**出路。
#[test]
fn forget_then_trust_is_the_only_way_to_change_a_pin() {
    let mut store = KnownHostsStore::in_memory();
    store
        .trust("api.example.com", 22, "SHA256:old")
        .expect("pin");

    assert!(matches!(
        store.trust("api.example.com", 22, "SHA256:new"),
        Ok(TrustDecision::RefusedDifferentPin { .. })
    ));

    store.forget("api.example.com", 22).expect("forget");
    assert_eq!(
        store
            .trust("api.example.com", 22, "SHA256:new")
            .expect("pin the re-confirmed fingerprint"),
        TrustDecision::Pin {
            fingerprint: "SHA256:new".into()
        }
    );
    assert_eq!(
        store.pinned_fingerprint("api.example.com", 22),
        Some("SHA256:new".into())
    );
}

#[test]
fn a_failed_persist_rolls_the_in_memory_pin_back() {
    let path = temp_path("rollback");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&path);

    let mut store = KnownHostsStore::load(&path).expect("load missing file");
    store
        .register("api.example.com", 22, "SHA256:old")
        .expect("initial pin");

    // A directory at the destination makes the final atomic rename fail while
    // leaving the in-memory map temporarily mutated enough to expose the bug.
    std::fs::remove_file(&path).expect("remove file");
    std::fs::create_dir(&path).expect("replace file with directory");

    assert!(store.register("api.example.com", 22, "SHA256:new").is_err());
    assert_eq!(
        store.pinned_fingerprint("api.example.com", 22),
        Some("SHA256:old".into()),
        "a failed write must not leave memory ahead of durable state"
    );

    std::fs::remove_dir(&path).expect("remove directory");
}

#[test]
fn concurrent_atomic_saves_never_leave_a_partial_file() {
    use std::sync::{Arc, Barrier};

    let path = temp_path("concurrent");
    let _ = std::fs::remove_file(&path);
    let barrier = Arc::new(Barrier::new(8));
    let mut workers = Vec::new();

    for worker in 0..8 {
        let path = path.clone();
        let barrier = Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            let mut store = KnownHostsStore::load(&path).expect("load");
            barrier.wait();
            store
                .register(&format!("host-{worker}.example.com"), 22, "SHA256:aaa")
                .expect("atomic save");
        }));
    }
    for worker in workers {
        worker.join().expect("writer thread");
    }

    let loaded = KnownHostsStore::load(&path).expect("final file must be complete");
    let final_pins = (0..8)
        .filter(|worker| {
            loaded.pinned_fingerprint(&format!("host-{worker}.example.com"), 22)
                == Some("SHA256:aaa".to_string())
        })
        .count();
    assert!(
        final_pins >= 1,
        "at least the last successful writer must survive"
    );

    let _ = std::fs::remove_file(&path);
}
