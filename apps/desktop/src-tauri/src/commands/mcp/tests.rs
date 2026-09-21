//! `mcp.rs` 的单元测试。
//!
//! 从 `mcp.rs` 拆出来只为可读性：这里驱动真实的 MCP supervisor、配置校验与 OAuth
//! 命令接线，与被测代码共享同一套 fixture。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use yukinal_credentials::memory::MemoryCredentialStore;

use super::*;

/* ── 测试脚手架 ───────────────────────────────────────────────────────── */

/// 一个临时数据库文件。
///
/// 不用 `Database::in_memory()`：它是 `yukinal-database` 自己的 `#[cfg(test)]`，跨 crate
/// 取不到（`crates/database/src/lib.rs`）。走文件顺带证明这些命令面对的就是磁盘上那份
/// schema 与那份迁移结果。名字带进程 id 与计数器，所以并行跑的测试互不干扰。
fn temp_db(name: &str) -> (PathBuf, Database) {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "yukinal-mcp-{}-{}-{}.sqlite",
        std::process::id(),
        name,
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    cleanup(&path);
    let database = Database::open(&path).expect("open the temp database");
    (path, database)
}

/// SQLite 的 WAL 会在旁边留下两个文件，一起删掉。
fn cleanup(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(PathBuf::from(format!("{}{suffix}", path.display())));
    }
}

fn save_input(id: &str, transport: &str, enabled: bool) -> McpServerSaveInput {
    McpServerSaveInput {
        id: id.to_string(),
        label: format!("label for {id}"),
        transport: transport.to_string(),
        command: Some("node".to_string()),
        args: None,
        url: None,
        http_auth_headers: None,
        oauth: None,
        enabled,
    }
}

fn auth_header(name: &str, secret: Option<&str>) -> McpHttpAuthHeaderInput {
    McpHttpAuthHeaderInput {
        name: name.to_string(),
        secret: secret.map(str::to_string),
    }
}

/// A public-client OAuth input; tests that need a flow or a secret layer it on with
/// struct-update syntax, so adding a field here does not touch every call site.
fn oauth_input(issuer: &str, client_id: &str, scopes: &[&str]) -> McpOAuthInput {
    McpOAuthInput {
        issuer: issuer.to_string(),
        client_id: client_id.to_string(),
        flow: None,
        client_auth: None,
        client_secret: None,
        dpop: false,
        scopes: scopes.iter().map(|scope| scope.to_string()).collect(),
    }
}

/// 直接写一行（绕过 `save` 的校验：**已经存在**于表里的 `http` 行就是这一类）。
fn insert(database: &Database, row: &McpServerConfig) {
    database.mcp_servers().upsert(row).expect("upsert row");
}

/// 一行存进去再读出来时比什么。
///
/// 不用 `assert_eq!`：`McpServerConfig`（`crates/database/src/models.rs`）没有 `PartialEq`，
/// 而为了一个测试去要求数据库模型实现它，是让测试的形状决定上了线的东西。比 JSON 另有好处
/// —— 它连以后新加的字段一起比，「往返不改写任何东西」这句话不会因为漏了一个字段而变假。
fn as_json(row: &McpServerConfig) -> Value {
    serde_json::to_value(row).expect("McpServerConfig is serializable")
}

fn fixture_path() -> PathBuf {
    // 与 `crates/core/tests/mcp_stdio.rs` 用的是**同一个**已提交的 fixture：cargo 把每个
    // 集成测试编译成独立 crate，所以它没法被共享成模块，但路径可以被共享。
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("crates")
        .join("core")
        .join("tests")
        .join("fixtures")
        .join("mcp-server.js")
}

fn node_path() -> Option<PathBuf> {
    std::env::var("YUKINAL_TEST_NODE")
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.is_file())
}

