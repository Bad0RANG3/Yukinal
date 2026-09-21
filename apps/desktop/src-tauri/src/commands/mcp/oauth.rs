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

mod callback;
mod flow;
use callback::*;
use flow::*;

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
mod tests;
