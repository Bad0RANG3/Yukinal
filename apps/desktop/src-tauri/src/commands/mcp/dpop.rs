//! DPoP 的密钥与 proof（RFC 9449），只服务于 MCP 的 OAuth 路径。
//!
//! 三件事在这里，别的都不在：**生成/读取密钥**（私钥只进系统凭据库）、**为一次请求签一个
//! proof**、以及**按 origin 记住服务器给的 nonce**。设计取舍见 ADR 0018。
//!
//! 为什么是 Ed25519：`ring` 已经在构建图里（rustls 用它做 TLS），JWK 只有 `kty`/`crv`/`x`
//! 三个短字段，签名是确定性的；P-256 要多两个坐标与 ASN.1 签名编码，出错的地方更多。

use std::collections::HashMap;
use std::sync::Mutex;

use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use serde_json::json;
use yukinal_credentials::{CredentialRef, CredentialStore};

use super::oauth::{base64url, random_base64url, unix_now};

pub(super) struct DpopKey {
    pair: Ed25519KeyPair,
    /// 公钥的 base64url（JWK 的 `x`），每次签 proof 都要写进头部。
    public: String,
}

impl DpopKey {
    /// 新生成一把密钥，返回它和要存进凭据库的编码形式。
    ///
    /// 编码是 PKCS#8（`ring` 自己的格式），base64url 之后当普通字符串存 —— 凭据库只认字节，
    /// 而这段字节只有在 `from_pkcs8` 里才有意义。
    pub(super) fn generate() -> Result<(Self, String), String> {
        let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .map_err(|_| "could not generate a DPoP key".to_string())?;
        let encoded = base64url(document.as_ref());
        let key = Self::from_pkcs8(&encoded)?;
        Ok((key, encoded))
    }

    fn from_pkcs8(encoded: &str) -> Result<Self, String> {
        let bytes = decode_base64url(encoded)
            .ok_or_else(|| "the stored DPoP key is not valid base64url".to_string())?;
        let pair = Ed25519KeyPair::from_pkcs8(&bytes)
            .map_err(|_| "the stored DPoP key is not a usable Ed25519 key".to_string())?;
        let public = base64url(pair.public_key().as_ref());
        Ok(Self { pair, public })
    }

    /// 从凭据库读回一把密钥。
    ///
    /// 读不出来就是**失败**，不是「那就退回 bearer」：已经绑定的令牌离开这把密钥根本用不了，
    /// 唯一诚实的出路是让用户重新授权。
    pub(super) fn load(credentials: &dyn CredentialStore, reference: &str) -> Result<Self, String> {
        let reference = CredentialRef::parse(reference)
            .map_err(|error| format!("invalid DPoP key reference: {error}"))?;
        let secret = credentials
            .get(&reference)
            .map_err(|error| format!("could not read the DPoP key: {error}"))?;
        let encoded = secret
            .as_utf8()
            .map_err(|error| format!("the DPoP key is not UTF-8: {error}"))?;
        Self::from_pkcs8(encoded.as_ref().trim())
    }

    /// 为一次请求签一个 proof。
    ///
    /// `htu` 只取 scheme/host/path（RFC 9449 §4.2 要求去掉 query 与 fragment），`htm` 是
    /// 大写方法；带 access token 的请求还要 `ath`（令牌的 SHA-256），token 端点请求不带。
    /// `jti` 每次都是新的 128 位随机值：我们不复用自己的 proof。
    pub(super) fn proof(
        &self,
        method: &str,
        url: &str,
        nonce: Option<&str>,
        access_token: Option<&str>,
    ) -> Result<String, String> {
        let header = json!({
            "typ": "dpop+jwt",
            "alg": "EdDSA",
            "jwk": { "kty": "OKP", "crv": "Ed25519", "x": self.public },
        });
        let mut claims = serde_json::Map::new();
        claims.insert("jti".to_string(), json!(random_base64url(16)));
        claims.insert("htm".to_string(), json!(method.to_ascii_uppercase()));
        claims.insert("htu".to_string(), json!(proof_target(url)?));
        claims.insert("iat".to_string(), json!(unix_now()));
        if let Some(nonce) = nonce {
            claims.insert("nonce".to_string(), json!(nonce));
        }
        if let Some(token) = access_token {
            claims.insert("ath".to_string(), json!(token_hash(token)));
        }

        let signing_input = format!(
            "{}.{}",
            base64url(&serde_json::to_vec(&header).map_err(encode_error)?),
            base64url(
                &serde_json::to_vec(&serde_json::Value::Object(claims)).map_err(encode_error)?
            ),
        );
        let signature = self.pair.sign(signing_input.as_bytes());
        Ok(format!("{signing_input}.{}", base64url(signature.as_ref())))
    }
}

/// `htu` 的规范形式：scheme://host[:port]/path，去掉 query 与 fragment。
fn proof_target(url: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|error| format!("the DPoP target is not a URL: {error}"))?;
    let mut target = parsed.clone();
    target.set_query(None);
    target.set_fragment(None);
    Ok(target.as_str().to_string())
}