fn required() -> bool {
    std::env::var("YUKINAL_TEST_REQUIRED")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// `None` = 这台机器没有 node（`node_or_skip` 已经决定那是跳过还是失败）。
fn node_or_skip() -> Option<PathBuf> {
    let node = node_path();
    if node.is_none() {
        let message = "missing YUKINAL_TEST_NODE: set it, or unset YUKINAL_TEST_REQUIRED to skip";
        if required() {
            panic!("{message}");
        }
        eprintln!("skipped: {message}");
    }
    node
}

/// fixture 行：把已提交的 Node MCP 服务进程真的跑起来。
fn fixture_row(id: &str, mode: &str, enabled: bool) -> McpServerConfig {
    // Tests that never launch the row (disabled rows and invalid HTTP endpoints)
    // must not require Node just to construct a fixture record.
    let node = node_path().unwrap_or_else(|| PathBuf::from("node"));
    McpServerConfig {
        id: id.to_string(),
        label: format!("fixture {mode}"),
        transport: "stdio".to_string(),
        command: Some(node.to_string_lossy().to_string()),
        args: Some(vec![
            fixture_path().to_string_lossy().to_string(),
            mode.to_string(),
        ]),
        url: None,
        http_auth_headers: Vec::new(),
        oauth: None,
        enabled,
        allowed_tools: vec!["echo".to_string(), "explode".to_string()],
        trust_level: "unreviewed".to_string(),
    }
}

/// 一个已配置好 fixture 的数据库与 supervisor。`None` = 这台机器没有 node。
fn fixture_setup(
    name: &str,
    mode: &str,
    server_id: &str,
) -> Option<(PathBuf, Database, McpSupervisor)> {
    node_or_skip()?;
    assert!(
        fixture_path().is_file(),
        "the fixture must be committed, not generated: {}",
        fixture_path().display()
    );
    let (path, database) = temp_db(name);
    insert(&database, &fixture_row(server_id, mode, true));
    Some((path, database, McpSupervisor::new()))
}

/* ── 配置往返 ─────────────────────────────────────────────────────────── */

#[test]
fn a_saved_row_reads_back_unchanged_through_get_and_list() {
    let (path, db) = temp_db("round-trip");
    let credentials = MemoryCredentialStore::new();
    let mut input = save_input("mcp_1", "stdio", true);
    input.args = Some(vec!["/srv/mcp/server.js".to_string(), "--mode".to_string()]);
    let saved = save(&db, &credentials, input).expect("save");

    assert_eq!(saved.transport, "stdio");
    assert_eq!(
        saved.trust_level, "unreviewed",
        "a new row starts unreviewed; nothing in this repo promotes it"
    );
    assert!(saved.enabled);

    let by_id = db.mcp_servers().get("mcp_1").expect("get");
    assert_eq!(
        as_json(&by_id),
        as_json(&saved),
        "the round trip must not rewrite anything"
    );

    let listed = db.mcp_servers().list().expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(as_json(&listed[0]), as_json(&saved));
    cleanup(&path);
}

#[test]
fn saving_again_preserves_the_fields_the_form_does_not_own() {
    let (path, db) = temp_db("preserve");
    let credentials = MemoryCredentialStore::new();
    save(&db, &credentials, save_input("mcp_1", "stdio", true)).expect("first save");

    // 模拟「这两个字段由别的东西写过」（它们目前没有界面）。
    let mut stored = db.mcp_servers().get("mcp_1").expect("get");
    stored.allowed_tools = vec!["echo".to_string()];
    stored.trust_level = "reviewed".to_string();
    insert(&db, &stored);

    let mut renamed = save_input("mcp_1", "stdio", false);
    renamed.label = "renamed".to_string();
    let saved = save(&db, &credentials, renamed).expect("second save");

    assert_eq!(saved.label, "renamed");
    assert!(!saved.enabled);
    assert_eq!(
        saved.allowed_tools,
        vec!["echo".to_string()],
        "a rename must not silently drop the reviewed tool list"
    );
    assert_eq!(saved.trust_level, "reviewed");

    let mut moved = save_input("mcp_1", "stdio", false);
    moved.command = Some("different-node".to_string());
    let moved = save(&db, &credentials, moved).expect("the moved endpoint is still saveable");
    assert!(
        moved.allowed_tools.is_empty(),
        "changing the command must revoke the old endpoint's review"
    );
    assert_eq!(moved.trust_level, "unreviewed");
    cleanup(&path);
}

#[test]
fn saving_a_connection_change_stops_the_old_supervisor_session() {
    let (path, db) = temp_db("restart-required");
    let credentials = MemoryCredentialStore::new();
    let first = save(&db, &credentials, save_input("mcp_1", "stdio", true)).expect("first save");

    let mut renamed = save_input("mcp_1", "stdio", true);
    renamed.label = "renamed".to_string();
    let renamed = save(&db, &credentials, renamed).expect("label-only save");
    assert!(!requires_restart(&first, &renamed));

    let mut moved = save_input("mcp_1", "stdio", true);
    moved.command = Some("different-node".to_string());
    let moved = save(&db, &credentials, moved).expect("command change");
    assert!(requires_restart(&renamed, &moved));

    let mut disabled = save_input("mcp_1", "stdio", false);
    disabled.command = moved.command.clone();
    let disabled = save(&db, &credentials, disabled).expect("disable");
    assert!(requires_restart(&moved, &disabled));
    cleanup(&path);
}

#[tokio::test]
async fn tool_review_accepts_only_currently_advertised_names() {
    let Some((path, db, supervisor)) = fixture_setup("review", "ok", "mcp_1") else {
        return;
    };
    let credentials = MemoryCredentialStore::new();
    catalog(&db, &supervisor, Arc::new(credentials))
        .await
        .expect("start and catalog");

    let saved = save_review(
        &db,
        &supervisor,
        McpServerReviewInput {
            server_id: "mcp_1".into(),
            allowed_tools: vec!["echo".into()],
            trust_level: "reviewed".into(),
        },
    )
    .await
    .expect("review succeeds");
    assert_eq!(saved.allowed_tools, vec!["echo".to_string()]);
    assert_eq!(saved.trust_level, "reviewed");

    let error = save_review(
        &db,
        &supervisor,
        McpServerReviewInput {
            server_id: "mcp_1".into(),
            allowed_tools: vec!["invented".into()],
            trust_level: "reviewed".into(),
        },
    )
    .await
    .expect_err("unknown tool must be rejected");
    assert!(error.contains("invented"), "{error}");
    assert_eq!(
        db.mcp_servers().get("mcp_1").expect("row").allowed_tools,
        vec!["echo".to_string()],
        "a rejected review must not partially update the row"
    );

    supervisor.shutdown("mcp_1").await;
    cleanup(&path);
}

#[test]
fn an_http_row_is_saved_with_only_its_endpoint() {
    let (path, db) = temp_db("http-saved");
    let credentials = MemoryCredentialStore::new();
    let mut input = save_input("mcp_http", "http", true);
    input.command = Some("must-not-survive".to_string());
    input.args = Some(vec!["--also-not".to_string()]);
    input.url = Some("https://mcp.example.com/mcp".to_string());
    let saved = save(&db, &credentials, input).expect("a valid HTTP row");
    assert_eq!(saved.transport, "http");
    assert!(saved.command.is_none());
    assert!(saved.args.is_none());
    assert_eq!(saved.url.as_deref(), Some("https://mcp.example.com/mcp"));

    let mut unsafe_transport = save_input("mcp_remote_http", "http", true);
    unsafe_transport.url = Some("http://mcp.example.com/mcp".to_string());
    let error =
        save(&db, &credentials, unsafe_transport).expect_err("remote plaintext HTTP is refused");
    assert!(error.contains("HTTPS"), "{error}");
    assert_eq!(db.mcp_servers().list().expect("list").len(), 1);
    cleanup(&path);
}

#[test]
fn http_auth_secret_is_stored_outside_the_config_and_rotated() {
    let (path, db) = temp_db("http-auth");
    let credentials = MemoryCredentialStore::new();
    let url = "https://mcp.example.com/mcp".to_string();

    let mut first = save_input("mcp_http", "http", true);
    first.url = Some(url.clone());
    first.http_auth_headers = Some(vec![
        auth_header("Authorization", Some("Bearer first-secret")),
        auth_header("X-API-Key", Some("second-secret")),
    ]);
    let saved = save(&db, &credentials, first).expect("save authenticated HTTP row");
    assert_eq!(
        saved
            .http_auth_headers
            .iter()
            .map(|header| header.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Authorization", "X-API-Key"]
    );
    assert!(
        !serde_json::to_string(&saved)
            .expect("serialize config")
            .contains("first-secret"),
        "the config model must never carry the secret value"
    );

    let first_reference =
        CredentialRef::parse(&saved.http_auth_headers[0].credential_ref).expect("reference");
    let second_reference =
        CredentialRef::parse(&saved.http_auth_headers[1].credential_ref).expect("reference");
    assert_eq!(
        credentials
            .get(&first_reference)
            .expect("stored secret")
            .as_utf8()
            .expect("UTF-8 secret"),
        "Bearer first-secret"
    );
    assert_eq!(
        credentials
            .get(&second_reference)
            .expect("stored second secret")
            .as_utf8()
            .expect("UTF-8 secret"),
        "second-secret"
    );

    let mut preserved = save_input("mcp_http", "http", true);
    preserved.label = "renamed".to_string();
    preserved.url = Some(url.clone());
    preserved.http_auth_headers = Some(vec![
        auth_header("X-API-Key", None),
        auth_header("Authorization", None),
    ]);
    let preserved = save(&db, &credentials, preserved).expect("preserve existing secret");
    assert_eq!(
        preserved.http_auth_headers[0].credential_ref, saved.http_auth_headers[1].credential_ref,
        "headers are matched by name even when their order changes"
    );
    assert_eq!(
        preserved.http_auth_headers[1].credential_ref,
        saved.http_auth_headers[0].credential_ref
    );
    assert!(credentials.has(&first_reference).expect("still present"));
    assert!(credentials.has(&second_reference).expect("still present"));

    let mut changed_without_secret = save_input("mcp_http", "http", true);
    changed_without_secret.url = Some(url.clone());
    changed_without_secret.http_auth_headers = Some(vec![auth_header("X-Other-Key", None)]);
    let error = save(&db, &credentials, changed_without_secret)
        .expect_err("a changed header cannot silently reuse the old secret");
    assert!(error.contains("没有新 secret"), "{error}");
    assert_eq!(
        db.mcp_servers()
            .get("mcp_http")
            .expect("unchanged row")
            .http_auth_headers
            .len(),
        2
    );

    let mut rotated = save_input("mcp_http", "http", true);
    rotated.url = Some(url.clone());
    rotated.http_auth_headers = Some(vec![
        auth_header("Authorization", Some("rotated-secret")),
        auth_header("X-API-Key", None),
    ]);
    let rotated = save(&db, &credentials, rotated).expect("rotate credential");
    let rotated_reference = CredentialRef::parse(&rotated.http_auth_headers[0].credential_ref)
        .expect("rotated reference");
    assert_ne!(rotated_reference, first_reference);
    assert_eq!(
        rotated.http_auth_headers[1].credential_ref,
        second_reference.to_string_ref()
    );
    assert!(
        !credentials
            .has(&first_reference)
            .expect("old reference lookup"),
        "the replaced secret must be reclaimed"
    );
    assert_eq!(
        credentials
            .get(&rotated_reference)
            .expect("rotated secret")
            .as_utf8()
            .expect("UTF-8 secret"),
        "rotated-secret"
    );
    assert!(credentials.has(&second_reference).expect("preserved"));

    let mut cleared = save_input("mcp_http", "http", true);
    cleared.url = Some(url);
    let cleared = save(&db, &credentials, cleared).expect("clear authentication");
    assert!(cleared.http_auth_headers.is_empty());
    assert!(
        !credentials
            .has(&rotated_reference)
            .expect("rotated reference lookup"),
        "clearing the headers must reclaim rotated credentials"
    );
    assert!(
        !credentials
            .has(&second_reference)
            .expect("preserved reference lookup"),
        "clearing the headers must reclaim every credential"
    );
    cleanup(&path);
}

/// A form payload for one OAuth row, with the client-authentication choice explicit.
fn oauth_save_input(
    client_auth: Option<McpOAuthClientAuth>,
    client_secret: Option<&str>,
    scopes: &[&str],
) -> McpServerSaveInput {
    let mut input = save_input("mcp_oauth", "http", true);
    input.url = Some("https://mcp.example.com/mcp".to_string());
    input.oauth = Some(McpOAuthInput {
        client_auth,
        client_secret: client_secret.map(str::to_string),
        ..oauth_input("https://auth.example.com", "confidential-client", scopes)
    });
    input
}

#[test]
fn oauth_client_secret_is_write_only_preserved_rotated_and_reclaimed() {
    let (path, db) = temp_db("oauth-client-secret");
    let credentials = MemoryCredentialStore::new();

    // 1. Configure `client_secret_post` with a secret.
    let saved = save(
        &db,
        &credentials,
        oauth_save_input(
            Some(McpOAuthClientAuth::ClientSecretPost),
            Some("first-secret"),
            &["mcp.read"],
        ),
    )
    .expect("save the client secret");
    let oauth = saved.oauth.as_ref().expect("OAuth config");
    assert_eq!(oauth.client_auth, McpOAuthClientAuth::ClientSecretPost);
    let first = CredentialRef::parse(oauth.client_secret_ref.as_deref().expect("secret ref"))
        .expect("credential reference");
    assert_eq!(
        credentials
            .get(&first)
            .expect("stored secret")
            .as_utf8()
            .expect("UTF-8 secret"),
        "first-secret"
    );

    // The settings response carries the reference and nothing else: the secret has no
    // field to travel in, and no JSON rendering may contain it.
    let rendered = serde_json::to_string(&saved).expect("serialize the stored row");
    assert!(!rendered.contains("first-secret"), "{rendered}");
    assert!(rendered.contains("clientSecretRef"), "{rendered}");
    let debugged = format!(
        "{:?}",
        oauth_save_input(
            Some(McpOAuthClientAuth::ClientSecretPost),
            Some("first-secret"),
            &["mcp.read"],
        )
    );
    assert!(!debugged.contains("first-secret"), "{debugged}");
    assert!(debugged.contains("<redacted>"), "{debugged}");

    // 2. An edit that carries the method but no new secret keeps the stored one.
    let preserved = save(
        &db,
        &credentials,
        oauth_save_input(
            Some(McpOAuthClientAuth::ClientSecretPost),
            None,
            &["mcp.read"],
        ),
    )
    .expect("preserve the client secret");
    assert_eq!(
        preserved
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.client_secret_ref.as_deref()),
        Some(first.to_string_ref().as_str())
    );
    assert!(credentials.has(&first).expect("still stored"));

    // 3. Switching the method without entering a secret is refused: the old secret is
    //    not carried across methods, and it is not silently reused either.
    let error = save(
        &db,
        &credentials,
        oauth_save_input(
            Some(McpOAuthClientAuth::ClientSecretBasic),
            None,
            &["mcp.read"],
        ),
    )
    .expect_err("a method switch needs the secret again");
    assert!(error.contains("client secret"), "{error}");
    assert!(
        credentials.has(&first).expect("lookup"),
        "a refused save must not have touched the stored secret"
    );

    // 4. Switching the method *with* a secret rotates it and reclaims the old value.
    let switched = save(
        &db,
        &credentials,
        oauth_save_input(
            Some(McpOAuthClientAuth::ClientSecretBasic),
            Some("second-secret"),
            &["mcp.read"],
        ),
    )
    .expect("switch to client_secret_basic");
    let second = CredentialRef::parse(
        switched
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.client_secret_ref.as_deref())
            .expect("new secret reference"),
    )
    .expect("credential reference");
    assert_ne!(second, first);
    assert!(
        !credentials.has(&first).expect("old secret lookup"),
        "the replaced secret must be reclaimed"
    );
    assert_eq!(
        credentials
            .get(&second)
            .expect("rotated secret")
            .as_utf8()
            .expect("UTF-8 secret"),
        "second-secret"
    );

    // 5. The stored row survives a reopen with only a reference, and the database files
    //    contain neither secret.
    drop(db);
    let reopened = Database::open(&path).expect("reopen");
    let row = reopened.mcp_servers().get("mcp_oauth").expect("stored row");
    let oauth = row.oauth.as_ref().expect("OAuth config");
    assert_eq!(oauth.client_auth, McpOAuthClientAuth::ClientSecretBasic);
    assert_eq!(
        oauth.client_secret_ref.as_deref(),
        Some(second.to_string_ref().as_str())
    );
    for suffix in ["", "-wal", "-shm"] {
        let file = PathBuf::from(format!("{}{suffix}", path.display()));
        let Ok(bytes) = std::fs::read(&file) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("first-secret") && !text.contains("second-secret"),
            "{} must not contain a client secret",
            file.display()
        );
    }

    // 6. Going back to a public client reclaims the secret as well.
    let public = save(
        &reopened,
        &credentials,
        oauth_save_input(None, None, &["mcp.read"]),
    )
    .expect("switch back to a public client");
    let oauth = public.oauth.as_ref().expect("OAuth config");
    assert_eq!(oauth.client_auth, McpOAuthClientAuth::None);
    assert!(oauth.client_secret_ref.is_none());
    assert!(
        !credentials.has(&second).expect("secret lookup"),
        "a public client must not leave an orphaned secret behind"
    );
    cleanup(&path);
}

