# Running Freightdock safely

Freightdock is a door to root on every host it manages. Treat it as one.

## The ground rules

- **Docker's socket is root.** Whoever can use it can run a privileged container with the host's disk mounted. So is
  anybody who signs in as an administrator, or with a role that may open a console or create containers.
- **Keep it off the open internet.** Put it on a private network or behind a VPN, or behind a proxy that asks who you
  are first (single sign-on in front, or IP rules). Port 7444 is safe to expose only to the hosts that run agents.
- **Use HTTPS all the way.** Trust Freightdock's CA or give it your own certificate ([install.md](install.md#https)).
  With `FREIGHTDOCK_TLS=off`, only behind a proxy that does TLS, on a network you control.
- **Back up, and keep the passphrase elsewhere** ([backups.md](backups.md)).

## What Freightdock does

- Passwords are Argon2id (12 characters at least); sessions and API tokens are kept as hashes, so a copy of the
  database signs nobody in.
- Every change needs the session's CSRF token and Freightdock's own `Origin`; consoles and other WebSockets need the
  `Origin` too, so a page elsewhere cannot open one.
- Five failed sign-ins in 15 minutes lock that address and that name for the rest of the window.
- Secrets it keeps for you (single sign-on's client secret, Git tokens, registry passwords, TLS host keys, notification
  tokens, the Portainer token, the S3 key, the backup passphrase) are sealed with `secret.key` and never sent back to
  the browser.
- Agents join with a token good once for a day that pins Freightdock's CA, then use a client certificate of their own; a
  new join token or removing the environment revokes it at once.
- Roles are checked on the server for every route, by one guard; a route without a rule is refused.
- Every change is in the activity log, every sign-in in the auth log.

## The data folder

`/data` holds what opens everything: `secret.key`, the agents' CA (`ca/`), the HTTPS CA (`tls/ca.key`), the database.
Whoever has it can read every sealed secret, issue agent certificates and certificates your machines trust. Keep it
on the host only, readable by root, out of shared backups that are not sealed.

## People and tokens

- Give people the least role that does the job: Helpdesk or Read-only for looking, Operator for restarting, Standard
  user for their own containers ([access.md](access.md)). Keep Administrators few.
- An Operator or a Standard user can still open a console (root in the container) and a Standard user can create a
  privileged container; there are no per-environment limits on that yet.
- Give programs and AI agents their own API tokens, read-only where they only look, with an expiry. Turn the MCP server
  off in Settings if nobody uses it.
- Webhook addresses are credentials: keep them in your CI's secrets.

## Reporting a vulnerability

See [SECURITY.md](../SECURITY.md).
