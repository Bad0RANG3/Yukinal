//! Host-key trust commands (ADR 0012): 状态 / 探针 / 钉住 / 遗忘。
//!
//! 这四个命令是「主机指纹没有手动核验入口」这个缺口的出口。四条规则决定了它们的形状：
//!
//! 1. **探针与信任分开。** 探针真的连一次并回答「服务器出示了什么」，但不持久化任何
//!    状态；钉住是另一个命令、另一个用户动作。把两者合成一个「检查并信任」会让「看一眼」
//!    变成一次副作用（ADR 0012 第 2、4 条）。
//! 2. **出示的不等于可信的。** 响应字段叫 `presentedFingerprint`，不叫 `verified`：
//!    探针结果是服务器对「我是谁」的声称，在用户把它与自己手上的指纹核对之前不可信。
//! 3. **不一致时没有「仍然继续」。** `server_host_key_trust` 只钉用户确认过的那个指纹，
//!    与已钉的不同就拒绝，并明确告诉用户先遗忘（ADR 0012 第 5 条）。
//! 4. **pin 按 `host:port` 关联**（第 7 条）。所以响应里回的是 host/port 而不是只有
//!    serverId —— 同一台机器在两个条目下共用同一条 pin，界面必须能说出是哪一条。
//!
//! 这一层不做 SSH：能力在 `crates/ssh`（`RusshBackend::probe_host_key` / `trust_host` /
//! `forget_host` / `host_key_pin`），策略规则在 `KnownHostsStore` 的纯函数里。

use serde::Serialize;
use tauri::State;

use crate::state::AppState;
use yukinal_ssh::known_hosts::{Check, ForgetOutcome, TrustDecision};
use yukinal_ssh::SshBackend;

/// `server_host_key_status`：这台服务器对应的 `host:port` 现在钉的是什么。
///
/// **不触网**：界面一打开就要画这个状态，为它去连一次服务器是不必要的（也是不该有的）
/// 副作用。
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServerHostKeyStatusResponse {
    pub host: String,
    pub port: u16,
    pub pinned: bool,
    /// 没有钉子时**整个字段不出现**（不是 `null`）：界面靠它的缺席显示「未核验」，
    /// 而 `null` 会让「没有指纹」与「指纹是空的」这两种说法在线上无法区分。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_fingerprint: Option<String>,
}

/// `server_host_key_probe`：服务器这次**出示**的指纹，以及它与钉子的关系。
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServerHostKeyProbeResponse {
    pub host: String,
    pub port: u16,
    /// 服务器出示的指纹。**不是**「已验证」—— 见模块文档第 2 条。
    pub presented_fingerprint: String,
    /// `unpinned` / `matches` / `mismatch`（词形由 `Comparison::as_str` 给出，
    /// 与 `@yukinal/shared` 的 `HOST_KEY_COMPARISONS` 是同一组词）。
    pub comparison: &'static str,
    /// 比对用的钉子，未钉过时缺席（同 `status`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_fingerprint: Option<String>,
}

/// `server_host_key_trust`：把用户确认过的指纹钉住了。
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServerHostKeyTrustResponse {
    pub host: String,
    pub port: u16,
    /// 现在钉着的指纹（就是用户确认的那个）。
    pub fingerprint: String,
    /// 本来就钉着**同一个**指纹、什么都没写。注意这里没有「新指纹被接受了」这种字段 ——
    /// 那件事不存在（ADR 0012 第 5 条）。
    pub already_pinned: bool,
}

/// `server_host_key_forget`：钉子删掉了没有。
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServerHostKeyForgetResponse {
    pub host: String,
    pub port: u16,
    /// `false` = 本来就没有钉子。不是错误（重复点「遗忘」不该报错），但也不假装删过。
    pub forgotten: bool,
}

/// 查出 `server_id` 对应的 `host:port`。
///
/// 每个命令都要它，而且每个失败都要说清下一步 —— 「找不到服务器」是最容易被写成
/// 一句 `not found` 就交差的错误，用户拿它没办法。
fn resolve_endpoint(state: &AppState, server_id: &str) -> Result<(String, u16), String> {
    let server = state.database.servers().get(server_id).map_err(|error| {
        format!("读取服务器 `{server_id}` 失败：{error}。请刷新服务器列表后重试；如果这台服务器已被删除，指纹面板对它不再适用。")
    })?;
    Ok((server.connection.host, server.connection.port))
}