#[test]
fn a_client_secret_method_needs_a_client_id_and_a_bounded_secret() {
    let (path, db) = temp_db("oauth-client-secret-bounds");
    let credentials = MemoryCredentialStore::new();

    // A dynamically registered public client cannot be turned confidential afterwards.
    let mut anonymous = oauth_save_input(
        Some(McpOAuthClientAuth::ClientSecretPost),
        Some("s3cret"),
        &["mcp.read"],
    );
    anonymous.oauth = Some(McpOAuthInput {
        client_auth: Some(McpOAuthClientAuth::ClientSecretPost),
        client_secret: Some("s3cret".to_string()),
        ..oauth_input("https://auth.example.com", "", &["mcp.read"])
    });
    let error = save(&db, &credentials, anonymous).expect_err("must refuse");
    assert!(error.contains("client id"), "{error}");

    // RFC 7617 splits the header on the first colon, so a colon in the client id would
    // silently change the credentials that are sent.
    let mut colon = oauth_save_input(
        Some(McpOAuthClientAuth::ClientSecretBasic),
        Some("s3cret"),
        &["mcp.read"],
    );
    colon.oauth = Some(McpOAuthInput {
        client_auth: Some(McpOAuthClientAuth::ClientSecretBasic),
        client_secret: Some("s3cret".to_string()),
        ..oauth_input("https://auth.example.com", "cli:ent", &["mcp.read"])
    });
    let error = save(&db, &credentials, colon).expect_err("must refuse");
    assert!(error.contains("冒号"), "{error}");

    let oversized = oauth_save_input(
        Some(McpOAuthClientAuth::ClientSecretPost),
        Some(&"x".repeat(8_193)),
        &["mcp.read"],
    );
    let error = save(&db, &credentials, oversized).expect_err("must refuse");
    assert!(error.contains("8192"), "{error}");

    assert!(
        db.mcp_servers().get("mcp_oauth").is_err(),
        "no refused save may leave a row behind"
    );
    cleanup(&path);
}

