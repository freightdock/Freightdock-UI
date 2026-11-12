//! Agents: a Docker host Freightdock cannot reach runs `freightdock-agent`, which dials out to Freightdock's agent endpoint, its
//! own listener (`FREIGHTDOCK_AGENT_BIND`, 7444 by default) with TLS from Freightdock's own CA whatever `FREIGHTDOCK_TLS` says.
//! The agent enrols once with a join token and gets a client certificate; from then on it opens a WebSocket with that
//! certificate, and both ends run yamux over it. The tunnel is offered to bollard as a unix socket
//! (`docker::sockets`) ("Agent" in DECISIONS.md).

pub mod ca;
pub mod endpoint;

#[cfg(test)]
mod e2e;

use std::sync::Arc;

pub struct Agents {
    pub ca: ca::Ca,
    /// The agent endpoint's TLS, made from the CA at start.
    pub tls: Arc<rustls::ServerConfig>,
    /// Where agents are told to dial (`FREIGHTDOCK_AGENT_ADDRESS`), when it is not this server's name and the endpoint's
    /// port: behind a proxy, or NAT.
    pub address: Option<String>,
    /// The agent endpoint's port, when it listens.
    pub port: Option<u16>,
    /// The agent's image, for the `docker run` line.
    pub image: String,
}

impl Agents {
    pub fn new(ca: ca::Ca, address: Option<String>, port: Option<u16>, image: String) -> anyhow::Result<Self> {
        Ok(Agents { tls: ca.server_config()?, ca, address, port, image })
    }

    #[cfg(test)]
    pub fn for_test() -> Self {
        Self::new(ca::Ca::generate().unwrap(), None, Some(7444), "freightdock-agent:latest".into()).unwrap()
    }

    /// Where an agent should dial: the address set, else the name the browser used for Freightdock with the endpoint's
    /// port. None when the endpoint is off and no address is set.
    pub fn address_for(&self, host_header: Option<&str>) -> Option<String> {
        if let Some(a) = &self.address {
            return Some(a.clone());
        }
        let port = self.port?;
        let host = host_header.map(host_of).filter(|h| !h.is_empty()).unwrap_or("localhost");
        Some(format!("{host}:{port}"))
    }
}

/// The host in a `Host` header: `a.example:7443` is `a.example`, `[::1]:7443` is `[::1]`.
fn host_of(h: &str) -> &str {
    if h.starts_with('[') {
        return h.find(']').map_or(h, |i| &h[..=i]);
    }
    h.rsplit_once(':').map_or(h, |(host, port)| if port.bytes().all(|b| b.is_ascii_digit()) { host } else { h })
}

/// The `docker run` line that starts an agent with its join token.
pub fn run_command(image: &str, address: &str, token: &str) -> String {
    format!(
        "docker run -d --name freightdock-agent --restart unless-stopped \\\n  -v /var/run/docker.sock:/var/run/docker.sock \\\n  -v freightdock-agent:/data \\\n  -e FREIGHTDOCK_SERVER={address} \\\n  -e FREIGHTDOCK_JOIN_TOKEN={token} \\\n  {image}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_is_the_one_set_or_the_browser_s_host_with_the_endpoint_s_port() {
        let mut a = Agents::for_test();
        assert_eq!(a.address_for(Some("freightdock.lan:7443")).as_deref(), Some("freightdock.lan:7444"));
        assert_eq!(a.address_for(Some("192.0.2.10")).as_deref(), Some("192.0.2.10:7444"));
        assert_eq!(a.address_for(Some("[fd00::1]:7443")).as_deref(), Some("[fd00::1]:7444"));
        assert_eq!(a.address_for(None).as_deref(), Some("localhost:7444"));
        a.port = None;
        assert_eq!(a.address_for(Some("x")), None, "no endpoint, nowhere to dial");
        a.address = Some("agents.example.com:443".into());
        assert_eq!(a.address_for(Some("x")).as_deref(), Some("agents.example.com:443"));
    }

    #[test]
    fn the_run_line_names_the_server_the_token_and_the_image() {
        let c = run_command("freightdock-agent:latest", "192.0.2.2:7444", "FDJT1-x");
        assert!(c.contains("-e FREIGHTDOCK_SERVER=192.0.2.2:7444"));
        assert!(c.contains("-e FREIGHTDOCK_JOIN_TOKEN=FDJT1-x"));
        assert!(c.contains("/var/run/docker.sock:/var/run/docker.sock"));
        assert!(c.trim_end().ends_with("freightdock-agent:latest"));
    }
}
