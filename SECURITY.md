# Security

Freightdock holds an admin token to a cluster's managers, and an administrator who signs in to it runs what they like
on every node. A hole in Freightdock is a hole in every node of the cluster, so reports are welcome and taken
seriously.

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

- Signing in, sessions, CSRF and `Origin` checks, roles, API tokens.
- How it reaches the managers (the cluster CA, the API token) and the database it replicates through them.
- Secrets at rest (`secret.key`, sealed values).
- Anything that reaches the DOM as HTML instead of text.

Out of scope: what an administrator can do on purpose (they are root on the nodes by design), and attacks that need
somebody's `secret.key`, data folder or the UI's API token already. The managers and workers have their own
repositories and their own reports.

## Hardening notes

How to run Freightdock safely (TLS, the token, who is an administrator) is in
[docs/security.md](docs/security.md).
