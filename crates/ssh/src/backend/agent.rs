//! ssh-agent 认证：连接（平台差异收在 [`connect_agent_inner`]）、身份遍历与错误分类。
//!
//! 「agent 没起来」必须看起来就是「agent 没起来」：连不上、没有身份、拒绝签名各有
//! 成套的错误，绝不塌缩成一句「认证失败」，也绝不在失败后静默改试别的认证方式。

use russh::client::{AuthResult, Handle};
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::agent::AgentIdentity;

use super::auth::{best_supported_rsa_hash, PrimaryOutcome};
use super::hostkey::ConnHandler;
use crate::{AgentError, Error, Result};

/// agent 连接 / 问候的上限。
///
/// 必须有：russh 的 `AgentClient::connect_named_pipe` 在管道忙（`ERROR_PIPE_BUSY`）
/// 时每 50ms 重试一次、且**没有次数上限**，而「agent 忙」恰恰是常见状态（另一个
/// `ssh-add` 正在写）。没有这层超时，这个循环会一直转下去 —— 首次连接被
/// `CONNECT_TIMEOUT` 兜住，但 `SessionHandle::reconnect` 没有外层超时，那条路径
/// 会永久挂起。
const AGENT_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// OpenSSH for Windows 的 agent 固定监听这个命名管道（Windows 上没有 Unix socket，
/// 所以「agent 在哪」在那里是一个常量）。
#[cfg(windows)]
const OPENSSH_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

/// agent 客户端统一装箱：Unix socket、Windows 命名管道、Pageant 的 stream 类型
/// 各不相同，装箱之后上面的认证代码只有一条路径。
type DynamicAgent = AgentClient<Box<dyn AgentStream + Send + Unpin>>;

/// agent 认证：连上 agent，把它持有的身份**逐个**交给服务器。
///
/// 逐个而不是只试第一个：真实 agent 里通常躺着好几把 key（旧的、别的用途的、别的
/// 机器的），只试第一个会把「right key 不在第一位」变成一次认证失败。agent 持有的
/// 证书身份走 `authenticate_certificate_with`（服务器要看的是证书本身，而不是证书
/// 里那把公钥），普通身份走 `authenticate_publickey_with`。
pub(super) async fn authenticate_with_agent(
    handle: &mut Handle<ConnHandler>,
    user: &str,
    socket_path: Option<&str>,
) -> Result<PrimaryOutcome> {
    let mut agent = connect_agent(socket_path).await?;
    let identities = agent
        .request_identities()
        .await
        .map_err(|error| Error::Agent(classify_agent_error(&error)))?;
    if identities.is_empty() {
        return Err(Error::Agent(AgentError::NoIdentities));
    }

    let hash = best_supported_rsa_hash(handle).await?;
    let mut identities_tried = 0usize;
    let mut partial_success = false;
    let mut keyboard_interactive_available = false;
    for identity in &identities {
        identities_tried += 1;
        let result = match identity {
            AgentIdentity::PublicKey { key, .. } => handle
                .authenticate_publickey_with(user, key.clone(), hash, &mut agent)
                .await
                .map_err(map_agent_sign_error)?,
            AgentIdentity::Certificate { certificate, .. } => handle
                .authenticate_certificate_with(user, certificate.clone(), hash, &mut agent)
                .await
                .map_err(map_agent_sign_error)?,
        };
        match result {
            AuthResult::Success => return Ok(PrimaryOutcome::Success),
            AuthResult::Failure {
                partial_success: partial,
                remaining_methods,
                ..
            } => {
                partial_success |= partial;
                keyboard_interactive_available |=
                    remaining_methods.contains(&russh::MethodKind::KeyboardInteractive);
            }
        }
    }
    Ok(PrimaryOutcome::Failure {
        keyboard_interactive_available,
        error: Error::Agent(AgentError::Rejected {
            identities_tried,
            partial_success,
        }),
    })
}