#[test]
fn oauth_accepts_an_empty_client_id_for_dynamic_registration() {
    let (path, db) = temp_db("oauth-dynamic-client");
    let credentials = MemoryCredentialStore::new();
    let mut input = save_input("mcp_oauth_dynamic", "http", true);
    input.url = Some("https://mcp.example.com/mcp".to_string());
    input.oauth = Some(oauth_input("", "", &["mcp.read"]));

    let saved = save(&db, &credentials, input).expect("save deferred registration");
    let oauth = saved.oauth.as_ref().expect("OAuth config");
    assert!(oauth.client_id.is_empty());
    assert!(oauth.token_endpoint.is_none());
    assert!(oauth.credential_ref.is_none());

    let unavailable = http_auth_config_error(&saved).expect("not connected yet");
    assert!(unavailable.to_string().contains("run Connect OAuth"));
    cleanup(&path);
}

#[test]
fn toggling_dpop_is_an_identity_change_and_reclaims_the_old_key() {
    let (path, db) = temp_db("dpop-config");
    let credentials = MemoryCredentialStore::new();

    // 连上过一次的 DPoP 服务器：密钥与令牌都在凭据库里。
    let mut first = save_input("mcp_oauth", "http", true);
    first.url = Some("https://mcp.example.com/mcp".to_string());
    first.oauth = Some(McpOAuthInput {
        dpop: true,
        ..oauth_input("https://auth.example.com", "desktop-client", &["mcp.read"])
    });
    let saved = save(&db, &credentials, first).expect("save the DPoP configuration");
    let key_reference = credentials
        .set(
            "mcp",
            "dpop-key",
            &Secret::from_utf8("pkcs8-placeholder".to_string()),
        )
        .expect("store the key");
    let token_reference = credentials
        .set("mcp", "dpop-token", &Secret::from_utf8("{}".to_string()))
        .expect("store the token");
    let mut connected = saved;
    {
        let oauth = connected.oauth.as_mut().expect("OAuth config");
        oauth.dpop_key_ref = Some(key_reference.to_string_ref());
        oauth.token_endpoint = Some("https://auth.example.com/token".to_string());
        oauth.credential_ref = Some(token_reference.to_string_ref());
    }
    db.mcp_servers().upsert(&connected).expect("connect row");

    // 同一个身份再存一次：密钥引用原样保留，令牌也留着。
    let mut same = save_input("mcp_oauth", "http", true);
    same.url = Some("https://mcp.example.com/mcp".to_string());
    same.oauth = Some(McpOAuthInput {
        dpop: true,
        ..oauth_input("https://auth.example.com", "desktop-client", &["mcp.read"])
    });
    let preserved = save(&db, &credentials, same).expect("save the same identity");
    let oauth = preserved.oauth.as_ref().expect("OAuth config");
    assert_eq!(
        oauth.dpop_key_ref.as_deref(),
        Some(key_reference.to_string_ref().as_str())
    );
    assert_eq!(
        oauth.credential_ref.as_deref(),
        Some(token_reference.to_string_ref().as_str())
    );

    // 关掉 DPoP ＝ 换身份：令牌作废，旧的私钥也回收（它已经没有对手了）。
    let mut off = save_input("mcp_oauth", "http", true);
    off.url = Some("https://mcp.example.com/mcp".to_string());
    off.oauth = Some(oauth_input(
        "https://auth.example.com",
        "desktop-client",
        &["mcp.read"],
    ));
    let changed = save(&db, &credentials, off).expect("turn DPoP off");
    let oauth = changed.oauth.as_ref().expect("OAuth config");
    assert!(!oauth.dpop);
    assert!(oauth.dpop_key_ref.is_none());
    assert!(oauth.credential_ref.is_none());
    assert!(
        !credentials.has(&key_reference).expect("key lookup"),
        "an orphaned private key helps nobody"
    );
    cleanup(&path);
}

