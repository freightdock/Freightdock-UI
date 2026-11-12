# Freightdock: what is left to do

Done so far: M0 (foundation) and M1 (one host managed in full), plus single sign-on with OpenID Connect (Authentik),
the patch notes, and from M3 environments with the agent and TLS hosts, groups and tags, Home and registries; M2
(stacks, Git stacks, app templates, webhooks); and M6 Swarm. Everything below is still to do, in the order of the plan
(ARCHITECTURE §8). Tick an item off in the same commit that does it, and move what it changes into CONTEXT,
ARCHITECTURE and DECISIONS.

## Loose ends from M0 and M1

- [x] `qa/run.sh`: the end-to-end suite against a throwaway `docker:dind` (create, run, exec, logs, stop, remove, pull,
      build, edit with rollback), run in CI. Today those were checked by hand against a real Docker.
- [x] CodeMirror 6, vendored and pre-bundled (YAML, Dockerfile, env), as an `fd-code-editor` component, used by the
      build page and by stacks and templates for compose files and `.env`.
- [x] Attach to a container's main process (as well as the exec console).
- [x] Connect and disconnect a running container to and from a network from the container page.
- [x] Settings, Appearance: choose a FinUI preset (served through the preset overlay); today it is Washi only.
- [x] Words through catalogues (FinStats' `t()`, FinUI's `words()` fed its part) so the UI can be translated: English
      and Norwegian Bokmål, chosen per browser in Account (DECISIONS "UI" 17).
- [x] Server log times in local time, not UTC.
- [ ] Set up the Authentik side for SSO (application and provider `freightdock`, groups `freightdock-read` and
      `freightdock-write`), then turn SSO on in Settings. Not done: it changes the live sign-in system, so it waits for a yes.

## M2: stacks and templates

- [x] Compose stacks through the bundled `docker compose` CLI: editor, `.env` variables, deploy, update, stop, start,
      remove, pull and redeploy.
- [x] Stacks started elsewhere (by `com.docker.compose.project` labels, Portainer's included) shown as external, and
      adoptable.
- [x] The compose and `.env` editors of stacks and templates are `fd-code-editor` (`codeEditor` in
      `web/assets/js/editor.js`).
- [x] Git stacks: repository, branch, path, credentials (sealed), polling for changes, and a redeploy webhook.
- [x] App templates: Portainer v2 and v3 template JSON by URL, so existing community lists work, plus custom templates.
- [x] Webhooks on containers and stacks for CI redeploys.
- [ ] Git stacks over SSH (a deploy key), and a push webhook that checks the provider's signature (GitHub, GitLab,
      Gitea) instead of only an unguessable address.

## M3: many hosts

- [x] Environments: add, edit and remove TCP+TLS (client certificates) and agent environments; health and last seen;
      `docker::endpoint` answers every kind; the host chosen on Home (a switcher above every page at first, gone
      since "UI" 14). A second local socket (another `unix://` path) is not offered: the local environment is the one
      setup made.
- [x] `freightdock-agent` (its own crate and image): join token, a client certificate from Freightdock's internal CA (rcgen),
      outbound WebSocket with yamux and mTLS, reconnect with backoff. The server exposes each tunnel as a local unix
      socket so bollard and compose treat it like any other host.
- [x] Publish the agent's image to a registry so a host can pull it: the `freightdock-agent` repository's CI builds it for
      amd64 and arm64 with the same tags as Freightdock's, and Freightdock's image names it (`AGENT_IMAGE`).
- [x] The Agent repository on its own: the server takes its crates as git dependencies at its tag, with no checkout
      beside it in CI or the image's build ("Agent" 11).
- [x] At the first release, tag `v1.0.0` in both repositories, and move the server's dependency from the commit to the tag.
- [ ] Host-level extras for the agent (M5), should any need more than Docker's API: a second kind of yamux stream. The
      volume browser did not (DECISIONS "Volume files" 1).
- [x] Environment groups and tags; a home page over every environment (Home, the landing page, no host chosen
      until one is picked; last known numbers for hosts that are down).
- [x] Registries (Docker Hub, GHCR, GitLab, ECR, Quay, any other) with sealed credentials used on pull, push and deploy;
      a registry browser for repositories and tags. Compose deploys and pulls sign in to them
      too (a `DOCKER_CONFIG` written for the job).

## M4: access

- [x] Users and teams managed in the UI.
- [x] Roles as Portainer Business has them (Administrator, Environment admin, Operator, Helpdesk, Standard user,
      Read-only), given per environment or group to a user or team, checked on the server for every route.
