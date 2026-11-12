# Freightdock documentation

## Getting started

- [Install](install.md): the UI beside a manager, first start, HTTPS and trusting Freightdock's CA, behind a reverse
  proxy, a UI on another node, upgrading, building from source.
- [Running Freightdock safely](security.md).

## Guides

- [Users, teams and roles](access.md), with API tokens and the logs.
- [Single sign-on](sso.md): OpenID Connect, with Authentik step by step, Keycloak and Entra ID; GitHub; LDAP and
  Active Directory; two-factor.
- [Appearance](appearance.md).

## Reference

- [Environment variables, commands and ports](reference/environment.md).
- [The HTTP API](reference/api.md).
- [Troubleshooting](troubleshooting.md).

The cluster itself (managers, workers, installing a node, the admin API) is documented in the Freightdock-Manager and
Freightdock-Worker repositories.

For working on Freightdock itself: [CONTRIBUTING.md](../CONTRIBUTING.md), [CONTEXT.md](../CONTEXT.md),
[ARCHITECTURE.md](../ARCHITECTURE.md) and [DECISIONS.md](../DECISIONS.md).
