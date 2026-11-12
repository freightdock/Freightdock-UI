//! A small S3 client for backups: put, get, list and delete objects in one bucket, signed with AWS Signature Version 4,
//! so it works with AWS and any S3-compatible store (MinIO, Garage, Ceph, Backblaze B2, Cloudflare R2, Wasabi).
//! Path-style addressing by default (`endpoint/bucket/key`), which every one of them takes; virtual-hosted style
//! (`bucket.endpoint/key`) when asked.

use anyhow::{Context, Result, bail};
use hmac::Mac;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

type Hmac = hmac::Hmac<Sha256>;

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut m = Hmac::new_from_slice(key).expect("HMAC takes a key of any length");
    m.update(data.as_bytes());
    m.finalize().into_bytes().to_vec()
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// RFC 3986's unreserved characters stay; everything else is %XX, and `/` too unless it separates a path's parts.
pub fn encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// What is signed: the request as Signature Version 4 sees it.
pub struct Signing<'a> {
    pub method: &'a str,
    /// The path, already as it goes on the wire (each part encoded once).
    pub path: &'a str,
    /// The query's pairs, not encoded.
    pub query: &'a [(&'a str, &'a str)],
    /// Every header that is signed, `host` and `x-amz-date` among them.
    pub headers: &'a [(&'a str, &'a str)],
    pub payload_hash: &'a str,
    /// `20150830T123600Z`.
    pub amz_date: &'a str,
    pub region: &'a str,
    pub service: &'a str,
    pub access_key: &'a str,
    pub secret_key: &'a str,
}

impl Signing<'_> {
    pub fn canonical_request(&self) -> (String, String) {
        let mut q: Vec<(String, String)> = self.query.iter().map(|(k, v)| (encode(k, false), encode(v, false))).collect();
        q.sort();
        let query = q.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
        let mut h: Vec<(String, String)> = self.headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.split_whitespace().collect::<Vec<_>>().join(" "))).collect();
        h.sort();
        let canonical_headers: String = h.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
        let signed = h.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
        (format!("{}\n{}\n{query}\n{canonical_headers}\n{signed}\n{}", self.method, self.path, self.payload_hash), signed)
    }

    /// The `Authorization` header's value.
    pub fn authorization(&self) -> String {
        let (canonical, signed) = self.canonical_request();
        let date = &self.amz_date[..8];
        let scope = format!("{date}/{}/{}/aws4_request", self.region, self.service);
        let to_sign = format!("AWS4-HMAC-SHA256\n{}\n{scope}\n{}", self.amz_date, sha256_hex(canonical.as_bytes()));
        let k = hmac(format!("AWS4{}", self.secret_key).as_bytes(), date);
        let k = hmac(&k, self.region);
        let k = hmac(&k, self.service);
        let k = hmac(&k, "aws4_request");
        let signature = hex::encode(hmac(&k, &to_sign));
        format!("AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={signature}", self.access_key)
    }
}

/// `20261109T211409Z` for a Unix time.
pub fn amz_date(at: i64) -> String {
    let (y, m, d, hh, mm, ss) = crate::db::utc(at);
    format!("{y:04}{m:02}{d:02}T{hh:02}{mm:02}{ss:02}Z")
}

/// Where backups go in S3, and with what.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Config {
    /// `https://s3.eu-north-1.amazonaws.com`, `http://minio:9000`.
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    /// Put before each archive's name: `freightdock/`.
    pub prefix: String,
    pub access_key: String,
    /// Sealed at rest.
    pub secret_key: String,
    /// `bucket.endpoint/key` rather than `endpoint/bucket/key`.
    pub virtual_host: bool,
}

pub struct Client {
    cfg: Config,
    /// The secret key, opened.
    secret: String,
    http: reqwest::Client,
}

/// One object in a listing.
#[derive(Debug, Clone, PartialEq)]
pub struct Object {
    pub key: String,
    pub size: u64,
}