- [x] Resource ownership: public, team or private, for containers, stacks, volumes, networks and images.
- [x] Teams from the provider's groups at every sign-in through it (Portainer's automatic team membership): rules by
      name or regular expression, missing teams made on request, hand-given memberships kept, in the auth log.
- [x] Make teams (and their members, where a user of that name exists) from the Portainer users and teams an import
      keeps in `portainer_people`.
- [x] More sign-in: a list of providers, each its own button (OpenID Connect, presets for Google and Microsoft Entra
      ID, GitHub's OAuth with orgs and teams as groups); LDAP and Active Directory with group sync (LDAPS or StartTLS,
      nested groups through AD's in-chain rule, a connection and a user test in Settings); TOTP two-factor (enrolled
      from Account with a QR code, recovery codes, required for everybody by choice, reset by an administrator).
- [x] API tokens: named, shown once, kept as a hash, read-only or not, with an expiry, last use and revoke, in the
      Account page; `Authorization: Bearer` on any route.
- [x] An MCP server at `/mcp` for AI agents (Claude Code), signed in by API token: environments, containers (list,
      inspect, logs, start, stop, restart, kill, remove, create, exec, stats), images (list, pull, remove), volumes,
      networks, disk usage and the activity log, each through the API's own routes and checks.
- [x] An AI agents (MCP) card in Settings: the server on or off for the instance, its address and the Claude Code line,
      every API token with its owner (revoked from there), and the CA to download.
- [x] Freightdock's own HTTPS certificate from a CA of its own, naming the host (loopback, host name, interfaces,
      `FREIGHTDOCK_TLS_NAMES`), made again when the names or the date call for it.
- [x] The auth log, filters, and CSV export for it and the activity log.
- [x] SSO lands in the local account of the same name (a switch), and a local account links to SSO from Account.
- [x] Per-environment security settings: stop non-administrators using privileged mode, bind mounts (an allow-list of
      host paths), host namespaces, devices, capabilities, security options, sysctls and consoles, on an environment or
      its group, everywhere a container is made (containers, templates, stacks, webhooks, Swarm, MCP).

## M5: what Portainer charges for

- [ ] One stack deployed to a group or tag of environments, with its state per environment.
- [x] Image update detection (local digest against the registry's), a badge, and scheduled auto-update.
- [x] Registry credentials for the update check and its pull: `updates::registry::credentials` answers the saved
      registry's sign-in (`registries::auth_for`).
- [x] Volume file browser (through the agent): a helper container through Docker's API, so it works on every kind of
      environment (DECISIONS "Volume files" 1 says why not a second kind of tunnel stream).
- [x] Scheduled backups of Freightdock's data, to a folder or S3, and restore.
- [x] Notifications (webhook, e-mail, ntfy, Gotify): container died or unhealthy, update available, agent down, deploy
      failed (`stacks::run` calls `notify::deploy_failed` for a deploy by hand, by webhook or by the Git poller).
- [x] Container resource history: stats sampled into SQLite, kept for a set number of days, drawn as charts.
- [x] A Portainer migration helper: done through Portainer's API rather than its data volume (Settings, Import from
      Portainer): connect with an access token (sealed; the certificate pinned, or not checked by a recorded choice),
      read only, environments matched by the Docker engine's ID, then a preview and an import of stacks (adopted in
      place, Git stacks as Git stacks), groups, tags, registries (without passwords), custom templates, and teams with
      the members Freightdock has a user of that name for. Idempotent.

## M6: Swarm

- [x] Nodes; services (scale, update, roll back, logs); Swarm stacks (`docker stack deploy`); secrets; configs; tasks;
      a cluster view.
- [ ] Make, join and leave a swarm from Freightdock (`docker swarm init`, join tokens); registry credentials on a service.
      Today a swarm is made on the command line and then managed here.
- [ ] Import Portainer's Swarm stacks and Swarm custom templates as Swarm stacks (`swarm_stacks`, the file recorded,
      the stack adopted by its `com.docker.stack.namespace` label): the import lists them today as "not imported yet"
      (`portainer::plan::stacks`). It needs the scan to know which of Freightdock's hosts is a swarm manager and which
      Swarm stacks it has, and an id to record the import by, as `swarm_stacks` has none.

Never in scope: Kubernetes, Nomad, Azure ACI, licence keys, telemetry. Podman may work through its Docker-compatible API,
as best effort only.