#[test]
fn oauth_configuration_keeps_tokens_only_for_the_same_identity() {
    let (path, db) = temp_db("oauth-config");
    let credentials = MemoryCredentialStore::new();
    let mut first = save_input("mcp_oauth", "http", true);
    first.url = Some("https://mcp.example.com/mcp".to_string());
    first.oauth = Some(oauth_input(
        "https://auth.example.com",
        "desktop-client",
        &["mcp.read"],
    ));
    let saved = save(&db, &credentials, first).expect("save OAuth configuration");
    let oauth = saved.oauth.as_ref().expect("OAuth config");
    assert_eq!(oauth.issuer, "https://auth.example.com");
    assert_eq!(oauth.client_id, "desktop-client");
    assert_eq!(oauth.flow, McpOAuthFlow::AuthorizationCode);
    assert_eq!(oauth.scopes, vec!["mcp.read"]);
    assert!(oauth.token_endpoint.is_none());
    assert!(oauth.credential_ref.is_none());

    let reference = credentials
        .set(
            "mcp",
            "oauth-token",
            &Secret::from_utf8("{\"access_token\":\"secret\"}"),
        )
        .expect("store token");
    let mut connected = saved;
    let oauth = connected.oauth.as_mut().expect("OAuth config");
    oauth.token_endpoint = Some("https://auth.example.com/token".to_string());
    oauth.credential_ref = Some(reference.to_string_ref());
    db.mcp_servers().upsert(&connected).expect("connect row");

    let mut preserved = save_input("mcp_oauth", "http", true);
    preserved.url = Some("https://mcp.example.com/mcp".to_string());
    preserved.oauth = Some(oauth_input(
        "https://auth.example.com/",
        "desktop-client",
        &["mcp.read"],
    ));
    let preserved = save(&db, &credentials, preserved).expect("preserve OAuth token");
    assert_eq!(
        preserved
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.credential_ref.as_deref()),
        Some(reference.to_string_ref().as_str())
    );
    assert!(credentials.has(&reference).expect("still stored"));

    let mut changed = save_input("mcp_oauth", "http", true);
    changed.url = Some("https://mcp.example.com/mcp".to_string());
    changed.oauth = Some(oauth_input(
        "https://auth.example.com",
        "desktop-client",
        &["mcp.write"],
    ));
    let changed = save(&db, &credentials, changed).expect("change OAuth identity");
    let oauth = changed.oauth.as_ref().expect("OAuth config");
    assert!(oauth.token_endpoint.is_none());
    assert!(oauth.credential_ref.is_none());
    assert!(
        !credentials.has(&reference).expect("old token lookup"),
        "changing the OAuth identity must reclaim the old token"
    );

    // Switching the flow is an identity change, not a display preference: the browser
    // flow's token was issued to a request the device flow would not have made.
    let flow_reference = credentials
        .set(
            "mcp",
            "oauth-token-flow",
            &Secret::from_utf8("{\"access_token\":\"secret\"}"),
        )
        .expect("store token for the flow switch");
    let mut reconnected = save_input("mcp_oauth", "http", true);
    reconnected.url = Some("https://mcp.example.com/mcp".to_string());
    reconnected.oauth = Some(oauth_input(
        "https://auth.example.com",
        "desktop-client",
        &["mcp.read"],
    ));
    let mut reconnected = save(&db, &credentials, reconnected).expect("restore the identity");
    let oauth = reconnected.oauth.as_mut().expect("OAuth config");
    oauth.token_endpoint = Some("https://auth.example.com/token".to_string());
    oauth.credential_ref = Some(flow_reference.to_string_ref());
    db.mcp_servers()
        .upsert(&reconnected)
        .expect("connect row again");

    let mut switched = save_input("mcp_oauth", "http", true);
    switched.url = Some("https://mcp.example.com/mcp".to_string());
    switched.oauth = Some(McpOAuthInput {
        flow: Some(McpOAuthFlow::DeviceCode),
        ..oauth_input("https://auth.example.com", "desktop-client", &["mcp.read"])
    });
    let switched = save(&db, &credentials, switched).expect("switch to the device code flow");
    let oauth = switched.oauth.as_ref().expect("OAuth config");
    assert_eq!(oauth.flow, McpOAuthFlow::DeviceCode);
    assert!(oauth.token_endpoint.is_none());
    assert!(oauth.credential_ref.is_none());
    assert!(
        !credentials.has(&flow_reference).expect("flow token lookup"),
        "switching the flow must reclaim the token the other flow obtained"
    );

    let mut mixed = save_input("mcp_oauth", "http", true);
    mixed.url = Some("https://mcp.example.com/mcp".to_string());
    mixed.http_auth_headers = Some(vec![auth_header("Authorization", Some("Bearer static"))]);
    mixed.oauth = Some(oauth_input(
        "https://auth.example.com",
        "desktop-client",
        &[],
    ));
    assert!(save(&db, &credentials, mixed).is_err());
    cleanup(&path);
}

