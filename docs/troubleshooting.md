# Troubleshooting

First look at Freightdock's log (`docker logs freightdock`); it says what it did and why it refused. `RUST_LOG=freightdock=debug`
says more. Changes are in the **Activity log**, sign-ins and refusals in the **Auth log**.

## An agent will not connect

Look at the agent's log on its host: `docker logs freightdock-agent`.

| The log says | What to do |
|---|---|
| `could not enrol ... connection refused`, `timed out` | The host cannot reach Freightdock's port 7444. Check `nc -vz freightdock.example.com 7444` from the host, Freightdock's `-p 7444:7444`, firewalls on the way, and that `FREIGHTDOCK_SERVER` is an address the host can reach (not `127.0.0.1` or `localhost`, which is the host itself). |
| `could not enrol ... certificate` or `handshake` | Something between them ends TLS (a reverse proxy). The agent endpoint must be reached directly or through a TCP passthrough ([agent.md](agent.md#behind-nat-or-a-proxy)). Or the token is from another Freightdock: its CA does not match. |
| `could not enrol ... the join token` (spent, expired, unknown) | A token is good once, for 24 hours. Make a new one (the lock icon on Environments) and start the agent with it. |
| `this agent has no certificate and no join token it has not spent yet` | Its volume was lost or it never enrolled: start it with a new token. |
| `Freightdock refused this agent's certificate` | The environment was removed or given a new join token. Start the agent with the new token ([agent.md](agent.md#revoking-and-joining-again)). |
| `FREIGHTDOCK_SERVER is not set` / `is not an address` | Give it `host:port`, as `freightdock.example.com:7444`. |
| Docker errors (`/var/run/docker.sock`) | The socket is not mounted: `-v /var/run/docker.sock:/var/run/docker.sock`. |

On Freightdock's side:

- **Adding an agent is refused: "The agent endpoint is off"**: `FREIGHTDOCK_AGENT_BIND=off` and no `FREIGHTDOCK_AGENT_ADDRESS`.
- **The `docker run` line names the wrong address**: it is built from the name your browser used. Change the address
  in the dialog, or set `FREIGHTDOCK_AGENT_ADDRESS`.
- **Freightdock does not start: the agent port is in use**: another Freightdock (or something else) has 7444. Set
  `FREIGHTDOCK_AGENT_BIND` to another port, or `off`.
- **Up, then down every so often**: a NAT or firewall that drops quiet connections. Both ends send keepalives every
  10 seconds after 30 idle; the agent comes back by itself within a minute.

## A TLS host is down

The environment's health shows the error on hover. Check from Freightdock's machine with the same files:

```sh
docker --tlsverify --tlscacert ca.pem --tlscert cert.pem --tlskey key.pem -H tcp://docker.example.com:2376 version
```

A certificate whose names do not hold the address in Freightdock is refused: the daemon's certificate must name it in its
subject alternative names ([agent.md](agent.md#a-tls-host)).

## Stacks cannot be deployed: Compose is missing

The log at start says `no Docker Compose found: stacks can be written but not deployed (set FREIGHTDOCK_COMPOSE)`, or
`Docker Compose at <path>` when it found one. The image has one. A binary run on its own looks at `FREIGHTDOCK_COMPOSE`,
then where Docker's packages put the plugin (`/usr/local/libexec/docker/cli-plugins/`, `/usr/libexec/docker/cli-plugins/`,
`/usr/local/lib/docker/cli-plugins/`, `/usr/lib/docker/cli-plugins/`, each `docker-compose`),
`~/.docker/cli-plugins/docker-compose`, and `docker-compose` on `PATH`. Install `docker-compose-plugin`, or set `FREIGHTDOCK_COMPOSE`.

Git stacks need `git`, Swarm stacks the `docker` CLI; the image has both.

## Permission denied on the Docker socket

Freightdock in its image runs as root and reads the socket you mount. Run as another user (`--user`), it must be in the
socket's group:

```sh
docker run ... --user 1000:1000 --group-add "$(stat -c %g /var/run/docker.sock)" ...
```

and own the data volume. Rootless Docker's socket is at `$XDG_RUNTIME_DIR/docker.sock`: mount that one instead.
SELinux hosts may need `:z` on the volume mounts.

## Certificate warnings

- **The browser warns**: trust Freightdock's CA once ([install.md](install.md#trust-the-ca)). After that a warning means
  the name you typed is not on the certificate: add it with `FREIGHTDOCK_TLS_NAMES` and restart. The log lists the names.
- **Claude Code or curl: `unable to verify the first certificate`, `self-signed certificate in certificate chain`**:
  Node and curl do not use the browser's trust. `NODE_EXTRA_CA_CERTS=freightdock-ca.pem`, `curl --cacert freightdock-ca.pem`.
- **`Hostname/IP does not match certificate's altnames`**: the address is not on the certificate:
  `FREIGHTDOCK_TLS_NAMES`.
- **Freightdock does not start: "... is not there: FREIGHTDOCK_TLS_CERT and FREIGHTDOCK_TLS_KEY name a certificate and key of
  your own, which Freightdock never makes"**: a path you gave has no file (mounted wrong, or not yet issued).
- **"... is not a certificate Freightdock made, so it is left as it is"**: there is a certificate at `/data/tls/cert.pem`
  that its CA did not sign. Name it with `FREIGHTDOCK_TLS_CERT` and `FREIGHTDOCK_TLS_KEY`, or remove it to have Freightdock make
  its own.
- **An expired certificate after a long uptime**: it is renewed at start; restart Freightdock.

## Signed out at once, or every change refused

- **"Forbidden" on every change, consoles that do not open**: the `Origin` is not the address the request came to. A
  proxy must pass the `Host` header through unchanged ([install.md](install.md#behind-a-reverse-proxy)).
- **Signed out after every sign-in behind a proxy with `FREIGHTDOCK_TLS=off`**: the browser reached Freightdock over plain
  HTTP somewhere; use HTTPS all the way to the proxy.
- **Sign-in says "Too many tries. Try again in N minutes."**: five misses in 15 minutes lock that address and that name for the rest of the window.
  Behind a proxy everybody shares the proxy's address.

## Forgot the administrator's password

Another administrator can give a new one (**Users**). If there is none, stop Freightdock and restore a backup, or start
over with a new data folder. There is no reset from the command line yet.

## The database is newer than this Freightdock

An older Freightdock refuses a database a newer one changed, and leaves it untouched. Run the newer image again, or
restore the backup made before the upgrade.
