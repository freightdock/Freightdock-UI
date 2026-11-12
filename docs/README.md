# Freightdock documentation

## Getting started

- [Install](install.md): Docker run and Compose, ports and volumes, HTTPS and trusting Freightdock's CA, behind a reverse
  proxy, first start, upgrading, building from source.
- [Add a Docker host](agent.md): an agent step by step, ports and firewalls, reconnects, revoking, upgrading and
  removing it; or a TLS host, and which to choose.
- [Backups and restore](backups.md).
- [Running Freightdock safely](security.md).

## Guides

- [Stacks, webhooks and templates](stacks.md): the editor, Git stacks, adopting, webhooks for CI, app templates.
- [Registries](registries.md): saved sign-ins, the registry browser.
- [Swarm](swarm.md): services, Swarm stacks, nodes, secrets, configs.
- [Users, teams, roles and ownership](access.md).
- [Single sign-on](sso.md): OpenID Connect, with Authentik step by step, Keycloak and Entra ID.
- [API tokens and the MCP server](api-and-mcp.md): scripts, and AI agents such as Claude Code.
- [Image updates](updates.md): badges, updating, automatic updates.
- [Notifications](notifications.md): webhook, e-mail, ntfy, Gotify.
- [Resource history and the volume browser](history-and-volumes.md).
- [Coming from Portainer](portainer.md).
- [Appearance](appearance.md).

## Reference

- [Environment variables, commands and ports](reference/environment.md).
- [The HTTP API](reference/api.md).
- [Troubleshooting](troubleshooting.md).

For working on Freightdock itself: [CONTRIBUTING.md](../CONTRIBUTING.md), [CONTEXT.md](../CONTEXT.md),
[ARCHITECTURE.md](../ARCHITECTURE.md) and [DECISIONS.md](../DECISIONS.md).