#[tokio::test]
async fn deleting_a_server_reclaims_its_oauth_token() {
    let (path, db) = temp_db("oauth-delete");
    let credentials = MemoryCredentialStore::new();
    let mut input = save_input("mcp_oauth", "http", true);
    input.url = Some("https://mcp.example.com/mcp".to_string());
    input.oauth = Some(oauth_input(
        "https://auth.example.com",
        "desktop-client",
        &[],
    ));
    let mut saved = save(&db, &credentials, input).expect("save OAuth row");
    let reference = credentials
        .set("mcp", "oauth-delete-token", &Secret::from_utf8("token"))
        .expect("store token");
    let oauth = saved.oauth.as_mut().expect("OAuth config");
    oauth.token_endpoint = Some("https://auth.example.com/token".to_string());
    oauth.credential_ref = Some(reference.to_string_ref());
    db.mcp_servers().upsert(&saved).expect("update row");

    let supervisor = McpSupervisor::new();
    delete_server(&db, &supervisor, &credentials, "mcp_oauth")
        .await
        .expect("delete");
    assert!(!credentials.has(&reference).expect("token lookup"));
    cleanup(&path);
}

#[tokio::test]
async fn deleting_a_server_reclaims_its_http_credential() {
    let (path, db) = temp_db("http-auth-delete");
    let credentials = MemoryCredentialStore::new();
    let mut input = save_input("mcp_http", "http", true);
    input.url = Some("https://mcp.example.com/mcp".to_string());
    input.http_auth_headers = Some(vec![
        auth_header("Authorization", Some("Bearer delete-me")),
        auth_header("X-API-Key", Some("delete-me-too")),
    ]);
    let saved = save(&db, &credentials, input).expect("save authenticated HTTP row");
    let references = saved
        .http_auth_headers
        .iter()
        .map(|header| CredentialRef::parse(&header.credential_ref).expect("reference"))
        .collect::<Vec<_>>();

    let supervisor = McpSupervisor::new();
    let deleted = delete_server(&db, &supervisor, &credentials, "mcp_http")
        .await
        .expect("delete");
    assert!(deleted.deleted);
    assert!(!deleted.stopped);
    assert!(matches!(
        db.mcp_servers().get("mcp_http"),
        Err(DatabaseError::NotFound)
    ));
    for reference in references {
        assert!(
            !credentials.has(&reference).expect("credential lookup"),
            "deleting the server must reclaim every secret"
        );
    }
    cleanup(&path);
}

