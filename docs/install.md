# Install

Freightdock is the UI of a Freightdock cluster, so a cluster comes first: a manager and a worker on one node is a
whole cluster (the Manager repository's README, `install-node.sh first <host>`). The UI runs beside a manager, as a
container, and talks to the managers' admin API.

## On a manager's node

The UI signs in to the managers with an API token and trusts the cluster CA alone. On the manager's node, as root:

```sh
mkdir -p /var/lib/freightdock-ui
freightdock-manager api-token create ui admin > /var/lib/freightdock-ui/api.token
freightdock-manager ca > /var/lib/freightdock-ui/ca.pem
chmod 600 /var/lib/freightdock-ui/api.token
```

Then the UI:

```sh
docker run -d --name freightdock --restart unless-stopped --network host \
  -e TZ=Europe/London \
  -e FREIGHTDOCK_CLUSTER_API=https://127.0.0.1:7452 \
  -e FREIGHTDOCK_CLUSTER_TOKEN_FILE=/run/freightdock/api.token \
  -e FREIGHTDOCK_CLUSTER_CA_FILE=/run/freightdock/ca.pem \
  -v /var/lib/freightdock-ui:/run/freightdock:ro \
  -v freightdock:/data \
  ghcr.io/freightdock/freightdock:latest
```

- `--network host`, so `127.0.0.1:7452` is the manager on this node, and the UI listens on 7443.
- `FREIGHTDOCK_CLUSTER_API` may list several managers, comma separated (`https://192.0.2.11:7452,https://192.0.2.12:7452`):
  the UI goes on to the next only when one cannot be reached, so a change is never sent twice.
- No Docker socket: the UI has no Docker of its own.

With Compose, the repository's [`docker-compose.yml`](../docker-compose.yml) is the same line written out.

## First start and setup

On its first start Freightdock loads its database from the managers (none there: this one becomes the first), and
makes in `/data`:

- `freightdock.db`, the database, a copy of the managers';
- `secret.key`, the key that seals the secrets it keeps (sign-in providers' secrets, two-factor seeds). It travels to
  the managers with the database, so a UI on another node opens the same secrets;
- `tls/`, a small certificate authority of Freightdock's own (`ca.pem`, `ca.key`) and the HTTPS certificate it signed
  (`cert.pem`, `key.pem`), unless you give a certificate of your own.

The log says the address it serves and the managers it uses:

```
INFO freightdock: the UI of the cluster whose managers are at https://127.0.0.1:7452
INFO freightdock: Freightdock 1.0.0 on https://0.0.0.0:7443
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
choose a name and a password of at least 12 characters. That makes the first administrator and signs you in. Setup
works once; after that it is refused. You land on **The cluster**.

Next: more people ([access.md](access.md)) and single sign-on ([sso.md](sso.md)).

## A UI on another node

Start the same container on another manager's node (its own token, or a copy of the first). It loads the same
database from the managers, so the same people, settings and sessions. Only one UI writes at a time: when another has
written since this one loaded, this one stops (exit code 75) and Docker's restart policy starts it again, loading the
latest. Put both behind one address (a proxy, or DNS) if people should not care which answers.

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
| Node tools | `NODE_EXTRA_CA_CERTS="$HOME/freightdock-ca.pem" node ...` |
| curl | `curl --cacert freightdock-ca.pem ...`, or trust it system-wide as above |

Trusting it means trusting whatever this Freightdock's CA key signs, for any name: keep the data folder to yourself. It is
not the cluster CA (the managers' and workers'); trusting one says nothing about the other.

### The names on the certificate

Tools check that the certificate names the address you typed. Freightdock's names `localhost`, `127.0.0.1`, `::1`, the
machine's host name, and the address it listens on (every interface's but loopback and link-local when it listens on
`0.0.0.0` or `[::]`).

With `--network host` those are the node's; in a container on a network of its own, the container's. Add the names people and tools
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
docker run ... -e FREIGHTDOCK_TLS=off -e FREIGHTDOCK_BIND=127.0.0.1:7080 ghcr.io/freightdock/freightdock:latest
```

What the proxy must do:

- **Pass the `Host` header through unchanged.** Every change and every WebSocket is refused unless its `Origin` is the
  address the request came to, which Freightdock reads from `Host`.
- **Allow bodies of a few megabytes.** A stack's Compose file is sent whole.
- **Redirect plain HTTP to HTTPS.** With `FREIGHTDOCK_TLS=off` the session cookie is not marked `Secure`, so it must never
  be offered over plain HTTP.

Things to know:

- Freightdock sees the proxy's address as everybody's. The activity log and the auth log show it, and the sign-in limit
  (five misses in 15 minutes from one address) counts everybody together: one person guessing locks the sign-in for all
  for the rest of the window. Names are counted on their own as well.

A Caddy example:

```
freightdock.example.com {
    reverse_proxy freightdock:7443
}
```

Caddy passes `Host` as it is. For nginx, set `proxy_set_header Host $host;` and `client_max_body_size 8m;`.

## Environment variables

The full list is in [reference/environment.md](reference/environment.md). The ones you are likely to set:

| Variable | Default | What |
|---|---|---|
| `FREIGHTDOCK_CLUSTER_API` | | The managers' admin API, `https://<manager>:7452`, several comma separated. |
| `FREIGHTDOCK_CLUSTER_TOKEN_FILE` | `/var/lib/freightdock-manager/admin.token` | The file with the API token, read at each call. |
| `FREIGHTDOCK_CLUSTER_CA_FILE` | | The cluster CA (`freightdock-manager ca`), needed for `https://`. |
| `FREIGHTDOCK_SETUP_TOKEN` | made at first start | The first run's setup token, 16 characters or more. |
| `FREIGHTDOCK_TLS` | `on` | `off`: plain HTTP, behind a proxy that does TLS. |
| `FREIGHTDOCK_TLS_CERT`, `FREIGHTDOCK_TLS_KEY` | | Your own certificate and key, only ever read. |
| `FREIGHTDOCK_TLS_NAMES` | | More names and addresses for Freightdock's own certificate: `freightdock.example.com,192.0.2.10`. |
| `TZ` | | The log's time zone. |

## Upgrading

The data is in the volume and at the managers, so an upgrade is a new container on the same volume:

```sh
docker pull ghcr.io/freightdock/freightdock:latest
docker rm -f freightdock
docker run -d --name freightdock ...   # the same line as before
```

On start a newer Freightdock brings the database up to date by itself (the log says `applied database migration N`)
and sends the new schema to the managers as a snapshot. There is no way back down: an older Freightdock refuses a
database a newer one has changed. Upgrade every UI of a cluster together.

Read the **Patch notes** page (a dot in the menu says there are new ones) for what changed.

## Building from source

You need Rust (1.90 or newer):

```sh
git clone https://github.com/freightdock/Freightdock-UI
cd Freightdock-UI
cargo build --release -p freightdock    # target/release/freightdock
docker build -t freightdock .
```

The binary runs outside a container too:

```sh
FREIGHTDOCK_DATA_DIR=./data FREIGHTDOCK_BIND=127.0.0.1:7443 FREIGHTDOCK_CLUSTER_API=http://127.0.0.1:7451 ./target/release/freightdock
```

(`http://127.0.0.1:7451` is the manager's admin API on the same machine, with its admin token.)
