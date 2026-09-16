//! OAuth 2.1 for Streamable HTTP MCP servers, in both shapes MCP allows: the
//! authorization-code + PKCE redirect and the RFC 8628 device code.
//!
//! The desktop host owns browser launch, loopback callback handling, discovery,
//! device-code polling and credential persistence. The MCP core only sees a small
//! token source.
//!
//! The two flows differ only in how the user proves they are present. They share
//! discovery, dynamic client registration, the token-endpoint parse and the tail that
//! stores the bundle, so a refresh issued later cannot depend on which one ran.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rand::Rng as _;
use reqwest::{Client, RequestBuilder, Response, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use yukinal_core::mcp::{
    validate_oauth_url, McpAuthScheme, McpAuthorization, McpAuthorizationRequest, McpOAuthFuture,
    McpOAuthSourceConfig, McpOAuthTokenSource,
};
use yukinal_credentials::{CredentialRef, CredentialStore, Secret};
use yukinal_database::models::{McpOAuthClientAuth, McpOAuthConfig, McpOAuthFlow, McpServerConfig};
use yukinal_database::Database;
use yukinal_net::OutboundProxy;

use super::dpop::{DpopKey, DpopNonces};
use crate::commands::server::next_id;

/// OAuth 的 HTTP 客户端：不跟随重定向，出站按应用级代理设置走（ADR 0022）。
///
/// discovery、授权与 token 请求共用这一个构造点：三处各自 `Client::builder()` 是
/// 「同一个进程里两条请求走了两条路」最容易发生的地方。
fn oauth_client(proxy: &OutboundProxy) -> Result<Client, String> {
    let builder = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10));
    let builder = yukinal_net::apply(builder, &proxy.proxy, proxy.credential.as_ref())?;
    builder
        .build()
        .map_err(|error| format!("could not create OAuth HTTP client: {error}"))
}

fn oauth_transport_error(proxy: &OutboundProxy, error: reqwest::Error) -> String {
    sanitize(&yukinal_net::route_context(
        &proxy.proxy,
        &error.without_url().to_string(),
    ))
}

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);
const TOKEN_TIMEOUT: Duration = Duration::from_secs(30);
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CALLBACK_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_METADATA_BYTES: usize = 256 * 1024;
const MAX_REGISTRATION_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_TOKEN_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_CALLBACK_BYTES: usize = 16 * 1024;
const MAX_CALLBACK_REQUESTS: usize = 32;
const TOKEN_EXPIRY_SKEW_SECS: u64 = 60;
const DYNAMIC_CLIENT_NAME: &str = "Yukinal Desktop";
/// RFC 9449 §4.2: the request header carrying one proof.
const DPOP_HEADER: &str = "DPoP";
/// RFC 9449 §8: the header a server uses to hand out its current nonce.
const DPOP_NONCE_HEADER: &str = "DPoP-Nonce";

/// RFC 8628's default when the authorization server omits `interval`.
const DEVICE_DEFAULT_POLL_SECONDS: u64 = 5;
/// The floor that keeps a server-supplied `interval` of 0 from becoming a hot loop.
/// An authorization server asking for "no wait" is asking for ad-hoc polling; it is
/// still not an invitation to spin.
const DEVICE_MIN_POLL: Duration = Duration::from_millis(250);
/// Ceiling for a server-supplied `interval`. Beyond this the code would still be valid
/// while the user waits on a poll that may never come.
const DEVICE_MAX_POLL: Duration = Duration::from_secs(300);
/// RFC 8628: `slow_down` means "add 5 seconds to your polling interval".
const DEVICE_SLOW_DOWN_STEP: Duration = Duration::from_secs(5);
/// An `expires_in` from the server is clamped to this, so a hostile or broken value
/// cannot make one click wait for hours.
const DEVICE_MAX_LIFETIME_SECS: u64 = 30 * 60;
/// Hard stop for the poll loop. With the 250 ms floor this is over two minutes of
/// polling even for a server that asks for no delay at all.
const MAX_DEVICE_POLLS: usize = 600;
/// The one sentence both flows report a user-requested stop with. The UI knows it asked,
/// so this text is for the error path (a stale dialog, a log, a report), and having one
/// spelling keeps those from disagreeing.
const CANCELLED_FLOW: &str = "OAuth authorization was cancelled";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuthConnectResult {
    pub server_id: String,
    pub issuer: String,
    pub scopes: Vec<String>,
    pub token_endpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OAuthTokenBundle {
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<u64>,
    token_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AuthorizationServerMetadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    registration_endpoint: Option<String>,
    /// RFC 8628 §4. Present only on servers that support the device flow.
    #[serde(default)]
    device_authorization_endpoint: Option<String>,
    #[serde(default)]
    response_types_supported: Vec<String>,
    #[serde(default)]
    code_challenge_methods_supported: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ProtectedResourceMetadata {
    resource: String,
    #[serde(default)]
    authorization_servers: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    // Absent on every error response (RFC 6749 §5.2), and those are exactly the bodies
    // the device-code poll has to read: without this default, `authorization_pending`
    // would fail to parse and be reported as a malformed response.
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// RFC 8628 §3.2. `expires_in` is required by the RFC; `interval` is not.
#[derive(Debug, Deserialize)]
struct DeviceAuthorizationResponse {
    #[serde(default)]
    device_code: String,
    #[serde(default)]
    user_code: String,
    #[serde(default)]
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default)]
    interval: Option<u64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// A device authorization the server accepted, with our bounds already applied.
struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    lifetime: Duration,
    interval: Duration,
}

/// What the desktop shows while the flow waits: the code to type and where to type it.
///
/// Deliberately carries no tokens: this value travels to the UI as an event, and an
/// event is not a credential store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeviceCodePrompt {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    /// Unix seconds, so the UI can stop saying "waiting" when it can no longer be true.
    pub expires_at: u64,
}

/// One poll attempt's outcome.
enum DevicePoll {
    Pending,
    SlowDown,
    Granted(OAuthTokenBundle),
}

/// How one token-endpoint request authenticates the client (RFC 6749 §2.3).
///
/// The secret is read from the credential store for the request that needs it rather than
/// parked in a config structure: a `Debug` of a config must not be able to print it, and
/// the value that never leaves this struct cannot end up in a log by accident.
struct ClientAuthMaterial {
    method: McpOAuthClientAuth,
    client_id: String,
    secret: Option<String>,
}

impl ClientAuthMaterial {
    /// Read the client credentials one stored OAuth configuration needs.
    ///
    /// A public client needs nothing beyond its id. A secret method says so instead of
    /// sending an empty secret when the credential is gone — an empty `client_secret` is
    /// a *different* request, not a simpler one.
    fn resolve(
        credentials: &dyn CredentialStore,
        method: McpOAuthClientAuth,
        client_id: &str,
        client_secret_ref: Option<&str>,
    ) -> Result<Self, String> {
        let secret = if method.needs_secret() {
            let reference = client_secret_ref.ok_or_else(|| {
                "this OAuth client authenticates with a secret, but none is stored; edit the \
                 MCP server and enter it again"
                    .to_string()
            })?;
            let reference = CredentialRef::parse(reference)
                .map_err(|error| format!("invalid OAuth client secret reference: {error}"))?;
            let secret = credentials
                .get(&reference)
                .map_err(|error| format!("could not read the OAuth client secret: {error}"))?
                .as_utf8()
                .map_err(|error| format!("the OAuth client secret is not UTF-8: {error}"))?
                .into_owned();
            if secret.is_empty() {
                return Err(
                    "the stored OAuth client secret is empty; edit the MCP server and enter it \
                     again"
                        .to_string(),
                );
            }
            Some(secret)
        } else {
            None
        };
        Ok(Self {
            method,
            client_id: client_id.to_string(),
            secret,
        })
    }

