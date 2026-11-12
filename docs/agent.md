# Add a Docker host

Freightdock manages the Docker it runs on as `local`. Every other host is an **environment** of one of two kinds:

- **Agent** (the usual choice): run `freightdock-agent` on the host. It dials out to Freightdock, so nothing on that host is
  opened to the network, and it works from behind NAT or a firewall that only lets traffic out.
- **TLS host**: Freightdock dials the host's Docker on its TLS port (`dockerd --tlsverify`, 2376) with a client
  certificate. Nothing to run on the host, but Docker's port must be reachable from Freightdock.

Both look the same once added: every page, stacks, templates, updates and the volume browser work on either.

## Which one

| | Agent | TLS host |
|---|---|---|
| Runs on the host | the `freightdock-agent` container | nothing new (Docker's own TLS port) |
| Who opens the connection | the host, to Freightdock's port 7444 | Freightdock, to the host's port 2376 |
| Ports to open | 7444 on Freightdock's side only | 2376 on the host |
| Behind NAT, on a laptop, in another network | works | needs a way in |
| Certificates | made for you (a join token) | you make a CA and certificates |
| Freightdock can see it is gone | at once (the tunnel ends) | at the next check (every 30 s) |

Choose a TLS host when the host's Docker already listens with TLS for other tools, or you may not run a container
there. Otherwise choose the agent.

## The agent, step by step

### 1. Make sure agents can reach Freightdock

The agent endpoint is its own listener, port **7444**, TLS from Freightdock's own certificate authority, whatever
`FREIGHTDOCK_TLS` says. Publish it (`-p 7444:7444`, as in [install.md](install.md)) and check from the host that will run
the agent:

```sh
nc -vz freightdock.example.com 7444
```

The connection goes one way only: **from the agent's host to Freightdock, TCP 7444**. Nothing has to reach the agent's
host. On Freightdock's side allow 7444 in from where your hosts are, for example with ufw:

```sh
sudo ufw allow from 192.0.2.0/24 to any port 7444 proto tcp
```

Docker publishes ports past ufw's rules on many systems (it writes its own iptables rules), so a port published with
`-p 7444:7444` may be open to everybody whatever ufw says. To keep it to one network, publish it on one address
(`-p 192.0.2.10:7444:7444`) or filter in the `DOCKER-USER` chain.

### 2. Add the environment and get a join token

In Freightdock: **Environments**, **Add environment**, kind **Agent**, a name (`vm-1`), and a group and tags if you like. Freightdock
answers with a dialog that shows the join token and the line to run, once:

- The **join token** (`FDJT1-...`) is good once, for 24 hours. It holds the fingerprint of Freightdock's CA, so the agent
  sends it to no other server.
- The **address** field is where the agent will dial: this server's name as your browser reached it, with port 7444,
  or what `FREIGHTDOCK_AGENT_ADDRESS` says. If the host reaches Freightdock by another name or address, change it here; the
  line below follows.

Lost the dialog, or the token ran out? The lock icon beside the environment (**A new join token**) makes a new one.

### 3. Get the agent's image onto the host

From a release:

```sh
docker pull ghcr.io/freightdock/agent:1.0.0
```

Until releases are published, build it from the [freightdock-agent](https://github.com/freightdock/Agent) repository
and carry it over:

```sh
git clone https://github.com/freightdock/Agent && cd freightdock-agent
docker build -t freightdock-agent:latest .
docker save freightdock-agent:latest | ssh vm-1 docker load
```

The line Freightdock gives names the image in `FREIGHTDOCK_AGENT_IMAGE` (`freightdock-agent:latest` unless set; a released
Freightdock names the agent image it was built with).

### 4. Run it on the host

Paste the line from the dialog on the host:

```sh
docker run -d --name freightdock-agent --restart unless-stopped \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v freightdock-agent:/data \
  -e FREIGHTDOCK_SERVER=freightdock.example.com:7444 \
  -e FREIGHTDOCK_JOIN_TOKEN=FDJT1-... \
  freightdock-agent:latest
```

| | |
|---|---|
| `--restart unless-stopped` | It comes back after a reboot of the host. |
| `-v /var/run/docker.sock:...` | The Docker the agent hands to Freightdock. Every connection Freightdock opens through the tunnel goes to this socket. |
| `-v freightdock-agent:/data` | The agent's key, its certificate and Freightdock's CA. Keep this volume: without it the agent has to join again. |
| `FREIGHTDOCK_SERVER` | Freightdock's agent endpoint, `host:port` (7444 when no port is given). A name or an address. |
| `FREIGHTDOCK_JOIN_TOKEN` | Spent once, on the certificate. Afterwards it is good for nothing, so it may stay in the container's settings. |

Its log, `docker logs freightdock-agent`:

```
INFO freightdock_agent: freightdock-agent 1.0.0 for Freightdock at freightdock.example.com:7444, Docker at /var/run/docker.sock
INFO freightdock_agent: enrolled with Freightdock at freightdock.example.com:7444 as "vm-1"
INFO freightdock_agent::tunnel: the tunnel to Freightdock at freightdock.example.com:7444 is up
```

In Freightdock the environment turns **up** on Home and Environments, with the agent's version and the address it dialled
from. Choose it on Home to work on it.

What happened: the agent made its own key, checked that the server it reached has a certificate from the CA the token
names, and sent the token with a certificate request. Freightdock signed it, kept its fingerprint, and spent the token.
From then on the agent opens a WebSocket with that certificate, and every request Freightdock makes to that host's
Docker is a stream inside it.

### With Docker Compose

```yaml
services:
  freightdock-agent:
    image: freightdock-agent:latest
    container_name: freightdock-agent
    restart: unless-stopped
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
      - freightdock-agent:/data
    environment:
      FREIGHTDOCK_SERVER: freightdock.example.com:7444
      FREIGHTDOCK_JOIN_TOKEN: FDJT1-...   # spent on the first start; may stay

volumes:
  freightdock-agent:
```

Do not deploy the agent as a Freightdock stack on the host it serves: a redeploy would cut the tunnel the redeploy runs
through.

## Reconnects

The agent dials again after any break: a second first, doubling up to a minute, a little either way at random so many
agents do not all come back at once after Freightdock restarts. A tunnel that has run for half a minute starts the wait
from a second again. Both ends send TCP keepalives, so a host that vanished is noticed within about a minute.

While an agent is away, its environment is **down**: Home shows its last known numbers, greyed, with their time; its
pages answer that the agent is not connected; and a notification channel that wants `environment.down` is told
([notifications.md](notifications.md)). Nothing is lost: when it dials again, it is up.

## Revoking and joining again

A **new join token** (the lock icon) revokes the agent's certificate and cuts its tunnel at once. The old certificate
opens nothing any more; the agent's log says:

```
WARN freightdock_agent: Freightdock refused this agent's certificate: This agent's certificate is not one Freightdock knows: its environment was removed or given a new join token.
```

and it keeps trying. To join it again, start it with the new token. The volume can stay; the agent sees the new token,
forgets the old certificate and enrols:

```sh
docker rm -f freightdock-agent
docker run -d --name freightdock-agent ... -e FREIGHTDOCK_JOIN_TOKEN=FDJT1-<the new one> freightdock-agent:latest
```

Use a new token when a host or its agent volume may have been copied or stolen, or to move the environment to
another machine.

## Upgrading the agent

Home marks an agent older than Freightdock as **outdated**. On the host:

```sh
docker pull ghcr.io/freightdock/agent:<version>   # or load a newly built one
docker rm -f freightdock-agent
docker run -d --name freightdock-agent ... <the same line, with the new image>
```

Keep the `freightdock-agent` volume; no new token is needed. With Compose, change the tag and `docker compose up -d`.

## Removing it

In Freightdock, remove the environment (the bin beside it): Freightdock forgets its certificate and cuts the tunnel. Then on
the host:

```sh
docker rm -f freightdock-agent
docker volume rm freightdock-agent
```

The containers the agent let Freightdock manage stay as they are. They were Docker's all along.

## Behind NAT or a proxy

Agents dial what the `docker run` line says. Freightdock fills it from the name your browser used and port 7444. When
agents must dial something else (a public address forwarded to Freightdock, a TCP proxy, another port), set it once:

```sh
-e FREIGHTDOCK_AGENT_ADDRESS=freightdock.example.com:443
```

A proxy in front of the agent endpoint must pass TCP through untouched (TLS passthrough, for example Traefik's
`HostSNI` with `passthrough: true`, or an nginx `stream` block). It must not end TLS: the agent signs in with its
client certificate, inside that TLS. The agent checks Freightdock by its CA, not by the name it dialled, so any name or
address that leads to the endpoint works.

`FREIGHTDOCK_AGENT_BIND` chooses where the endpoint listens (`0.0.0.0:7444`), or `off` to have none; with it off and no
`FREIGHTDOCK_AGENT_ADDRESS`, adding an agent is refused, since it would have nowhere to dial.

## The agent's settings

| Variable | Default | What |
|---|---|---|
| `FREIGHTDOCK_SERVER` | (required) | Freightdock's agent endpoint, `host:port`; 7444 when no port |
| `FREIGHTDOCK_JOIN_TOKEN` | | The join token, used once |
| `FREIGHTDOCK_AGENT_DATA` | `/data` | `key.pem`, `cert.pem`, `ca.pem`, and `token.sha256` (the token last spent) |
| `FREIGHTDOCK_AGENT_DOCKER` | `unix:///var/run/docker.sock` | The Docker socket it leads to |
| `RUST_LOG` | `freightdock_agent=info,warn` | How much it logs |

## A TLS host

Docker's own TLS port, with client certificates. Freightdock needs the CA that signed the daemon's certificate, a client
certificate signed by a CA the daemon trusts, and that client's key.

### 1. Make the certificates

On the Docker host, as Docker documents it (`https://docs.docker.com/engine/security/protect-access/`), shortened:

```sh
# A CA
openssl genrsa -out ca-key.pem 4096
openssl req -new -x509 -days 3650 -key ca-key.pem -sha256 -subj "/CN=docker-ca" -out ca.pem

# The daemon's certificate, for the names and addresses Freightdock will use
openssl genrsa -out server-key.pem 4096
openssl req -new -key server-key.pem -subj "/CN=docker.example.com" -out server.csr
echo "subjectAltName = DNS:docker.example.com,IP:192.0.2.20
extendedKeyUsage = serverAuth" > server.cnf
openssl x509 -req -days 825 -sha256 -in server.csr -CA ca.pem -CAkey ca-key.pem -CAcreateserial \
  -extfile server.cnf -out server-cert.pem

# Freightdock's client certificate
openssl genrsa -out key.pem 4096
openssl req -new -key key.pem -subj "/CN=freightdock" -out client.csr
echo "extendedKeyUsage = clientAuth" > client.cnf
openssl x509 -req -days 825 -sha256 -in client.csr -CA ca.pem -CAkey ca-key.pem -CAcreateserial \
  -extfile client.cnf -out cert.pem
```

### 2. Let dockerd listen with TLS

`/etc/docker/daemon.json`:

```json
{
  "hosts": ["unix:///var/run/docker.sock", "tcp://0.0.0.0:2376"],
  "tls": true,
  "tlsverify": true,
  "tlscacert": "/etc/docker/tls/ca.pem",
  "tlscert": "/etc/docker/tls/server-cert.pem",
  "tlskey": "/etc/docker/tls/server-key.pem"
}
```

On systemd hosts the unit passes `-H fd://`, which clashes with `hosts` in the file: override it with
`ExecStart=/usr/bin/dockerd` (`systemctl edit docker`), then `systemctl restart docker`. Allow TCP 2376 in **from
Freightdock only**: anyone with a client certificate from that CA is root on the host.

Check it from Freightdock's machine:

```sh
docker --tlsverify --tlscacert ca.pem --tlscert cert.pem --tlskey key.pem -H tcp://docker.example.com:2376 version
```

### 3. Add it in Freightdock

**Environments**, **Add environment**, kind **TLS host**: a name, the address (`tcp://docker.example.com:2376`), and paste
`ca.pem`, `cert.pem` and `key.pem`. All three are checked when you save, and the key is sealed and never shown again;
it is opened in memory only, never written to disk in the clear. Editing later, an empty key field keeps the one
there is.

Freightdock asks every environment for its version every 30 seconds; a TLS host that stops answering is marked down at
the next check.

## When it does not work

See [troubleshooting.md](troubleshooting.md#an-agent-will-not-connect).