/// 把 `Check` 收成 IPC 的 `comparison` 字段。
///
/// 纯映射：`Check` 是 store 的比较结果，`comparison` 是它的线上词形。分开写是因为
/// 「出示的与钉住的什么关系」这个判断必须只有一个来源（`KnownHostsStore::check`），
/// 而这里只负责改名字。
fn comparison_word(check: &Check) -> &'static str {
    check.comparison().as_str()
}

/// `trust` 被拒绝时给用户看的那句话。
///
/// 单独成函数是为了**可测**：这句文案就是用户看到的全部，而它必须包含下一步动作
/// （先遗忘再重新确认）。写成 `Err(format!(...))` 内联在命令里，唯一能测它的方式是把
/// 那句话再抄一遍到测试里 —— 抄一遍等于没测。
fn refusal_message(host: &str, port: u16, pinned: &str, confirmed: &str) -> String {
    format!(
        "{host}:{port} 已经钉着另一个指纹（{pinned}），因此**没有**接受 {confirmed}。\
         如果服务器确实换过密钥，请先在这台服务器的主机指纹面板上「遗忘」旧钉子，\
         再重新探针并确认新指纹。Yukinal 不提供「不一致时仍然继续」：一次变更只能由\
         两次显式动作完成。"
    )
}

/// `server_host_key_status`：本地状态，不触网。
#[tauri::command]
pub async fn server_host_key_status(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<ServerHostKeyStatusResponse, String> {
    let (host, port) = resolve_endpoint(&state, &server_id)?;
    let pinned = state.ssh.host_key_pin(&host, port).map_err(|error| {
        format!(
            "读取 known_hosts 失败：{error}。这是本机数据目录下的文件；请确认它没有被别的程序\
             占用、也没有被改成无法解析的内容，然后重试。"
        )
    })?;

    Ok(ServerHostKeyStatusResponse {
        host,
        port,
        pinned: pinned.is_some(),
        pinned_fingerprint: pinned,
    })
}

/// `server_host_key_probe`：真连一次，返回服务器**出示**的指纹。什么都不写。
///
/// 几件要说清楚的事：
///
/// - 它会真的发起一次连接并在服务器上留下一次（通常未认证的）连接与日志记录。它不是一个
///   纯本地操作，界面文案必须讲这一点。
/// - 它**不认证**，所以不需要凭据，也不会登录。
/// - 它**不写入任何东西**：探针之后 `status` 与探针之前完全一样。这是「看一眼不应该有
///   副作用」这条要求的落地（ADR 0012 第 4 条）。
#[tauri::command]
pub async fn server_host_key_probe(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<ServerHostKeyProbeResponse, String> {
    let (host, port) = resolve_endpoint(&state, &server_id)?;

    let probe = state
        .ssh
        .probe_host_key(&host, port)
        .await
        .map_err(|error| {
            format!(
                "探针连接 {host}:{port} 失败：{error}。请确认主机名/端口填对了、这台机器现在可达，\
             并且本机允许出站 SSH 连接。探针只读取指纹，失败不代表指纹有问题。"
            )
        })?;

    // 比较用的是 store 自己的比较（`check`），不是这里重写一遍的 `if`。
    let check = state
        .ssh
        .host_key_check(&host, port, &probe.fingerprint)
        .map_err(|error| format!("读取 known_hosts 失败：{error}。请重试；持续失败请检查数据目录下 known_hosts 的权限。"))?;

    Ok(ServerHostKeyProbeResponse {
        host,
        port,
        presented_fingerprint: probe.fingerprint,
        comparison: comparison_word(&check),
        pinned_fingerprint: check.pinned().map(str::to_string),
    })
}

/// `server_host_key_trust`：把用户**确认过**的指纹钉住。
///
/// 与已钉指纹不同时拒绝，并告诉用户下一步：先 `server_host_key_forget`，再重新探针确认。
/// 这里没有任何「不一致但继续」的路径 —— 那会是一个把中间人攻击变成一次点击的按钮
/// （ADR 0012 第 5 条，被否决的备选方案之一）。
#[tauri::command]
pub async fn server_host_key_trust(
    state: State<'_, AppState>,
    server_id: String,
    fingerprint: String,
) -> Result<ServerHostKeyTrustResponse, String> {
    let (host, port) = resolve_endpoint(&state, &server_id)?;

    let decision = state.ssh.trust_host(&host, port, &fingerprint).map_err(|error| {
        format!("写入 known_hosts 失败：{error}。指纹**没有**被记住；请检查数据目录是否可写，然后重试。")
    })?;

    match decision {
        TrustDecision::Pin { fingerprint } => Ok(ServerHostKeyTrustResponse {
            host,
            port,
            fingerprint,
            already_pinned: false,
        }),
        TrustDecision::AlreadyPinned { fingerprint } => Ok(ServerHostKeyTrustResponse {
            host,
            port,
            fingerprint,
            already_pinned: true,
        }),
        TrustDecision::RefusedDifferentPin { pinned, confirmed } => {
            Err(refusal_message(&host, port, &pinned, &confirmed))
        }
    }
}

/// `server_host_key_forget`：删除这条 pin。
///
/// 后果要说明白，而且这条 doc 注释是它唯一的书面出处：**下一次连接这台 `host:port` 会回到
/// TOFU** —— 也就是首次连接那套「自动信任并记录」。所以遗忘是一个危险动作：它不只是
/// 「清掉一条记录」，它把下一次连接降级成「不核验就接受」。界面必须写明这一点，而不是把
/// 「遗忘」做成一个没有提示的行内按钮（ADR 0012 的 Consequences）。
///
/// 另外两点：
///
/// - pin 按 `host:port` 关联（第 7 条），所以同一台机器在别的服务器条目下也一起回到 TOFU。
/// - 已经建立的连接不受影响：pin 只在建连时使用，这次遗忘管的是**下一次**连接。
#[tauri::command]
pub async fn server_host_key_forget(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<ServerHostKeyForgetResponse, String> {
    let (host, port) = resolve_endpoint(&state, &server_id)?;

    let outcome = state.ssh.forget_host(&host, port).map_err(|error| {
        format!("从 known_hosts 删除 {host}:{port} 失败：{error}。钉子仍然在，连接行为没有改变；请检查数据目录是否可写，然后重试。")
    })?;

    Ok(ServerHostKeyForgetResponse {
        host,
        port,
        forgotten: matches!(outcome, ForgetOutcome::Removed { .. }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 共享 fixture 的两侧闸门：这一侧断言 Rust 序列化出来的**逐字**等于
    /// `packages/shared/fixtures/ipc/<command>.json`。另一侧（`ipc.test.ts`）断言
    /// zod schema 能解析同一个文件。两边都过 = 「Rust 发的东西 UI 认」。
    const STATUS_FIXTURE: &str =
        include_str!("../../../../../packages/shared/fixtures/ipc/server_host_key_status.json");
    const STATUS_UNPINNED_FIXTURE: &str = include_str!(
        "../../../../../packages/shared/fixtures/ipc/server_host_key_status_unpinned.json"
    );
    const PROBE_FIXTURE: &str =
        include_str!("../../../../../packages/shared/fixtures/ipc/server_host_key_probe.json");
    const PROBE_UNPINNED_FIXTURE: &str = include_str!(
        "../../../../../packages/shared/fixtures/ipc/server_host_key_probe_unpinned.json"
    );
    const TRUST_FIXTURE: &str =
        include_str!("../../../../../packages/shared/fixtures/ipc/server_host_key_trust.json");
    const FORGET_FIXTURE: &str =
        include_str!("../../../../../packages/shared/fixtures/ipc/server_host_key_forget.json");

    const PINNED: &str = "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU";
    const PRESENTED: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    fn fixture(raw: &str) -> serde_json::Value {
        serde_json::from_str(raw).expect("fixture json")
    }

    #[test]
    fn the_status_response_matches_the_shared_fixture() {
        let actual = serde_json::to_value(ServerHostKeyStatusResponse {
            host: "api.example.com".into(),
            port: 22,
            pinned: true,
            pinned_fingerprint: Some(PINNED.into()),
        })
        .expect("serialize");
        assert_eq!(actual, fixture(STATUS_FIXTURE));
    }

    /// 未核验的形状：字段**缺席**，而不是 `null`。
    ///
    /// 值得单独一条 fixture，因为「没有 fingerprint 字段」正是界面显示「未核验」的依据，
    /// 而 `skip_serializing_if` 掉对了没有，只有对着一份逐字 JSON 才看得出来。
    #[test]
    fn the_unpinned_status_response_omits_the_fingerprint() {
        let actual = serde_json::to_value(ServerHostKeyStatusResponse {
            host: "api.example.com".into(),
            port: 22,
            pinned: false,
            pinned_fingerprint: None,
        })
        .expect("serialize");
        assert_eq!(actual, fixture(STATUS_UNPINNED_FIXTURE));
        assert!(
            actual.get("pinnedFingerprint").is_none(),
            "未核验必须是「没有这个字段」，不是 null：{actual}",
        );
    }

    /// 探针的主要 fixture 就是 mismatch —— 契约里最要紧的形状（两个指纹并列）。
    #[test]
    fn the_probe_response_matches_the_shared_fixture() {
        let actual = serde_json::to_value(ServerHostKeyProbeResponse {
            host: "api.example.com".into(),
            port: 22,
            presented_fingerprint: PRESENTED.into(),
            comparison: "mismatch",
            pinned_fingerprint: Some(PINNED.into()),
        })
        .expect("serialize");
        assert_eq!(actual, fixture(PROBE_FIXTURE));

        let actual = serde_json::to_value(ServerHostKeyProbeResponse {
            host: "api.example.com".into(),
            port: 22,
            presented_fingerprint: PRESENTED.into(),
            comparison: "unpinned",
            pinned_fingerprint: None,
        })
        .expect("serialize");
        assert_eq!(actual, fixture(PROBE_UNPINNED_FIXTURE));
    }

    #[test]
    fn the_trust_response_matches_the_shared_fixture() {
        let actual = serde_json::to_value(ServerHostKeyTrustResponse {
            host: "api.example.com".into(),
            port: 22,
            fingerprint: PINNED.into(),
            already_pinned: false,
        })
        .expect("serialize");
        assert_eq!(actual, fixture(TRUST_FIXTURE));
    }

    #[test]
    fn the_forget_response_matches_the_shared_fixture() {
        let actual = serde_json::to_value(ServerHostKeyForgetResponse {
            host: "api.example.com".into(),
            port: 22,
            forgotten: true,
        })
        .expect("serialize");
        assert_eq!(actual, fixture(FORGET_FIXTURE));
    }

    /// 线上词形与 `@yukinal/shared` 的 `HOST_KEY_COMPARISONS` 逐字一致。
    ///
    /// 三个词是契约；把它们写进 fixture 就能让这一侧也失败，但 `Check` → 词形这条映射
    /// 本身（`comparison_word`）仍然需要一条直接断言，否则改错了只会在真机上暴露。
    #[test]
    fn every_comparison_maps_to_the_shared_word() {
        assert_eq!(comparison_word(&Check::Unknown), "unpinned");
        assert_eq!(
            comparison_word(&Check::Matches {
                pinned: PINNED.into()
            }),
            "matches",
        );
        assert_eq!(
            comparison_word(&Check::Mismatch {
                pinned: PINNED.into(),
                presented: PRESENTED.into(),
            }),
            "mismatch",
        );
    }

    /// 拒绝的文案必须指向下一步动作（先遗忘），而不是只说「不能接受」。
    ///
    /// 这条测的是用户实际会读到的那句话：界面把 `Err(String)` 原样显示，所以「怎么做才对」
    /// 必须在那句话里，而不是只存在于某段注释里。
    #[test]
    fn the_refusal_tells_the_user_to_forget_first() {
        let message = refusal_message("api.example.com", 22, PINNED, PRESENTED);
        assert!(message.contains("api.example.com:22"), "{message}");
        assert!(message.contains("遗忘"), "必须说出下一步：{message}");
        assert!(
            message.contains(PINNED),
            "要说清现有的钉子是哪个：{message}"
        );
        assert!(
            message.contains(PRESENTED),
            "也要说清被拒绝的是哪个指纹：{message}",
        );
        assert!(
            message.contains("不一致时仍然继续"),
            "要明确说没有「仍然继续」这条路：{message}",
        );
    }
}
