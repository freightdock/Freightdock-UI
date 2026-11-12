//! Amazon ECR: an access key and its secret are not what a registry takes. They are exchanged, with ECR's
//! `GetAuthorizationToken` signed by AWS Signature Version 4, for a token good for 12 hours, which is the password of
//! the user `AWS`. A token is kept until five minutes before it runs out ("Registries" 6 in DECISIONS.md).

use std::collections::HashMap;
use std::sync::Mutex;

use base64::Engine;
use reqwest::header;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::v2::Fail;

/// A token this close to running out is asked for again.
const MARGIN_S: i64 = 300;

/// HMAC-SHA256 (RFC 2104), on sha2 alone.
pub fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let pad = |b: u8| k.iter().map(|x| x ^ b).collect::<Vec<u8>>();
    let inner = Sha256::new().chain_update(pad(0x36)).chain_update(data).finalize();
    Sha256::new().chain_update(pad(0x5c)).chain_update(inner).finalize().into()
}

fn sha_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// `20150830T123600Z` for a moment in Unix seconds.
pub fn amz_date(unix: i64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(unix).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!("{:04}{:02}{:02}T{:02}{:02}{:02}Z", t.year(), t.month() as u8, t.day(), t.hour(), t.minute(), t.second())
}

/// What a request is, for its signature.
pub struct Request<'a> {
    pub method: &'a str,
    pub path: &'a str,
    /// Already in canonical form (sorted, encoded), or empty.
    pub query: &'a str,
    /// Lower-case names; `host` and `x-amz-date` among them.
    pub headers: &'a [(&'a str, &'a str)],
    pub payload: &'a [u8],
}

/// The `Authorization` header AWS Signature Version 4 gives `req`, signed at `amz_date` for `service` in `region`.
pub fn authorization(access_key: &str, secret_key: &str, region: &str, service: &str, amz_date: &str, req: &Request) -> String {
    let mut headers: Vec<(String, String)> = req.headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.split_whitespace().collect::<Vec<_>>().join(" "))).collect();
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed: String = headers.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let canonical = format!("{}\n{}\n{}\n{canonical_headers}\n{signed}\n{}", req.method, req.path, req.query, sha_hex(req.payload));
    let date = &amz_date[..amz_date.len().min(8)];
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", sha_hex(canonical.as_bytes()));
    let k = hmac(format!("AWS4{secret_key}").as_bytes(), date.as_bytes());
    let k = hmac(&k, region.as_bytes());
    let k = hmac(&k, service.as_bytes());
    let k = hmac(&k, b"aws4_request");
    let signature = hex::encode(hmac(&k, to_sign.as_bytes()));
    format!("AWS4-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed}, Signature={signature}")
}

/// What ECR gave: the user name and password for the registry, until when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub user: String,
    pub password: String,
    pub expires_at: i64,
}

/// ECR's API in `region`.
pub fn endpoint(region: &str) -> String {
    format!("https://api.ecr.{region}.amazonaws.com")
}

/// The region an ECR registry's host names (`<account>.dkr.ecr.<region>.amazonaws.com`).
pub fn region_of(host: &str) -> Option<String> {
    let rest = host.split_once(".dkr.ecr.")?.1;
    let region = rest.strip_suffix(".amazonaws.com").or_else(|| rest.strip_suffix(".amazonaws.com.cn"))?;
    valid_region(region).then(|| region.to_string())
}

pub fn valid_region(r: &str) -> bool {
    !r.is_empty() && r.len() <= 32 && r.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn cache() -> &'static Mutex<HashMap<String, Token>> {
    static C: std::sync::OnceLock<Mutex<HashMap<String, Token>>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// A token for the registry, from ECR's API at `api` (`endpoint(region)`), kept until it nearly runs out.
pub async fn token(api: &str, access_key: &str, secret_key: &str, region: &str) -> Result<Token, Fail> {
    let key = format!("{api}\0{access_key}\0{}\0{region}", sha_hex(secret_key.as_bytes()));
    let now = crate::db::now();
    if let Some(t) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key).filter(|t| t.expires_at - MARGIN_S > now) {
        return Ok(t.clone());
    }
    let t = exchange(api, access_key, secret_key, region, now).await?;
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    c.retain(|_, t| t.expires_at > now);
    c.insert(key, t.clone());
    Ok(t)
}

