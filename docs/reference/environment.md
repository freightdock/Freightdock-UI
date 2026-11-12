# Environment variables

## Freightdock

| Variable | Default | What |
|---|---|---|
| `FREIGHTDOCK_DATA_DIR` | `/data` | The data folder: `freightdock.db`, `secret.key`, `ca/` (the agents' CA), `tls/` (the HTTPS CA and certificate), `stacks/`, `run/` (the remote environments' sockets). |
| `FREIGHTDOCK_BIND` | `0.0.0.0:7443` | Where the web UI and the API listen. |
| `FREIGHTDOCK_TLS` | `on` | `off` (or `0`, `false`, `no`): plain HTTP, for a proxy that does TLS in front. |
| `FREIGHTDOCK_TLS_CERT` | | Your own certificate (a full chain, PEM). Only read, at start; Freightdock does not start when it is missing. |
| `FREIGHTDOCK_TLS_KEY` | | Its key (PEM). |
| `FREIGHTDOCK_TLS_NAMES` | | More names and addresses for Freightdock's own certificate, comma-separated: `freightdock.example.com,192.0.2.10`. |
| `FREIGHTDOCK_DOCKER_HOST` | `DOCKER_HOST`, else `unix:///var/run/docker.sock` | The Docker the `local` environment is. `unix://`, `tcp://` or `http://`. |
| `FREIGHTDOCK_AGENT_BIND` | `0.0.0.0:7444` | Where the agent endpoint listens, or `off`. |
| `FREIGHTDOCK_AGENT_ADDRESS` | | What agents dial, for the `docker run` line, when it is not this server's name and the endpoint's port. |
| `FREIGHTDOCK_AGENT_IMAGE` | `freightdock-agent:latest` | The agent's image the `docker run` line names. A released image names the agent image it was built with. |
| `FREIGHTDOCK_COMPOSE` | the image's Compose plugin | The Compose binary for stacks; else where Docker's packages put the plugin, `~/.docker/cli-plugins`, or `docker-compose` on `PATH`. |
| `FREIGHTDOCK_DOCKER_CLI` | `docker` | The Docker CLI, for Swarm stacks (`docker stack deploy`). |
| `FREIGHTDOCK_CLUSTER_API` | unset | The admin API of a Freightdock cluster's manager on this host (`http://127.0.0.1:7451`); set, Freightdock shows that cluster under Cluster. |
| `FREIGHTDOCK_CLUSTER_TOKEN_FILE` | `/var/lib/freightdock-manager/admin.token` | The manager's admin token, read at each call; mount it read-only when Freightdock runs in a container. |
| `FREIGHTDOCK_SETUP_TOKEN` | made at first start | The first run's setup token (16 characters or more), for an automated install; else one is made, kept in `<data>/setup-token` until setup is done, and printed to the log. |
| `FREIGHTDOCK_BACKUP_PASSPHRASE` | | The passphrase for `freightdock restore <archive>`; typed when not set. |
| `TZ` | `/etc/localtime` | The zone the log's times are written in. |
| `RUST_LOG` | `freightdock=info,warn` | How much it logs: `freightdock=debug` for more. |

Commands of the binary:

| | |
|---|---|
| `freightdock` | Serve. |
| `freightdock --version` | The version. |
| `freightdock setup-token` | The first run's setup token, while nobody is set up yet (`docker exec freightdock freightdock setup-token`). |
| `freightdock --help` | The commands; anything else is refused. |
| `freightdock healthcheck` | Exit 0 when the server accepts; the image's `HEALTHCHECK`. |
| `freightdock restore <archive>` | Check and stage a backup while Freightdock is stopped ([backups.md](../backups.md)). |

## The agent

| Variable | Default | What |
|---|---|---|
| `FREIGHTDOCK_SERVER` | (required) | Freightdock's agent endpoint, `host:port`; 7444 when no port. |
| `FREIGHTDOCK_JOIN_TOKEN` | | The join token, spent once. |
| `FREIGHTDOCK_AGENT_DATA` | `/data` | `key.pem`, `cert.pem`, `ca.pem`, `token.sha256`. |
| `FREIGHTDOCK_AGENT_DOCKER` | `unix:///var/run/docker.sock` | The Docker socket the tunnel leads to. |
| `RUST_LOG` | `freightdock_agent=info,warn` | How much it logs. |

## Ports

| Port | What | Reached from |
|---|---|---|
| 7443/tcp | The web UI, the API, `/mcp`, webhooks | browsers, CI, programs (or only the proxy) |
| 7444/tcp | The agent endpoint (mutual TLS from Freightdock's own CA) | the hosts that run an agent; never through a TLS-ending proxy |
| 2376/tcp, on a TLS host | Docker's TLS port | Freightdock only |
