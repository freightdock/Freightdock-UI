# Environment variables

## Freightdock

| Variable | Default | What |
|---|---|---|
| `FREIGHTDOCK_DATA_DIR` | `/data` | The data folder: `freightdock.db`, `secret.key`, `tls/` (the HTTPS CA and certificate). |
| `FREIGHTDOCK_BIND` | `0.0.0.0:7443` | Where the web UI and the API listen. |
| `FREIGHTDOCK_TLS` | `on` | `off` (or `0`, `false`, `no`): plain HTTP, for a proxy that does TLS in front. |
| `FREIGHTDOCK_TLS_CERT` | | Your own certificate (a full chain, PEM). Only read, at start; Freightdock does not start when it is missing. |
| `FREIGHTDOCK_TLS_KEY` | | Its key (PEM). |
| `FREIGHTDOCK_TLS_NAMES` | | More names and addresses for Freightdock's own certificate, comma-separated: `freightdock.example.com,192.0.2.10`. |
| `FREIGHTDOCK_CLUSTER_API` | unset | The managers' admin API: `https://<manager>:7452`, several comma separated, or a manager's `http://127.0.0.1:7451` on the same machine. Unset, Freightdock serves sign-in and settings and says the cluster is not connected. |
| `FREIGHTDOCK_CLUSTER_TOKEN_FILE` | `/var/lib/freightdock-manager/admin.token` | The token it signs in to the managers with (an admin API token, or the manager's admin token over `http://127.0.0.1`), read at each call; mount it read-only. |
| `FREIGHTDOCK_CLUSTER_CA_FILE` | | The cluster CA (`freightdock-manager ca`), the only one trusted over `https://`. |
| `FREIGHTDOCK_CLUSTER_TOKEN` | | The token itself, taken before `FREIGHTDOCK_CLUSTER_TOKEN_FILE`: how the managers start the UI they run. Never written to the log. |
| `FREIGHTDOCK_CLUSTER_CA` | | The cluster CA itself (PEM), taken before `FREIGHTDOCK_CLUSTER_CA_FILE`. |
| `FREIGHTDOCK_SETUP_TOKEN` | made at first start | The first run's setup token (16 characters or more), for an automated install; else one is made, kept in `<data>/setup-token` until setup is done, and printed to the log. |
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

Exit code 75: another Freightdock wrote to the cluster's database since this one loaded it; start it again (Docker's
restart policy does) and it loads the latest.

## Ports

| Port | What | Reached from |
|---|---|---|
| 7443/tcp | The web UI and the API | browsers and programs (or only the proxy) |

The managers' ports (7450 for workers and managers, 7452 for the admin API over TLS) are the Manager repository's.
