# Security

Freightdock holds the keys to Docker hosts. Whoever can use Docker's socket is root on that host, and so is anybody who
signs in to Freightdock as an administrator. A hole in Freightdock is a hole in every host it manages, so reports are welcome
and taken seriously.

## Reporting a vulnerability

Please report it privately, not in a public issue:

1. Open the repository's **Security** tab on GitHub.
2. Choose **Report a vulnerability**. This opens a private advisory that only the maintainer sees.

Say what you found, the version (`freightdock --version`, or Patch notes in the app), how to reproduce it, and what it
lets somebody do. A proof of concept helps; please do not test against hosts that are not yours.

You get an answer within a week. Once it is fixed, the release notes say so under **Security**, and you are named
there if you want to be.

## Supported versions

Freightdock is young. Only the latest release gets security fixes; upgrade to it first
([docs/install.md](docs/install.md#upgrading)).

| Version | Supported |
|---|---|
| latest 0.x release | yes |
| older | no |

## What is in scope

- Signing in, sessions, CSRF and `Origin` checks, roles and ownership, API tokens, the MCP server, webhooks.
- The agent and its endpoint (enrolment, the tunnel, the certificates), TLS hosts.
- Secrets at rest (`secret.key`, sealed values), backups and restore.
- Anything that reaches the DOM as HTML instead of text.

Out of scope: what an administrator can do on purpose (they are root on the host by design), and attacks that need
somebody's `secret.key`, data folder or Docker socket already.

## Hardening notes

How to run Freightdock safely (TLS, the agent port, the socket, backups) is in
[docs/security.md](docs/security.md).
