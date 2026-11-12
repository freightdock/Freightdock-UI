# Troubleshooting

First look at Freightdock's log (`docker logs freightdock`); it says what it did and why it refused. `RUST_LOG=freightdock=debug`
says more. Changes are in the **Activity log**, sign-ins and refusals in the **Auth log**.

## The cluster page says it is not connected

The UI could not reach any manager in `FREIGHTDOCK_CLUSTER_API`, or none took its token. The log says which:

| The log or the page says | What to do |
|---|---|
| `connection refused`, `timed out` | The manager's admin API is not listening there: `ss -ltn` on the manager for 7452 (or 7451 on loopback), and `FREIGHTDOCK_MANAGER_API_TLS_BIND`. With `--network host`, `127.0.0.1` is the node itself. |
| a certificate error | `FREIGHTDOCK_CLUSTER_CA_FILE` is not the cluster CA (`freightdock-manager ca`), or not mounted. The UI trusts that CA alone, for the name `manager.freightdock.internal` whatever address reaches the manager. |
| 401 | The token in `FREIGHTDOCK_CLUSTER_TOKEN_FILE` was revoked, or is not an API token of this cluster. Make another (`freightdock-manager api-token create ui admin`). |
| 403 on a change | The token is a read token: the UI needs an admin one. |

## The UI keeps restarting with exit code 75

Another Freightdock of the same cluster wrote to the database since this one loaded it, so this one stops to load the
latest; Docker's restart policy starts it again. Once is normal when a second UI starts. Again and again means two UIs
writing at once: run one at a time, or put them behind one address so people use one.

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

Another administrator can give a new one (**Users**). If there is none, there is no reset from the command line yet:
the database is the managers', so a new data folder loads the same one.

## The database is newer than this Freightdock

An older Freightdock refuses a database a newer one changed, and leaves it untouched. Run the newer image again, on
every node that runs the UI.