    /// Apply the client credentials to one outgoing request.
    ///
    /// RFC 6749 §2.3 says a request uses exactly one authentication method, so the body
    /// gets either both parameters (`client_secret_post`) or neither (`client_secret_basic`,
    /// where the credentials travel in the `Authorization` header that reqwest marks
    /// sensitive). Sending a secret twice would be a credential leak with no upside.
    fn apply(
        &self,
        request: RequestBuilder,
        form: &mut HashMap<String, String>,
    ) -> Result<RequestBuilder, String> {
        match self.method {
            McpOAuthClientAuth::None => {
                form.insert("client_id".to_string(), self.client_id.clone());
                Ok(request)
            }
            McpOAuthClientAuth::ClientSecretPost => {
                let secret = self.secret.as_deref().ok_or_else(|| {
                    "the OAuth client secret is missing from this request".to_string()
                })?;
                form.insert("client_id".to_string(), self.client_id.clone());
                form.insert("client_secret".to_string(), secret.to_string());
                Ok(request)
            }
            McpOAuthClientAuth::ClientSecretBasic => {
                let secret = self.secret.as_deref().ok_or_else(|| {
                    "the OAuth client secret is missing from this request".to_string()
                })?;
                Ok(request.basic_auth(self.client_id.clone(), Some(secret.to_string())))
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct DynamicClientRegistrationRequest {
    /// Empty for the device flow: there is no redirect target to register.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    redirect_uris: Vec<String>,
    client_name: String,
    grant_types: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    response_types: Vec<String>,
    token_endpoint_auth_method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DynamicClientRegistrationResponse {
    #[serde(default)]
    client_id: String,
    /// RFC 7591 lets a server hand a secret to a client that registered as public.
    ///
    /// `register_client` refuses such a response instead of adopting the value: this client
    /// asked to be public (a desktop app cannot keep a secret), and "the server volunteered
    /// one" is not a reason to start sending a credential the user never chose. It is parsed
    /// here so that refusal can name what happened rather than reporting a missing field.
    #[serde(default)]
    client_secret: Option<String>,
    #[serde(default)]
    token_endpoint_auth_method: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

struct OAuthTokenSource {
    source: McpOAuthSourceConfig,
    credentials: Arc<dyn CredentialStore>,
    client: Client,
    cache: tokio::sync::Mutex<Option<OAuthTokenBundle>>,
    /// 发送方约束（RFC 9449）用的密钥。`None` 表示这台服务器只要 bearer。
    ///
    /// 在构造时读一次并留在这里：每个请求都要签一个 proof，每次都去凭据库读一遍没有意义；
    /// 读不出来就**构造失败**，而不是等到请求时才发现令牌已经用不了。
    dpop: Option<DpopKey>,
    nonces: DpopNonces,
}

impl OAuthTokenSource {
    fn new(
        source: McpOAuthSourceConfig,
        credentials: Arc<dyn CredentialStore>,
    ) -> Result<Self, String> {
        let endpoint = validate_oauth_url(&source.server_id, &source.token_endpoint)
            .map_err(|error| error.to_string())?;
        if endpoint != source.token_endpoint {
            return Err("the OAuth token endpoint is not in canonical form".to_string());
        }
        ensure_crypto_provider();
        let client = oauth_client(&source.proxy)?;
        let dpop = match source.dpop_key_ref.as_deref() {
            Some(reference) => Some(DpopKey::load(credentials.as_ref(), reference)?),
            None => None,
        };
        Ok(Self {
            source,
            credentials,
            client,
            cache: tokio::sync::Mutex::new(None),
            dpop,
            nonces: DpopNonces::default(),
        })
    }

    async fn token_inner(&self, force_refresh: bool) -> Result<String, String> {
        let mut cache = self.cache.lock().await;
        if !force_refresh {
            if let Some(bundle) = cache.as_ref().filter(|bundle| token_is_fresh(bundle)) {
                return Ok(bundle.access_token.clone());
            }
        }

        let reference = CredentialRef::parse(&self.source.credential_ref)
            .map_err(|error| format!("invalid OAuth token reference: {error}"))?;
        let stored = self
            .credentials
            .get(&reference)
            .map_err(|error| format!("could not read OAuth token bundle: {error}"))?;
        let stored = stored
            .as_utf8()
            .map_err(|error| format!("OAuth token bundle is not UTF-8: {error}"))?;
        let bundle: OAuthTokenBundle = serde_json::from_str(stored.as_ref())
            .map_err(|error| format!("OAuth token bundle is invalid: {error}"))?;

        if !force_refresh && token_is_fresh(&bundle) {
            *cache = Some(bundle.clone());
            return Ok(bundle.access_token);
        }

        let refresh_token = bundle.refresh_token.clone().ok_or_else(|| {
            "the OAuth access token is no longer valid and no refresh token was issued".to_string()
        })?;
        let refreshed = self.refresh(&refresh_token).await?;
        let access_token = refreshed.access_token.clone();
        self.persist(&reference, &refreshed)?;
        *cache = Some(refreshed);
        Ok(access_token)
    }

    async fn refresh(&self, refresh_token: &str) -> Result<OAuthTokenBundle, String> {
        // The refresh path authenticates exactly like the exchange that produced the token:
        // a server that required a secret to get the token requires it to renew it.
        let auth = ClientAuthMaterial::resolve(
            self.credentials.as_ref(),
            self.source.client_auth,
            &self.source.client_id,
            self.source.client_secret_ref.as_deref(),
        )?;
        let mut form = HashMap::new();
        form.insert("grant_type".to_string(), "refresh_token".to_string());
        form.insert("refresh_token".to_string(), refresh_token.to_string());
        form.insert("resource".to_string(), self.source.resource.clone());
        if !self.source.scopes.is_empty() {
            form.insert("scope".to_string(), self.source.scopes.join(" "));
        }
        let mut bundle = request_token(
            &self.client,
            &self.source.proxy,
            &self.source.server_id,
            &self.source.token_endpoint,
            &auth,
            &form,
            None,
            self.dpop_context(),
        )
        .await?;
        if bundle.refresh_token.is_none() {
            bundle.refresh_token = Some(refresh_token.to_string());
        }
        Ok(bundle)
    }

    /// 本次 token 请求要用的 DPoP 材料；服务器只要 bearer 时是 `None`。
    fn dpop_context(&self) -> Option<DpopContext<'_>> {
        self.dpop.as_ref().map(|key| DpopContext {
            key,
            nonces: &self.nonces,
        })
    }

    fn persist(&self, reference: &CredentialRef, bundle: &OAuthTokenBundle) -> Result<(), String> {
        let encoded = serde_json::to_string(bundle)
            .map_err(|error| format!("could not encode OAuth token bundle: {error}"))?;
        self.credentials
            .set(
                reference.service(),
                reference.account(),
                &Secret::from_utf8(encoded),
            )
            .map_err(|error| format!("could not store refreshed OAuth token: {error}"))?;
        Ok(())
    }
}

impl McpOAuthTokenSource for OAuthTokenSource {
    fn authorization<'a>(
        &'a self,
        request: McpAuthorizationRequest,
    ) -> McpOAuthFuture<'a, McpAuthorization> {
        Box::pin(async move {
            let token = self.token_inner(request.force_refresh).await?;
            let Some(key) = self.dpop.as_ref() else {
                return Ok(McpAuthorization {
                    scheme: McpAuthScheme::Bearer,
                    token,
                    proof: None,
                });
            };
            // 服务器刚挑战的 nonce 先记下来：同一 origin 的下一个请求就不必再被挑战一次。
            let nonce = match request.nonce.as_deref() {
                Some(value) => {
                    self.nonces.remember(&request.url, value);
                    Some(value.to_string())
                }
                None => self.nonces.get(&request.url),
            };
            // 带 access token 的请求必须带 `ath`（RFC 9449 §7.1）：没有它，proof 只是
            // 证明「有人拿着这把钥匙」，而不是「这把钥匙配的是这个令牌」。
            let proof = key.proof(
                &request.method,
                &request.url,
                nonce.as_deref(),
                Some(token.as_str()),
            )?;
            Ok(McpAuthorization {
                scheme: McpAuthScheme::Dpop,
                token,
                proof: Some(proof),
            })
        })
    }
}

pub(super) fn source_from_config(
    credentials: Arc<dyn CredentialStore>,
    source: &McpOAuthSourceConfig,
) -> Result<Arc<dyn McpOAuthTokenSource>, String> {
    Ok(Arc::new(OAuthTokenSource::new(
        source.clone(),
        credentials,
    )?))
}

pub(super) async fn connect(
    database: &Database,
    credentials: Arc<dyn CredentialStore>,
    server_id: &str,
    proxy: OutboundProxy,
    open_browser: impl FnOnce(&str) -> Result<(), String>,
    prompt: impl FnOnce(DeviceCodePrompt) -> Result<(), String>,
    cancel: CancellationToken,
) -> Result<McpOAuthConnectResult, String> {
    let mut row = database
        .mcp_servers()
        .get(server_id)
        .map_err(|error| format!("could not load MCP server `{server_id}` for OAuth: {error}"))?;
    if row.transport != "http" {
        return Err("OAuth can only be configured for Streamable HTTP MCP servers".to_string());
    }
    let resource = row
        .url
        .clone()
        .ok_or_else(|| "the MCP endpoint URL must be saved before connecting OAuth".to_string())?;
    let mut oauth = row
        .oauth
        .clone()
        .ok_or_else(|| "save OAuth configuration before connecting".to_string())?;

    let nonces = DpopNonces::default();
    // 开启 DPoP 时先把密钥备好，并**立刻**把引用写回数据库：否则一次失败的授权会留下一把
    // 谁也不知道的私钥（引用只在这个函数里存在过），而开关看起来已经打开了。
    let dpop_key = if oauth.dpop {
        let reference = match oauth.dpop_key_ref.clone() {
            Some(reference) => reference,
            None => {
                let (_, encoded) = DpopKey::generate()?;
                let reference = credentials
                    .set(
                        "mcp",
                        &format!("{server_id}-{}", next_id("dpop")),
                        &Secret::from_utf8(encoded),
                    )
                    .map_err(|error| format!("could not store the DPoP key: {error}"))?;
                oauth.dpop_key_ref = Some(reference.to_string_ref());
                row.oauth = Some(oauth.clone());
                database
                    .mcp_servers()
                    .upsert(&row)
                    .map_err(|error| format!("could not save the DPoP key reference: {error}"))?;
                reference.to_string_ref()
            }
        };
        Some(DpopKey::load(credentials.as_ref(), &reference)?)
    } else {
        None
    };

    ensure_crypto_provider();
    let client = oauth_client(&proxy)?;
    let issuer = if oauth.issuer.trim().is_empty() {
        discover_resource_issuer(&client, &proxy, server_id, &resource).await?
    } else {
        oauth.issuer.clone()
    };
    let metadata = discover(&client, &proxy, server_id, &issuer).await?;

    // The device flow never binds a listener: that is the whole point of it, and it is
    // also why it has to return early instead of falling through to the callback path.
    if oauth.flow == McpOAuthFlow::DeviceCode {
        let bundle = {
            let mut context = FlowContext {
                client: &client,
                proxy: &proxy,
                database,
                credentials: credentials.as_ref(),
                server_id,
                resource: &resource,
                metadata: &metadata,
                row: &mut row,
                oauth: &mut oauth,
                dpop: dpop_key.as_ref(),
                nonces: &nonces,
            };
            device_code_flow(&mut context, open_browser, prompt, &cancel).await?
        };
        return store_connected_bundle(
            database,
            credentials,
            server_id,
            row,
            oauth,
            bundle,
            &metadata,
        )
        .await;
    }

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|error| format!("could not bind OAuth callback listener: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("could not read OAuth callback address: {error}"))?;
    let redirect_uri = format!("http://127.0.0.1:{}/callback", address.port());
    let client_id = {
        let mut context = FlowContext {
            client: &client,
            proxy: &proxy,
            database,
            credentials: credentials.as_ref(),
            server_id,
            resource: &resource,
            metadata: &metadata,
            row: &mut row,
            oauth: &mut oauth,
            dpop: dpop_key.as_ref(),
            nonces: &nonces,
        };
        resolve_client_id(
            &mut context,
            McpOAuthFlow::AuthorizationCode,
            Some(&redirect_uri),
        )
        .await?
    };
    // Resolved before the browser opens: the client id is final by now, and a missing
    // secret must not cost the user a trip through the authorization page before it is
    // reported.
    let auth = ClientAuthMaterial::resolve(
        credentials.as_ref(),
        oauth.client_auth,
        &client_id,
        oauth.client_secret_ref.as_deref(),
    )?;
    let state = random_base64url(32);
    let verifier = random_base64url(64);
    let challenge = base64url(&Sha256::digest(verifier.as_bytes()));

    let mut authorization_url = Url::parse(&metadata.authorization_endpoint)
        .map_err(|error| format!("authorization endpoint is invalid: {error}"))?;
    {
        let mut query = authorization_url.query_pairs_mut();
        query.append_pair("response_type", "code");
        query.append_pair("client_id", &client_id);
        query.append_pair("redirect_uri", &redirect_uri);
        if !oauth.scopes.is_empty() {
            query.append_pair("scope", &oauth.scopes.join(" "));
        }
        query.append_pair("state", &state);
        query.append_pair("code_challenge", &challenge);
        query.append_pair("code_challenge_method", "S256");
        query.append_pair("resource", &resource);
    }

    open_browser(authorization_url.as_str())?;
    let code = wait_for_callback(listener, &state, &cancel).await?;
    if cancel.is_cancelled() {
        return Err(CANCELLED_FLOW.to_string());
    }
    let mut form = HashMap::new();
    form.insert("grant_type".to_string(), "authorization_code".to_string());
    form.insert("code".to_string(), code);
    form.insert("redirect_uri".to_string(), redirect_uri);
    form.insert("code_verifier".to_string(), verifier);
    form.insert("resource".to_string(), resource);
    let bundle = request_token(
        &client,
        &proxy,
        server_id,
        &metadata.token_endpoint,
        &auth,
        &form,
        None,
        dpop_key.as_ref().map(|key| DpopContext {
            key,
            nonces: &nonces,
        }),
    )
    .await?;

    let reference = credentials
        .set(
            "mcp",
            &format!("{server_id}-{}", next_id("oauth")),
            &Secret::from_utf8(
                serde_json::to_string(&bundle)
                    .map_err(|error| format!("could not encode OAuth token bundle: {error}"))?,
            ),
        )
        .map_err(|error| format!("could not store OAuth token bundle: {error}"))?;
    let previous_reference = oauth.credential_ref.clone();
    let mut updated = oauth;
    updated.issuer = metadata.issuer.clone();
    updated.token_endpoint = Some(metadata.token_endpoint.clone());
    updated.credential_ref = Some(reference.to_string_ref());
    row.oauth = Some(updated.clone());
    if let Err(error) = database.mcp_servers().upsert(&row) {
        let _ =
            crate::state::credential_cleanup::reclaim(database, credentials.as_ref(), &reference);
        return Err(format!("could not save the connected OAuth state: {error}"));
    }
    if let Some(previous_reference) = previous_reference {
        if previous_reference != reference.to_string_ref() {
            if let Ok(previous_reference) = CredentialRef::parse(&previous_reference) {
                if let Err(error) = crate::state::credential_cleanup::reclaim(
                    database,
                    credentials.as_ref(),
                    &previous_reference,
                ) {
                    tracing::warn!(
                        server_id,
                        "OAuth token was replaced but the previous token could not be reclaimed immediately: {error}"
                    );
                }
            }
        }
    }

    Ok(McpOAuthConnectResult {
        server_id: server_id.to_string(),
        issuer: updated.issuer,
        scopes: updated.scopes,
        token_endpoint: metadata.token_endpoint,
    })
}

/// What an authorization flow reads and writes while it runs.
///
/// Gathered into one place because the alternative is a ten-argument function whose call
/// site cannot be read: the interesting part of a device-code flow is the *sequence* of
/// requests, and that is what should be visible at the call site.
struct FlowContext<'a> {
    client: &'a Client,
    proxy: &'a OutboundProxy,
    database: &'a Database,
    credentials: &'a dyn CredentialStore,
    server_id: &'a str,
    /// Canonical resource URI sent with every authorization and token request.
    resource: &'a str,
    metadata: &'a AuthorizationServerMetadata,
    /// The row being connected; the flow writes a registered client id and the token
    /// reference back into it.
    row: &'a mut McpServerConfig,
    oauth: &'a mut McpOAuthConfig,
    /// 发送方约束用的密钥；`None` 表示这台服务器只要 bearer（ADR 0018）。
    dpop: Option<&'a DpopKey>,
    nonces: &'a DpopNonces,
}

/// The client id to use, registering a public client when the configuration has none.
///
/// The registration is persisted before the flow continues because it is the one part of
/// an authorization that must survive the attempt: if the user abandons the browser (or
/// the device-code page) and starts again, re-registering would leave the abandoned
/// client id registered at the server with nothing pointing at it.
async fn resolve_client_id(
    context: &mut FlowContext<'_>,
    flow: McpOAuthFlow,
    redirect_uri: Option<&str>,
) -> Result<String, String> {
    if !context.oauth.client_id.trim().is_empty() {
        return Ok(context.oauth.client_id.clone());
    }
    let registration_endpoint = context
        .metadata
        .registration_endpoint
        .as_deref()
        .ok_or_else(|| {
            "OAuth client id is empty and the authorization server does not advertise dynamic \
             client registration"
                .to_string()
        })?;
    let client_id = register_client(
        context.client,
        context.proxy,
        context.server_id,
        registration_endpoint,
        flow,
        redirect_uri,
        &context.oauth.scopes,
    )
    .await?;
    context.oauth.client_id = client_id.clone();
    context.row.oauth = Some(context.oauth.clone());
    context
        .database
        .mcp_servers()
        .upsert(context.row)
        .map_err(|error| {
            format!("could not save the dynamically registered OAuth client id: {error}")
        })?;
    Ok(client_id)
}

/// RFC 8628: ask for a `user_code`, show it, then poll until the user approves.
///
/// Nothing here is spawned. The wait between polls is awaited inside the caller's future,
/// so a dropped request (a closed window, a cancelled `invoke`) stops the polling by
/// construction, and the two ways to end early — the user cancels, or `expires_in` runs
/// out — are both ordinary returns rather than something left running behind them.
async fn device_code_flow(
    context: &mut FlowContext<'_>,
    open_browser: impl FnOnce(&str) -> Result<(), String>,
    prompt: impl FnOnce(DeviceCodePrompt) -> Result<(), String>,
    cancel: &CancellationToken,
) -> Result<OAuthTokenBundle, String> {
    let endpoint = context
        .metadata
        .device_authorization_endpoint
        .clone()
        .ok_or_else(|| {
            "the authorization server metadata has no device_authorization_endpoint, so this \
             server cannot do the device code flow; choose the authorization code flow instead"
                .to_string()
        })?;
    let client_id = resolve_client_id(context, McpOAuthFlow::DeviceCode, None).await?;
    let auth = ClientAuthMaterial::resolve(
        context.credentials,
        context.oauth.client_auth,
        &client_id,
        context.oauth.client_secret_ref.as_deref(),
    )?;

    let scopes = context.oauth.scopes.join(" ");
    let mut authorization_form = HashMap::new();
    authorization_form.insert("resource".to_string(), context.resource.to_string());
    if !scopes.is_empty() {
        authorization_form.insert("scope".to_string(), scopes.clone());
    }
    // 变量名刻意不叫 `authorization`: 秘密扫描把「`authorization =` 后面跟着一长串
    // 标识符」当成一次凭据赋值（`check-secrets.mjs` 的 assigned-credential 规则会跨行匹配），
    // 而这里只是收下一个设备码响应。
    let device = request_device_authorization(
        context.client,
        context.proxy,
        context.server_id,
        &endpoint,
        &auth,
        &authorization_form,
    )
    .await?;
    let deadline = tokio::time::Instant::now() + device.lifetime;

    prompt(DeviceCodePrompt {
        user_code: device.user_code.clone(),
        verification_uri: device.verification_uri.clone(),
        verification_uri_complete: device.verification_uri_complete.clone(),
        expires_at: unix_now().saturating_add(device.lifetime.as_secs()),
    })?;
    // Prefer the URI that already carries the code: it is one click instead of "read this
    // code, then type it in", and the code is still on screen if the page does not prefill.
    let target = device
        .verification_uri_complete
        .as_deref()
        .unwrap_or(&device.verification_uri)
        .to_string();
    open_browser(&target)?;

    let mut poll_form = HashMap::new();
    poll_form.insert("grant_type".to_string(), DEVICE_CODE_GRANT.to_string());
    poll_form.insert("device_code".to_string(), device.device_code.clone());
    poll_form.insert("resource".to_string(), context.resource.to_string());
    if !scopes.is_empty() {
        poll_form.insert("scope".to_string(), scopes);
    }

    let mut interval = device.interval;
    let mut polls = 0_usize;
    loop {
        if polls >= MAX_DEVICE_POLLS {
            return Err(
                "OAuth device authorization is still pending after the polling budget; start \
                 again"
                    .to_string(),
            );
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(
                "OAuth device code expired before it was approved (expired_token)".to_string(),
            );
        }
        tokio::select! {
            _ = cancel.cancelled() => return Err(CANCELLED_FLOW.to_string()),
            // The last wait is shortened to the deadline: the server answers `expired_token`
            // there, which is a better reason to report than our own guess about the clock.
            _ = tokio::time::sleep(interval.min(deadline - now)) => {}
        }
        polls += 1;
        let polled = poll_device_token(
            context.client,
            context.proxy,
            context.server_id,
            &context.metadata.token_endpoint,
            &auth,
            &poll_form,
            context.dpop.map(|key| DpopContext {
                key,
                nonces: context.nonces,
            }),
        )
        .await?;
        match polled {
            DevicePoll::Pending => {}
            DevicePoll::SlowDown => interval = apply_slow_down(interval),
            DevicePoll::Granted(bundle) => {
                // A grant that lands in the same instant as a cancel is the user changing
                // their mind: report the cancel and store nothing, rather than leaving a
                // token behind that the UI has already stopped talking about.
                if cancel.is_cancelled() {
                    return Err(CANCELLED_FLOW.to_string());
                }
                return Ok(bundle);
            }
        }
    }
}

/// Ask for a device authorization (RFC 8628 §3.1/§3.2).
async fn request_device_authorization(
    client: &Client,
    proxy: &OutboundProxy,
    server_id: &str,
    endpoint: &str,
    auth: &ClientAuthMaterial,
    form: &HashMap<String, String>,
) -> Result<DeviceAuthorization, String> {
    let endpoint = validate_oauth_url(server_id, endpoint).map_err(|error| error.to_string())?;
    let mut form = form.clone();
    let request = auth.apply(client.post(endpoint), &mut form)?;
    let mut response = request
        .form(&form)
        .timeout(TOKEN_TIMEOUT)
        .send()
        .await
        .map_err(|error| {
            format!(
                "OAuth device authorization request failed: {}",
                oauth_transport_error(proxy, error)
            )
        })?;
    let status = response.status();
    let body = read_bounded_body(&mut response, MAX_TOKEN_RESPONSE_BYTES).await?;
    let parsed: DeviceAuthorizationResponse = serde_json::from_slice(&body)
        .map_err(|error| format!("OAuth device authorization response is invalid: {error}"))?;
    if !status.is_success() || parsed.error.is_some() {
        let detail = parsed
            .error_description
            .as_deref()
            .or(parsed.error.as_deref())
            .unwrap_or("the authorization server refused the device authorization request");
        return Err(format!(
            "OAuth device authorization returned HTTP {status}: {}",
            sanitize(detail)
        ));
    }
    if parsed.device_code.trim().is_empty() || parsed.user_code.trim().is_empty() {
        return Err(
            "OAuth device authorization response has no device code or user code".to_string(),
        );
    }
    if parsed.user_code.len() > 256 || parsed.user_code.chars().any(char::is_control) {
        return Err("OAuth device authorization returned an unusable user code".to_string());
    }
    let verification_uri = validate_oauth_url(server_id, &parsed.verification_uri)
        .map_err(|error| error.to_string())?;
    let verification_uri_complete = parsed
        .verification_uri_complete
        .as_deref()
        .map(|raw| validate_verification_uri_complete(server_id, raw))
        .transpose()?;
    Ok(DeviceAuthorization {
        device_code: parsed.device_code,
        user_code: parsed.user_code,
        verification_uri,
        verification_uri_complete,
        lifetime: Duration::from_secs(parsed.expires_in.clamp(1, DEVICE_MAX_LIFETIME_SECS)),
        interval: device_poll_interval(parsed.interval),
    })
}

/// One token-endpoint poll (RFC 8628 §3.4/§3.5).
///
/// `authorization_pending` and `slow_down` are *answers*, not failures: they are what the
/// server says while it waits for the user, and turning either into an error would abort
/// a flow that is working exactly as designed.
async fn poll_device_token(
    client: &Client,
    proxy: &OutboundProxy,
    server_id: &str,
    endpoint: &str,
    auth: &ClientAuthMaterial,
    form: &HashMap<String, String>,
    dpop: Option<DpopContext<'_>>,
) -> Result<DevicePoll, String> {
    let endpoint = validate_oauth_url(server_id, endpoint).map_err(|error| error.to_string())?;
    let nonce = dpop.and_then(|context| context.nonces.get(&endpoint));
    let mut attempt =
        post_token_form(client, proxy, &endpoint, auth, form, dpop, nonce.as_deref()).await?;
    // 每次轮询都当一次独立请求：nonce 挑战可以重试一次，和别的 token 请求同一个上限。
    if attempt.status == reqwest::StatusCode::UNAUTHORIZED {
        if let (Some(context), Some(challenge)) = (dpop, attempt.challenge.clone()) {
            if nonce.as_deref() != Some(challenge.as_str()) {
                context.nonces.remember(&endpoint, &challenge);
                attempt =
                    post_token_form(client, proxy, &endpoint, auth, form, dpop, Some(&challenge))
                        .await?;
            }
        }
    }
    if let Some(error) = attempt
        .parsed
        .as_ref()
        .and_then(|parsed| parsed.error.as_deref())
    {
        let description = sanitize(
            attempt
                .parsed
                .as_ref()
                .and_then(|parsed| parsed.error_description.as_deref())
                .unwrap_or("no description"),
        );
        return match error {
            "authorization_pending" => Ok(DevicePoll::Pending),
            "slow_down" => Ok(DevicePoll::SlowDown),
            "access_denied" => Err(format!(
                "OAuth device authorization was denied by the user (access_denied): {description}"
            )),
            "expired_token" => {
                Err("OAuth device code expired before it was approved (expired_token)".to_string())
            }
            other => Err(format!(
                "OAuth token request was refused ({other}): {description}"
            )),
        };
    }
    if !attempt.status.is_success() {
        return Err(format!(
            "OAuth token request returned HTTP {} without an error code",
            attempt.status
        ));
    }
    let parsed = attempt
        .parsed
        .ok_or_else(|| format!("OAuth token response is invalid: {}", attempt.body))?;
    Ok(DevicePoll::Granted(token_bundle(
        parsed,
        None,
        dpop.is_some(),
    )?))
}

/// The gap before the next poll, from the server's `interval`.
fn device_poll_interval(interval_seconds: Option<u64>) -> Duration {
    Duration::from_secs(interval_seconds.unwrap_or(DEVICE_DEFAULT_POLL_SECONDS))
        .clamp(DEVICE_MIN_POLL, DEVICE_MAX_POLL)
}

/// RFC 8628 §3.5: `slow_down` means the next poll must be at least 5 seconds later.
fn apply_slow_down(interval: Duration) -> Duration {
    (interval + DEVICE_SLOW_DOWN_STEP).min(DEVICE_MAX_POLL)
}

/// `verification_uri_complete` is the one OAuth URL that legitimately carries a query:
/// the code lives there. Everything else about it follows the same origin rules.
fn validate_verification_uri_complete(server_id: &str, raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    let mut origin = Url::parse(trimmed)
        .map_err(|error| format!("the device authorization returned an invalid URL: {error}"))?;
    origin.set_query(None);
    validate_oauth_url(server_id, origin.as_str()).map_err(|error| error.to_string())?;
    Ok(trimmed.to_string())
}

/// Persist a completed authorization and point the row at it.
///
/// Both flows end here on purpose: the stored bundle, the `token_endpoint` used later for
/// refresh, and the reclamation of the previous token cannot depend on which flow ran.
async fn store_connected_bundle(
    database: &Database,
    credentials: Arc<dyn CredentialStore>,
    server_id: &str,
    mut row: McpServerConfig,
    mut oauth: McpOAuthConfig,
    bundle: OAuthTokenBundle,
    metadata: &AuthorizationServerMetadata,
) -> Result<McpOAuthConnectResult, String> {
    let reference = credentials
        .set(
            "mcp",
            &format!("{server_id}-{}", next_id("oauth")),
            &Secret::from_utf8(
                serde_json::to_string(&bundle)
                    .map_err(|error| format!("could not encode OAuth token bundle: {error}"))?,
            ),
        )
        .map_err(|error| format!("could not store OAuth token bundle: {error}"))?;
    let previous_reference = oauth.credential_ref.clone();
    oauth.issuer = metadata.issuer.clone();
    oauth.token_endpoint = Some(metadata.token_endpoint.clone());
    oauth.credential_ref = Some(reference.to_string_ref());
    row.oauth = Some(oauth.clone());
    if let Err(error) = database.mcp_servers().upsert(&row) {
        let _ =
            crate::state::credential_cleanup::reclaim(database, credentials.as_ref(), &reference);
        return Err(format!("could not save the connected OAuth state: {error}"));
    }
    if let Some(previous_reference) = previous_reference {
        if previous_reference != reference.to_string_ref() {
            if let Ok(previous_reference) = CredentialRef::parse(&previous_reference) {
                if let Err(error) = crate::state::credential_cleanup::reclaim(
                    database,
                    credentials.as_ref(),
                    &previous_reference,
                ) {
                    tracing::warn!(
                        server_id,
                        "OAuth token was replaced but the previous token could not be reclaimed immediately: {error}"
                    );
                }
            }
        }
    }
    Ok(McpOAuthConnectResult {
        server_id: server_id.to_string(),
        issuer: oauth.issuer,
        scopes: oauth.scopes,
        token_endpoint: metadata.token_endpoint.clone(),
    })
}

async fn discover(
    client: &Client,
    proxy: &OutboundProxy,
    server_id: &str,
    issuer: &str,
) -> Result<AuthorizationServerMetadata, String> {
    let issuer = Url::parse(&validate_oauth_url(server_id, issuer).map_err(|e| e.to_string())?)
        .map_err(|error| format!("OAuth issuer is invalid: {error}"))?;
    let issuer = issuer.as_str().trim_end_matches('/').to_string();
    let mut discovery =
        Url::parse(&issuer).map_err(|error| format!("OAuth issuer is invalid: {error}"))?;
    let path = discovery
        .path()
        .trim_start_matches('/')
        .trim_end_matches('/');
    discovery.set_path(&if path.is_empty() {
        "/.well-known/oauth-authorization-server".to_string()
    } else {
        format!("/.well-known/oauth-authorization-server/{path}")
    });

    let mut response = client
        .get(discovery)
        .timeout(DISCOVERY_TIMEOUT)
        .send()
        .await
        .map_err(|error| {
            format!(
                "could not fetch OAuth authorization-server metadata: {}",
                oauth_transport_error(proxy, error)
            )
        })?;
    if !response.status().is_success() {
        return Err(format!(
            "OAuth authorization-server metadata returned HTTP {}",
            response.status()
        ));
    }
    let body = read_bounded_body(&mut response, MAX_METADATA_BYTES).await?;
    let metadata: AuthorizationServerMetadata = serde_json::from_slice(&body)
        .map_err(|error| format!("OAuth authorization-server metadata is invalid: {error}"))?;
    if metadata.issuer.trim_end_matches('/') != issuer {
        return Err("OAuth metadata issuer does not match the configured issuer".to_string());
    }
    if !metadata.response_types_supported.is_empty()
        && !metadata
            .response_types_supported
            .iter()
            .any(|value| value == "code")
    {
        return Err(
            "OAuth authorization server does not advertise the authorization code flow".to_string(),
        );
    }
    if !metadata.code_challenge_methods_supported.is_empty()
        && !metadata
            .code_challenge_methods_supported
            .iter()
            .any(|value| value == "S256")
    {
        return Err("OAuth authorization server does not advertise PKCE S256".to_string());
    }
    let authorization_endpoint = validate_oauth_url(server_id, &metadata.authorization_endpoint)
        .map_err(|error| error.to_string())?;
    let token_endpoint = validate_oauth_url(server_id, &metadata.token_endpoint)
        .map_err(|error| error.to_string())?;
    let registration_endpoint = metadata
        .registration_endpoint
        .as_deref()
        .map(|endpoint| validate_oauth_url(server_id, endpoint).map_err(|error| error.to_string()))
        .transpose()?;
    let device_authorization_endpoint = metadata
        .device_authorization_endpoint
        .as_deref()
        .map(|endpoint| validate_oauth_url(server_id, endpoint).map_err(|error| error.to_string()))
        .transpose()?;
    Ok(AuthorizationServerMetadata {
        issuer,
        authorization_endpoint,
        token_endpoint,
        registration_endpoint,
        device_authorization_endpoint,
        response_types_supported: metadata.response_types_supported,
        code_challenge_methods_supported: metadata.code_challenge_methods_supported,
    })
}

/// RFC 8628 §3.4: the grant type a device-code poll uses.
const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Register a public client (RFC 7591) for one flow.
///
/// The grant list is flow-specific rather than "everything we might use": a registration
/// that claims `authorization_code` without a registered callback, or claims the device
/// grant on a server that will validate the list, is a client the server may refuse.
async fn register_client(
    client: &Client,
    proxy: &OutboundProxy,
    server_id: &str,
    endpoint: &str,
    flow: McpOAuthFlow,
    redirect_uri: Option<&str>,
    scopes: &[String],
) -> Result<String, String> {
    let endpoint = validate_oauth_url(server_id, endpoint).map_err(|error| error.to_string())?;
    let (grant_types, response_types, redirect_uris) = match flow {
        McpOAuthFlow::AuthorizationCode => (
            vec![
                "authorization_code".to_string(),
                "refresh_token".to_string(),
            ],
            vec!["code".to_string()],
            redirect_uri
                .map(|uri| vec![uri.to_string()])
                .unwrap_or_default(),
        ),
        McpOAuthFlow::DeviceCode => (
            vec![DEVICE_CODE_GRANT.to_string(), "refresh_token".to_string()],
            Vec::new(),
            Vec::new(),
        ),
    };
    let request = DynamicClientRegistrationRequest {
        redirect_uris,
        client_name: DYNAMIC_CLIENT_NAME.to_string(),
        grant_types,
        response_types,
        token_endpoint_auth_method: "none".to_string(),
        scope: (!scopes.is_empty()).then(|| scopes.join(" ")),
    };
    let body = serde_json::to_vec(&request)
        .map_err(|error| format!("could not encode OAuth client registration: {error}"))?;
    let mut response = client
        .post(endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .timeout(TOKEN_TIMEOUT)
        .send()
        .await
        .map_err(|error| {
            format!(
                "OAuth dynamic client registration failed: {}",
                oauth_transport_error(proxy, error)
            )
        })?;
    let status = response.status();
    let body = read_bounded_body(&mut response, MAX_REGISTRATION_RESPONSE_BYTES).await?;
    let parsed: DynamicClientRegistrationResponse =
        serde_json::from_slice(&body).map_err(|error| {
            format!("OAuth dynamic client registration response is invalid: {error}")
        })?;
    if !status.is_success() {
        let detail = parsed
            .error_description
            .as_deref()
            .or(parsed.error.as_deref())
            .unwrap_or("the authorization server rejected dynamic client registration");
        return Err(format!(
            "OAuth dynamic client registration returned HTTP {status}: {}",
            sanitize(detail)
        ));
    }
    if parsed
        .client_secret
        .as_deref()
        .is_some_and(|secret| !secret.is_empty())
    {
        return Err(
            "OAuth dynamic client registration issued a client secret, but a public native \
             client was requested"
                .to_string(),
        );
    }
    if parsed.token_endpoint_auth_method.as_deref() != Some("none") {
        return Err(
            "OAuth dynamic client registration did not confirm token_endpoint_auth_method none"
                .to_string(),
        );
    }
    let client_id = parsed.client_id.trim();
    if client_id.is_empty() || client_id.len() > 512 || client_id.chars().any(char::is_control) {
        return Err("OAuth dynamic client registration returned an invalid client id".to_string());
    }
    Ok(client_id.to_string())
}

async fn discover_resource_issuer(
    client: &Client,
    proxy: &OutboundProxy,
    server_id: &str,
    resource: &str,
) -> Result<String, String> {
    let resource_url =
        Url::parse(&validate_oauth_url(server_id, resource).map_err(|error| error.to_string())?)
            .map_err(|error| format!("MCP resource URL is invalid: {error}"))?;
    let probe = client
        .get(resource_url.clone())
        .header("accept", "application/json, text/event-stream")
        .timeout(DISCOVERY_TIMEOUT)
        .send()
        .await
        .map_err(|error| {
            format!(
                "could not probe MCP protected-resource metadata: {}",
                oauth_transport_error(proxy, error)
            )
        })?;
    let metadata_url = resource_metadata_from_headers(&resource_url, &probe)
        .unwrap_or_else(|| protected_resource_metadata_url(&resource_url));
    let metadata_url =
        validate_oauth_url(server_id, metadata_url.as_str()).map_err(|error| error.to_string())?;
    let metadata: ProtectedResourceMetadata = fetch_json(
        client,
        proxy,
        &metadata_url,
        MAX_METADATA_BYTES,
        "protected-resource metadata",
    )
    .await?;
    if metadata.resource.trim_end_matches('/') != resource_url.as_str().trim_end_matches('/') {
        return Err(
            "OAuth protected-resource metadata does not match the configured MCP endpoint"
                .to_string(),
        );
    }
    for issuer in &metadata.authorization_servers {
        if let Ok(issuer) = validate_oauth_url(server_id, issuer) {
            return Ok(issuer.trim_end_matches('/').to_string());
        }
    }
    Err("OAuth protected-resource metadata advertises no valid authorization server".to_string())
}

fn resource_metadata_from_headers(resource: &Url, response: &Response) -> Option<Url> {
    for value in response.headers().get_all("www-authenticate") {
        let Ok(value) = value.to_str() else {
            continue;
        };
        let lower = value.to_ascii_lowercase();
        let Some(index) = lower.find("resource_metadata=") else {
            continue;
        };
        let rest = &value[index + "resource_metadata=".len()..];
        let raw = if let Some(rest) = rest.strip_prefix('"') {
            rest.split('"').next().unwrap_or_default()
        } else {
            rest.split([',', ' ']).next().unwrap_or_default()
        };
        if let Ok(url) = resource.join(raw) {
            return Some(url);
        }
    }
    None
}

fn protected_resource_metadata_url(resource: &Url) -> Url {
    let mut metadata = resource.clone();
    let path = resource
        .path()
        .trim_start_matches('/')
        .trim_end_matches('/');
    metadata.set_query(None);
    metadata.set_fragment(None);
    metadata.set_path(&if path.is_empty() {
        "/.well-known/oauth-protected-resource".to_string()
    } else {
        format!("/.well-known/oauth-protected-resource/{path}")
    });
    metadata
}

async fn fetch_json<T: DeserializeOwned>(
    client: &Client,
    proxy: &OutboundProxy,
    url: &str,
    limit: usize,
    label: &str,
) -> Result<T, String> {
    let mut response = client
        .get(url)
        .timeout(DISCOVERY_TIMEOUT)
        .send()
        .await
        .map_err(|error| {
            format!(
                "could not fetch OAuth {label}: {}",
                oauth_transport_error(proxy, error)
            )
        })?;
    if !response.status().is_success() {
        return Err(format!("OAuth {label} returned HTTP {}", response.status()));
    }
    let body = read_bounded_body(&mut response, limit).await?;
    serde_json::from_slice(&body).map_err(|error| format!("OAuth {label} is invalid: {error}"))
}

/// 一次 token 端点请求要用的 DPoP 材料。
///
/// 借来的两个字段都活在 `OAuthTokenSource` 或 `connect` 的栈上：nonce 是服务器的即时挑战，
/// 只在内存里记着，key 只在凭据库里存着。
#[derive(Clone, Copy)]
struct DpopContext<'a> {
    key: &'a DpopKey,
    nonces: &'a DpopNonces,
}

/// 一次 token 端点往返的结果：状态、服务器可能给的 nonce，以及解析过的响应。
struct TokenAttempt {
    status: reqwest::StatusCode,
    challenge: Option<String>,
    /// `None` 表示正文不是 JSON —— 401 挑战的正文常常是空的，那不是解析失败。
    parsed: Option<TokenResponse>,
    /// 解析失败时留在错误信息里的正文（已截断、已脱敏）。
    body: String,
}

#[allow(clippy::too_many_arguments)]
async fn request_token(
    client: &Client,
    proxy: &OutboundProxy,
    server_id: &str,
    endpoint: &str,
    auth: &ClientAuthMaterial,
    form: &HashMap<String, String>,
    previous_refresh_token: Option<&str>,
    dpop: Option<DpopContext<'_>>,
) -> Result<OAuthTokenBundle, String> {
    let endpoint = validate_oauth_url(server_id, endpoint).map_err(|error| error.to_string())?;
    let cached = dpop.and_then(|context| context.nonces.get(&endpoint));
    let first = post_token_form(
        client,
        proxy,
        &endpoint,
        auth,
        form,
        dpop,
        cached.as_deref(),
    )
    .await?;
    // nonce 挑战只重试一次（ADR 0018）：服务器说「用这个 nonce 再来一次」，我们就再来一次，
    // 再被挑战就是失败，而不是无限换 nonce。
    if first.status == reqwest::StatusCode::UNAUTHORIZED {
        if let (Some(context), Some(challenge)) = (dpop, first.challenge.as_deref()) {
            if cached.as_deref() != Some(challenge) {
                context.nonces.remember(&endpoint, challenge);
                let second =
                    post_token_form(client, proxy, &endpoint, auth, form, dpop, Some(challenge))
                        .await?;
                return finish_token_attempt(second, previous_refresh_token, dpop.is_some());
            }
        }
    }
    finish_token_attempt(first, previous_refresh_token, dpop.is_some())
}

async fn post_token_form(
    client: &Client,
    proxy: &OutboundProxy,
    endpoint: &str,
    auth: &ClientAuthMaterial,
    form: &HashMap<String, String>,
    dpop: Option<DpopContext<'_>>,
    nonce: Option<&str>,
) -> Result<TokenAttempt, String> {
    let mut form = form.clone();
    let mut request = auth.apply(client.post(endpoint), &mut form)?;
    if let Some(context) = dpop {
        // token 端点请求也带 proof，但不带 `ath`：这里出示的是 refresh token 或授权码，
        // 不是 access token（RFC 9449 §5）。
        request = request.header(
            DPOP_HEADER,
            context.key.proof("POST", endpoint, nonce, None)?,
        );
    }
    let mut response = request
        .form(&form)
        .timeout(TOKEN_TIMEOUT)
        .send()
        .await
        .map_err(|error| {
            format!(
                "OAuth token request failed: {}",
                oauth_transport_error(proxy, error)
            )
        })?;
    let status = response.status();
    let challenge = response_nonce(&response);
    let body = read_bounded_body(&mut response, MAX_TOKEN_RESPONSE_BYTES).await?;
    let parsed = serde_json::from_slice::<TokenResponse>(&body).ok();
    let body = if parsed.is_some() {
        String::new()
    } else {
        sanitize(
            &String::from_utf8_lossy(&body)
                .chars()
                .take(256)
                .collect::<String>(),
        )
    };
    Ok(TokenAttempt {
        status,
        challenge,
        parsed,
        body,
    })
}

/// 服务器在响应头里给的 nonce 挑战（`DPoP-Nonce`），值要过一遍长度与控制字符检查。
fn response_nonce(response: &Response) -> Option<String> {
    let value = response
        .headers()
        .get(DPOP_NONCE_HEADER)
        .and_then(|value| value.to_str().ok())?
        .trim();
    if value.is_empty() || value.len() > 1_024 || value.chars().any(char::is_control) {
        return None;
    }
    Some(value.to_string())
}

fn finish_token_attempt(
    attempt: TokenAttempt,
    previous_refresh_token: Option<&str>,
    dpop: bool,
) -> Result<OAuthTokenBundle, String> {
    if !attempt.status.is_success() {
        let detail = attempt
            .parsed
            .as_ref()
            .and_then(|parsed| parsed.error_description.as_deref())
            .or_else(|| {
                attempt
                    .parsed
                    .as_ref()
                    .and_then(|parsed| parsed.error.as_deref())
            })
            .unwrap_or("the authorization server rejected the token request");
        return Err(format!(
            "OAuth token request returned HTTP {}: {}",
            attempt.status,
            sanitize(detail)
        ));
    }
    let parsed = attempt.parsed.ok_or_else(|| {
        format!(
            "OAuth token response is invalid: {}",
            if attempt.body.is_empty() {
                "the body is empty".to_string()
            } else {
                attempt.body
            }
        )
    })?;
    token_bundle(parsed, previous_refresh_token, dpop)
}

/// A parsed token response → the bundle that goes into the credential store.
///
/// Shared by the code exchange, the device-code poll and the refresh path so that "what
/// counts as a usable token" is answered once: a response without an access token, or one
/// that is not a bearer token, is refused the same way wherever it came from.
fn token_bundle(
    parsed: TokenResponse,
    previous_refresh_token: Option<&str>,
    dpop: bool,
) -> Result<OAuthTokenBundle, String> {
    if parsed.access_token.trim().is_empty() {
        return Err("OAuth token response has no access token".to_string());
    }
    // 开启发送方约束时 `token_type` 必须是 `DPoP`：服务器不认这把钥匙、或者根本不验证
    // proof，都必须在**存下令牌之前**说清楚，不能悄悄退回 bearer（ADR 0018）。
    let expected = if dpop { "dpop" } else { "bearer" };
    match parsed.token_type.as_deref() {
        Some(value) if value.eq_ignore_ascii_case(expected) => {}
        Some(value) => {
            return Err(format!(
                "OAuth token response is not a {expected} token (the server said `{}`)",
                sanitize(value)
            ))
        }
        None if dpop => {
            return Err(
                "OAuth token response is not a DPoP token: the server did not name a token type"
                    .to_string(),
            )
        }
        None => {}
    }
    let expires_at = parsed
        .expires_in
        .map(|seconds| unix_now().saturating_add(seconds));
    Ok(OAuthTokenBundle {
        access_token: parsed.access_token,
        refresh_token: parsed
            .refresh_token
            .or_else(|| previous_refresh_token.map(str::to_string)),
        expires_at,
        token_type: if dpop { "DPoP" } else { "Bearer" }.to_string(),
        scope: parsed.scope,
    })
}

async fn wait_for_callback(
    listener: TcpListener,
    expected_state: &str,
    cancel: &CancellationToken,
) -> Result<String, String> {
    let deadline = tokio::time::Instant::now() + CALLBACK_TIMEOUT;
    for _ in 0..MAX_CALLBACK_REQUESTS {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let accepted = tokio::select! {
            _ = cancel.cancelled() => return Err(CANCELLED_FLOW.to_string()),
            accepted = tokio::time::timeout(remaining, listener.accept()) => accepted,
        };
        let (mut stream, _) = accepted
            .map_err(|_| "OAuth callback timed out".to_string())?
            .map_err(|error| format!("OAuth callback listener failed: {error}"))?;
        let request = tokio::time::timeout(
            CALLBACK_REQUEST_TIMEOUT.min(remaining),
            read_http_request(&mut stream),
        )
        .await;
        let request = match request {
            Ok(Ok(request)) => request,
            Ok(Err(error)) => {
                respond(&mut stream, 400, "Invalid OAuth callback.").await;
                return Err(error);
            }
            Err(_) => {
                respond(&mut stream, 408, "OAuth callback timed out.").await;
                continue;
            }
        };
        let Some(target) = request_target(&request) else {
            respond(&mut stream, 400, "Invalid OAuth callback.").await;
            continue;
        };
        let url = match Url::parse(&format!("http://127.0.0.1{target}")) {
            Ok(url) => url,
            Err(_) => {
                respond(&mut stream, 400, "Invalid OAuth callback.").await;
                continue;
            }
        };
        if url.path() != "/callback" {
            respond(&mut stream, 404, "Not found.").await;
            continue;
        }
        let parameters = url
            .query_pairs()
            .into_owned()
            .collect::<HashMap<String, String>>();
        if parameters.get("state").map(String::as_str) != Some(expected_state) {
            respond(&mut stream, 400, "OAuth state validation failed.").await;
            return Err("OAuth callback state did not match".to_string());
        }
        if let Some(error) = parameters.get("error") {
            let description = parameters
                .get("error_description")
                .map_or("authorization was rejected".to_string(), |value| {
                    sanitize(value)
                });
            respond(&mut stream, 400, "OAuth authorization failed.").await;
            return Err(format!(
                "OAuth authorization failed: {}: {description}",
                sanitize(error)
            ));
        }
        let Some(code) = parameters.get("code").filter(|code| !code.is_empty()) else {
            respond(&mut stream, 400, "OAuth callback has no code.").await;
            return Err("OAuth callback did not contain an authorization code".to_string());
        };
        respond(
            &mut stream,
            200,
            "OAuth authorization complete. You can close this window and return to Yukinal.",
        )
        .await;
        return Ok(code.clone());
    }
    Err("OAuth callback timed out".to_string())
}

async fn read_http_request(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream
            .read(&mut buffer)
            .await
            .map_err(|error| format!("could not read OAuth callback request: {error}"))?;
        if read == 0 {
            return Err("OAuth callback request ended before its headers".to_string());
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(request);
        }
        if request.len() > MAX_CALLBACK_BYTES {
            return Err("OAuth callback request headers are too large".to_string());
        }
    }
}

fn request_target(request: &[u8]) -> Option<String> {
    let line_end = request.windows(2).position(|window| window == b"\r\n")?;
    let line = std::str::from_utf8(&request[..line_end]).ok()?;
    let mut parts = line.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    Some(parts.next()?.to_string())
}

async fn respond(stream: &mut TcpStream, status: u16, message: &str) {
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Yukinal OAuth</title></head><body><p>{message}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
        if status == 200 { "OK" } else { "Bad Request" },
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn read_bounded_body(response: &mut Response, limit: usize) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(format!(
            "OAuth HTTP response exceeds the {limit}-byte limit"
        ));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        format!(
            "could not read OAuth HTTP response: {}",
            sanitize(&error.to_string())
        )
    })? {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(format!(
                "OAuth HTTP response exceeds the {limit}-byte limit"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn token_is_fresh(bundle: &OAuthTokenBundle) -> bool {
    bundle
        .expires_at
        .is_none_or(|expires_at| expires_at > unix_now().saturating_add(TOKEN_EXPIRY_SKEW_SECS))
}

pub(super) fn random_base64url(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    rand::rng().fill_bytes(&mut value);
    base64url(&value)
}

pub(super) fn base64url(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0] as u32;
        let second = chunk.get(1).copied().unwrap_or(0) as u32;
        let third = chunk.get(2).copied().unwrap_or(0) as u32;
        let value = (first << 16) | (second << 8) | third;
        output.push(TABLE[((value >> 18) & 63) as usize] as char);
        output.push(TABLE[((value >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            output.push(TABLE[((value >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            output.push(TABLE[(value & 63) as usize] as char);
        }
    }
    output
}

pub(super) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(512)
        .collect()
}

fn ensure_crypto_provider() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::io::{Read as _, Write as _};
    use std::net::{SocketAddr, TcpListener as StdTcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    use serde_json::{json, Value};
    use yukinal_credentials::memory::MemoryCredentialStore;
    use yukinal_database::models::{McpOAuthConfig, McpServerConfig};

    use super::*;

    /// One scripted answer to a `device_code` poll, consumed in order.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum DeviceStep {
        Pending,
        SlowDown,
        Denied,
        Expired,
        Grant,
    }

    /// How the test server expects a client to authenticate on each request.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ClientAuthVia {
        /// Credentials in the request body (`client_secret_post`, or a public `client_id`).
        Body,
        /// Credentials in an `Authorization: Basic …` header (`client_secret_basic`).
        Header,
    }

    #[derive(Debug, Clone)]
    struct ClientExpectation {
        client_id: String,
        secret: Option<String>,
        via: ClientAuthVia,
    }

    /// 服务器侧对 DPoP proof 的要求与它的检查结果。
    ///
    /// 这是**真的**在验证：用 proof 头里的 JWK 验签、比对方法/URL、检查时间窗、拒绝重复的
    /// `jti`。客户端只负责签对，服务器相信它的理由必须是它自己算过。
    #[derive(Default)]
    struct DpopPolicy {
        required: bool,
        /// 要求 proof 带上的 nonce；不带就回一次挑战。
        nonce: Option<String>,
        /// 挑战过几次：用来断言重试只有一轮。
        challenges: usize,
        /// 见过的 `jti`：重复即重放。
        seen_jti: HashSet<String>,
        /// 强制回答里的 `token_type`（不设就按是否要 DPoP 决定）。
        token_type: Option<String>,
    }

    /// 一次 proof 检查的结论。
    enum DpopVerdict {
        NotRequired,
        Accepted,
        Challenge(String),
        Rejected(String),
    }

    /// The scripted behaviour of the test server: what it advertises, how it answers, and
    /// which client it expects to hear from.
    ///
    /// Scripted rather than dynamic because the interesting client behaviour *is* the
    /// sequence — "pending, then slow_down, then granted" is the flow, and a fake that
    /// answered at random could not assert that the client kept waiting.
    struct ServerScript {
        steps: Mutex<Vec<DeviceStep>>,
        /// Unix milliseconds of each `device_code` poll, so a test can prove the client
        /// waited instead of hammering the endpoint.
        polls: Mutex<Vec<u64>>,
        interval: Mutex<Option<u64>>,
        supported: Mutex<bool>,
        /// `expires_in` the device authorization advertises.
        lifetime: Mutex<u64>,
        /// Defaults to the public `test-client`; a client-secret test replaces it.
        client: Mutex<ClientExpectation>,
        /// A secret to volunteer in the registration response, as some servers do. A public
        /// client must refuse the whole registration rather than start using it.
        registration_secret: Mutex<Option<String>>,
        dpop: Mutex<DpopPolicy>,
    }

    impl ServerScript {
        fn new() -> Self {
            Self {
                // A granted flow is the default: tests that care about the sequence set one.
                steps: Mutex::new(vec![DeviceStep::Grant]),
                polls: Mutex::new(Vec::new()),
                // `interval: 0` keeps the fixtures fast; the client's 250 ms floor is what
                // keeps that from being a busy loop.
                interval: Mutex::new(Some(0)),
                supported: Mutex::new(true),
                lifetime: Mutex::new(300),
                client: Mutex::new(ClientExpectation {
                    client_id: "test-client".to_string(),
                    secret: None,
                    via: ClientAuthVia::Body,
                }),
                registration_secret: Mutex::new(None),
                dpop: Mutex::new(DpopPolicy::default()),
            }
        }

        /// 要求每个 token 请求带一个可验证的 proof，并在第一次挑战里给出 `nonce`。
        fn require_dpop(&self, nonce: Option<&str>) {
            let mut policy = self.dpop.lock().expect("dpop");
            policy.required = true;
            policy.nonce = nonce.map(str::to_string);
        }

        /// 强制回答里的 `token_type`，用来测「服务器回 Bearer」这条拒绝路径。
        fn force_token_type(&self, value: &str) {
            self.dpop.lock().expect("dpop").token_type = Some(value.to_string());
        }

        fn challenges(&self) -> usize {
            self.dpop.lock().expect("dpop").challenges
        }

        fn token_type(&self) -> String {
            let policy = self.dpop.lock().expect("dpop");
            policy.token_type.clone().unwrap_or_else(|| {
                if policy.required {
                    "DPoP".to_string()
                } else {
                    "Bearer".to_string()
                }
            })
        }

        /// 服务器侧校验一个 proof（RFC 9449 §4.3 的检查表，去掉我们不需要的项）。
        fn check_dpop(
            &self,
            headers: &HashMap<String, String>,
            method: &str,
            target: &str,
            address: SocketAddr,
        ) -> DpopVerdict {
            let mut policy = self.dpop.lock().expect("dpop");
            if !policy.required {
                return DpopVerdict::NotRequired;
            }
            let Some(proof) = headers.get("dpop") else {
                return DpopVerdict::Rejected("no proof was sent".to_string());
            };
            let parts: Vec<&str> = proof.split('.').collect();
            if parts.len() != 3 {
                return DpopVerdict::Rejected("the proof is not a JWS".to_string());
            }
            let Some(header) = crate::commands::mcp::dpop::decode_base64url(parts[0])
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            else {
                return DpopVerdict::Rejected("the proof header is not JSON".to_string());
            };
            if header["typ"] != "dpop+jwt" || header["alg"] != "EdDSA" {
                return DpopVerdict::Rejected(format!("unexpected proof header: {header}"));
            }
            if header["jwk"]["kty"] != "OKP" || header["jwk"]["crv"] != "Ed25519" {
                return DpopVerdict::Rejected(format!("unexpected proof key: {header}"));
            }
            let Some(public) = header["jwk"]["x"]
                .as_str()
                .and_then(crate::commands::mcp::dpop::decode_base64url)
            else {
                return DpopVerdict::Rejected("the proof key is not base64url".to_string());
            };
            let Some(signature) = crate::commands::mcp::dpop::decode_base64url(parts[2]) else {
                return DpopVerdict::Rejected("the proof signature is not base64url".to_string());
            };
            let signing_input = format!("{}.{}", parts[0], parts[1]);
            if ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public)
                .verify(signing_input.as_bytes(), &signature)
                .is_err()
            {
                return DpopVerdict::Rejected("the proof signature does not verify".to_string());
            }
            let Some(claims) = crate::commands::mcp::dpop::decode_base64url(parts[1])
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            else {
                return DpopVerdict::Rejected("the proof claims are not JSON".to_string());
            };
            // htm/htu：proof 只对这一次请求有效，方法与 URL 都要对得上。
            if claims["htm"] != method {
                return DpopVerdict::Rejected(format!(
                    "the proof was made for {} but this request is a {method}",
                    claims["htm"]
                ));
            }
            let expected_target = format!("http://{address}{target}");
            if claims["htu"] != expected_target {
                return DpopVerdict::Rejected(format!(
                    "the proof was made for {} but this request went to {expected_target}",
                    claims["htu"]
                ));
            }
            // 时间窗：服务器允许两分钟的时钟偏差，超出就是过期或未来。
            let iat = claims["iat"].as_u64().unwrap_or_default();
            if iat.abs_diff(unix_now()) > 120 {
                return DpopVerdict::Rejected(format!("the proof is stale (iat={iat})"));
            }
            // token 端点请求不该带 `ath`：那里出示的不是 access token。
            if claims.get("ath").is_some() {
                return DpopVerdict::Rejected("a token request must not carry ath".to_string());
            }
            let Some(jti) = claims["jti"].as_str() else {
                return DpopVerdict::Rejected("the proof has no jti".to_string());
            };
            if !policy.seen_jti.insert(jti.to_string()) {
                return DpopVerdict::Rejected(format!("the proof was replayed (jti={jti})"));
            }
            if let Some(nonce) = policy.nonce.clone() {
                if claims["nonce"].as_str() != Some(nonce.as_str()) {
                    policy.challenges += 1;
                    return DpopVerdict::Challenge(nonce);
                }
            }
            DpopVerdict::Accepted
        }

        fn with_steps(steps: &[DeviceStep]) -> Self {
            let script = Self::new();
            *script.steps.lock().expect("steps") = steps.to_vec();
            script
        }

        fn next_step(&self) -> DeviceStep {
            let mut steps = self.steps.lock().expect("steps");
            if steps.len() > 1 {
                steps.remove(0)
            } else {
                steps.first().copied().unwrap_or(DeviceStep::Grant)
            }
        }

        fn record_poll(&self) -> u64 {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            self.polls.lock().expect("polls").push(now);
            now
        }

        fn polls(&self) -> Vec<u64> {
            self.polls.lock().expect("polls").clone()
        }

        /// Require a specific client on every request that carries client authentication.
        fn expect_client(&self, client_id: &str, secret: Option<&str>, via: ClientAuthVia) {
            *self.client.lock().expect("client") = ClientExpectation {
                client_id: client_id.to_string(),
                secret: secret.map(str::to_string),
                via,
            };
        }

        /// The assertion the server makes on a request, so a missing or duplicated
        /// credential fails in the fixture rather than in a token response.
        fn assert_client(&self, client_id: &str, secret: Option<&str>, via: ClientAuthVia) {
            let expected = self.client.lock().expect("client").clone();
            assert_eq!(
                client_id, expected.client_id,
                "the request carried the wrong client id"
            );
            assert_eq!(
                secret,
                expected.secret.as_deref(),
                "the client secret was {} when it should have been {}",
                if secret.is_some() { "sent" } else { "absent" },
                if expected.secret.is_some() {
                    "sent"
                } else {
                    "absent"
                }
            );
            assert_eq!(
                via, expected.via,
                "the client credentials arrived in the wrong place (RFC 6749 §2.3 allows one \
                 method per request, and this server only accepts the configured one)"
            );
        }
    }

    struct TestAuthorizationServer {
        address: SocketAddr,
        stop: Arc<AtomicBool>,
        requests: Arc<Mutex<Vec<(String, String)>>>,
        registrations: Arc<Mutex<Vec<Value>>>,
        script: Arc<ServerScript>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl TestAuthorizationServer {
        fn start() -> Self {
            Self::with_script(Arc::new(ServerScript::new()))
        }

        fn with_script(script: Arc<ServerScript>) -> Self {
            let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind OAuth test server");
            let address = listener.local_addr().expect("OAuth test address");
            let stop = Arc::new(AtomicBool::new(false));
            let requests = Arc::new(Mutex::new(Vec::new()));
            let registrations = Arc::new(Mutex::new(Vec::new()));
            let thread_stop = stop.clone();
            let thread_requests = requests.clone();
            let thread_registrations = registrations.clone();
            let thread_script = script.clone();
            let thread = std::thread::spawn(move || {
                for incoming in listener.incoming() {
                    if thread_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(mut stream) = incoming else {
                        break;
                    };
                    let _ = handle_oauth_test_request(
                        &mut stream,
                        address,
                        &thread_requests,
                        &thread_registrations,
                        &thread_script,
                    );
                }
            });
            Self {
                address,
                stop,
                requests,
                registrations,
                script,
                thread: Some(thread),
            }
        }

        fn issuer(&self) -> String {
            format!("http://{}", self.address)
        }

        fn requests(&self) -> Vec<(String, String)> {
            self.requests.lock().expect("requests").clone()
        }

        fn registrations(&self) -> Vec<Value> {
            self.registrations.lock().expect("registrations").clone()
        }

        fn script(&self) -> &ServerScript {
            &self.script
        }
    }

    impl Drop for TestAuthorizationServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = TcpStream::connect(self.address);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn handle_oauth_test_request(
        stream: &mut TcpStream,
        address: SocketAddr,
        requests: &Arc<Mutex<Vec<(String, String)>>>,
        registrations: &Arc<Mutex<Vec<Value>>>,
        script: &ServerScript,
    ) -> std::io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        let header_end = loop {
            let read = stream.read(&mut buffer)?;
            if read == 0 {
                return Ok(());
            }
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break index;
            }
        };
        let head = String::from_utf8_lossy(&bytes[..header_end]);
        let mut lines = head.lines();
        let request_line = lines.next().unwrap_or_default();
        let mut request_parts = request_line.split_whitespace();
        let method = request_parts.next().unwrap_or_default().to_string();
        let target = request_parts.next().unwrap_or_default().to_string();
        let mut headers = HashMap::new();
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
            }
        }
        let content_length = headers
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let body_start = header_end + 4;
        while bytes.len() < body_start + content_length {
            let read = stream.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
        }
        let body =
            String::from_utf8_lossy(&bytes[body_start..body_start + content_length]).into_owned();
        requests
            .lock()
            .expect("requests")
            .push((method.clone(), target.clone()));

        let issuer = format!("http://{address}");
        if target == "/mcp" {
            write!(
                stream,
                "HTTP/1.1 401 Unauthorized\r\n\
                 WWW-Authenticate: Bearer resource_metadata=\"http://{address}/resource-metadata\"\r\n\
                 Content-Length: 0\r\nConnection: close\r\n\r\n"
            )?;
            stream.flush()?;
            return Ok(());
        }
        if target == "/resource-metadata" {
            let body = serde_json::to_vec(&json!({
                "resource": format!("http://{address}/mcp"),
                "authorization_servers": [issuer],
            }))
            .expect("protected resource JSON");
            return write_response(stream, 200, "OK", &body);
        }
        if target == "/.well-known/oauth-authorization-server" {
            let mut metadata = json!({
                "issuer": issuer,
                "authorization_endpoint": format!("http://{address}/authorize"),
                "token_endpoint": format!("http://{address}/token"),
                "registration_endpoint": format!("http://{address}/register"),
                "response_types_supported": ["code"],
                "code_challenge_methods_supported": ["S256"]
            });
            if *script.supported.lock().expect("supported") {
                metadata["device_authorization_endpoint"] =
                    json!(format!("http://{address}/device"));
            }
            let body = serde_json::to_vec(&metadata).expect("metadata JSON");
            return write_response(stream, 200, "OK", &body);
        }
        if target == "/device" && method == "POST" {
            let form = Url::parse(&format!("http://127.0.0.1/?{body}"))
                .expect("form URL")
                .query_pairs()
                .into_owned()
                .collect::<HashMap<_, _>>();
            let credentials = client_credentials(&headers, &form);
            script.assert_client(
                &credentials.client_id,
                credentials.secret.as_deref(),
                credentials.via,
            );
            assert_eq!(
                form.get("resource").map(String::as_str),
                Some(format!("http://{address}/mcp").as_str()),
                "RFC 8707 requires the protected resource on the device authorization request"
            );
            assert_eq!(form.get("scope").map(String::as_str), Some("mcp.read"));
            let interval = *script.interval.lock().expect("interval");
            let lifetime = *script.lifetime.lock().expect("lifetime");
            let mut response = json!({
                "device_code": "device-code",
                "user_code": "WDJB-MJHT",
                "verification_uri": format!("http://{address}/activate"),
                "expires_in": lifetime,
            });
            response["verification_uri_complete"] =
                json!(format!("http://{address}/activate?user_code=WDJB-MJHT"));
            if let Some(interval) = interval {
                response["interval"] = json!(interval);
            }
            let body = serde_json::to_vec(&response).expect("device authorization JSON");
            return write_response(stream, 200, "OK", &body);
        }
        if target == "/register" && method == "POST" {
            if headers.get("content-type").map(String::as_str) != Some("application/json") {
                return write_response(stream, 400, "Bad Request", b"");
            }
            let registration: Value =
                serde_json::from_str(&body).expect("dynamic registration JSON");
            registrations
                .lock()
                .expect("registrations")
                .push(registration);
            let mut response = json!({
                "client_id": "dynamic-client",
                "token_endpoint_auth_method": "none",
            });
            if let Some(secret) = script
                .registration_secret
                .lock()
                .expect("registration secret")
                .clone()
            {
                response["client_secret"] = json!(secret);
            }
            let body = serde_json::to_vec(&response).expect("registration response JSON");
            return write_response(stream, 201, "Created", &body);
        }
        if target == "/token" && method == "POST" {
            let form = Url::parse(&format!("http://127.0.0.1/?{body}"))
                .expect("form URL")
                .query_pairs()
                .into_owned()
                .collect::<HashMap<_, _>>();
            let credentials = client_credentials(&headers, &form);
            script.assert_client(
                &credentials.client_id,
                credentials.secret.as_deref(),
                credentials.via,
            );
            // 服务器侧的 proof 检查在这一层：签名、htm/htu、时间窗、jti 重放、nonce 挑战。
            // 检查不过就是**测试失败**（fixture 是被信任的对手），而不是一个要被客户端
            // 处理的服务端行为 —— 所以直接 panic，让原因留在失败信息里。
            match script.check_dpop(&headers, &method, &target, address) {
                DpopVerdict::Challenge(nonce) => {
                    return write_response_with(
                        stream,
                        401,
                        "Unauthorized",
                        &[
                            ("DPoP-Nonce", nonce.as_str()),
                            ("WWW-Authenticate", "DPoP error=\"use_dpop_nonce\""),
                        ],
                        b"",
                    );
                }
                DpopVerdict::Rejected(reason) => {
                    panic!("the fixture rejected the DPoP proof: {reason}")
                }
                DpopVerdict::NotRequired | DpopVerdict::Accepted => {}
            }
            // RFC 6749 §2.3: one method per request. A Basic header *and* a body secret
            // would send the same credential twice, which this asserts against.
            if credentials.via == ClientAuthVia::Header {
                assert!(
                    !form.contains_key("client_secret"),
                    "client_secret_basic must not repeat the secret in the body: {form:?}"
                );
                assert!(
                    !form.contains_key("client_id"),
                    "client_secret_basic carries the client id in the header, not twice: {form:?}"
                );
            }
            let response = match form.get("grant_type").map(String::as_str) {
                Some(grant) if grant == DEVICE_CODE_GRANT => {
                    assert_eq!(
                        form.get("device_code").map(String::as_str),
                        Some("device-code")
                    );
                    assert_eq!(
                        form.get("resource").map(String::as_str),
                        Some(format!("http://{address}/mcp").as_str())
                    );
                    script.record_poll();
                    match script.next_step() {
                        DeviceStep::Pending => {
                            let body = serde_json::to_vec(&json!({
                                "error": "authorization_pending"
                            }))
                            .expect("pending JSON");
                            return write_response(stream, 400, "Bad Request", &body);
                        }
                        DeviceStep::SlowDown => {
                            let body = serde_json::to_vec(&json!({ "error": "slow_down" }))
                                .expect("slow_down JSON");
                            return write_response(stream, 400, "Bad Request", &body);
                        }
                        DeviceStep::Denied => {
                            let body = serde_json::to_vec(&json!({
                                "error": "access_denied",
                                "error_description": "the user refused the request"
                            }))
                            .expect("denied JSON");
                            return write_response(stream, 400, "Bad Request", &body);
                        }
                        DeviceStep::Expired => {
                            let body = serde_json::to_vec(&json!({ "error": "expired_token" }))
                                .expect("expired JSON");
                            return write_response(stream, 400, "Bad Request", &body);
                        }
                        DeviceStep::Grant => json!({
                            "access_token": "device-access",
                            "refresh_token": "device-refresh",
                            "token_type": script.token_type(),
                            "expires_in": 1
                        }),
                    }
                }
                Some("authorization_code") => {
                    assert_eq!(form.get("code").map(String::as_str), Some("auth-code"));
                    assert!(form
                        .get("code_verifier")
                        .is_some_and(|verifier| verifier.len() >= 43));
                    assert_eq!(
                        form.get("resource").map(String::as_str),
                        Some(format!("http://{address}/mcp").as_str())
                    );
                    json!({
                        "access_token": "access-one",
                        "refresh_token": "refresh-one",
                        "token_type": script.token_type(),
                        "expires_in": 1
                    })
                }
                Some("refresh_token") => {
                    let refresh_token = form
                        .get("refresh_token")
                        .map(String::as_str)
                        .unwrap_or_default();
                    assert!(
                        // The code flow and the device flow each mint their own refresh
                        // token; both must refresh through the same request shape.
                        ["refresh-one", "device-refresh"].contains(&refresh_token),
                        "unexpected OAuth refresh token: {refresh_token}"
                    );
                    json!({
                        "access_token": "access-two",
                        "token_type": script.token_type(),
                        "expires_in": 3600
                    })
                }
                other => panic!("unexpected OAuth grant: {other:?}"),
            };
            let body = serde_json::to_vec(&response).expect("token JSON");
            return write_response(stream, 200, "OK", &body);
        }
        write_response(stream, 404, "Not Found", b"")
    }

    /// 带额外响应头的回答，DPoP 的 nonce 挑战要用（`DPoP-Nonce` 与 `WWW-Authenticate`）。
    fn write_response_with(
        stream: &mut TcpStream,
        status: u16,
        reason: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> std::io::Result<()> {
        let extra: String = headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}\r\n"))
            .collect();
        write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
             {extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )?;
        stream.write_all(body)?;
        stream.flush()
    }

    fn write_response(
        stream: &mut TcpStream,
        status: u16,
        reason: &str,
        body: &[u8],
    ) -> std::io::Result<()> {
        write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )?;
        stream.write_all(body)?;
        stream.flush()
    }

    /// What a request claimed about its client, and where it said it.
    struct ReceivedClient {
        client_id: String,
        secret: Option<String>,
        via: ClientAuthVia,
    }

    /// Read client credentials the way RFC 6749 §2.3 allows them to be sent.
    ///
    /// The test server reads both places instead of trusting one, so a client that puts the
    /// secret in the wrong place — or in both places — is visible to the fixture rather than
    /// to a token response that quietly ignores it.
    fn client_credentials(
        headers: &HashMap<String, String>,
        form: &HashMap<String, String>,
    ) -> ReceivedClient {
        if let Some(header) = headers.get("authorization") {
            let encoded = header.strip_prefix("Basic ").unwrap_or_else(|| {
                panic!("only Basic client authentication is expected: {header}")
            });
            let decoded =
                decode_base64(encoded).expect("the client's Basic credentials must be base64");
            let decoded = String::from_utf8(decoded).expect("Basic credentials must be UTF-8");
            let (client_id, secret) = decoded
                .split_once(':')
                .expect("Basic credentials are `client_id:secret`");
            return ReceivedClient {
                client_id: client_id.to_string(),
                secret: Some(secret.to_string()),
                via: ClientAuthVia::Header,
            };
        }
        ReceivedClient {
            client_id: form.get("client_id").cloned().unwrap_or_default(),
            secret: form.get("client_secret").cloned(),
            via: ClientAuthVia::Body,
        }
    }

    /// Standard base64 with padding, for reading what reqwest wrote.
    ///
    /// Pinned by the RFC 7617 example below: a decoder that is wrong in a way that cancels
    /// out the encoder it is checking would make every client-secret assertion meaningless.
    fn decode_base64(value: &str) -> Option<Vec<u8>> {
        let mut output = Vec::new();
        let mut buffer = 0_u32;
        let mut bits = 0_u32;
        for character in value.chars() {
            if character == '=' {
                break;
            }
            let digit = match character {
                'A'..='Z' => character as u32 - 'A' as u32,
                'a'..='z' => character as u32 - 'a' as u32 + 26,
                '0'..='9' => character as u32 - '0' as u32 + 52,
                '+' => 62,
                '/' => 63,
                _ => return None,
            };
            buffer = (buffer << 6) | digit;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                output.push(((buffer >> bits) & 0xff) as u8);
            }
        }
        Some(output)
    }

