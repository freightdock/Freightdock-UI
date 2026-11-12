# Install

Freightdock is one container. It needs Docker's socket (to manage the Docker it runs on) and a volume for its data.

## Docker run

```sh
docker run -d --name freightdock --restart unless-stopped \
  -p 7443:7443 \
  -p 7444:7444 \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v freightdock:/data \
  -e TZ=Etc/UTC \
  ghcr.io/freightdock/freightdock:latest
```

| Tag | What it is |
|---|---|
| `latest` | the newest release |
| `X.Y.Z`, `X.Y` | a release, or the newest of a series |
| `edge` | the newest build from `main`, untested by a release |

Each is built for amd64 and arm64. Until the first release is published, build the image yourself
([Building from source](#building-from-source)) and use `freightdock:latest` instead.

What each part is for:

| | |
|---|---|
| `-p 7443:7443` | The web UI and the API, over HTTPS. |
| `-p 7444:7444` | The agent endpoint, which agents on other hosts dial ([agent.md](agent.md)). Leave it out if you will not use agents. |
| `-v /var/run/docker.sock:...` | The Docker this host runs. Freightdock adds it as the `local` environment at setup. |
| `-v freightdock:/data` | The database, the keys, the certificates and the stacks' files. Back it up ([backups.md](backups.md)). |
| `-e TZ=...` | The zone the log's times are written in. |

Open `https://<host>:7443`. The browser warns about the certificate until you trust Freightdock's own certificate
authority once ([HTTPS](#https)).

**Mind the socket.** Whoever can use Docker's socket is root on that host, and so is anybody who signs in to Freightdock as
an administrator. Keep Freightdock off the open internet, or behind a proxy that asks who you are first.

## Docker Compose

```yaml
services:
  freightdock:
    image: ghcr.io/freightdock/freightdock:latest
    container_name: freightdock
    restart: unless-stopped
    ports:
      - "7443:7443"   # the web UI and the API
      - "7444:7444"   # the agent endpoint (agents on other hosts dial it)
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
      - freightdock:/data
    environment:
      TZ: Etc/UTC

volumes:
  freightdock:
```

```sh
docker compose up -d
docker compose logs -f freightdock
```

The repository's [`docker-compose.yml`](../docker-compose.yml) is the same with the optional settings written out, and
a commented `build: .` to build the image from the source.

## First start and setup

On its first start Freightdock makes, in `/data`:

- `freightdock.db`, the database;
- `secret.key`, the key that seals the secrets it keeps (passwords, tokens). Without it no sealed secret opens again;
- `ca/`, its own certificate authority, which signs the agents' certificates;
- `tls/`, a small certificate authority of Freightdock's own (`ca.pem`, `ca.key`) and the HTTPS certificate it signed
  (`cert.pem`, `key.pem`), unless you give a certificate of your own.

The log says where each went and the address it serves:

```
INFO freightdock: Freightdock 1.0.0 on https://0.0.0.0:7443
INFO freightdock: the agent endpoint on 0.0.0.0:7444 (TLS from Freightdock's own CA)
```

Until there is an administrator, Freightdock wants a **setup token** that only somebody who can reach the server has, so
nobody else on the network can make the first administrator before you. Ask Freightdock for it:

```sh
docker exec freightdock freightdock setup-token
```

It prints 32 letters and digits, the same across restarts until setup is done (it is in the log too:
`docker logs freightdock`). For an automated install, choose it yourself with `FREIGHTDOCK_SETUP_TOKEN` (16 characters or
more).

Then open it in a browser. Every page sends you to **Set up** until there is an administrator: paste the token, and
choose a name and a password of at least 12 characters. That makes the first administrator, adds this host's Docker as
the `local` environment, and signs you in. Setup works once; after that it is refused.

You land on **Home**, the list of every host. Choose `local` to work on it.

Next, depending on what you need:

- Other Docker hosts: [agent.md](agent.md).
- More people: [access.md](access.md), and single sign-on: [sso.md](sso.md).
- Scheduled backups of Freightdock itself: [backups.md](backups.md).
- Coming from Portainer: [portainer.md](portainer.md).

## HTTPS

Freightdock serves HTTPS by default. Without a certificate of your own it keeps a small certificate authority (CA) of its
own in `/data/tls/` (`ca.pem`, and `ca.key`, readable by Freightdock only), made on first start, and an HTTPS certificate
signed by it. **Trust the CA once** and browsers, curl and Claude Code check Freightdock's certificate with no warning.

### Trust the CA

Download it from **Account** or **Settings** (**Download the CA**), or:

```sh
curl -fsSk -o freightdock-ca.pem https://freightdock.example.com:7443/api/tls/ca.pem
```

(`-k` this once: you are fetching what you will trust, so compare it with the one in the data folder,
`docker exec freightdock cat /data/tls/ca.pem`, before you trust it.) Then:

| | |
|---|---|
| Debian, Ubuntu | `sudo cp freightdock-ca.pem /usr/local/share/ca-certificates/freightdock-ca.crt && sudo update-ca-certificates` |
| Fedora, Arch | `sudo trust anchor --store freightdock-ca.pem` |
| macOS | `sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain freightdock-ca.pem` |
| Windows (PowerShell as administrator) | `Import-Certificate -FilePath freightdock-ca.pem -CertStoreLocation Cert:\LocalMachine\Root` |
| Firefox | Settings, Privacy & Security, Certificates, View Certificates, Authorities, Import |
| Node tools (Claude Code) | `NODE_EXTRA_CA_CERTS="$HOME/freightdock-ca.pem" claude` |
| curl | `curl --cacert freightdock-ca.pem ...`, or trust it system-wide as above |

Trusting it means trusting whatever this Freightdock's CA key signs, for any name: keep the data folder to yourself. It is
not the agents' CA ([agent.md](agent.md)); trusting one says nothing about the other.

### The names on the certificate

Tools check that the certificate names the address you typed. Freightdock's names `localhost`, `127.0.0.1`, `::1`, the
machine's host name, and the address it listens on (every interface's but loopback and link-local when it listens on
`0.0.0.0` or `[::]`).

In a container that is the container's own name and address, not the Docker host's. Add the names people and tools
use with `FREIGHTDOCK_TLS_NAMES`, comma-separated host names and addresses:

```sh
docker run ... -e FREIGHTDOCK_TLS_NAMES=freightdock.example.com,192.0.2.10 ghcr.io/freightdock/freightdock:latest
```

The certificate is good for 825 days. It is made again at a start when its names no longer cover these (a new address,
a new name in `FREIGHTDOCK_TLS_NAMES`), when it has less than 30 days left, or when its key is gone; the log says so
(`made the HTTPS certificate in /data/tls/cert.pem (...) for localhost, 127.0.0.1, ...`). It is checked at start only,
so restart Freightdock at least once in those last 30 days. The CA stays, so nothing needs trusting again.

### Your own certificate

A full chain and its key, PEM:

```sh
docker run ... \
  -v /etc/ssl/freightdock:/certs:ro \
  -e FREIGHTDOCK_TLS_CERT=/certs/fullchain.pem \
  -e FREIGHTDOCK_TLS_KEY=/certs/privkey.pem \
  ghcr.io/freightdock/freightdock:latest
```

Freightdock only reads it, at start (restart it after a renewal), and never makes, renews or replaces it. When either file
is not there it says so and does not start. A certificate you put at the default path, `/data/tls/cert.pem`, that its
CA did not sign is left alone too, with a warning. With your own certificate there is no CA to download
(`/api/tls/ca.pem` answers 404).

## Behind a reverse proxy

A proxy that does TLS in front (Traefik, Caddy, nginx) can talk plain HTTP to Freightdock:

```sh
docker run ... -e FREIGHTDOCK_TLS=off ghcr.io/freightdock/freightdock:latest
```

What the proxy must do:

- **Pass the `Host` header through unchanged.** Every change and every WebSocket is refused unless its `Origin` is the
  address the request came to, which Freightdock reads from `Host`.
- **Pass WebSockets.** Logs, stats, consoles and events are WebSockets.
- **Allow long requests and large bodies.** A pull, a build or a stack's deploy answers as it goes and can take
  minutes; an upload to a volume can be 256 MB, a backup to restore 1 GB.
- **Redirect plain HTTP to HTTPS.** With `FREIGHTDOCK_TLS=off` the session cookie is not marked `Secure`, so it must never
  be offered over plain HTTP.

Things to know:

- Freightdock sees the proxy's address as everybody's. The activity log and the auth log show it, and the sign-in limit
  (five misses in 15 minutes from one address) counts everybody together: one person guessing locks the sign-in for all
  for the rest of the window. Names are counted on their own as well.
- **The agent endpoint does not go through the proxy.** Agents sign in with a client certificate, which a proxy that
  ends TLS cannot pass on. Publish port 7444 directly, or through a proxy that passes TCP through untouched, and tell
  Freightdock where agents should dial with `FREIGHTDOCK_AGENT_ADDRESS` ([agent.md](agent.md#behind-nat-or-a-proxy)).

A Caddy example:

```
freightdock.example.com {
    reverse_proxy freightdock:7443
}
```

Caddy passes `Host` and WebSockets as they are. For nginx, set `proxy_set_header Host $host;`, the `Upgrade` and
`Connection` headers, `proxy_read_timeout 3600s;` and `client_max_body_size 1g;`.

## Environment variables

The full list is in [reference/environment.md](reference/environment.md). The ones you are likely to set:

| Variable | Default | What |
|---|---|---|
| `FREIGHTDOCK_SETUP_TOKEN` | made at first start | The first run's setup token, 16 characters or more. |
| `FREIGHTDOCK_TLS` | `on` | `off`: plain HTTP, behind a proxy that does TLS. |
| `FREIGHTDOCK_TLS_CERT`, `FREIGHTDOCK_TLS_KEY` | | Your own certificate and key, only ever read. |
| `FREIGHTDOCK_TLS_NAMES` | | More names and addresses for Freightdock's own certificate: `freightdock.example.com,192.0.2.10`. |
| `FREIGHTDOCK_AGENT_BIND` | `0.0.0.0:7444` | Where the agent endpoint listens, or `off`. |
| `FREIGHTDOCK_AGENT_ADDRESS` | | What agents dial, when not this server's name and port 7444. |
| `FREIGHTDOCK_DOCKER_HOST` | `DOCKER_HOST`, else the socket | The local environment's Docker. |
| `TZ` | | The log's time zone. |

## Upgrading

Your data is in the volume, so an upgrade is a new container on the same volume:

```sh
docker pull ghcr.io/freightdock/freightdock:latest
docker rm -f freightdock
docker run -d --name freightdock ...   # the same line as before
```

With Compose: `docker compose pull && docker compose up -d`.

On start a newer Freightdock brings the database up to date by itself (the log says `applied database migration N`).
There is no way back down: an older Freightdock refuses a database a newer one has changed, and leaves it untouched. So
**make a backup before you upgrade** ([backups.md](backups.md)); going back is the older image with that backup.

Read the **Patch notes** page (a dot in the menu says there are new ones) for what changed. After an upgrade, update
the agents too ([agent.md](agent.md#upgrading-the-agent)): Home marks an agent older than Freightdock as outdated.

## Backups and restore

Backups of Freightdock itself (the database, its keys, the agents' CA, the stacks' files), sealed with a passphrase, to a
folder or S3, on a schedule: [backups.md](backups.md).

## Building from source

You need Rust (1.90 or newer) and Git. Cargo fetches the tunnel protocol from the
[Agent](https://github.com/freightdock/Agent) repository itself:

```sh
git clone https://github.com/freightdock/Freightdock
cd Freightdock
cargo build --release -p freightdock    # target/release/freightdock
docker build -t freightdock .
```

The agent is built from [its own repository](https://github.com/freightdock/Agent).

The binary runs without Docker too. It needs the Compose plugin for stacks (`FREIGHTDOCK_COMPOSE`, else where Docker's
packages put it), `git` for Git stacks and `docker` (the CLI) for Swarm stacks:

```sh
FREIGHTDOCK_DATA_DIR=./data FREIGHTDOCK_BIND=127.0.0.1:7443 ./target/release/freightdock
```
