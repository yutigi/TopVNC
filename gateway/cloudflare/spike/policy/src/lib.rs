//! Bounded authentication and routing policy shared by the Phase 0 transports.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rsa::{BigUint, RsaPublicKey, pkcs1v15, sha2::Sha256, signature::Verifier};
use serde::Deserialize;
use std::net::IpAddr;
use url::Url;

pub const MAX_TOKEN_BYTES: usize = 16 * 1024;
pub const MAX_JWKS_BYTES: usize = 64 * 1024;
pub const READ_CHUNK_BYTES: usize = 16 * 1024;
pub const KEY_TTL_SECONDS: u64 = 300;
pub const KEY_REFRESH_SECONDS: u64 = 10;

pub type PolicyResult<T> = Result<T, &'static str>;

#[derive(Clone, Debug)]
pub struct AccessPolicy {
    pub issuer: String,
    pub audience: String,
    pub owner_email: String,
}

impl AccessPolicy {
    pub fn new(issuer: &str, audience: &str, owner_email: &str) -> PolicyResult<Self> {
        let issuer = issuer.trim_end_matches('/');
        let url = Url::parse(issuer).map_err(|_| "invalid Access issuer")?;
        let host = url.host_str().ok_or("missing Access team hostname")?;
        let team = host
            .strip_suffix(".cloudflareaccess.com")
            .ok_or("invalid Access team hostname")?;
        if url.scheme() != "https"
            || team.is_empty()
            || team.contains('.')
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || audience.is_empty()
            || audience.len() > 256
            || !owner_email.contains('@')
            || owner_email.len() > 254
            || owner_email.chars().any(char::is_whitespace)
        {
            return Err("invalid Access configuration");
        }
        Ok(Self {
            issuer: issuer.into(),
            audience: audience.into(),
            owner_email: owner_email.into(),
        })
    }

    pub fn jwks_url(&self) -> String {
        format!("{}/cdn-cgi/access/certs", self.issuer)
    }
}

#[derive(Debug)]
pub struct Identity {
    pub expires_at: u64,
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    kid: String,
    crit: Option<Vec<String>>,
    b64: Option<bool>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct Claims {
    iss: String,
    aud: Audience,
    exp: u64,
    nbf: Option<u64>,
    email: String,
}

#[derive(Deserialize)]
struct Jwk {
    kid: String,
    kty: String,
    alg: Option<String>,
    #[serde(rename = "use")]
    usage: Option<String>,
    n: String,
    e: String,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

pub struct KeySet(Vec<(String, pkcs1v15::VerifyingKey<Sha256>)>);

fn token_parts(token: &str) -> PolicyResult<(&str, &str, &str)> {
    if token.is_empty() || token.len() > MAX_TOKEN_BYTES {
        return Err("invalid token size");
    }
    let mut parts = token.split('.');
    let (Some(header), Some(claims), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("invalid JWT structure");
    };
    if header.is_empty() || claims.is_empty() || signature.is_empty() {
        return Err("empty JWT component");
    }
    Ok((header, claims, signature))
}

pub fn token_key_id(token: &str) -> PolicyResult<String> {
    let (header, _, _) = token_parts(token)?;
    if header.len() > 1024 {
        return Err("JWT header too large");
    }
    let raw = URL_SAFE_NO_PAD
        .decode(header)
        .map_err(|_| "invalid JWT header encoding")?;
    let header: Header = serde_json::from_slice(&raw).map_err(|_| "invalid JWT header")?;
    if header.alg != "RS256"
        || header.kid.is_empty()
        || header.kid.len() > 128
        || header.crit.is_some()
        || header.b64 == Some(false)
    {
        return Err("unsupported JWT algorithm or key");
    }
    Ok(header.kid)
}

impl KeySet {
    pub fn parse(bytes: &[u8]) -> PolicyResult<Self> {
        if bytes.len() > MAX_JWKS_BYTES {
            return Err("JWKS too large");
        }
        let jwks: Jwks = serde_json::from_slice(bytes).map_err(|_| "invalid JWKS")?;
        if jwks.keys.is_empty() || jwks.keys.len() > 8 {
            return Err("invalid JWKS key count");
        }
        let mut keys = Vec::new();
        for key in jwks.keys {
            if key.kty != "RSA"
                || key.alg.as_deref().is_some_and(|a| a != "RS256")
                || key.usage.as_deref().is_some_and(|u| u != "sig")
            {
                continue;
            }
            if key.kid.is_empty()
                || key.kid.len() > 128
                || key.n.len() > 684
                || key.e.len() > 12
                || keys.iter().any(|(kid, _)| kid == &key.kid)
            {
                return Err("invalid JWK metadata");
            }
            let n = URL_SAFE_NO_PAD
                .decode(key.n)
                .map_err(|_| "invalid RSA modulus")?;
            let e = URL_SAFE_NO_PAD
                .decode(key.e)
                .map_err(|_| "invalid RSA exponent")?;
            let n = BigUint::from_bytes_be(&n);
            if !(2048..=4096).contains(&n.bits()) || e.is_empty() || e.len() > 4 {
                return Err("invalid RSA key size");
            }
            let public = RsaPublicKey::new(n, BigUint::from_bytes_be(&e))
                .map_err(|_| "invalid RSA public key")?;
            keys.push((key.kid, pkcs1v15::VerifyingKey::<Sha256>::new(public)));
        }
        if keys.is_empty() {
            return Err("no usable signing keys");
        }
        Ok(Self(keys))
    }

