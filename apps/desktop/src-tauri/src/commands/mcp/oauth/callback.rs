//! Bounded loopback callback server for OAuth authorization-code flow.
//!
//! This module accepts only a small, local HTTP request and returns a validated code; it
//! never stores tokens and it cannot outlive the awaiting OAuth command.

use super::*;

pub(super) async fn wait_for_callback(
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

pub(super) async fn read_bounded_body(
    response: &mut Response,
    limit: usize,
) -> Result<Vec<u8>, String> {
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