const TARGET: &str = "AmazonEC2ContainerRegistry_V20150921.GetAuthorizationToken";
const JSON_1_1: &str = "application/x-amz-json-1.1";

async fn exchange(api: &str, access_key: &str, secret_key: &str, region: &str, now: i64) -> Result<Token, Fail> {
    let url = url::Url::parse(api).map_err(|_| Fail::Unreachable(format!("ECR's API is not an address: {api}")))?;
    let host = match (url.host_str(), url.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_string(),
        _ => return Err(Fail::Unreachable(format!("ECR's API is not an address: {api}"))),
    };
    let date = amz_date(now);
    let body = b"{}";
    let headers = [("content-type", JSON_1_1), ("host", host.as_str()), ("x-amz-date", date.as_str()), ("x-amz-target", TARGET)];
    let auth = authorization(access_key, secret_key, region, "ecr", &date, &Request { method: "POST", path: "/", query: "", headers: &headers, payload: body });
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent(concat!("Freightdock/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_default();
    let mut u = url.clone();
    u.set_path("/");
    let r = http
        .post(u)
        .header(header::CONTENT_TYPE, JSON_1_1)
        .header("x-amz-date", &date)
        .header("x-amz-target", TARGET)
        .header(header::AUTHORIZATION, auth)
        .body(body.to_vec())
        .send()
        .await
        .map_err(|e| Fail::Unreachable(format!("ECR could not be reached: {}.", e.without_url())))?;
    let status = r.status();
    let text = r.text().await.unwrap_or_default();
    if !status.is_success() {
        #[derive(Deserialize)]
        struct E {
            message: Option<String>,
            #[serde(rename = "Message")]
            message2: Option<String>,
        }
        let said = serde_json::from_str::<E>(&text).ok().and_then(|e| e.message.or(e.message2)).unwrap_or_else(|| format!("it answered {status}"));
        return Err(if status.is_client_error() { Fail::Denied(format!("ECR refused the access key: {said}")) } else { Fail::Unreachable(format!("ECR did not give a token: {said}")) });
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Data {
        authorization_token: String,
        expires_at: Option<f64>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Answer {
        authorization_data: Vec<Data>,
    }
    let bad = || Fail::Unreachable("ECR answered something that is not a token.".into());
    let a: Answer = serde_json::from_str(&text).map_err(|_| bad())?;
    let d = a.authorization_data.into_iter().next().ok_or_else(bad)?;
    let plain = base64::engine::general_purpose::STANDARD.decode(d.authorization_token.trim()).map_err(|_| bad())?;
    let plain = String::from_utf8(plain).map_err(|_| bad())?;
    let (user, password) = plain.split_once(':').ok_or_else(bad)?;
    Ok(Token { user: user.to_string(), password: password.to_string(), expires_at: d.expires_at.map_or(now + 12 * 3600, |t| t as i64) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn hmac_is_rfc_4231_s() {
        assert_eq!(hex::encode(hmac(b"Jefe", b"what do ya want for nothing?")), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        // A key longer than the block is hashed first.
        assert_eq!(
            hex::encode(hmac(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn a_signature_is_aws_s_own_test_vector() {
        // get-vanilla, from AWS's Signature Version 4 test suite.
        let req = Request { method: "GET", path: "/", query: "", headers: &[("host", "example.amazonaws.com"), ("x-amz-date", "20150830T123600Z")], payload: b"" };
        assert_eq!(
            authorization("AKIDEXAMPLE", "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", "us-east-1", "service", "20150830T123600Z", &req),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;x-amz-date, Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
        assert_eq!(amz_date(1440938160), "20150830T123600Z");
    }

    #[test]
    fn an_ecr_host_names_its_region() {
        assert_eq!(region_of("123456789012.dkr.ecr.eu-north-1.amazonaws.com").as_deref(), Some("eu-north-1"));
        assert_eq!(region_of("ghcr.io"), None);
    }

    struct FakeEcr {
        asks: AtomicUsize,
        /// How long each token it gives is good for.
        good_for: i64,
    }

    async fn fake(good_for: i64) -> (String, Arc<FakeEcr>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let f = Arc::new(FakeEcr { asks: AtomicUsize::new(0), good_for });
        let state = f.clone();
        let handler = move |h: HeaderMap, body: axum::body::Bytes| {
            let f = state.clone();
            async move {
                f.asks.fetch_add(1, Ordering::SeqCst);
                let get = |n: &str| h.get(n).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
                assert_eq!(get("x-amz-target"), "AmazonEC2ContainerRegistry_V20150921.GetAuthorizationToken");
                assert_eq!(get("content-type"), "application/x-amz-json-1.1");
                let (date, host) = (get("x-amz-date"), get("host"));
                // The fake knows the one good key: it signs the request again and compares.
                let headers = [("content-type", "application/x-amz-json-1.1"), ("host", host.as_str()), ("x-amz-date", date.as_str()), ("x-amz-target", "AmazonEC2ContainerRegistry_V20150921.GetAuthorizationToken")];
                let req = Request { method: "POST", path: "/", query: "", headers: &headers, payload: &body };
                let access = get("authorization").split("Credential=").nth(1).and_then(|c| c.split('/').next()).unwrap_or_default().to_string();
                if authorization(&access, "the-secret", "eu-north-1", "ecr", &date, &req) != get("authorization") {
                    return (axum::http::StatusCode::BAD_REQUEST, r#"{"__type":"UnrecognizedClientException","message":"The security token included in the request is invalid."}"#).into_response();
                }
                let token = base64::engine::general_purpose::STANDARD.encode("AWS:ecr-password");
                let at = crate::db::now() + f.good_for;
                axum::Json(serde_json::json!({ "authorizationData": [{ "authorizationToken": token, "expiresAt": at as f64 + 0.5, "proxyEndpoint": "https://1.dkr.ecr.eu-north-1.amazonaws.com" }] })).into_response()
            }
        };
        tokio::spawn(async move { axum::serve(listener, axum::Router::new().route("/", axum::routing::post(handler))).await.unwrap() });
        (format!("http://{addr}"), f)
    }

    #[tokio::test]
    async fn keys_are_exchanged_for_a_token_kept_until_it_nearly_runs_out() {
        let (api, f) = fake(12 * 3600).await;
        let t = token(&api, "AKIA-KEPT", "the-secret", "eu-north-1").await.unwrap();
        assert_eq!((t.user.as_str(), t.password.as_str()), ("AWS", "ecr-password"));
        assert!(t.expires_at > crate::db::now() + 11 * 3600);
        assert_eq!(token(&api, "AKIA-KEPT", "the-secret", "eu-north-1").await.unwrap(), t);
        assert_eq!(f.asks.load(Ordering::SeqCst), 1, "the second came from the cache");

        let (api, f) = fake(60).await;
        token(&api, "AKIA-SHORT", "the-secret", "eu-north-1").await.unwrap();
        token(&api, "AKIA-SHORT", "the-secret", "eu-north-1").await.unwrap();
        assert_eq!(f.asks.load(Ordering::SeqCst), 2, "a token about to run out is asked for again");
    }

    #[tokio::test]
    async fn a_wrong_secret_is_denied_with_aws_s_words() {
        let (api, _) = fake(3600).await;
        let e = token(&api, "AKIA-WRONG", "not-the-secret", "eu-north-1").await.unwrap_err();
        assert!(matches!(&e, Fail::Denied(m) if m.contains("security token included in the request is invalid")), "{e:?}");
    }
}