    pub fn contains(&self, kid: &str) -> bool {
        self.0.iter().any(|(id, _)| id == kid)
    }

    pub fn verify(&self, token: &str, policy: &AccessPolicy, now: u64) -> PolicyResult<Identity> {
        let kid = token_key_id(token)?;
        let (_, payload, signature) = token_parts(token)?;
        let key = &self
            .0
            .iter()
            .find(|(id, _)| id == &kid)
            .ok_or("unknown signing key")?
            .1;
        let bytes = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| "invalid JWT signature encoding")?;
        let signature =
            pkcs1v15::Signature::try_from(bytes.as_slice()).map_err(|_| "invalid JWT signature")?;
        let signed = token.rsplit_once('.').ok_or("invalid JWT structure")?.0;
        key.verify(signed.as_bytes(), &signature)
            .map_err(|_| "JWT signature rejected")?;
        let raw = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| "invalid JWT payload encoding")?;
        let claims: Claims = serde_json::from_slice(&raw).map_err(|_| "invalid JWT claims")?;
        let audience_matches = match claims.aud {
            Audience::One(a) => a == policy.audience,
            Audience::Many(a) => a.len() <= 16 && a.iter().any(|a| a == &policy.audience),
        };
        if claims.iss != policy.issuer
            || !audience_matches
            || claims.exp <= now
            || claims.nbf.is_some_and(|nbf| nbf > now)
            || !claims.email.eq_ignore_ascii_case(&policy.owner_email)
        {
            return Err("JWT claims rejected");
        }
        Ok(Identity {
            expires_at: claims.exp,
        })
    }
}

pub fn validate_origin(origin: &str) -> PolicyResult<()> {
    let url = Url::parse(origin).map_err(|_| "invalid app origin")?;
    if url.scheme() != "https"
        || !url.host_str().is_some_and(|h| h.ends_with(".workers.dev"))
        || url.origin().ascii_serialization() != origin
    {
        return Err("app origin must be an exact HTTPS workers.dev origin");
    }
    Ok(())
}

pub fn validate_upgrade(
    method: &str,
    path: &str,
    query: Option<&str>,
    origin: Option<&str>,
    protocols: Option<&str>,
    upgrade: Option<&str>,
    app_origin: &str,
) -> PolicyResult<()> {
    validate_origin(app_origin)?;
    if method != "GET" || path != "/ws" || query.is_some() {
        return Err("invalid upgrade route");
    }
    if origin != Some(app_origin) {
        return Err("Origin rejected");
    }
    if !protocols.is_some_and(|p| p.len() <= 128 && p.split(',').any(|v| v.trim() == "binary"))
        || !upgrade.is_some_and(|u| u.eq_ignore_ascii_case("websocket"))
    {
        return Err("binary WebSocket upgrade required");
    }
    Ok(())
}

pub fn validate_upstream(upstream: &str) -> PolicyResult<()> {
    let url = Url::parse(upstream).map_err(|_| "invalid upstream URL")?;
    let ip: IpAddr = url
        .host_str()
        .ok_or("missing upstream host")?
        .trim_matches(['[', ']'])
        .parse()
        .map_err(|_| "upstream must use a private IP literal")?;
    let private = match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local(),
    };
    if !private
        || url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_none()
        || url.path() != "/ws"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("upstream must be one fixed private HTTP /ws endpoint");
    }
    Ok(())
}