/// 连接 agent（平台差异收在这一层），并给整次连接加超时。
///
/// 所有失败都变成 [`AgentError::Unavailable`] 且带上 `agent_label`：这条要求是有意的
/// —— 「agent 连不上」和「密码不对」在界面上必须是两句话，否则用户会去改密码，而
/// 真正要做的是把 agent（或 `ssh-add`）跑起来。
async fn connect_agent(socket_path: Option<&str>) -> Result<DynamicAgent> {
    let label = agent_label(socket_path);
    match tokio::time::timeout(AGENT_CONNECT_TIMEOUT, connect_agent_inner(socket_path)).await {
        Ok(Ok(agent)) => Ok(agent),
        Ok(Err(error)) => Err(Error::Agent(AgentError::Unavailable {
            detail: format!("{label}: {error}"),
        })),
        Err(_) => Err(Error::Agent(AgentError::Unavailable {
            detail: format!(
                "{label}: no answer within {}s",
                AGENT_CONNECT_TIMEOUT.as_secs()
            ),
        })),
    }
}

/// 「我们连的是什么」的人类可读名字，只用于错误消息。
fn agent_label(socket_path: Option<&str>) -> String {
    match socket_path {
        Some(path) => format!("the ssh-agent at {path}"),
        #[cfg(unix)]
        None => "the ssh-agent named by SSH_AUTH_SOCK".to_string(),
        #[cfg(windows)]
        None => format!("the ssh-agent (OpenSSH pipe {OPENSSH_AGENT_PIPE}, then Pageant)"),
        #[cfg(not(any(unix, windows)))]
        None => "the ssh-agent".to_string(),
    }
}

/// Unix：显式路径优先，否则按 `SSH_AUTH_SOCK` 发现（russh 的 `connect_env` 顺带把
/// 「变量指着一个不存在的路径」与「变量根本没设」分成两个不同的错误）。
#[cfg(unix)]
async fn connect_agent_inner(
    socket_path: Option<&str>,
) -> std::result::Result<DynamicAgent, russh::keys::Error> {
    let client = match socket_path {
        Some(path) => AgentClient::connect_uds(path).await?,
        None => AgentClient::connect_env().await?,
    };
    Ok(client.dynamic())
}

/// Windows：显式路径当作命名管道；否则先试 OpenSSH for Windows 的固定管道，再退到
/// Pageant（PuTTY 的 agent 走窗口消息而不是管道，russh 用 `pageant` crate 实现；
/// 它是 russh 在 Windows 上的**非可选**依赖，所以不需要额外 cargo feature）。
#[cfg(windows)]
async fn connect_agent_inner(
    socket_path: Option<&str>,
) -> std::result::Result<DynamicAgent, russh::keys::Error> {
    if let Some(path) = socket_path {
        return Ok(AgentClient::connect_named_pipe(path).await?.dynamic());
    }
    let pipe_error = match AgentClient::connect_named_pipe(OPENSSH_AGENT_PIPE).await {
        Ok(client) => return Ok(client.dynamic()),
        Err(error) => error,
    };
    let pageant_error = match AgentClient::connect_pageant().await {
        Ok(client) => return Ok(client.dynamic()),
        Err(error) => error,
    };
    Err(russh::keys::Error::IO(std::io::Error::other(format!(
        "OpenSSH pipe unavailable ({pipe_error}); Pageant unavailable ({pageant_error})"
    ))))
}

#[cfg(not(any(unix, windows)))]
async fn connect_agent_inner(
    _socket_path: Option<&str>,
) -> std::result::Result<DynamicAgent, russh::keys::Error> {
    Err(russh::keys::Error::IO(std::io::Error::other(
        "no ssh-agent transport is known for this platform",
    )))
}

/// agent 通讯错误 → 类型化错误。
///
/// 这里不再细分「连不上」与「连上后断了」：对调用点而言两者都是「agent 现在不可用」，
/// 底层措辞（连接被拒 / 管道不在 / IO 错误）留在 `detail` 里。
fn classify_agent_error(error: &russh::keys::Error) -> AgentError {
    match error {
        russh::keys::Error::EnvVar(name) => AgentError::Unavailable {
            detail: format!("{name} is not set"),
        },
        russh::keys::Error::BadAuthSock => AgentError::Unavailable {
            detail: "SSH_AUTH_SOCK points at a path that does not exist".into(),
        },
        russh::keys::Error::AgentFailure => AgentError::SigningRejected {
            detail: "the agent answered with a failure message".into(),
        },
        russh::keys::Error::AgentProtocolError => AgentError::Protocol {
            detail: "unexpected reply frame".into(),
        },
        other => AgentError::Unavailable {
            detail: other.to_string(),
        },
    }
}