/// The keys and sizes in a ListObjectsV2 answer, and the token for the next page.
pub fn parse_list(xml: &str) -> (Vec<Object>, Option<String>) {
    let tag = |s: &str, name: &str| -> Option<String> {
        let open = format!("<{name}>");
        let start = s.find(&open)? + open.len();
        let end = s[start..].find(&format!("</{name}>"))? + start;
        Some(unescape(&s[start..end]))
    };
    let mut out = Vec::new();
    for part in xml.split("<Contents>").skip(1) {
        let part = part.split("</Contents>").next().unwrap_or("");
        if let Some(key) = tag(part, "Key") {
            out.push(Object { key, size: tag(part, "Size").and_then(|s| s.parse().ok()).unwrap_or(0) });
        }
    }
    let next = (tag(xml, "IsTruncated").as_deref() == Some("true")).then(|| tag(xml, "NextContinuationToken")).flatten();
    (out, next)
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// S3's error code in an error answer: `NoSuchBucket`, `SignatureDoesNotMatch`.
fn error_code(xml: &str) -> Option<String> {
    let start = xml.find("<Code>")? + 6;
    Some(xml[start..start + xml[start..].find("</Code>")?].to_string())
}

impl Client {
    pub fn new(cfg: Config, secret: String) -> Result<Client> {
        let u = url::Url::parse(cfg.endpoint.trim()).context("the S3 endpoint is not an address")?;
        if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
            bail!("the S3 endpoint starts http:// or https://");
        }
        let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(300)).build()?;
        Ok(Client { cfg, secret, http })
    }

    fn region(&self) -> &str {
        if self.cfg.region.trim().is_empty() { "us-east-1" } else { self.cfg.region.trim() }
    }

    /// The host and path for `key` in the bucket (`None`: the bucket itself).
    fn locate(&self, key: Option<&str>) -> Result<(String, String, String)> {
        let u = url::Url::parse(self.cfg.endpoint.trim())?;
        let host = u.host_str().unwrap_or_default().to_string();
        let host = match u.port() {
            Some(p) => format!("{host}:{p}"),
            None => host,
        };
        let base = u.path().trim_end_matches('/').to_string();
        let key = key.map(|k| format!("/{}", encode(k, true))).unwrap_or_else(|| "/".into());
        Ok(if self.cfg.virtual_host {
            (u.scheme().to_string(), format!("{}.{host}", self.cfg.bucket), format!("{base}{key}"))
        } else {
            let k = if key == "/" { String::new() } else { key };
            (u.scheme().to_string(), host, format!("{base}/{}{k}", encode(&self.cfg.bucket, false)))
        })
    }

    async fn send(&self, method: reqwest::Method, key: Option<&str>, query: &[(&str, &str)], body: Vec<u8>) -> Result<reqwest::Response> {
        let (scheme, host, path) = self.locate(key)?;
        let hash = sha256_hex(&body);
        let date = amz_date(crate::db::now());
        let headers = [("host", host.as_str()), ("x-amz-content-sha256", hash.as_str()), ("x-amz-date", date.as_str())];
        let auth = Signing { method: method.as_str(), path: &path, query, headers: &headers, payload_hash: &hash, amz_date: &date, region: self.region(), service: "s3", access_key: &self.cfg.access_key, secret_key: &self.secret }.authorization();
        let q = query.iter().map(|(k, v)| format!("{}={}", encode(k, false), encode(v, false))).collect::<Vec<_>>().join("&");
        let url = format!("{scheme}://{host}{path}{}", if q.is_empty() { String::new() } else { format!("?{q}") });
        let r = self
            .http
            .request(method, &url)
            .header("x-amz-content-sha256", &hash)
            .header("x-amz-date", &date)
            .header(reqwest::header::AUTHORIZATION, auth)
            .body(body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("{}: {}", self.cfg.endpoint, e.without_url()))?;
        Ok(r)
    }

    async fn ok(&self, r: reqwest::Response, what: &str) -> Result<reqwest::Response> {
        if r.status().is_success() {
            return Ok(r);
        }
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        let code = error_code(&text).unwrap_or_default();
        bail!("S3 {what}: {status}{}", if code.is_empty() { String::new() } else { format!(" ({code})") })
    }

    pub fn key_for(&self, name: &str) -> String {
        format!("{}{name}", self.cfg.prefix.trim_start_matches('/'))
    }

    /// Puts an object; makes the bucket first when it is not there.
    pub async fn put(&self, name: &str, data: Vec<u8>) -> Result<()> {
        let key = self.key_for(name);
        let r = self.send(reqwest::Method::PUT, Some(&key), &[], data.clone()).await?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            let text = r.text().await.unwrap_or_default();
            if error_code(&text).as_deref() == Some("NoSuchBucket") {
                let made = self.send(reqwest::Method::PUT, None, &[], Vec::new()).await?;
                self.ok(made, "making the bucket").await?;
                let r = self.send(reqwest::Method::PUT, Some(&key), &[], data).await?;
                self.ok(r, "putting the backup").await?;
                return Ok(());
            }
            bail!("S3 putting the backup: 404 Not Found");
        }
        self.ok(r, "putting the backup").await?;
        Ok(())
    }

    pub async fn get(&self, name: &str) -> Result<Vec<u8>> {
        let r = self.send(reqwest::Method::GET, Some(&self.key_for(name)), &[], Vec::new()).await?;
        Ok(self.ok(r, "getting the backup").await?.bytes().await?.to_vec())
    }

    pub async fn delete(&self, name: &str) -> Result<()> {
        let r = self.send(reqwest::Method::DELETE, Some(&self.key_for(name)), &[], Vec::new()).await?;
        self.ok(r, "deleting an old backup").await?;
        Ok(())
    }

    /// Every object under the prefix, by its name after the prefix. A bucket not there yet holds nothing.
    pub async fn list(&self) -> Result<Vec<Object>> {
        let prefix = self.cfg.prefix.trim_start_matches('/').to_string();
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut q: Vec<(&str, &str)> = vec![("list-type", "2"), ("prefix", &prefix)];
            if let Some(t) = &token {
                q.push(("continuation-token", t));
            }
            let r = self.send(reqwest::Method::GET, None, &q, Vec::new()).await?;
            if r.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(out);
            }
            let text = self.ok(r, "listing the backups").await?.text().await?;
            let (objects, next) = parse_list(&text);
            out.extend(objects.into_iter().filter_map(|o| Some(Object { key: o.key.strip_prefix(&prefix)?.to_string(), size: o.size })));
            match next {
                Some(t) => token = Some(t),
                None => return Ok(out),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SHA-256 of nothing, an empty body's payload hash.
    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn the_empty_payload_hash_is_sha256_of_nothing() {
        assert_eq!(sha256_hex(b""), EMPTY);
    }

    // AWS's published examples: the Signature Version 4 test suite's get-vanilla, and the S3 documentation's
    // "Examples: signature calculations" (GET Object with a range, ListObjects), with their example keys.
    #[test]
    fn get_vanilla_from_aws_s_test_suite() {
        let s = Signing {
            method: "GET", path: "/", query: &[], headers: &[("Host", "example.amazonaws.com"), ("X-Amz-Date", "20150830T123600Z")],
            payload_hash: EMPTY, amz_date: "20150830T123600Z", region: "us-east-1", service: "service",
            access_key: "AKIDEXAMPLE", secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
        };
        assert_eq!(s.canonical_request().0, format!("GET\n/\n\nhost:example.amazonaws.com\nx-amz-date:20150830T123600Z\n\nhost;x-amz-date\n{EMPTY}"));
        assert_eq!(
            s.authorization(),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;x-amz-date, Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    const S3_SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

    #[test]
    fn s3_get_object_with_a_range() {
        let s = Signing {
            method: "GET", path: "/test.txt", query: &[],
            headers: &[("Host", "examplebucket.s3.amazonaws.com"), ("Range", "bytes=0-9"), ("x-amz-content-sha256", EMPTY), ("x-amz-date", "20130524T000000Z")],
            payload_hash: EMPTY, amz_date: "20130524T000000Z", region: "us-east-1", service: "s3", access_key: "AKIAIOSFODNN7EXAMPLE", secret_key: S3_SECRET,
        };
        assert!(s.authorization().ends_with("SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"), "{}", s.authorization());
    }

    #[test]
    fn s3_list_objects_with_a_query() {
        let s = Signing {
            method: "GET", path: "/", query: &[("prefix", "J"), ("max-keys", "2")],
            headers: &[("Host", "examplebucket.s3.amazonaws.com"), ("x-amz-content-sha256", EMPTY), ("x-amz-date", "20130524T000000Z")],
            payload_hash: EMPTY, amz_date: "20130524T000000Z", region: "us-east-1", service: "s3", access_key: "AKIAIOSFODNN7EXAMPLE", secret_key: S3_SECRET,
        };
        assert!(s.canonical_request().0.starts_with("GET\n/\nmax-keys=2&prefix=J\n"), "the query sorted");
        assert!(s.authorization().ends_with("Signature=34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"), "{}", s.authorization());
    }

    #[test]
    fn paths_and_values_are_encoded_once_as_s3_wants() {
        assert_eq!(encode("freightdock/a b+c.backup", true), "freightdock/a%20b%2Bc.backup");
        assert_eq!(encode("a/b", false), "a%2Fb");
        assert_eq!(amz_date(1_440_938_160), "20150830T123600Z");
    }

    #[test]
    fn a_bucket_is_addressed_by_path_or_by_host() {
        let c = |virtual_host| Client::new(Config { endpoint: "http://127.0.0.1:9000".into(), bucket: "backups".into(), virtual_host, ..Default::default() }, String::new()).unwrap();
        assert_eq!(c(false).locate(Some("freightdock/x.backup")).unwrap(), ("http".into(), "127.0.0.1:9000".into(), "/backups/freightdock/x.backup".into()));
        assert_eq!(c(false).locate(None).unwrap().2, "/backups");
        assert_eq!(c(true).locate(Some("x")).unwrap(), ("http".into(), "backups.127.0.0.1:9000".into(), "/x".into()));
        assert!(Client::new(Config { endpoint: "ftp://x".into(), ..Default::default() }, String::new()).is_err());
    }

    #[test]
    fn a_listing_is_read_with_its_next_page() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?><ListBucketResult><Name>b</Name><IsTruncated>true</IsTruncated>
            <Contents><Key>freightdock/freightdock-20261109-010000.backup</Key><Size>1024</Size></Contents>
            <Contents><Key>freightdock/a&amp;b</Key><Size>7</Size></Contents><NextContinuationToken>tok/1</NextContinuationToken></ListBucketResult>"#;
        let (objects, next) = parse_list(xml);
        assert_eq!(objects, vec![Object { key: "freightdock/freightdock-20261109-010000.backup".into(), size: 1024 }, Object { key: "freightdock/a&b".into(), size: 7 }]);
        assert_eq!(next.as_deref(), Some("tok/1"));
        assert_eq!(error_code("<Error><Code>NoSuchBucket</Code></Error>").as_deref(), Some("NoSuchBucket"));
    }
}
