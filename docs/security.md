# Running Freightdock safely

Freightdock's administrators run what they like on every node of the cluster. Treat it as a door to root.

## The ground rules

- **An Administrator is root on every node.** A stack can run a privileged container with a node's disk mounted, and
  an Administrator deploys stacks. So does whoever holds the UI's admin API token for the managers.
- **Keep it off the open internet.** Put it on a private network or behind a VPN, or behind a proxy that asks who you
  are first (single sign-on in front, or IP rules).
- **Use HTTPS all the way.** Trust Freightdock's CA or give it your own certificate ([install.md](install.md#https)).
  With `FREIGHTDOCK_TLS=off`, only behind a proxy that does TLS, on a network you control.

## What Freightdock does

- Passwords are Argon2id (12 characters at least); sessions and API tokens are kept as hashes, so a copy of the
  database signs nobody in.
- Every change needs the session's CSRF token and Freightdock's own `Origin`.
- Five failed sign-ins in 15 minutes lock that address and that name for the rest of the window.
- Secrets it keeps for you (sign-in providers' client secrets and bind passwords, two-factor seeds) are sealed with
  `secret.key` and never sent back to the browser.
- It reaches the managers over TLS, trusting the cluster CA alone, with an API token kept in a file it only reads.
- Roles are checked on the server for every route, by one guard; a route without a rule is refused.
- Every change is in the activity log, every sign-in in the auth log.

## The data folder

`/data` holds `secret.key`, the HTTPS CA (`tls/ca.key`) and the database. Whoever has it can read every sealed secret
and issue certificates your machines trust. The managers hold the database and `secret.key` too (readable only with an
admin token), so guard their data folders and the UI's API token the same way.

## People and tokens

- Keep Administrators few: a user reads the cluster and changes nothing ([access.md](access.md)).
- Give programs their own API tokens, read-only where they only look, with an expiry.
- The UI's token for the managers is an admin token: keep its file readable by root only (`chmod 600`), mounted
  read-only into the container.

## Reporting a vulnerability

See [SECURITY.md](../SECURITY.md).