/// RFC 9449 §4.2 的 `ath`：access token 的 SHA-256，base64url 无填充。
fn token_hash(token: &str) -> String {
    base64url(ring::digest::digest(&ring::digest::SHA256, token.as_bytes()).as_ref())
}

fn encode_error(error: serde_json::Error) -> String {
    format!("could not encode the DPoP proof: {error}")
}

/// The base64url alphabet in reverse: used to load a key back, and by the test fixture
/// that plays the server side of a DPoP exchange.
pub(crate) fn decode_base64url(value: &str) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(value.len() * 3 / 4);
    let mut buffer = 0_u32;
    let mut bits = 0_u32;
    for byte in value.bytes() {
        if byte == b'=' {
            continue;
        }
        let digit = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        } as u32;
        buffer = (buffer << 6) | digit;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
        }
    }
    Some(output)
}

/// 服务器给的 nonce，按 origin 记着。
///
/// 只活在内存里：nonce 是服务器的即时挑战，写进数据库只会让一份过期值活得更久。
#[derive(Default)]
pub(super) struct DpopNonces {
    values: Mutex<HashMap<String, String>>,
}

impl DpopNonces {
    pub(super) fn get(&self, url: &str) -> Option<String> {
        let origin = origin_of(url)?;
        lock(&self.values).get(&origin).cloned()
    }

    pub(super) fn remember(&self, url: &str, nonce: &str) {
        let Some(origin) = origin_of(url) else {
            return;
        };
        lock(&self.values).insert(origin, nonce.to_string());
    }
}

fn origin_of(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    Some(parsed.origin().ascii_serialization())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proof_carries_the_public_key_and_the_request_it_signs() {
        let (key, encoded) = DpopKey::generate().expect("generate");
        let proof = key
            .proof(
                "post",
                "https://example.test/mcp?x=1#frag",
                Some("n-1"),
                Some("token"),
            )
            .expect("proof");
        let parts: Vec<&str> = proof.split('.').collect();
        assert_eq!(parts.len(), 3, "a JWS is header.claims.signature");

        let header: serde_json::Value =
            serde_json::from_slice(&decode_base64url(parts[0]).expect("header")).expect("json");
        assert_eq!(header["typ"], "dpop+jwt");
        assert_eq!(header["alg"], "EdDSA");
        assert_eq!(header["jwk"]["crv"], "Ed25519");
        assert_eq!(header["jwk"]["kty"], "OKP");

        // 私钥在凭据库里的编码能读回同一把公钥；读回来的密钥也能签出可验证的 proof。
        let reloaded = DpopKey::from_pkcs8(&encoded).expect("reload");
        assert_eq!(reloaded.public, key.public);

        let claims: serde_json::Value =
            serde_json::from_slice(&decode_base64url(parts[1]).expect("claims")).expect("json");
        assert_eq!(claims["htm"], "POST");
        assert_eq!(claims["htu"], "https://example.test/mcp");
        assert_eq!(claims["nonce"], "n-1");
        assert_eq!(claims["ath"], token_hash("token"));
        assert!(claims["jti"]
            .as_str()
            .is_some_and(|value| value.len() >= 20));
        assert!(claims["iat"].as_u64().is_some());
    }

    #[test]
    fn a_proof_without_a_token_or_a_nonce_leaves_those_claims_out() {
        let (key, _) = DpopKey::generate().expect("generate");
        let proof = key
            .proof("DELETE", "https://example.test/mcp", None, None)
            .expect("proof");
        let parts: Vec<&str> = proof.split('.').collect();
        let claims: serde_json::Value =
            serde_json::from_slice(&decode_base64url(parts[1]).expect("claims")).expect("json");
        assert!(claims.get("nonce").is_none());
        assert!(claims.get("ath").is_none());
    }

    #[test]
    fn every_proof_is_unique_because_the_nonce_claim_is_random() {
        let (key, _) = DpopKey::generate().expect("generate");
        let first = key
            .proof("POST", "https://example.test/mcp", None, None)
            .expect("first");
        let second = key
            .proof("POST", "https://example.test/mcp", None, None)
            .expect("second");
        assert_ne!(first, second, "复用同一个 proof 正是重放攻击要的东西");
    }

    #[test]
    fn nonces_are_remembered_per_origin() {
        let nonces = DpopNonces::default();
        assert_eq!(nonces.get("https://example.test/mcp"), None);
        nonces.remember("https://example.test/mcp", "n-1");
        nonces.remember("https://other.test/mcp", "n-2");
        assert_eq!(
            nonces.get("https://example.test/mcp"),
            Some("n-1".to_string())
        );
        assert_eq!(
            nonces.get("https://other.test/mcp"),
            Some("n-2".to_string())
        );
        assert_eq!(nonces.get("https://third.test/mcp"), None);
    }

    #[test]
    fn an_unreadable_key_is_an_error_not_a_fallback() {
        assert!(DpopKey::from_pkcs8("not base64url at all!").is_err());
        assert!(DpopKey::from_pkcs8(&base64url(b"not a pkcs8 document")).is_err());
    }
}
