//! IPC fixture 契约测试。
//!
//! 从 `commands/mod.rs` 拆出来只为可读性：每个命令的响应形状都要能被共享 fixture
//! 反序列化，这条契约由这些测试钉住。

use serde::de::DeserializeOwned;

use super::*;

fn assert_fixture<T: DeserializeOwned + Serialize>(name: &str, raw: &str) {
    let expected: serde_json::Value = serde_json::from_str(raw)
        .unwrap_or_else(|error| panic!("{name} is not valid JSON: {error}"));
    let typed = serde_json::from_value::<T>(expected.clone())
        .unwrap_or_else(|error| panic!("{name} no longer matches its Rust response type: {error}"));
    let actual = serde_json::to_value(typed).expect("response type must serialize");
    assert_eq!(
        actual, expected,
        "{name} drops or renames a field at the Rust boundary"
    );
}

#[test]
fn every_remaining_ipc_fixture_deserializes_into_its_response_type() {
    assert_fixture::<agent_run::RunStopResponse>(
        "agent_run_stop",
        include_str!("../../../../../packages/shared/fixtures/ipc/agent_run_stop.json"),
    );
    assert_fixture::<agent_run::ApprovalRespondResponse>(
        "agent_approval_respond",
        include_str!("../../../../../packages/shared/fixtures/ipc/agent_approval_respond.json"),
    );

    assert_fixture::<provider::ProviderListResponse>(
        "provider_list",
        include_str!("../../../../../packages/shared/fixtures/ipc/provider_list.json"),
    );
    for (name, raw) in [
        (
            "provider_save",
            include_str!("../../../../../packages/shared/fixtures/ipc/provider_save.json"),
        ),
        (
            "provider_save_anthropic",
            include_str!(
                "../../../../../packages/shared/fixtures/ipc/provider_save_anthropic.json"
            ),
        ),
        (
            "provider_save_gemini",
            include_str!("../../../../../packages/shared/fixtures/ipc/provider_save_gemini.json"),
        ),
    ] {
        assert_fixture::<provider::ProviderSaveResponse>(name, raw);
    }
    assert_fixture::<provider::ProviderActivateResponse>(
        "provider_activate",
        include_str!("../../../../../packages/shared/fixtures/ipc/provider_activate.json"),
    );
    assert_fixture::<provider::ProviderDeleteResponse>(
        "provider_delete",
        include_str!("../../../../../packages/shared/fixtures/ipc/provider_delete.json"),
    );
    assert_fixture::<provider::ProviderModelsResponse>(
        "provider_models",
        include_str!("../../../../../packages/shared/fixtures/ipc/provider_models.json"),
    );
    assert_fixture::<provider::ProviderTestResponse>(
        "provider_test",
        include_str!("../../../../../packages/shared/fixtures/ipc/provider_test.json"),
    );

    assert_fixture::<server::ServerListResponse>(
        "server_list",
        include_str!("../../../../../packages/shared/fixtures/ipc/server_list.json"),
    );
    assert_fixture::<server::ServerAddResponse>(
        "server_add",
        include_str!("../../../../../packages/shared/fixtures/ipc/server_add.json"),
    );
    assert_fixture::<server::ServerAddResponse>(
        "server_update",
        include_str!("../../../../../packages/shared/fixtures/ipc/server_update.json"),
    );
    assert_fixture::<server::ServerConnectResponse>(
        "server_connect",
        include_str!("../../../../../packages/shared/fixtures/ipc/server_connect.json"),
    );
    assert_fixture::<server::ServerDeleteResponse>(
        "server_delete",
        include_str!("../../../../../packages/shared/fixtures/ipc/server_delete.json"),
    );
    assert_fixture::<server::ServerAuthResponse>(
        "server_auth_respond",
        include_str!("../../../../../packages/shared/fixtures/ipc/server_auth_respond.json"),
    );
    assert_fixture::<server::ServerAuthResponse>(
        "server_auth_cancel",
        include_str!("../../../../../packages/shared/fixtures/ipc/server_auth_cancel.json"),
    );
    assert_fixture::<server::ServerSnapshotResponse>(
        "server_snapshot",
        include_str!("../../../../../packages/shared/fixtures/ipc/server_snapshot.json"),
    );
    assert_fixture::<EmptyResponse>(
        "server_disconnect",
        include_str!("../../../../../packages/shared/fixtures/ipc/server_disconnect.json"),
    );

    assert_fixture::<terminal::TerminalOpenResponse>(
        "terminal_open",
        include_str!("../../../../../packages/shared/fixtures/ipc/terminal_open.json"),
    );
    for (name, raw) in [
        (
            "terminal_write",
            include_str!("../../../../../packages/shared/fixtures/ipc/terminal_write.json"),
        ),
        (
            "terminal_resize",
            include_str!("../../../../../packages/shared/fixtures/ipc/terminal_resize.json"),
        ),
        (
            "terminal_close",
            include_str!("../../../../../packages/shared/fixtures/ipc/terminal_close.json"),
        ),
    ] {
        assert_fixture::<EmptyResponse>(name, raw);
    }

    assert_fixture::<files::RemoteFileListResponse>(
        "remote_file_list",
        include_str!("../../../../../packages/shared/fixtures/ipc/remote_file_list.json"),
    );
    assert_fixture::<files::RemoteFileReadResponse>(
        "remote_file_read",
        include_str!("../../../../../packages/shared/fixtures/ipc/remote_file_read.json"),
    );

    assert_fixture::<mcp::McpServerListResponse>(
        "mcp_server_list",
        include_str!("../../../../../packages/shared/fixtures/ipc/mcp_server_list.json"),
    );
    assert_fixture::<mcp::McpServerView>(
        "mcp_server_save",
        include_str!("../../../../../packages/shared/fixtures/ipc/mcp_server_save.json"),
    );
    assert_fixture::<mcp::McpServerView>(
        "mcp_server_start",
        include_str!("../../../../../packages/shared/fixtures/ipc/mcp_server_start.json"),
    );
    assert_fixture::<mcp::McpServerView>(
        "mcp_server_review",
        include_str!("../../../../../packages/shared/fixtures/ipc/mcp_server_review.json"),
    );
    assert_fixture::<mcp::McpOAuthConnectResult>(
        "mcp_oauth_connect",
        include_str!("../../../../../packages/shared/fixtures/ipc/mcp_oauth_connect.json"),
    );
    assert_fixture::<mcp::McpServerDeleteResponse>(
        "mcp_server_delete",
        include_str!("../../../../../packages/shared/fixtures/ipc/mcp_server_delete.json"),
    );
    assert_fixture::<mcp::McpServerStopResponse>(
        "mcp_server_stop",
        include_str!("../../../../../packages/shared/fixtures/ipc/mcp_server_stop.json"),
    );
    // 网络设置（ADR 0022）：两个命令的响应是同一个类型，fixture 覆盖「经代理」与
    // 「直连」两种解析结果。
    assert_fixture::<network::NetworkProxyView>(
        "network_proxy_get",
        include_str!("../../../../../packages/shared/fixtures/ipc/network_proxy_get.json"),
    );
    assert_fixture::<network::NetworkProxyView>(
        "network_proxy_save",
        include_str!("../../../../../packages/shared/fixtures/ipc/network_proxy_save.json"),
    );
    assert_fixture::<investigation::InvestigationBriefResponse>(
        "investigation_brief_select",
        include_str!("../../../../../packages/shared/fixtures/ipc/investigation_brief_select.json"),
    );
    assert_fixture::<investigation::InvestigationTaskStartResponse>(
        "investigation_task_start",
        include_str!("../../../../../packages/shared/fixtures/ipc/investigation_task_start.json"),
    );
    assert_fixture::<investigation::InvestigationTaskResponse>(
        "investigation_task_stop",
        include_str!("../../../../../packages/shared/fixtures/ipc/investigation_task_stop.json"),
    );
}