#[test]
fn an_unknown_transport_and_an_impossible_id_are_refused() {
    let (path, db) = temp_db("transport-refused");
    let credentials = MemoryCredentialStore::new();
    let error =
        save(&db, &credentials, save_input("mcp_sse", "sse", true)).expect_err("must refuse");
    assert!(error.contains("unsupported transport"), "{error}");

    // `mcp_1` 规范化成 `mcp-1`，合法；`mcp..1` 永远变不成名字段。
    let error =
        save(&db, &credentials, save_input("mcp..1", "stdio", true)).expect_err("must refuse");
    assert!(error.contains("mcp..1"), "{error}");
    assert!(save(&db, &credentials, save_input("mcp_1", "stdio", true)).is_ok());
    cleanup(&path);
}

#[test]
fn a_draft_row_is_saveable_but_a_blank_id_or_label_is_not() {
    let (path, db) = temp_db("draft");
    let credentials = MemoryCredentialStore::new();
    // 草稿：禁用 + 没有 command。可以保存。
    let mut draft = save_input("mcp_draft", "stdio", false);
    draft.command = None;
    assert!(
        save(&db, &credentials, draft).is_ok(),
        "a draft is a legitimate row"
    );

    let mut blank = save_input("", "stdio", true);
    blank.label = "x".into();
    assert!(save(&db, &credentials, blank).is_err());

    let mut unlabelled = save_input("mcp_1", "stdio", true);
    unlabelled.label = "   ".into();
    assert!(save(&db, &credentials, unlabelled).is_err());
    cleanup(&path);
}

#[tokio::test]
async fn a_missing_row_is_reported_as_such() {
    let (path, db) = temp_db("missing");
    let error = load(&db, "mcp_missing").expect_err("must report");
    assert!(error.contains("mcp_missing"), "{error}");
    assert!(error.contains("刷新"), "要说下一步：{error}");
    cleanup(&path);
}

/* ── 表里的无效 HTTP 行 ───────────────────────────────────────────────── */

/// 表里有一行绕过保存校验的远程明文 HTTP 时，目录与视图都必须说出来，且不能发起网络请求。
#[tokio::test]
async fn an_invalid_http_row_in_the_table_refuses_with_the_same_reason() {
    let (path, db) = temp_db("http-row");
    let mut http = fixture_row("mcp_http", "ok", true);
    http.transport = "http".to_string();
    http.command = None;
    http.args = None;
    http.url = Some("http://mcp.example.com/mcp".to_string());
    insert(&db, &http);

    let supervisor = McpSupervisor::new();
    let credentials = MemoryCredentialStore::new();
    let response = catalog(&db, &supervisor, Arc::new(credentials))
        .await
        .expect("catalog never fails");

    assert!(
        response.servers.is_empty(),
        "an invalid HTTP endpoint must not appear as usable"
    );
    assert_eq!(response.failures.len(), 1, "{:?}", response.failures);
    let failure = &response.failures[0];
    assert_eq!(failure.code, McpFailureCode::InvalidConfig);
    assert_eq!(failure.server_id, "mcp_http");
    assert!(
        failure.message.contains("HTTPS"),
        "the reason must say what endpoint rule was broken: {}",
        failure.message
    );

    // 视图（列表用的那个函数）说同一件事。
    let view = view(&supervisor, http).await;
    assert!(!view.status.running);
    let unavailable = view.unavailable.expect("a reason must be present");
    assert_eq!(unavailable.code, McpFailureCode::InvalidConfig);
    assert!(supervisor.servers().await.is_empty());
    cleanup(&path);
}

