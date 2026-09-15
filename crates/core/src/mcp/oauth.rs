//! OAuth token source boundary for Streamable HTTP MCP endpoints.
//!
//! The MCP core owns when a token must be fetched or refreshed; the desktop host
//! owns the credential store, authorization flow and HTTP token endpoint calls.
//! Keeping that split here avoids teaching `yukinal-core` about keychains or browser
//! windows while still allowing every MCP request to ask for a fresh bearer token.

use std::future::Future;
use std::pin::Pin;

use yukinal_database::models::McpOAuthClientAuth;
use yukinal_net::OutboundProxy;

pub type McpOAuthFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

/// One request's authentication material, resolved per attempt.
///
/// The method and URL travel with the request because a DPoP proof (RFC 9449) signs
/// both: the same token must produce a *different* proof for `POST /mcp` and
/// `DELETE /mcp`, and the server rejects one minted for the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpAuthorizationRequest {
    /// Upper-case HTTP method, as the proof's `htm` claim spells it.
    pub method: String,
    /// Absolute request URL. The proof's `htu` drops query and fragment.
    pub url: String,
    /// Used after a `401 Unauthorized`: proactive expiry checks can race server-side
    /// revocation, so the HTTP transport gets one chance to replace the token before
    /// reporting the request as failed.
    pub force_refresh: bool,
    /// The `DPoP-Nonce` the server sent with its challenge, if this attempt is a retry.
    pub nonce: Option<String>,
}

/// How the transport presents the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpAuthScheme {
    /// `Authorization: Bearer <token>`; nothing else is sent.
    Bearer,
    /// `Authorization: DPoP <token>` plus a per-request `DPoP` proof.
    Dpop,
}

/// What goes on the wire for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpAuthorization {
    pub scheme: McpAuthScheme,
    pub token: String,
    /// The proof JWT; present exactly when `scheme` is [`McpAuthScheme::Dpop`].
    pub proof: Option<String>,
}

/// A dynamic token source.
pub trait McpOAuthTokenSource: Send + Sync + 'static {
    fn authorization<'a>(
        &'a self,
        request: McpAuthorizationRequest,
    ) -> McpOAuthFuture<'a, McpAuthorization>;
}

/// Host-side inputs needed to construct a token source after OAuth discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpOAuthSourceConfig {
    pub server_id: String,
    /// Canonical MCP resource URI sent with authorization and token requests.
    pub resource: String,
    pub token_endpoint: String,
    pub client_id: String,
    /// How the client authenticates at `token_endpoint`.
    ///
    /// Carried here rather than hard-coded to "public client" because the host is the only
    /// side that can read the secret: the source resolves `client_secret_ref` through the
    /// credential store on each request, and this is what tells it whether to.
    pub client_auth: McpOAuthClientAuth,
    /// Credential-store reference for the client secret; `None` for a public client.
    pub client_secret_ref: Option<String>,
    /// Credential-store reference for the DPoP private key; `None` when the server does
    /// not ask for sender-constrained tokens.
    pub dpop_key_ref: Option<String>,
    /// 出站代理（ADR 0022）：discovery、授权与 token 请求都走它。
    pub proxy: OutboundProxy,
    pub scopes: Vec<String>,
    pub credential_ref: String,
}