/// Deterministic signing material is compiled only into tests, never the gateway.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use super::*;
    use rand::{SeedableRng, rngs::StdRng};
    use rsa::{
        RsaPrivateKey,
        signature::{SignatureEncoding, Signer},
        traits::PublicKeyParts,
    };
    use std::sync::OnceLock;
    pub const NOW: u64 = 1_800_000_000;
    pub const ORIGIN: &str = "https://topvnc.owner.workers.dev";
    fn key() -> &'static RsaPrivateKey {
        static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
        KEY.get_or_init(|| RsaPrivateKey::new(&mut StdRng::seed_from_u64(13), 2048).unwrap())
    }
    pub fn policy() -> AccessPolicy {
        AccessPolicy::new(
            "https://topvnc-test.cloudflareaccess.com",
            "test-audience",
            "owner@example.com",
        )
        .unwrap()
    }
    pub fn jwks() -> String {
        serde_json::json!({"keys":[{"kid":"test-key","kty":"RSA","alg":"RS256","use":"sig",
            "n":URL_SAFE_NO_PAD.encode(key().n().to_bytes_be()),"e":URL_SAFE_NO_PAD.encode(key().e().to_bytes_be())}]}).to_string()
    }
    pub fn sign(claims: serde_json::Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","kid":"test-key"}"#);
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let signed = format!("{header}.{payload}");
        let signature = pkcs1v15::SigningKey::<Sha256>::new(key().clone()).sign(signed.as_bytes());
        format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
    }
    pub fn token(exp: u64) -> String {
        let p = policy();
        sign(serde_json::json!({"iss":p.issuer,"aud":[p.audience],"email":p.owner_email,"exp":exp}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_query_strings_and_wrong_origin_before_relay() {
        assert!(
            validate_upgrade(
                "GET",
                "/ws",
                Some("target=127.0.0.1:22"),
                Some(ORIGIN),
                Some("binary"),
                Some("websocket"),
                ORIGIN
            )
            .is_err()
        );
        assert!(
            validate_upgrade(
                "GET",
                "/ws",
                None,
                Some("https://attacker.example"),
                Some("binary"),
                Some("websocket"),
                ORIGIN
            )
            .is_err()
        );
        assert!(
            validate_upgrade(
                "GET",
                "/ws",
                None,
                Some(ORIGIN),
                Some("binary"),
                Some("websocket"),
                ORIGIN
            )
            .is_ok()
        );
    }

    const ORIGIN: &str = "https://topvnc.owner.workers.dev";

    #[test]
    fn restricts_upstream_to_configured_private_websocket_service() {
        assert!(validate_upstream("http://127.0.0.1:6080/ws").is_ok());
        for invalid in [
            "http://example.com:6080/ws",
            "http://8.8.8.8:6080/ws",
            "http://127.0.0.1:6080/ws?target=x",
            "http://user:pass@127.0.0.1:6080/ws",
            "http://127.0.0.1:6080/admin",
        ] {
            assert!(validate_upstream(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn verifies_signature_claims_and_owner() {
        let keys = KeySet::parse(fixtures::jwks().as_bytes()).unwrap();
        let policy = fixtures::policy();
        let token = fixtures::token(fixtures::NOW + 300);
        assert_eq!(
            keys.verify(&token, &policy, fixtures::NOW)
                .unwrap()
                .expires_at,
            fixtures::NOW + 300
        );
        assert!(keys.verify(&token, &policy, fixtures::NOW + 300).is_err());
        let wrong =
            AccessPolicy::new(&policy.issuer, "different-app", &policy.owner_email).unwrap();
        assert!(keys.verify(&token, &wrong, fixtures::NOW).is_err());
        let wrong =
            AccessPolicy::new(&policy.issuer, &policy.audience, "other@example.com").unwrap();
        assert!(keys.verify(&token, &wrong, fixtures::NOW).is_err());
        let mut parts: Vec<_> = token.split('.').map(str::to_owned).collect();
        parts[1] = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            br#"{"exp":9999999999,"email":"owner@example.com"}"#,
        );
        assert!(
            keys.verify(&parts.join("."), &policy, fixtures::NOW)
                .is_err()
        );
    }

    #[test]
    fn rejects_missing_forged_and_oversized_tokens_and_keysets() {
        let keys = KeySet::parse(fixtures::jwks().as_bytes()).unwrap();
        for token in [
            String::new(),
            "e30.e30.e30".into(),
            "x".repeat(MAX_TOKEN_BYTES + 1),
        ] {
            assert!(
                keys.verify(&token, &fixtures::policy(), fixtures::NOW)
                    .is_err()
            );
        }
        assert!(KeySet::parse(&vec![b' '; MAX_JWKS_BYTES + 1]).is_err());
    }

    #[test]
    fn rejects_unknown_jwt_algorithms_and_critical_extensions() {
        for header in [
            serde_json::json!({"alg":"none","kid":"test-key"}),
            serde_json::json!({"alg":"HS256","kid":"test-key"}),
            serde_json::json!({"alg":"RS256","kid":"test-key","crit":["unsupported"]}),
            serde_json::json!({"alg":"RS256","kid":"test-key","b64":false}),
        ] {
            let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
            assert!(
                token_key_id(&format!("{encoded}.e30.AA")).is_err(),
                "{header}"
            );
        }
    }

    #[test]
    fn rejects_signed_tokens_with_wrong_issuer_future_nbf_or_missing_expiry() {
        let policy = fixtures::policy();
        let keys = KeySet::parse(fixtures::jwks().as_bytes()).unwrap();
        let valid = serde_json::json!({"iss":policy.issuer,"aud":policy.audience,
            "email":policy.owner_email,"exp":fixtures::NOW + 300});
        for invalid in [
            {
                let mut c = valid.clone();
                c["iss"] = "https://another.cloudflareaccess.com".into();
                c
            },
            {
                let mut c = valid.clone();
                c["nbf"] = (fixtures::NOW + 1).into();
                c
            },
            {
                let mut c = valid.clone();
                c.as_object_mut().unwrap().remove("exp");
                c
            },
        ] {
            assert!(
                keys.verify(&fixtures::sign(invalid), &policy, fixtures::NOW)
                    .is_err()
            );
        }
    }
}