/// `tools/call` 的结果被**搬运**，不被解释：工具自己报错时 `isError` 原样带出去，宿主仍然
/// 回 `success`（翻译成哪一类失败是适配器的事）。
#[tokio::test]
async fn an_mcp_tool_name_is_routed_to_its_server_and_errors_are_not_translated_here() {
    let Some((path, db, supervisor)) = fixture_setup("execute", "ok", "mcp_1") else {
        return;
    };
    let credentials = MemoryCredentialStore::new();
    catalog(&db, &supervisor, Arc::new(credentials))
        .await
        .expect("catalog");
    let cancel = CancellationToken::new();

    let ok = execute(
        &supervisor,
        "mcp.mcp-1.echo",
        &json!({ "text": "hello" }),
        &cancel,
    )
    .await
    .expect("host response");
    assert_eq!(ok["status"], json!("success"));
    assert_eq!(ok["output"]["serverId"], json!("mcp_1"));
    assert_eq!(ok["output"]["tool"], json!("echo"));
    assert_eq!(ok["output"]["isError"], json!(false));
    assert_eq!(ok["output"]["text"], json!("echo: hello"));

    let reported = execute(&supervisor, "mcp.mcp-1.explode", &json!({}), &cancel)
        .await
        .expect("host response");
    assert_eq!(
        reported["status"],
        json!("success"),
        "a tool-level error is a successful call that reports isError"
    );
    assert_eq!(reported["output"]["isError"], json!(true));

    // 名字本身先说清楚是哪一种不合法。
    let malformed = execute(&supervisor, "mcp.mcp-1", &json!({}), &cancel)
        .await
        .expect("host response");
    assert_eq!(malformed["status"], json!("failed"));
    assert_eq!(malformed["error"]["code"], json!("not_found"));

    let unknown_tool = execute(&supervisor, "mcp.mcp-1.nope", &json!({}), &cancel)
        .await
        .expect("host response");
    assert_eq!(unknown_tool["status"], json!("failed"));
    assert_eq!(unknown_tool["error"]["code"], json!("not_found"));

    supervisor.shutdown("mcp_1").await;
    cleanup(&path);
}

/// A crash is reported, then recovered with a fresh process. The interrupted call is not
/// replayed, and the exit record remains visible after recovery.
#[tokio::test]
async fn a_crashed_server_is_recovered_without_hiding_its_exit_code() {
    let Some((path, db, supervisor)) = fixture_setup("crash", "misbehave", "mcp_1") else {
        return;
    };
    let cancel = CancellationToken::new();
    let credentials = MemoryCredentialStore::new();
    let first = catalog(&db, &supervisor, Arc::new(credentials))
        .await
        .expect("catalog");
    assert_eq!(first.servers.len(), 1);
    let killed_pid = supervisor
        .handle("mcp_1")
        .await
        .expect("tracked")
        .info()
        .pid
        .expect("stdio pid");

    // `quit` 不回任何东西就退 7：这一次调用以「进程没了」结束。
    let crashed = execute(&supervisor, "mcp.mcp-1.quit", &json!({}), &cancel)
        .await
        .expect("host response");
    assert_eq!(crashed["status"], json!("failed"));
    assert_eq!(crashed["error"]["code"], json!("transport"));
    assert_eq!(
        crashed["error"]["retryable"],
        json!(false),
        "一个死掉的进程不会自己回来，所以重试没有意义"
    );
    assert_eq!(crashed["error"]["detail"]["exitCode"], json!(7));
    assert_eq!(crashed["error"]["detail"]["restarted"], json!(false));

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let status = loop {
        let status = supervisor.status("mcp_1").await;
        if status.running && status.pid != Some(killed_pid) {
            break status;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the crash must be recovered within the published budget"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(
        status.last_exit.as_ref().and_then(|exit| exit.code),
        Some(7),
        "recovery must keep the crash visible"
    );
    assert_eq!(
        status.restart.as_ref().map(|restart| restart.attempt),
        Some(1)
    );
    assert!(!status.restart.as_ref().expect("restart record").exhausted);

    let recovered = execute(
        &supervisor,
        "mcp.mcp-1.echo",
        &json!({ "text": "after recovery" }),
        &cancel,
    )
    .await
    .expect("host response");
    assert_eq!(recovered["status"], json!("success"));
    assert_eq!(recovered["output"]["text"], json!("echo: after recovery"));

    supervisor.shutdown("mcp_1").await;
    cleanup(&path);
}

/// 取消：宿主不再等，而且如实说这是取消（不是工具的失败）。
#[tokio::test]
async fn a_cancelled_call_stops_waiting_and_says_so() {
    let Some((path, db, supervisor)) = fixture_setup("cancel", "misbehave", "mcp_1") else {
        return;
    };
    let credentials = MemoryCredentialStore::new();
    catalog(&db, &supervisor, Arc::new(credentials))
        .await
        .expect("catalog");

    let cancel = CancellationToken::new();
    cancel.cancel();
    let cancelled = execute(&supervisor, "mcp.mcp-1.never-answer", &json!({}), &cancel)
        .await
        .expect("host response");
    assert_eq!(cancelled["status"], json!("failed"));
    assert_eq!(cancelled["error"]["code"], json!("cancelled"));

    supervisor.shutdown("mcp_1").await;
    cleanup(&path);
}

/// 一个从来没启动过的段（比如用户把它停了）：执行路径给出的是一句能读懂的话，而不是一个
/// `not_found` 了事。
#[tokio::test]
async fn an_unknown_server_segment_explains_itself() {
    let supervisor = McpSupervisor::new();
    let cancel = CancellationToken::new();
    let response = execute(&supervisor, "mcp.mcp-9.echo", &json!({}), &cancel)
        .await
        .expect("host response");
    assert_eq!(response["status"], json!("failed"));
    assert_eq!(response["error"]["code"], json!("not_found"));
    let message = response["error"]["message"].as_str().expect("a message");
    assert!(message.contains("mcp-9"), "{message}");
    assert!(
        message.contains("automatic recovery"),
        "要说明自动恢复的状态：{message}"
    );
}

/// 目录的预算：一个什么都不答、最后自己退 9 的服务进程不能让目录请求挂住（它是 sidecar
/// 握手期间唯一会问的东西）。
/// 时它们失败（一个绿色的 CI 不能意味着「这些测试从来没跑过」）。
#[test]
fn the_node_gate_matches_the_one_the_core_integration_tests_use() {
    if node_path().is_none() {
        assert!(node_or_skip().is_none());
    } else {
        assert!(node_or_skip().is_some());
    }
}
