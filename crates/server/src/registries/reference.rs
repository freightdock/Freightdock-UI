//! Image references as Docker reads them: which registry a reference names, which repository in it, and which tag or
//! digest. A first part with a dot or a colon, or `localhost`, is a registry's host; anything else is on Docker Hub,
//! whose one-part names are under `library/`. Hosts are compared in one form: lower case, no scheme or path, Docker
//! Hub's aliases all `docker.io` ("Registries" 2 in DECISIONS.md).

/// Docker Hub, as Freightdock names it.
pub const DOCKER_HUB: &str = "docker.io";

/// The names Docker Hub goes by, all the same registry.
const HUB_ALIASES: &[&str] = &["docker.io", "index.docker.io", "registry-1.docker.io", "registry.hub.docker.com"];

#[derive(Debug, PartialEq, Eq)]
pub struct Reference {
    /// The registry, in the form `host()` gives.
    pub host: String,
    /// The repository inside it, as its API names it (`library/nginx` on Docker Hub).
    pub repository: String,
    pub tag: Option<String>,
    pub digest: Option<String>,
}

/// A registry's address in the one form references are matched in: `host[:port]`, lower case, without a scheme, a
/// path or a trailing slash; Docker Hub's aliases are `docker.io`.
pub fn host(address: &str) -> String {
    let a = address.trim();
    let a = a.strip_prefix("https://").or_else(|| a.strip_prefix("http://")).unwrap_or(a);
    let a = a.split('/').next().unwrap_or_default().to_ascii_lowercase();
    if HUB_ALIASES.contains(&a.as_str()) { DOCKER_HUB.to_string() } else { a }
}

/// A part of a repository's path as registries take it: lower-case letters, digits and the separators `.`, `_`, `-`
/// between them (upper case is let through: Docker refuses it, with its own words).
fn path_part(p: &str) -> bool {
    !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// What `image` names, or None when it is not an image reference.
pub fn parse(image: &str) -> Option<Reference> {
    if image.is_empty() || image.len() > 512 || image.starts_with('-') || image.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let (rest, digest) = match image.split_once('@') {
        Some((r, d)) if !d.is_empty() && d.contains(':') => (r, Some(d.to_string())),
        Some(_) => return None,
        None => (image, None),
    };
    let last = rest.rfind('/').map_or(0, |i| i + 1);
    let (name, tag) = match rest[last..].rfind(':') {
        Some(i) if i + 1 < rest.len() - last => (&rest[..last + i], Some(rest[last + i + 1..].to_string())),
        Some(_) => return None,
        None => (rest, None),
    };
    let (registry, path) = match name.split_once('/') {
        Some((first, path)) if first.contains(['.', ':']) || first == "localhost" => (host(first), path.to_string()),
        _ => (DOCKER_HUB.to_string(), name.to_string()),
    };
    if !path.split('/').all(path_part) {
        return None;
    }
    let repository = if registry == DOCKER_HUB && !path.contains('/') { format!("library/{path}") } else { path };
    Some(Reference { host: registry, repository, tag, digest })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(host: &str, repository: &str, tag: Option<&str>, digest: Option<&str>) -> Option<Reference> {
        Some(Reference { host: host.into(), repository: repository.into(), tag: tag.map(Into::into), digest: digest.map(Into::into) })
    }

    #[test]
    fn a_name_without_a_host_is_on_docker_hub() {
        assert_eq!(parse("nginx"), r("docker.io", "library/nginx", None, None));
        assert_eq!(parse("nginx:1.25"), r("docker.io", "library/nginx", Some("1.25"), None));
        assert_eq!(parse("grafana/grafana:11.0"), r("docker.io", "grafana/grafana", Some("11.0"), None));
        assert_eq!(parse("Team/app"), r("docker.io", "Team/app", None, None), "an upper-case first part is no host");
    }

    #[test]
    fn a_first_part_with_a_dot_or_a_port_or_localhost_is_the_registry() {
        assert_eq!(parse("ghcr.io/owner/app:v2"), r("ghcr.io", "owner/app", Some("v2"), None));
        assert_eq!(parse("registry:5000/team/app"), r("registry:5000", "team/app", None, None));
        assert_eq!(parse("localhost/app"), r("localhost", "app", None, None));
        assert_eq!(parse("127.0.0.1:5000/qa/alpine:1"), r("127.0.0.1:5000", "qa/alpine", Some("1"), None));
        assert_eq!(parse("GHCR.IO/owner/app"), r("ghcr.io", "owner/app", None, None), "a host is case-blind");
        assert_eq!(parse("quay.io/a/b@sha256:abc"), r("quay.io", "a/b", None, Some("sha256:abc")));
        assert_eq!(parse("quay.io/a/b:1@sha256:abc"), r("quay.io", "a/b", Some("1"), Some("sha256:abc")));
    }

    #[test]
    fn docker_hub_s_aliases_are_docker_hub() {
        for a in ["docker.io/library/nginx", "index.docker.io/library/nginx", "registry-1.docker.io/library/nginx"] {
            assert_eq!(parse(a), r("docker.io", "library/nginx", None, None), "{a}");
        }
        assert_eq!(parse("docker.io/nginx"), r("docker.io", "library/nginx", None, None));
    }

    #[test]
    fn what_is_no_reference_is_none() {
        for bad in ["", "a b", "ghcr.io/", "/app", "app:", "ghcr.io//app", "app@", "-x"] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_registry_s_address_is_matched_by_its_host() {
        assert_eq!(host("ghcr.io"), "ghcr.io");
        assert_eq!(host("https://GHCR.io/"), "ghcr.io");
        assert_eq!(host("http://127.0.0.1:5000"), "127.0.0.1:5000");
        assert_eq!(host("https://index.docker.io/v1/"), "docker.io");
        assert_eq!(host("registry-1.docker.io"), "docker.io");
        assert_eq!(host(" registry.example.com:443/some/path "), "registry.example.com:443");
    }
}