/// Preserve the two failure owners russh exposes while signing through an agent.
fn map_agent_sign_error(error: russh::AgentAuthError) -> Error {
    match error {
        // The SSH side could not carry the sign request/reply; this is not evidence
        // that the agent itself rejected the identity.
        russh::AgentAuthError::Send(error) => Error::Transport(format!(
            "SSH channel closed while exchanging an ssh-agent signature: {error}"
        )),
        russh::AgentAuthError::Key(russh::keys::Error::AgentFailure) => {
            Error::Agent(AgentError::SigningRejected {
                detail: "the agent answered with a failure message".into(),
            })
        }
        russh::AgentAuthError::Key(russh::keys::Error::AgentProtocolError) => {
            Error::Agent(AgentError::Protocol {
                detail: "unexpected reply frame while signing".into(),
            })
        }
        russh::AgentAuthError::Key(error) => Error::Agent(AgentError::ExchangeFailed {
            detail: error.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// agent 连不上必须是**类型化的 agent 错误**，且消息里点名是哪个 agent：
    /// 「认证失败」对用户没有任何可操作性。
    #[tokio::test]
    async fn unreachable_agent_is_reported_as_a_named_agent_error() {
        // 两边的「路径」形状不同（Windows 命名管道 / Unix UDS），但都保证不存在。
        let path = if cfg!(windows) {
            format!(r"\\.\pipe\yukinal-no-such-agent-{}", std::process::id())
        } else {
            format!("/tmp/yukinal-no-such-agent-{}", std::process::id())
        };
        match connect_agent(Some(&path)).await {
            Err(Error::Agent(AgentError::Unavailable { detail })) => {
                assert!(
                    detail.contains(&path),
                    "agent 错误必须点名是在连哪个 agent：{detail}",
                );
            }
            Err(other) => panic!("expected a typed unavailable-agent error, got {other:?}"),
            Ok(_) => panic!("there is no agent at {path}, the connection must not succeed"),
        }
    }

    /// agent 通讯错误的分类：变量没设 / socket 不在 / agent 说失败 / 应答不合协议。
    #[test]
    fn agent_errors_are_classified() {
        match classify_agent_error(&russh::keys::Error::EnvVar("SSH_AUTH_SOCK")) {
            AgentError::Unavailable { detail } => {
                assert!(
                    detail.contains("SSH_AUTH_SOCK"),
                    "细节里要有变量名：{detail}"
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            classify_agent_error(&russh::keys::Error::BadAuthSock),
            AgentError::Unavailable { .. }
        ));
        assert!(matches!(
            classify_agent_error(&russh::keys::Error::IO(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "gone",
            ))),
            AgentError::Unavailable { .. }
        ));
        assert!(matches!(
            classify_agent_error(&russh::keys::Error::AgentFailure),
            AgentError::SigningRejected { .. }
        ));
        assert!(matches!(
            classify_agent_error(&russh::keys::Error::AgentProtocolError),
            AgentError::Protocol { .. }
        ));
    }

    /// Agent refusal, protocol failure, and SSH transport failure stay distinct.
    #[test]
    fn agent_signing_failures_keep_their_owner() {
        match map_agent_sign_error(russh::AgentAuthError::Key(russh::keys::Error::AgentFailure)) {
            Error::Agent(AgentError::SigningRejected { detail }) => {
                assert!(detail.contains("failure"), "{detail}");
            }
            other => panic!("{other:?}"),
        }

        match map_agent_sign_error(russh::AgentAuthError::Key(
            russh::keys::Error::AgentProtocolError,
        )) {
            Error::Agent(AgentError::Protocol { detail }) => {
                assert!(detail.contains("reply frame"), "{detail}");
            }
            other => panic!("{other:?}"),
        }

        match map_agent_sign_error(russh::AgentAuthError::Key(russh::keys::Error::IO(
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "agent vanished"),
        ))) {
            Error::Agent(AgentError::ExchangeFailed { detail }) => {
                assert!(detail.contains("agent vanished"), "{detail}");
            }
            other => panic!("{other:?}"),
        }

        match map_agent_sign_error(russh::AgentAuthError::Send(russh::SendError {})) {
            Error::Transport(message) => {
                assert!(message.contains("SSH channel closed"), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }
}