    fn temp_db(name: &str) -> (PathBuf, Database) {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "yukinal-mcp-oauth-{}-{}-{}.sqlite",
            std::process::id(),
            name,
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        cleanup(&path);
        let database = Database::open(&path).expect("open temp database");
        (path, database)
    }

    fn cleanup(path: &Path) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(PathBuf::from(format!("{}{suffix}", path.display())));
        }
    }

    /// The authorization-code fixtures never reach the device-code prompt, so they pass
    /// this instead of a recorder.
    fn noop_prompt() -> impl FnOnce(DeviceCodePrompt) -> Result<(), String> {
        |_| Ok(())
    }

    /// The device-flow fixtures that only care about the code open no page.
    fn noop_launcher() -> impl FnOnce(&str) -> Result<(), String> {
        |_| Ok(())
    }

    fn browser_launcher() -> impl FnOnce(&str) -> Result<(), String> {
        |authorization_url| {
            let authorization = Url::parse(authorization_url).map_err(|error| error.to_string())?;
            let parameters = authorization
                .query_pairs()
                .into_owned()
                .collect::<HashMap<_, _>>();
            let redirect_uri = parameters
                .get("redirect_uri")
                .ok_or_else(|| "missing redirect URI".to_string())?
                .clone();
            let state = parameters
                .get("state")
                .ok_or_else(|| "missing state".to_string())?
                .clone();
            std::thread::spawn(move || {
                let callback = Url::parse(&redirect_uri).expect("callback URL");
                let target = format!("/callback?code=auth-code&state={state}");
                let mut stream = TcpStream::connect((
                    callback.host_str().expect("callback host"),
                    callback.port().expect("callback port"),
                ))
                .expect("connect callback");
                write!(
                    stream,
                    "GET {target} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                    callback.host_str().expect("callback host")
                )
                .expect("write callback");
                let _ = stream.shutdown(std::net::Shutdown::Write);
                let mut ignored = Vec::new();
                let _ = stream.read_to_end(&mut ignored);
            });
            Ok(())
        }
    }

    #[tokio::test]
    async fn protected_resource_metadata_discovers_the_issuer() {
        let server = TestAuthorizationServer::start();
        let (path, database) = temp_db("resource-discovery");
        database
            .mcp_servers()
            .upsert(&McpServerConfig {
                id: "mcp_oauth".to_string(),
                label: "OAuth discovery fixture".to_string(),
                transport: "http".to_string(),
                command: None,
                args: None,
                url: Some(format!("http://{}/mcp", server.address)),
                http_auth_headers: Vec::new(),
                oauth: Some(oauth_config(
                    String::new(),
                    "test-client",
                    McpOAuthFlow::AuthorizationCode,
                    &["mcp.read"],
                )),
                enabled: true,
                allowed_tools: Vec::new(),
                trust_level: "unreviewed".to_string(),
            })
            .expect("insert automatic OAuth row");
        let credentials = Arc::new(MemoryCredentialStore::new());
        let result = connect(
            &database,
            credentials,
            "mcp_oauth",
            OutboundProxy::default(),
            browser_launcher(),
            noop_prompt(),
            CancellationToken::new(),
        )
        .await
        .expect("automatic discovery must complete the same OAuth flow");
        assert_eq!(result.issuer, server.issuer());
        assert_eq!(
            database
                .mcp_servers()
                .get("mcp_oauth")
                .expect("stored row")
                .oauth
                .expect("OAuth config")
                .issuer,
            server.issuer()
        );
        assert_eq!(
            server.requests(),
            vec![
                ("GET".to_string(), "/mcp".to_string()),
                ("GET".to_string(), "/resource-metadata".to_string()),
                (
                    "GET".to_string(),
                    "/.well-known/oauth-authorization-server".to_string()
                ),
                ("POST".to_string(), "/token".to_string()),
            ]
        );
        cleanup(&path);
    }

    #[tokio::test]
    async fn dynamic_registration_supplies_a_public_client_id() {
        let server = TestAuthorizationServer::start();
        // The registration response carries a client secret; the client still authenticates
        // as the public client it asked to be.
        server
            .script()
            .expect_client("dynamic-client", None, ClientAuthVia::Body);
        let (path, database) = temp_db("dynamic-registration");
        database
            .mcp_servers()
            .upsert(&McpServerConfig {
                id: "mcp_oauth".to_string(),
                label: "OAuth dynamic registration fixture".to_string(),
                transport: "http".to_string(),
                command: None,
                args: None,
                url: Some(format!("http://{}/mcp", server.address)),
                http_auth_headers: Vec::new(),
                oauth: Some(oauth_config(
                    String::new(),
                    "",
                    McpOAuthFlow::AuthorizationCode,
                    &["mcp.read", "mcp.tools"],
                )),
                enabled: true,
                allowed_tools: Vec::new(),
                trust_level: "unreviewed".to_string(),
            })
            .expect("insert dynamic-registration OAuth row");
        let credentials = Arc::new(MemoryCredentialStore::new());

        connect(
            &database,
            credentials,
            "mcp_oauth",
            OutboundProxy::default(),
            browser_launcher(),
            noop_prompt(),
            CancellationToken::new(),
        )
        .await
        .expect("dynamic registration must complete the OAuth flow");

        let oauth = database
            .mcp_servers()
            .get("mcp_oauth")
            .expect("stored row")
            .oauth
            .expect("OAuth config");
        assert_eq!(oauth.client_id, "dynamic-client");
        assert_eq!(
            oauth.client_auth,
            McpOAuthClientAuth::None,
            "the registered client asked to be public"
        );
        assert!(
            oauth.client_secret_ref.is_none(),
            "a secret the server volunteered must not be adopted"
        );
        assert_eq!(
            server.requests(),
            vec![
                ("GET".to_string(), "/mcp".to_string()),
                ("GET".to_string(), "/resource-metadata".to_string()),
                (
                    "GET".to_string(),
                    "/.well-known/oauth-authorization-server".to_string()
                ),
                ("POST".to_string(), "/register".to_string()),
                ("POST".to_string(), "/token".to_string()),
            ]
        );

        let registrations = server.registrations();
        assert_eq!(registrations.len(), 1);
        let registration = &registrations[0];
        assert_eq!(
            registration["client_name"].as_str(),
            Some("Yukinal Desktop")
        );
        assert_eq!(
            registration["token_endpoint_auth_method"].as_str(),
            Some("none")
        );
        assert_eq!(
            registration["grant_types"],
            json!(["authorization_code", "refresh_token"])
        );
        assert_eq!(registration["response_types"], json!(["code"]));
        assert_eq!(registration["scope"].as_str(), Some("mcp.read mcp.tools"));
        assert!(
            registration["redirect_uris"][0]
                .as_str()
                .is_some_and(
                    |uri| uri.starts_with("http://127.0.0.1:") && uri.ends_with("/callback")
                ),
            "dynamic registration must use the exact loopback callback: {registration}"
        );
        cleanup(&path);
    }

    #[tokio::test]
    async fn authorization_code_pkce_is_stored_and_refreshed() {
        let server = TestAuthorizationServer::start();
        let (path, database) = temp_db("flow");
        let endpoint = format!("http://{}/mcp", server.address);
        database
            .mcp_servers()
            .upsert(&McpServerConfig {
                id: "mcp_oauth".to_string(),
                label: "OAuth fixture".to_string(),
                transport: "http".to_string(),
                command: None,
                args: None,
                url: Some(endpoint.clone()),
                http_auth_headers: Vec::new(),
                oauth: Some(oauth_config(
                    server.issuer(),
                    "test-client",
                    McpOAuthFlow::AuthorizationCode,
                    &["mcp.read"],
                )),
                enabled: true,
                allowed_tools: Vec::new(),
                trust_level: "unreviewed".to_string(),
            })
            .expect("insert OAuth row");
        let credentials = Arc::new(MemoryCredentialStore::new());
        let result = connect(
            &database,
            credentials.clone(),
            "mcp_oauth",
            OutboundProxy::default(),
            browser_launcher(),
            noop_prompt(),
            CancellationToken::new(),
        )
        .await
        .expect("complete OAuth flow");
        assert_eq!(result.server_id, "mcp_oauth");
        assert_eq!(
            result.token_endpoint,
            format!("http://{}/token", server.address)
        );

        let row = database.mcp_servers().get("mcp_oauth").expect("stored row");
        let oauth = row.oauth.expect("OAuth config");
        let credential_ref =
            CredentialRef::parse(oauth.credential_ref.as_deref().expect("token reference"))
                .expect("credential reference");
        let bundle: OAuthTokenBundle = serde_json::from_str(
            credentials
                .get(&credential_ref)
                .expect("stored token")
                .as_utf8()
                .expect("UTF-8 token bundle")
                .as_ref(),
        )
        .expect("valid token bundle");
        assert_eq!(bundle.access_token, "access-one");

        let source = source_from_config(
            credentials.clone(),
            &McpOAuthSourceConfig {
                server_id: "mcp_oauth".to_string(),
                resource: endpoint,
                token_endpoint: oauth.token_endpoint.expect("token endpoint"),
                client_id: oauth.client_id,
                client_auth: oauth.client_auth,
                client_secret_ref: oauth.client_secret_ref,
                dpop_key_ref: oauth.dpop_key_ref,
                proxy: OutboundProxy::default(),
                scopes: oauth.scopes,
                credential_ref: oauth.credential_ref.expect("credential reference"),
            },
        )
        .expect("token source");
        assert_eq!(token_of(&source, false).await, "access-two");
        let refreshed_bundle: OAuthTokenBundle = serde_json::from_str(
            credentials
                .get(&credential_ref)
                .expect("refreshed token")
                .as_utf8()
                .expect("UTF-8 refreshed token")
                .as_ref(),
        )
        .expect("valid refreshed bundle");
        assert_eq!(refreshed_bundle.access_token, "access-two");
        assert_eq!(
            refreshed_bundle.refresh_token.as_deref(),
            Some("refresh-one")
        );
        assert_eq!(token_of(&source, false).await, "access-two");
        assert_eq!(
            server.requests(),
            vec![
                (
                    "GET".to_string(),
                    "/.well-known/oauth-authorization-server".to_string()
                ),
                ("POST".to_string(), "/token".to_string()),
                ("POST".to_string(), "/token".to_string()),
            ]
        );
        cleanup(&path);
    }

    /* ── P0-1：设备码流程（RFC 8628） ────────────────────────────────────────── */

    /// A device-flow row identical to the code-flow fixture except for `flow`.
    fn insert_device_row(
        database: &Database,
        server: &TestAuthorizationServer,
        client_id: &str,
    ) -> McpServerConfig {
        insert_row(
            database,
            server,
            McpOAuthFlow::DeviceCode,
            client_id,
            McpOAuthClientAuth::None,
            None,
        )
    }

    /// One stored OAuth row, with the client-authentication choice laid out explicitly.
    fn insert_row(
        database: &Database,
        server: &TestAuthorizationServer,
        flow: McpOAuthFlow,
        client_id: &str,
        client_auth: McpOAuthClientAuth,
        client_secret_ref: Option<&str>,
    ) -> McpServerConfig {
        let mut oauth = oauth_config(server.issuer(), client_id, flow, &["mcp.read"]);
        oauth.client_auth = client_auth;
        oauth.client_secret_ref = client_secret_ref.map(str::to_string);
        let row = McpServerConfig {
            id: "mcp_oauth".to_string(),
            label: "OAuth fixture".to_string(),
            transport: "http".to_string(),
            command: None,
            args: None,
            url: Some(format!("http://{}/mcp", server.address)),
            http_auth_headers: Vec::new(),
            oauth: Some(oauth),
            enabled: true,
            allowed_tools: Vec::new(),
            trust_level: "unreviewed".to_string(),
        };
        database
            .mcp_servers()
            .upsert(&row)
            .expect("insert OAuth row");
        row
    }

    /// A code-flow row that authenticates with a client secret, plus the stored secret.
    fn insert_secret_flow_row(
        database: &Database,
        server: &TestAuthorizationServer,
        credentials: &MemoryCredentialStore,
        flow: McpOAuthFlow,
        method: McpOAuthClientAuth,
        secret: &str,
    ) -> CredentialRef {
        let reference = credentials
            .set(
                "mcp",
                "oauth-client-secret",
                &Secret::from_utf8(secret.to_string()),
            )
            .expect("store the client secret");
        insert_row(
            database,
            server,
            flow,
            "confidential-client",
            method,
            Some(reference.to_string_ref().as_str()),
        );
        reference
    }

    type Prompts = Arc<Mutex<Vec<DeviceCodePrompt>>>;
    type Opened = Arc<Mutex<Vec<String>>>;

    /// 一行开启了 DPoP 的配置，外加一把真的存在凭据库里的密钥。
    fn insert_dpop_row(
        database: &Database,
        server: &TestAuthorizationServer,
        credentials: &MemoryCredentialStore,
    ) -> CredentialRef {
        let (_, encoded) = DpopKey::generate().expect("generate a DPoP key");
        let reference = credentials
            .set("mcp", "dpop-key", &Secret::from_utf8(encoded))
            .expect("store the DPoP key");
        let mut oauth = oauth_config(
            server.issuer(),
            "test-client",
            McpOAuthFlow::DeviceCode,
            &["mcp.read"],
        );
        oauth.dpop = true;
        oauth.dpop_key_ref = Some(reference.to_string_ref());
        let row = McpServerConfig {
            id: "mcp_oauth".to_string(),
            label: "OAuth fixture".to_string(),
            transport: "http".to_string(),
            command: None,
            args: None,
            url: Some(format!("http://{}/mcp", server.address)),
            http_auth_headers: Vec::new(),
            oauth: Some(oauth),
            enabled: true,
            allowed_tools: Vec::new(),
            trust_level: "unreviewed".to_string(),
        };
        database
            .mcp_servers()
            .upsert(&row)
            .expect("insert the DPoP row");
        reference
    }

    fn recording_prompt() -> (Prompts, impl FnOnce(DeviceCodePrompt) -> Result<(), String>) {
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let recorder = prompts.clone();
        let prompt = move |value: DeviceCodePrompt| {
            recorder.lock().expect("prompts").push(value);
            Ok(())
        };
        (prompts, prompt)
    }

    fn recording_launcher() -> (Opened, impl FnOnce(&str) -> Result<(), String>) {
        let opened = Arc::new(Mutex::new(Vec::new()));
        let recorder = opened.clone();
        let launcher = move |url: &str| {
            recorder.lock().expect("opened").push(url.to_string());
            Ok(())
        };
        (opened, launcher)
    }

    /// A stored OAuth configuration for the fixtures. Keeping it in one place means a new
    /// non-secret field does not have to be added to four literals by hand.
    fn oauth_config(
        issuer: String,
        client_id: &str,
        flow: McpOAuthFlow,
        scopes: &[&str],
    ) -> McpOAuthConfig {
        McpOAuthConfig {
            issuer,
            client_id: client_id.to_string(),
            flow,
            client_auth: McpOAuthClientAuth::None,
            client_secret_ref: None,
            dpop: false,
            dpop_key_ref: None,
            scopes: scopes.iter().map(|scope| scope.to_string()).collect(),
            token_endpoint: None,
            credential_ref: None,
        }
    }

    /// Build a token source from a stored row, so a test cannot forget a field the
    /// production path passes (which is exactly how a client-auth method would go missing).
    fn source_for_row(
        credentials: Arc<MemoryCredentialStore>,
        row: &McpServerConfig,
        resource: String,
    ) -> Arc<dyn McpOAuthTokenSource> {
        let oauth = row.oauth.clone().expect("OAuth config");
        source_from_config(
            credentials,
            &McpOAuthSourceConfig {
                server_id: row.id.clone(),
                resource,
                token_endpoint: oauth.token_endpoint.expect("token endpoint"),
                client_id: oauth.client_id,
                client_auth: oauth.client_auth,
                client_secret_ref: oauth.client_secret_ref,
                dpop_key_ref: oauth.dpop_key_ref,
                proxy: OutboundProxy::default(),
                scopes: oauth.scopes,
                credential_ref: oauth.credential_ref.expect("credential reference"),
            },
        )
        .expect("token source")
    }

    /// 取一次令牌。测试关心的是「拿到的是哪个 token」，不是 proof 的形状（那有专门的
    /// 用例），所以这里按 bearer 的路子取一次就够。
    async fn token_of(source: &Arc<dyn McpOAuthTokenSource>, force_refresh: bool) -> String {
        source
            .authorization(McpAuthorizationRequest {
                method: "POST".to_string(),
                url: "https://example.test/mcp".to_string(),
                force_refresh,
                nonce: None,
            })
            .await
            .expect("token")
            .token
    }

    #[tokio::test]
    async fn a_dpop_server_gets_a_verifiable_proof_and_a_nonce_retry_on_every_token_request() {
        let script = Arc::new(ServerScript::new());
        // 服务器要求 proof，并挑战一个 nonce：客户端必须先被拒一次，再带着它重来。
        script.require_dpop(Some("server-nonce"));
        let server = TestAuthorizationServer::with_script(script.clone());
        let (path, database) = temp_db("dpop-flow");
        let credentials = Arc::new(MemoryCredentialStore::new());
        let key = insert_dpop_row(&database, &server, credentials.as_ref());
        let (_, prompt) = recording_prompt();
        let (_, launcher) = recording_launcher();

        let result = connect(
            &database,
            credentials.clone(),
            "mcp_oauth",
            OutboundProxy::default(),
            launcher,
            prompt,
            CancellationToken::new(),
        )
        .await
        .expect("the device flow must complete against a DPoP server");
        assert_eq!(result.issuer, server.issuer());

        let row = database.mcp_servers().get("mcp_oauth").expect("row");
        let oauth = row.oauth.expect("oauth config");
        assert_eq!(
            oauth.dpop_key_ref.as_deref(),
            Some(key.to_string_ref().as_str()),
            "the key that signed the requests must be the one the row points at"
        );
        let reference = CredentialRef::parse(
            oauth
                .credential_ref
                .as_deref()
                .expect("the token must be stored"),
        )
        .expect("token reference");
        let stored: OAuthTokenBundle = serde_json::from_str(
            credentials
                .get(&reference)
                .expect("stored bundle")
                .as_utf8()
                .expect("UTF-8 bundle")
                .as_ref(),
        )
        .expect("valid bundle");
        assert_eq!(
            stored.token_type, "DPoP",
            "a DPoP-bound token must be stored as one"
        );

        // 每个 token 请求都被挑战了一次，并且**只**被挑战一次：带 nonce 的重发通过了。
        assert_eq!(
            script.challenges(),
            1,
            "one poll, one challenge, one retry — a second challenge would mean a loop"
        );
        cleanup(&path);
    }

    #[tokio::test]
    async fn a_bearer_token_is_refused_when_the_server_was_asked_for_dpop() {
        let script = Arc::new(ServerScript::new());
        script.require_dpop(None);
        script.force_token_type("Bearer");
        let server = TestAuthorizationServer::with_script(script.clone());
        let (path, database) = temp_db("dpop-bearer");
        let credentials = Arc::new(MemoryCredentialStore::new());
        insert_dpop_row(&database, &server, credentials.as_ref());
        let (_, prompt) = recording_prompt();
        let (_, launcher) = recording_launcher();

        let error = connect(
            &database,
            credentials.clone(),
            "mcp_oauth",
            OutboundProxy::default(),
            launcher,
            prompt,
            CancellationToken::new(),
        )
        .await
        .expect_err("a bearer token must not be adopted when DPoP was requested");
        assert!(
            error.contains("not a dpop token"),
            "the error must say what was wrong with the token, not just that it failed: {error}"
        );
        let row = database.mcp_servers().get("mcp_oauth").expect("row");
        assert!(
            row.oauth.expect("oauth config").credential_ref.is_none(),
            "a refused token must not be stored"
        );
        cleanup(&path);
    }

    #[test]
    fn a_missing_dpop_key_refuses_instead_of_falling_back_to_bearer() {
        let server = TestAuthorizationServer::start();
        let (path, database) = temp_db("dpop-missing-key");
        let credentials = Arc::new(MemoryCredentialStore::new());
        // 配置说要 DPoP，但凭据库里没有那把钥匙（换机器、被清理）：这时令牌已经用不了。
        let mut oauth = oauth_config(
            server.issuer(),
            "test-client",
            McpOAuthFlow::DeviceCode,
            &["mcp.read"],
        );
        oauth.dpop = true;
        oauth.dpop_key_ref = Some("keychain://mcp/does-not-exist".to_string());
        oauth.token_endpoint = Some(format!("http://{}/token", server.address));
        oauth.credential_ref = Some("keychain://mcp/token".to_string());
        let row = McpServerConfig {
            id: "mcp_oauth".to_string(),
            label: "OAuth fixture".to_string(),
            transport: "http".to_string(),
            command: None,
            args: None,
            url: Some(format!("http://{}/mcp", server.address)),
            http_auth_headers: Vec::new(),
            oauth: Some(oauth),
            enabled: true,
            allowed_tools: Vec::new(),
            trust_level: "unreviewed".to_string(),
        };
        database.mcp_servers().upsert(&row).expect("insert row");

        let config = McpOAuthSourceConfig {
            server_id: row.id.clone(),
            resource: row.url.clone().expect("endpoint"),
            token_endpoint: row
                .oauth
                .as_ref()
                .and_then(|oauth| oauth.token_endpoint.clone())
                .expect("token endpoint"),
            client_id: "test-client".to_string(),
            client_auth: McpOAuthClientAuth::None,
            client_secret_ref: None,
            dpop_key_ref: Some("keychain://mcp/does-not-exist".to_string()),
            proxy: OutboundProxy::default(),
            scopes: vec!["mcp.read".to_string()],
            credential_ref: "keychain://mcp/token".to_string(),
        };
        let error = source_from_config(credentials, &config)
            .err()
            .expect("a missing DPoP key must refuse the source");
        assert!(
            error.contains("DPoP key"),
            "the reason must name the key, not the token: {error}"
        );
        cleanup(&path);
    }

    #[tokio::test]
    async fn the_device_flow_shows_a_user_code_then_stores_a_refreshable_bundle() {
        let device = Arc::new(ServerScript::with_steps(&[
            DeviceStep::Pending,
            DeviceStep::Grant,
        ]));
        let server = TestAuthorizationServer::with_script(device.clone());
        let (path, database) = temp_db("device-flow");
        let endpoint = format!("http://{}/mcp", server.address);
        insert_device_row(&database, &server, "test-client");
        let credentials = Arc::new(MemoryCredentialStore::new());
        let (prompts, prompt) = recording_prompt();
        let (opened, launcher) = recording_launcher();

        let result = connect(
            &database,
            credentials.clone(),
            "mcp_oauth",
            OutboundProxy::default(),
            launcher,
            prompt,
            CancellationToken::new(),
        )
        .await
        .expect("the device flow must complete");

        assert_eq!(result.issuer, server.issuer());
        assert_eq!(
            result.token_endpoint,
            format!("http://{}/token", server.address)
        );

        // The user code and where to type it: shown exactly once, with a real deadline.
        let prompts = prompts.lock().expect("prompts").clone();
        assert_eq!(prompts.len(), 1, "the code must be announced once");
        assert_eq!(prompts[0].user_code, "WDJB-MJHT");
        assert_eq!(
            prompts[0].verification_uri,
            format!("http://{}/activate", server.address)
        );
        assert!(
            prompts[0].expires_at > unix_now(),
            "an expiry in the past would tell the UI to stop waiting immediately"
        );
        assert_eq!(
            opened.lock().expect("opened").as_slice(),
            [format!(
                "http://{}/activate?user_code=WDJB-MJHT",
                server.address
            )],
            "the complete URI opens directly; the plain one is what the user types into"
        );

        // The stored bundle has the same shape the code flow stores, so the refresh path
        // cannot behave differently depending on which flow produced it.
        let oauth = database
            .mcp_servers()
            .get("mcp_oauth")
            .expect("stored row")
            .oauth
            .expect("OAuth config");
        assert_eq!(oauth.flow, McpOAuthFlow::DeviceCode);
        assert_eq!(oauth.client_id, "test-client");
        let credential_ref =
            CredentialRef::parse(oauth.credential_ref.as_deref().expect("reference"))
                .expect("credential reference");
        let bundle: OAuthTokenBundle = serde_json::from_str(
            credentials
                .get(&credential_ref)
                .expect("stored bundle")
                .as_utf8()
                .expect("UTF-8 bundle")
                .as_ref(),
        )
        .expect("valid bundle");
        assert_eq!(bundle.access_token, "device-access");
        assert_eq!(bundle.refresh_token.as_deref(), Some("device-refresh"));

        let row = database.mcp_servers().get("mcp_oauth").expect("row");
        let source = source_for_row(credentials.clone(), &row, endpoint);
        assert_eq!(token_of(&source, false).await, "access-two");

        let polls = device.polls();
        assert_eq!(polls.len(), 2, "one poll per scripted answer");
        assert!(
            polls[1] - polls[0] >= DEVICE_MIN_POLL.as_millis() as u64,
            "the client must wait between polls instead of spinning: {polls:?}"
        );
        assert_eq!(
            server.requests(),
            vec![
                (
                    "GET".to_string(),
                    "/.well-known/oauth-authorization-server".to_string()
                ),
                ("POST".to_string(), "/device".to_string()),
                ("POST".to_string(), "/token".to_string()),
                ("POST".to_string(), "/token".to_string()),
                ("POST".to_string(), "/token".to_string()),
            ],
            "device authorization, then one poll per answer, then the refresh"
        );
        cleanup(&path);
    }

    #[tokio::test]
    async fn slow_down_keeps_waiting_and_the_deadline_ends_the_flow() {
        // Two properties in one fast fixture. `slow_down` must not be fatal: the flow polls
        // again instead of aborting. And the wait it asks for is *bounded by the code's own
        // lifetime*: a one-second `expires_in` means the shortened wait happens and then the
        // flow stops, rather than polling forever. The exact +5 s the RFC specifies is
        // pinned by `slow_down_adds_five_seconds` below, since asserting it here would mean
        // a five-second test for an arithmetic constant.
        let device = Arc::new(ServerScript::with_steps(&[
            DeviceStep::SlowDown,
            DeviceStep::Pending,
        ]));
        *device.lifetime.lock().expect("lifetime") = 1;
        let server = TestAuthorizationServer::with_script(device.clone());
        let (path, database) = temp_db("device-slow-down");
        insert_device_row(&database, &server, "test-client");
        let credentials = Arc::new(MemoryCredentialStore::new());

        let error = connect(
            &database,
            credentials,
            "mcp_oauth",
            OutboundProxy::default(),
            noop_launcher(),
            noop_prompt(),
            CancellationToken::new(),
        )
        .await
        .expect_err("a code that is never approved must end the flow");

        assert!(
            error.contains("expired"),
            "the deadline must be reported as expiry, not as a slow_down failure: {error}"
        );
        assert_eq!(
            device.polls().len(),
            2,
            "the poll after slow_down must happen; the one after the deadline must not"
        );
        assert!(database
            .mcp_servers()
            .get("mcp_oauth")
            .expect("row")
            .oauth
            .expect("OAuth config")
            .credential_ref
            .is_none());
        cleanup(&path);
    }

    #[tokio::test]
    async fn a_refusal_and_an_expired_code_are_reported_as_themselves() {
        for (step, expected) in [
            (DeviceStep::Denied, "access_denied"),
            (DeviceStep::Expired, "expired_token"),
        ] {
            let device = Arc::new(ServerScript::with_steps(&[step]));
            let server = TestAuthorizationServer::with_script(device);
            let (path, database) = temp_db("device-terminal");
            insert_device_row(&database, &server, "test-client");
            let credentials = Arc::new(MemoryCredentialStore::new());

            let error = connect(
                &database,
                credentials.clone(),
                "mcp_oauth",
                OutboundProxy::default(),
                noop_launcher(),
                noop_prompt(),
                CancellationToken::new(),
            )
            .await
            .expect_err("a terminal device answer must fail the flow");

            assert!(error.contains(expected), "{step:?}: {error}");
            let oauth = database
                .mcp_servers()
                .get("mcp_oauth")
                .expect("row")
                .oauth
                .expect("OAuth config");
            assert!(
                oauth.credential_ref.is_none() && oauth.token_endpoint.is_none(),
                "{step:?}: a failed flow must not leave a half-connected row"
            );
            cleanup(&path);
        }
    }

    #[tokio::test]
    async fn cancelling_a_device_flow_stops_the_polling_and_stores_nothing() {
        let device = Arc::new(ServerScript::with_steps(&[DeviceStep::Pending]));
        let server = TestAuthorizationServer::with_script(device.clone());
        let (path, database) = temp_db("device-cancel");
        insert_device_row(&database, &server, "test-client");
        let credentials = Arc::new(MemoryCredentialStore::new());
        let cancel = CancellationToken::new();

        let flow = connect(
            &database,
            credentials.clone(),
            "mcp_oauth",
            OutboundProxy::default(),
            noop_launcher(),
            noop_prompt(),
            cancel.clone(),
        );
        tokio::pin!(flow);
        let polled = async {
            while device.polls().is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::select! {
            _ = polled => {}
            result = &mut flow => panic!("the flow ended before it could be cancelled: {result:?}"),
        }

        cancel.cancel();
        let error = flow
            .await
            .expect_err("a cancelled flow must not report success");
        assert_eq!(error, CANCELLED_FLOW);

        // The claim is "no background poller", so it has to survive a wait: were the loop
        // detached, this is where it would keep asking.
        let polls = device.polls().len();
        tokio::time::sleep(4 * DEVICE_MIN_POLL).await;
        assert_eq!(
            device.polls().len(),
            polls,
            "a cancelled flow must not keep polling"
        );
        // The row is the observable half: a credential nothing points at cannot be read
        // back (the store has no enumeration), so "no token was kept" means no reference.
        assert!(
            database
                .mcp_servers()
                .get("mcp_oauth")
                .expect("row")
                .oauth
                .expect("OAuth config")
                .credential_ref
                .is_none(),
            "a cancelled flow must not leave a token reference behind"
        );
        cleanup(&path);
    }

    #[tokio::test]
    async fn a_server_without_a_device_endpoint_refuses_instead_of_falling_back() {
        let server = TestAuthorizationServer::start();
        *server.script().supported.lock().expect("supported") = false;
        let (path, database) = temp_db("device-unsupported");
        insert_device_row(&database, &server, "test-client");
        let credentials = Arc::new(MemoryCredentialStore::new());

        let error = connect(
            &database,
            credentials,
            "mcp_oauth",
            OutboundProxy::default(),
            noop_launcher(),
            noop_prompt(),
            CancellationToken::new(),
        )
        .await
        .expect_err("a server without the endpoint must refuse the device flow");

        assert!(error.contains("device_authorization_endpoint"), "{error}");
        assert!(
            error.contains("authorization code"),
            "the refusal must name the alternative: {error}"
        );
        assert_eq!(
            server.requests(),
            vec![(
                "GET".to_string(),
                "/.well-known/oauth-authorization-server".to_string()
            )],
            "no device request may be attempted against a server that cannot serve one"
        );
        cleanup(&path);
    }

    #[tokio::test]
    async fn a_registration_that_answers_with_a_secret_is_refused_not_downgraded() {
        // The user configured no secret and left the client id empty, which asks for a public
        // client. A server that answers with a secret is telling us it did not register one —
        // adopting the value would silently make this a confidential client that cannot keep
        // its credential, so the flow stops instead.
        let script = Arc::new(ServerScript::new());
        *script.registration_secret.lock().expect("secret") = Some("server-issued".to_string());
        let server = TestAuthorizationServer::with_script(script);
        let (path, database) = temp_db("registration-secret");
        insert_row(
            &database,
            &server,
            McpOAuthFlow::AuthorizationCode,
            "",
            McpOAuthClientAuth::None,
            None,
        );
        let credentials = Arc::new(MemoryCredentialStore::new());

        let error = connect(
            &database,
            credentials,
            "mcp_oauth",
            OutboundProxy::default(),
            noop_launcher(),
            noop_prompt(),
            CancellationToken::new(),
        )
        .await
        .expect_err("a public client must refuse a secret it cannot protect");

        assert!(
            error.contains("client secret"),
            "the refusal must name what was issued: {error}"
        );
        assert_eq!(
            server.requests(),
            vec![
                (
                    "GET".to_string(),
                    "/.well-known/oauth-authorization-server".to_string()
                ),
                ("POST".to_string(), "/register".to_string()),
            ],
            "the refusal happens at registration: no token request may follow"
        );
        let oauth = database
            .mcp_servers()
            .get("mcp_oauth")
            .expect("row")
            .oauth
            .expect("OAuth config");
        assert_eq!(oauth.client_auth, McpOAuthClientAuth::None);
        assert!(oauth.client_secret_ref.is_none());
        cleanup(&path);
    }

    #[tokio::test]
    async fn an_empty_client_id_registers_a_device_grant_public_client() {
        let server = TestAuthorizationServer::start();
        server
            .script()
            .expect_client("dynamic-client", None, ClientAuthVia::Body);
        let (path, database) = temp_db("device-registration");
        insert_device_row(&database, &server, "");
        let credentials = Arc::new(MemoryCredentialStore::new());

        connect(
            &database,
            credentials,
            "mcp_oauth",
            OutboundProxy::default(),
            noop_launcher(),
            noop_prompt(),
            CancellationToken::new(),
        )
        .await
        .expect("dynamic registration must complete the device flow");

        let registrations = server.registrations();
        assert_eq!(registrations.len(), 1);
        assert_eq!(
            registrations[0]["grant_types"],
            json!([DEVICE_CODE_GRANT, "refresh_token"])
        );
        assert!(
            registrations[0].get("redirect_uris").is_none(),
            "a device client has no callback to register: {}",
            registrations[0]
        );
        assert_eq!(
            registrations[0]["token_endpoint_auth_method"].as_str(),
            Some("none"),
            "the device flow is still a public client"
        );
        assert_eq!(
            database
                .mcp_servers()
                .get("mcp_oauth")
                .expect("row")
                .oauth
                .expect("OAuth config")
                .client_id,
            "dynamic-client"
        );
        cleanup(&path);
    }

    /* ── P0-2：客户端密钥（client_secret_post / client_secret_basic） ────────── */

    #[test]
    fn the_test_servers_base64_decoder_matches_the_rfc_7617_example() {
        // RFC 7617 §2: "Aladdin:open sesame" → QWxhZGRpbjpvcGVuIHNlc2FtZQ==
        assert_eq!(
            decode_base64("QWxhZGRpbjpvcGVuIHNlc2FtZQ==").expect("base64"),
            b"Aladdin:open sesame".to_vec()
        );
        assert_eq!(
            decode_base64("dGVzdC1jbGllbnQ6czNjcmV0").expect("base64"),
            b"test-client:s3cret".to_vec()
        );
        assert!(
            decode_base64("not base64!").is_none(),
            "a header that is not base64 must not decode into something plausible"
        );
    }

    /// Both secret methods run the same flow; only the place the credentials travel differs.
    async fn assert_secret_flow(method: McpOAuthClientAuth, via: ClientAuthVia, name: &str) {
        let server = TestAuthorizationServer::start();
        server
            .script()
            .expect_client("confidential-client", Some("s3cret-value"), via);
        let (path, database) = temp_db(name);
        let credentials = Arc::new(MemoryCredentialStore::new());
        let secret_reference = insert_secret_flow_row(
            &database,
            &server,
            &credentials,
            McpOAuthFlow::AuthorizationCode,
            method,
            "s3cret-value",
        );

        connect(
            &database,
            credentials.clone(),
            "mcp_oauth",
            OutboundProxy::default(),
            browser_launcher(),
            noop_prompt(),
            CancellationToken::new(),
        )
        .await
        .expect("a confidential client must be able to complete the code flow");

        let row = database.mcp_servers().get("mcp_oauth").expect("stored row");
        let oauth = row.oauth.clone().expect("OAuth config");
        assert_eq!(oauth.client_auth, method);
        assert_eq!(
            oauth.client_secret_ref.as_deref(),
            Some(secret_reference.to_string_ref().as_str()),
            "the row must keep pointing at the stored secret"
        );

        // The refresh is a second, independent token request: it authenticates the same way,
        // or the server sees an anonymous renewal of a confidential client. The fixture fails
        // the request if the credentials are missing or in the wrong place.
        let source = source_for_row(
            credentials.clone(),
            &row,
            format!("http://{}/mcp", server.address),
        );
        assert_eq!(token_of(&source, false).await, "access-two");
        assert_eq!(
            server
                .requests()
                .iter()
                .filter(|(_, target)| target == "/token")
                .count(),
            2,
            "one code exchange and one refresh"
        );
        cleanup(&path);
    }

    #[tokio::test]
    async fn client_secret_post_authenticates_the_exchange_and_the_refresh() {
        assert_secret_flow(
            McpOAuthClientAuth::ClientSecretPost,
            ClientAuthVia::Body,
            "client-secret-post",
        )
        .await;
    }

    #[tokio::test]
    async fn client_secret_basic_authenticates_the_exchange_and_the_refresh() {
        assert_secret_flow(
            McpOAuthClientAuth::ClientSecretBasic,
            ClientAuthVia::Header,
            "client-secret-basic",
        )
        .await;
    }

    #[tokio::test]
    async fn a_secret_method_without_a_stored_secret_refuses_before_any_token_request() {
        let server = TestAuthorizationServer::start();
        let (path, database) = temp_db("client-secret-missing");
        let credentials = Arc::new(MemoryCredentialStore::new());
        insert_row(
            &database,
            &server,
            McpOAuthFlow::AuthorizationCode,
            "confidential-client",
            McpOAuthClientAuth::ClientSecretPost,
            None,
        );

        let error = connect(
            &database,
            credentials,
            "mcp_oauth",
            OutboundProxy::default(),
            noop_launcher(),
            noop_prompt(),
            CancellationToken::new(),
        )
        .await
        .expect_err("a confidential client without its secret cannot authenticate");

        assert!(
            error.contains("secret") && error.contains("enter it again"),
            "the refusal must say what is missing and what to do: {error}"
        );
        assert_eq!(
            server.requests(),
            vec![(
                "GET".to_string(),
                "/.well-known/oauth-authorization-server".to_string()
            )],
            "discovery is the only request: nothing may be sent without the credential"
        );
        cleanup(&path);
    }

    #[tokio::test]
    async fn the_device_flow_authenticates_the_device_authorization_and_every_poll() {
        let script = Arc::new(ServerScript::with_steps(&[
            DeviceStep::Pending,
            DeviceStep::Grant,
        ]));
        script.expect_client(
            "confidential-client",
            Some("s3cret-value"),
            ClientAuthVia::Body,
        );
        let server = TestAuthorizationServer::with_script(script.clone());
        let (path, database) = temp_db("device-client-secret");
        let credentials = Arc::new(MemoryCredentialStore::new());
        insert_secret_flow_row(
            &database,
            &server,
            &credentials,
            McpOAuthFlow::DeviceCode,
            McpOAuthClientAuth::ClientSecretPost,
            "s3cret-value",
        );

        connect(
            &database,
            credentials,
            "mcp_oauth",
            OutboundProxy::default(),
            noop_launcher(),
            noop_prompt(),
            CancellationToken::new(),
        )
        .await
        .expect("a confidential client must be able to complete the device flow");

        assert_eq!(
            server.requests(),
            vec![
                (
                    "GET".to_string(),
                    "/.well-known/oauth-authorization-server".to_string()
                ),
                ("POST".to_string(), "/device".to_string()),
                ("POST".to_string(), "/token".to_string()),
                ("POST".to_string(), "/token".to_string()),
            ],
            "the device authorization and each poll must carry the client credentials"
        );
        cleanup(&path);
    }

    #[test]
    fn a_server_interval_is_honoured_but_bounded_at_both_ends() {
        assert_eq!(
            device_poll_interval(None),
            Duration::from_secs(DEVICE_DEFAULT_POLL_SECONDS),
            "RFC 8628's default is five seconds"
        );
        assert_eq!(device_poll_interval(Some(12)), Duration::from_secs(12));
        assert_eq!(
            device_poll_interval(Some(0)),
            DEVICE_MIN_POLL,
            "\"poll immediately\" must still not be a hot loop"
        );
        assert_eq!(
            device_poll_interval(Some(9_999)),
            DEVICE_MAX_POLL,
            "a server cannot make one click wait for hours"
        );
    }

    #[test]
    fn slow_down_adds_five_seconds() {
        assert_eq!(
            apply_slow_down(Duration::from_millis(250)),
            Duration::from_millis(5_250)
        );
        assert_eq!(
            apply_slow_down(Duration::from_secs(5)),
            Duration::from_secs(10)
        );
        assert_eq!(
            apply_slow_down(DEVICE_MAX_POLL),
            DEVICE_MAX_POLL,
            "the back-off is capped, not unbounded"
        );
    }

    #[test]
    fn only_the_complete_verification_uri_may_carry_a_query() {
        assert_eq!(
            validate_verification_uri_complete(
                "mcp_1",
                "https://auth.example.com/device?user_code=WDJB-MJHT"
            )
            .expect("the code lives in the query"),
            "https://auth.example.com/device?user_code=WDJB-MJHT"
        );
        // Everything else about the origin follows the rules every other OAuth URL follows.
        for rejected in [
            "http://auth.example.com/device?user_code=x",
            "https://user:pass@auth.example.com/device?user_code=x",
            "https://auth.example.com/device?user_code=x#fragment",
        ] {
            assert!(
                validate_verification_uri_complete("mcp_1", rejected).is_err(),
                "{rejected} must be refused"
            );
        }
    }

    async fn send_callback_request(address: SocketAddr, target: &str) -> Vec<u8> {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        use tokio::net::TcpStream as TokioTcpStream;

        let mut stream = TokioTcpStream::connect(address)
            .await
            .expect("connect callback listener");
        let request =
            format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write callback request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read callback response");
        response
    }

    #[tokio::test]
    async fn authorization_error_callback_requires_matching_state() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind callback listener");
        let address = listener.local_addr().expect("callback address");
        let client = tokio::spawn(send_callback_request(
            address,
            "/callback?error=access_denied&state=attacker",
        ));

        let error = wait_for_callback(listener, "expected", &CancellationToken::new())
            .await
            .expect_err("a callback with the wrong state must be rejected");
        assert_eq!(error, "OAuth callback state did not match");
        let response = client.await.expect("callback client task");
        assert!(response.starts_with(b"HTTP/1.1 400"));
    }

    #[tokio::test]
    async fn authorization_error_callback_sanitizes_untrusted_details() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind callback listener");
        let address = listener.local_addr().expect("callback address");
        let client = tokio::spawn(send_callback_request(
            address,
            "/callback?error=access_denied&error_description=bad%0Aline%00&state=expected",
        ));

        let error = wait_for_callback(listener, "expected", &CancellationToken::new())
            .await
            .expect_err("an OAuth error callback must finish the flow");
        assert_eq!(error, "OAuth authorization failed: access_denied: badline");
        assert!(error.chars().all(|character| !character.is_control()));
        let response = client.await.expect("callback client task");
        assert!(response.starts_with(b"HTTP/1.1 400"));
    }
}
