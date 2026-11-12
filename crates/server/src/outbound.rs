//! What Freightdock's server asks of hosts other people name (a registry an image says it is from, a sign-in
//! provider's picture): every answer read to a size limit, and where a redirect may lead ("Outbound" in DECISIONS.md).

/// The answer's body, up to `max` bytes: an answer that says it is larger, or turns out to be, is refused before
/// more of it is kept.
pub async fn capped(mut r: reqwest::Response, max: usize) -> Result<Vec<u8>, String> {
    if r.content_length().is_some_and(|n| n > max as u64) {
        return Err(too_big(max));
    }
    let mut out = Vec::new();
    while let Some(c) = r.chunk().await.map_err(|e| e.without_url().to_string())? {
        if out.len() + c.len() > max {
            return Err(too_big(max));
        }
        out.extend_from_slice(&c);
    }
    Ok(out)
}

fn too_big(max: usize) -> String {
    if max >= 1024 * 1024 { format!("the answer is too big (over {} MiB)", max / 1024 / 1024) } else { format!("the answer is too big (over {} KiB)", max / 1024) }
}

/// An address inside: this machine, a private network, link-local (a cloud's metadata service), shared address space
/// (CGNAT, a tailnet), or none at all. A host somebody else names should not send the server there.
pub fn internal_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v) => {
            let [a, b, ..] = v.octets();
            v.is_loopback() || v.is_private() || v.is_link_local() || v.is_unspecified() || v.is_broadcast() || a == 0 || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return internal_ip(IpAddr::V4(v4));
            }
            let first = v.segments()[0];
            v.is_loopback() || v.is_unspecified() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// Whether a URL's host is inside as written: `localhost` (and a name under it) or an internal address. A name is not
/// looked up here; `resolve` does that.
pub fn internal_host(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(d) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost")
        }
        url::Host::Ipv4(a) => internal_ip((*a).into()),
        url::Host::Ipv6(a) => internal_ip((*a).into()),
    }
}

/// Redirects followed (up to three) only over https and, unless the first address was itself inside, never to an
/// address written as one inside (`internal_host`). For a picture, whose host may hand off to a CDN.
pub fn outward() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let started_inside = attempt.previous().first().and_then(|f| f.host()).is_some_and(|h| internal_host(&h));
        let to = attempt.url();
        let ok = attempt.previous().len() <= 3 && to.scheme() == "https" && (started_inside || !to.host().is_some_and(|h| internal_host(&h)));
        if ok { attempt.follow() } else { attempt.stop() }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_inside_is_told_from_one_outside() {
        for inside in ["127.0.0.1", "10.1.2.3", "172.31.0.1", "192.168.0.1", "169.254.169.254", "100.100.100.100", "0.0.0.0", "255.255.255.255", "::1", "::", "fd12::1", "fe80::1", "::ffff:10.0.0.1"] { // gitleaks:allow (private ranges, refused on purpose)
            assert!(internal_ip(inside.parse().unwrap()), "{inside}");
        }
        for outside in ["1.1.1.1", "100.128.0.1", "172.32.0.1", "2606:4700::1111", "::ffff:8.8.8.8"] {
            assert!(!internal_ip(outside.parse().unwrap()), "{outside}");
        }
        let host = |u: &str| url::Url::parse(u).unwrap().host().map(|h| internal_host(&h)).unwrap();
        assert!(host("http://localhost/") && host("http://a.localhost/") && host("http://[::1]/") && host("http://10.0.0.1/")); // gitleaks:allow (private ranges, refused on purpose)
        assert!(!host("https://ghcr.io/") && !host("https://registry.lan/"), "a name is not looked up here");
    }

    async fn serve(body: Vec<u8>, chunked: bool) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let app = axum::Router::new().route(
            "/",
            axum::routing::get(move || {
                let body = body.clone();
                async move {
                    if chunked {
                        let s = futures_util::stream::iter(body.chunks(1024).map(|c| Ok::<_, std::io::Error>(axum::body::Bytes::copy_from_slice(c))).collect::<Vec<_>>());
                        axum::body::Body::from_stream(s)
                    } else {
                        axum::body::Body::from(body)
                    }
                }
            }),
        );
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}/")
    }

    #[tokio::test]
    async fn an_answer_is_read_to_its_limit_and_no_further() {
        for chunked in [false, true] {
            let url = serve(vec![b'x'; 10_000], chunked).await;
            let get = || async { reqwest::get(&url).await.unwrap() };
            assert_eq!(capped(get().await, 10_000).await.unwrap().len(), 10_000, "chunked: {chunked}");
            let e = capped(get().await, 9_999).await.unwrap_err();
            assert!(e.contains("too big"), "chunked {chunked}: {e}");
        }
    }
}
