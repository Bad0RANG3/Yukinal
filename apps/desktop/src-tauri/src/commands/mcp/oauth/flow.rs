//! OAuth authorization-code and device-code flow orchestration.
//!
//! Both flows share one context so the sequence of discovery, registration, user presence
//! and token exchange remains explicit without spreading ten parameters across callers.

use super::*;

/// What an authorization flow reads and writes while it runs.
///
/// Gathered into one place because the alternative is a ten-argument function whose call
/// site cannot be read: the interesting part of a device-code flow is the *sequence* of
/// requests, and that is what should be visible at the call site.
pub(super) struct FlowContext<'a> {
    pub(super) client: &'a Client,
    pub(super) proxy: &'a OutboundProxy,
    pub(super) database: &'a Database,
    pub(super) credentials: &'a dyn CredentialStore,
    pub(super) server_id: &'a str,
    /// Canonical resource URI sent with every authorization and token request.
    pub(super) resource: &'a str,
    pub(super) metadata: &'a AuthorizationServerMetadata,
    /// The row being connected; the flow writes a registered client id and the token
    /// reference back into it.
    pub(super) row: &'a mut McpServerConfig,
    pub(super) oauth: &'a mut McpOAuthConfig,
    /// 发送方约束用的密钥；`None` 表示这台服务器只要 bearer（ADR 0018）。
    pub(super) dpop: Option<&'a DpopKey>,
    pub(super) nonces: &'a DpopNonces,
}

/// The client id to use, registering a public client when the configuration has none.
///
/// The registration is persisted before the flow continues because it is the one part of
/// an authorization that must survive the attempt: if the user abandons the browser (or
/// the device-code page) and starts again, re-registering would leave the abandoned
/// client id registered at the server with nothing pointing at it.
pub(super) async fn resolve_client_id(
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
pub(super) async fn device_code_flow(
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
pub(super) async fn request_device_authorization(
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
pub(super) async fn poll_device_token(
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
pub(super) fn device_poll_interval(interval_seconds: Option<u64>) -> Duration {
    Duration::from_secs(interval_seconds.unwrap_or(DEVICE_DEFAULT_POLL_SECONDS))
        .clamp(DEVICE_MIN_POLL, DEVICE_MAX_POLL)
}

/// RFC 8628 §3.5: `slow_down` means the next poll must be at least 5 seconds later.
pub(super) fn apply_slow_down(interval: Duration) -> Duration {
    (interval + DEVICE_SLOW_DOWN_STEP).min(DEVICE_MAX_POLL)
}

/// `verification_uri_complete` is the one OAuth URL that legitimately carries a query:
/// the code lives there. Everything else about it follows the same origin rules.
pub(super) fn validate_verification_uri_complete(
    server_id: &str,
    raw: &str,
) -> Result<String, String> {
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
pub(super) async fn store_connected_bundle(
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
