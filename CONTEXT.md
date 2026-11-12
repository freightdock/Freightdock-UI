# Freightdock: context

*Last updated: 2026-11-10 (1.0.0, M0 and M1, SSO, patch notes, the end-to-end suite, environments and the agent, M2:
stacks, templates, webhooks; groups, tags and Home; registries; API tokens and the MCP server; Swarm; M5, and import from
Portainer; M4: users, teams, roles, ownership (volumes, networks and images too), the auth log, security settings;
sign-in providers (OpenID Connect, Google, Microsoft, GitHub), LDAP and Active Directory, teams from groups, TOTP
two-factor; the MCP card in Settings, a CA for the HTTPS certificate).*

The map. How the pieces work is in [`ARCHITECTURE.md`](ARCHITECTURE.md); the rules and their reasons in
[`DECISIONS.md`](DECISIONS.md). The plan, milestone by milestone (M0 to M6), is in ARCHITECTURE §8.

## Layout

```
Cargo.toml                 workspace: the server. The agent and the tunnel protocol are the Agent repository
                           (`freightdock-tunnel`, `freightdock-agent`, git dependencies at its tag in crates/server/Cargo.toml)
.cargo/config.toml.example the patch that builds them from ../freightdock-agent, for protocol work only (copied to
                           .cargo/config.toml, not committed; Cargo.lock is not committed while it is in place)
crates/server/             the `freightdock` binary
  src/mcp.rs               /mcp, the MCP server: JSON-RPC over POST, sessions, the tools (each a request to the API
                           through api::router with the caller's token); its switch and the Settings card's data
  src/main.rs              config from the environment, the HTTPS server (hyper driven directly), the agent endpoint's
                           listener, the watches, `healthcheck`, `setup-token`, `restore <archive>`, `--help` (anything else is refused), starting itself again after a restore
  src/agent/mod.rs         Agents (the CA, the endpoint's TLS, where agents dial, the image), the `docker run` line
  src/agent/ca.rs          Freightdock's own CAs: the agents' (made and kept in <data>/ca, signs agent and endpoint
                           certificates) and, by another name, the web server's in <data>/tls (issue_server)
  src/agent/endpoint.rs    the agent endpoint: POST /agent/v1/enrol, GET /agent/v1/tunnel (WebSocket, yamux)
  src/agent/e2e.rs         tests: a real agent through a real endpoint to a fake Docker; a TLS host
  src/environments.rs      /api/environments: list, add, edit (group, tags), remove, join token; check() and watch()
                           for health and the last snapshot
  src/groups.rs            /api/environment-groups and /api/tags; Placement (an environment's group and tags)
  src/api.rs               /api routes for sign-in, extractors Authed / Admin / Allowed / Access / Peer, the CSRF and
                           Origin rules; puts the access guard on every route
  src/access/mod.rs        roles, actions, TABLE (role × action), resolve (grants to a role), Access, levels/load
  src/access/routes.rs     RULES (every route's rule), the guard, may_container; the test that every route has a rule
  src/access/ownership.rs  public / team / private / the administrators': Owners, the record, Wanted, freightdock.owner
  src/access/security.rs   per-environment security settings: Rules (kept per environment or group), Spec from a create
                           body, a resolved compose file, a service spec or a volume; judge, Judge, applies/check/hold;
                           its routes; tests in security/ (rules, and every role through the API)
  src/access/api_tests.rs  tests: a fake Docker and one user per role (World), every role against every area
  src/users.rs             /api/users, /api/teams, an environment's or group's grants, /api/me/password
  src/authlog.rs           the auth log, /api/auth-log; CSV (csv_file, field) for it and the activity log
  src/appearance.rs        the FinUI preset this instance wears: its code, the overlay, its settings
  src/auth.rs              Argon2id, sessions (hashed tokens), the sign-in limiter
  src/audit.rs             the activity log
  src/changelog.rs         CHANGELOG.md, compiled in and parsed for /api/changelog
  src/replica.rs           the database replicated by the managers in a cluster: triggers to _replog, load, push, snapshots
  src/cluster.rs           the cluster this Freightdock is the UI of: the manager's admin API asked for /api/cluster/...
  src/db.rs                SQLite pool, MIGRATIONS, Db::call
  src/docker/mod.rs        Docker (one endpoint), endpoint(env) for any kind, negotiate, api_error, valid_name/valid_ref
  src/docker/sockets.rs    Sockets: a remote environment as <data>/run/env-<id>.sock, joined to its Dial
  src/docker/summary.rs    one host in numbers (containers running/stopped/unhealthy, images, volumes, stacks, nodes,
                           CPUs, memory) for the home page, kept as the environment's snapshot
  src/docker/tls_host.rs   a TLS host: address, pasted certificates checked, the dial with its client certificate
  src/docker/api.rs        the /api/envs/{env}/... routes
  src/docker/containers.rs list, inspect, actions, create/edit (make, a task of its own: replace, and with settle stay up or be put back),
                           recreate, rename, remove, prune, config
  src/docker/images.rs     list, inspect, tag, remove, prune; pull/push/build/load as NDJSON; export
  src/docker/volumes.rs    list (with users), inspect, create, remove, prune
  src/docker/volume_files.rs the volume file browser: a busybox helper per request; list, download (tar read as it
                           streams), upload, mkdir, delete; clean() keeps a path inside the volume
  src/docker/networks.rs   list (with counts), inspect, create, remove, prune, connect, disconnect
  src/docker/own.rs        the container Freightdock runs in (found at start from its mounts), whose console is an
                           Administrator's alone
  src/docker/streams.rs    WebSockets: logs, stats, exec, attach, events; logs download; one stats sample of all
  src/docker/system.rs     info, df, system prune
  src/docker/raw.rs        the Engine API as JSON in and out (what bollard lacks; specs sent back whole; get() for the
                           background passes), each answer held to MAX_ANSWER, log frames (to MAX_FRAME),
                           follow() for JSON lines (events, stats; a line to MAX_LINE)
  src/docker/cli.rs        Docker's CLI for an environment: docker_host(env) is its DOCKER_HOST; run with a time limit
  src/docker/swarm/        Swarm: mod.rs (role from /info, routes), nodes, services (scale, update, roll back, logs,
                           tasks), objects (secrets and configs), stacks (docker stack deploy, files in swarm_stacks)
  src/error.rs             ApiError: status, sentence, field
  src/notify.rs            notifications: channels (webhook, e-mail, ntfy, Gotify), notify() with its quiet window, the
                           events watch (deaths, failing health checks), deploy_failed for stacks, the test route
  src/updates/mod.rs       image updates: the pass (verdicts per container and image, the cache), update (pull, make
                           again, put back), auto-update, the watch, its routes and settings
  src/updates/registry.rs  a registry asked: references, the manifest HEAD, WWW-Authenticate, an index's platforms,
                           verdict(); credentials() (the saved registry's sign-in, registries::auth_for)
  src/outbound.rs          answers from hosts others name, read to a size limit (capped)
  src/backup/mod.rs        backups: settings, make (online copy, archive, seal, put, retention), the watch, restore
                           (stage into <data>/restore, apply_staged at start), the routes
  src/backup/crypt.rs      the archive's sealing: Argon2id key, XChaCha20-Poly1305 in 1 MiB chunks
  src/backup/s3.rs         a small S3 client: Signature Version 4, put/get/list/delete, the bucket made when missing
  src/history.rs           resource history: the sampler (watch), retention, /history as chart points, its settings
  src/oidc.rs              single sign-on through an OAuth provider of the list: discovery, PKCE login and callback
                           (each provider's own, and the old /api/auth/oidc/callback), ID token, sso_user, linking
  src/signin/mod.rs        Identity, admit() (domains, role by groups, the account found/linked/made, teams), the
                           sign-in page's list of buttons
  src/signin/providers.rs  the providers list (settings `sso`), presets, check(), role_for, the settings routes
  src/signin/github.rs     GitHub: the code swapped, /user, /user/emails, /user/orgs and /user/teams
  src/signin/ldap.rs       LDAP and Active Directory: filter escaping, connect (LDAPS, StartTLS, a CA), bind, find,
                           groups, the password form's second try, the Settings test
  src/signin/teams.rs      teams from groups: rules (a name or /regex/), wanted(), apply() with synced_by
  src/signin/totp.rs       TOTP: RFC 6238, base32, recovery codes, sign-in tickets and the code step, Account's
                           routes, an administrator's reset, the `two_factor` setting
  src/registries/mod.rs    /api/registries: list, add, edit, remove (tested on save), browse; auth_for(host),
                           auth_for_image, docker_config for compose
  src/registries/reference.rs  an image reference's registry host, repository, tag, digest; Docker Hub's names
  src/registries/v2.rs     the registry API: the WWW-Authenticate challenge (Basic, Bearer token per scope), check,
                           catalog, tags
  src/registries/hub.rs    Docker Hub's own API: an account's or organisation's repositories
  src/registries/ecr.rs    ECR: SigV4 (HMAC-SHA256 on sha2), GetAuthorizationToken, the token kept until it nearly ends
  src/portainer/mod.rs     import from Portainer: the connection (token sealed), scan, import, portainer_imports
  src/portainer/client.rs  Portainer's API, GET only: X-API-Key, no redirects, the certificate pinned or not checked
  src/portainer/plan.rs    pure: environments matched (engine ID, host name, name, address), each stack's line, .env, each team and its members
  src/portainer/fixtures/  Portainer Business 2.45.3's answers, anonymised (endpoints, stacks, docker info)
  src/secret.rs            Sealer: XChaCha20-Poly1305 under <data>/secret.key
  src/stacks/mod.rs        stacks: the store in <data>/stacks/<id>/, the lock per stack, jobs as NDJSON, adopt; routes
  src/stacks/compose.rs    the Compose binary: finding it, arguments, docker_env(env) (how Compose reaches an
                           environment, the one place), running it with an emptied environment; registry_config
                           (the saved registries' sign-ins as a DOCKER_CONFIG for a job that pulls)
  src/stacks/projects.rs   containers grouped into Compose projects by label (external stacks)
  src/stacks/confine.rs    a stack kept to its own folder: inside() follows every link and .., read(), references()
                           (every file a compose file makes Compose read, includes and extends followed)
  src/stacks/git.rs        Git stacks: ls-remote (polling), shallow clone and fetch, credentials as a header
  src/templates/mod.rs     app templates: Portainer's v1/v2/v3 JSON, the lists by address (cached), custom
                           templates, a template as Docker's create body or a stack; fixtures/ (v2, v3)
  src/webhooks.rs          webhooks: tokens (hash only), the rate limits (Gate), making, removing and calling them
  src/state.rs             AppState (db, limiter, docker, sealer, oidc, stacks, templates, webhooks)
  src/tls.rs               HTTPS: Freightdock's own CA in <data>/tls, the certificate's names (host, interfaces,
                           FREIGHTDOCK_TLS_NAMES), the renewal rule (verdict), the acceptor; GET /api/tls/ca.pem
  src/tokens.rs            API tokens: make, parse, the bearer header, lookup; /api/tokens
  src/web.rs               the embedded UI, /assets/finui.css joined (the preset's overlay after tokens.css), the SPA fallback;
                           harden: CSP, X-Frame-Options, nosniff, Referrer-Policy on every answer, no-store on /api
web/
  index.html               the one page
  assets/app.css           the app's layout, in FinUI tokens
  assets/i18n/             en.json (the source) and nb.json: every word of the UI, FinUI's under `finui` ("UI" 17)
  assets/js/               main (boot: the language, then app), app (routes), i18n (t, tOr, tParts, lang, the language
                           choice), router, api, state, shell, dom (num, pct, bytes, ago, when through Intl), view (loadInto, table, filterBox),
                           stream (NDJSON progress), confirm, docker (names, states, ports), editor (codeEditor:
                           fd-code-editor in a labelled field), webhook (the webhook card), swarm (is the host a
                           manager, swarmPage, how tasks, nodes and services read), ownership (the Access card,
                           accessPicker), grants (who has which role on an environment or group, the dialog),
                           security (an environment's or group's security settings, the dialog);
                           state.can(action) for the host shown
  assets/js/cluster.js     whether this Freightdock is a cluster's UI (asked of /api/cluster), for the menus
  assets/js/pages/         cluster (the cluster: managers, nodes, events, commands, join tokens), cluster-stacks (the cluster's Compose stacks and their workloads), home (every host, the landing page), env-groups (groups and tags: fields, cards), dashboard,
                           containers, container (tabs), container-edit (new/edit/duplicate), images
                           (list, one, build), volumes, volume-files, networks, events, environments, registries (list
                           and browser), notifications, backups, activity, settings,
                           changelog, account, setup, login, prune, rename, stacks (list, new, external), stack (one),
                           stack-git, templates (lists, custom, the editor); swarm (cluster, nodes), swarm-services
                           (list, one, create), swarm-stacks (list, deploy, one), swarm-objects (secrets, configs),
                           portainer (import from Portainer), users (users and teams), signins (the auth log), signin-settings (Settings' providers and
                           two-factor), two-factor (the QR code and key, recovery codes; sign-in and Account)
  assets/js/components/    Freightdock's own, FinUI's contract with an ss- prefix: terminal (xterm), output-view,
                           code-editor (CodeMirror: YAML, Dockerfile, env, properties, shell), qr-code (an SVG QR
                           code from qr-encode.js, a QR encoder of our own: byte mode, level M, versions 1 to 40)
  assets/vendor/xterm/     xterm.js 5.5.0 + addon-fit 0.10.0, MIT, unchanged
  assets/vendor/codemirror/ CodeMirror 6 (view 6.43.15) bundled into one ES module, MIT; VERSION says how to rebuild it
  assets/finui/            FinUI, copied source (Washi, as it ships), with create/preset.js, create/presets.json and
                           the preset overlay p/<axis>/<option>.css that FinUI's site publishes
  assets/finmotion/        FinMotion, copied source
crates/server/tests/qa.rs  the end-to-end suite (ignored by `cargo test`; qa/run.sh runs it)
crates/server/tests/log_time.rs  the real binary logs in the zone TZ names
docs/                      the user's guides (README.md is the index): install, agent (adding a host), stacks,
                           registries, swarm, access, sso, api-and-mcp, updates, notifications, history-and-volumes,
                           backups, portainer, appearance, security, troubleshooting; reference/ (environment, api);
                           screenshots/ (<page>.png light and <page>-dark.png dark, made by qa/screenshots.sh). Kept
                           current with the code
qa/                        checks against real things: run.sh (the end-to-end suite against docker:dind),
                           screenshots.sh with screenshots.mjs (docs/screenshots/ from a throwaway Freightdock with four
                           dinds: two real agents and a TLS host, set up through the API, taken in a browser over the
                           DevTools protocol), lib.sh (what the two share: a free port, the pinned Compose), js-check.sh
                           (node --check, i18n-check.mjs: the modules' words against the catalogues; in a node
                           container, or `--here` with the node on the PATH, as CI does);
                           finui-presets.mjs makes web/assets/finui/p again from the copied FinUI
Dockerfile, docker-compose.yml   the image (`AGENT_IMAGE` build argument: the agent image its `docker run` line names)
.github/workflows/ci.yml   gitleaks, tests, QA, the image for amd64 and arm64, :edge from main, releases from tags;
                           .github/dependabot.yml (the pins, weekly)
.github/ISSUE_TEMPLATE/, PULL_REQUEST_TEMPLATE.md   issue forms (a bug, an idea) and the pull request checklist
.gitleaks.toml             gitleaks' rules: its own, Freightdock's tokens, private addresses, the owner's domain; allowlist
.githooks/pre-commit       the hook (`git config core.hooksPath .githooks`): files refused, a fast check, gitleaks
SECURITY.md, CONTRIBUTING.md, CODE_OF_CONDUCT.md   reporting a vulnerability; how work is done here; conduct
README.md                  FinStats' shape: what it is, what it does, a docker run, links into docs/
```

## Tables (migrations 1 to 20)

| Table | What |
|---|---|
| `settings` | key/value; `sso` is `{providers: [...]}`, each sign-in provider (id, kind `oidc`/`github`/`ldap`, preset, groups, domains, `teams` rules, `ldap` settings; client secret and bind password sealed; migration 18 made the old `oidc` setting its first, id `oidc`); `two_factor` is `{required}`; `appearance` is `{"preset": code}`; `history` is `{enabled, every_s, keep_days}`; `updates` is `{enabled, check_hours, auto_update}`; `backups` is `{enabled, every_hours, keep, destination, folder, s3 {endpoint, region, bucket, prefix, access_key, secret_key (sealed), virtual_host}, passphrase (sealed)}`, `backups.state` how the last went; `templates` the template list addresses; `portainer` the Portainer connection (url, token sealed, ca, skip_verify, version, instance); `portainer_people` Portainer's users, teams and memberships as Portainer has them (the import's copy); `mcp` is `{enabled}` (on when missing) |
| `users` | name (unique, case-blind), Argon2id hash (`!` for SSO and directory users), role `admin`/`user`, disabled; `oidc_issuer` + `oidc_sub` (unique; the issuer is `ldap:<provider id>` for a directory), `email`; `totp_secret` (sealed), `totp_pending` (sealed, while it is set up), `totp_last_step`; `picture` (the provider's, "Users" 1) |
| `avatars` | the copy kept of each user's picture: `content_type`, `data`, `fetched_at` |
| `recovery_codes` | user, the SHA-256 of a recovery code; deleted when used, gone with the user |
| `sessions` | SHA-256 of the token, user, CSRF token, created/expires/last seen, ip, user agent |
| `environments` | name (unique, case-blind), kind `local`/`tls`/`agent`, url; `status` up/down/unknown, `last_seen_at`, `last_error`, `docker_version`; agent: `join_token_hash` + `join_token_expires_at` (the token's secret, hashed, good once), `agent_cert_sha256`; tls: `tls_ca`, `tls_cert`, `tls_key` (sealed); `group_id` (at most one group); agent: `agent_version` (from its user agent), `agent_peer` (where it last dialled from); `snapshot` (JSON, the last summary) and `snapshot_at`. Setup makes `local` |
| `environment_groups` | name (unique, case-blind), description; an environment's `group_id` points here (null when the group goes) |
| `tags`, `environment_tags` | a tag's name (unique, case-blind); which environment has which (both sides cascade) |
| `registries` | name (unique, case-blind), kind `dockerhub`/`ghcr`/`gitlab`/`ecr`/`quay`/`forgejo`/`gitea`/`custom` (the last two from migration 20), url (`host[:port]`, `http://` for plain HTTP), host (unique: what references match, Docker Hub as `docker.io`), username (ECR: the access key id), secret (sealed), region (ECR) |
| `audit` | at, who, action, target, detail, ip, `via` (`token <name>` for a change made with an API token) |
| `stacks` | environment, name (the Compose project, unique per environment), source `editor`/`git`, the Git settings (url, ref, path, user, token sealed, poll seconds, commit deployed, last check and its error), who made it, made/changed/deployed |
| `webhooks` | SHA-256 of the token (unique), environment, kind `stack`/`container`, target (a stack's id, a container's name), who made it, made, last called |
| `templates` | custom app templates: title, description, note, categories, logo, compose file, the variables asked for (JSON) |
| `api_tokens` | user, name (unique per user, case-blind), SHA-256 of the token, `hint` (its last four), read_only, created/expires/last used, last used ip |
| `swarm_stacks` | env (gone with its environment), name, the Compose file last deployed, updated_at, updated_by |
| `notification_channels` | name, kind `webhook`/`email`/`ntfy`/`gotify`, config (JSON; its `secret` sealed), events (JSON list), enabled, last_sent_at, last_error |
| `image_checks` | reference (`registry/repository:tag`), digest, is_index, platforms (an index's, JSON, once fetched), checked_at, error |
| `update_status` | env, kind `container`/`image`, id, name, reference, status `current`/`outdated`/`unknown`/`pinned`/`error`, detail, remote digest, labelled, checked_at. Goes with its environment |
| `auto_updates` | env, container name: switched to automatic updates on its page |
| `container_stats` | env, container name, at; cpu (% of one CPU), mem, mem_limit, and the counters net_rx/tx, disk_read/write as Docker gives them, pids. Goes with its environment |
| `portainer_imports` | what an import from Portainer made: Portainer's InstanceID, kind (stack, group, tag, registry, template, team), Portainer's id, Freightdock's id, when |
| `teams`, `team_members` | a team's name (unique, case-blind); who is in which (both sides cascade), and `synced_by`: the provider whose groups put them there (NULL: by hand or by a Portainer import, and a sign-in never takes them out) |
| `grants` | a role (`env_admin`, `operator`, `helpdesk`, `standard`, `read_only`) for a user or a team, on an environment or a group; one per pair |
| `resource_access` | a container's, volume's or network's (by name), stack's (by id) or image's (by its id) ownership per environment: `public`, `team` (team_id), `private` (user_id); none is the administrators' (a volume or network then as its stack's; Docker's own networks public) |
| `security_rules` | an environment's (`env_id`) or a group's (`group_id`) security settings, `rules` as JSON (`block_privileged`, `block_bind_mounts`, `bind_allow`, `block_host_pid`/`_ipc`/`_network`/`_uts`, `block_devices`, `block_cap_add`, `block_security_opt`, `block_sysctls`, `block_consoles`), who changed them and when; an environment's own hold whole over its group's; none is everything allowed. Gone with its environment or group |
| `auth_log` | at, event (`login`, `login.failed`, `logout`, `login.sso`, `login.sso.failed`, `sso.link`, `teams.sync`, `login.totp.failed`, `password.failed`, `totp.enrol`, `totp.off`, `totp.reset`, `token.use`), method (`password`, `ldap`, `sso`, `totp`, `session`, `token`), ok, user, ip, user agent, detail; kept a year (`authlog::KEEP_DAYS`) |

## API

"Session" is the cookie or an API token (`Authorization: Bearer fdk_...`, no CSRF token or `Origin` needed); a
read-only token only reads.

| Route | Who | What |
|---|---|---|
| `GET /api/health` | anyone | `ok` |
| `GET /api/cluster` | session | `{connected}`, and with a cluster the manager's `/v1/cluster` as `cluster` |
| `GET /api/cluster/nodes`, `GET /api/cluster/events?after=&node=`, `GET /api/cluster/commands/{id}` | session | the manager's answer, as it is |
| `POST /api/cluster/nodes/{id}/commands` | admin | `{op, ...}` to the node; 202 and the command |
| `DELETE /api/cluster/nodes/{id}` | admin | the node out of the cluster |
| `PUT /api/cluster/nodes/{id}/labels` | admin | `{labels}`: the node's cluster labels, all of them |
| `POST /api/cluster/nodes/{id}/authorize` | admin | a node that came back fenced runs workloads again |
| `GET /api/cluster/recoveries?stack=` | session | the managers' recovery history, newest first |
| `POST /api/cluster/join-tokens` | admin | `{kind: worker or manager, ttl_s?}`: `{token}`, shown once |
| `GET /api/cluster/stacks`, `GET /api/cluster/stacks/{name}` | session | the manager's stacks with their workloads; one with its Compose file |
| `POST /api/cluster/stacks` | admin | `{name, compose}`: made or changed; `{stack, warnings}` |
| `POST /api/cluster/stacks/{name}/{start,stop,restart}`, `DELETE /api/cluster/stacks/{name}` | admin | the stack's workloads run, stop, restart or go |
| `GET /api/status` | anyone | name, version, configured, `sso: {providers: [{id, label, kind, preset}], ldap}` (the buttons; whether a directory takes the password form) |
| `GET /api/auth/sso/{id}/login?next=` | anyone | off to that provider |
| `GET /api/auth/sso/{id}/callback` | anyone | back from it (only that provider's sign-ins): signed in, or `/login?sso_error=` |
| `GET /api/auth/oidc/login?next=`, `GET /api/auth/oidc/callback` | anyone | the same for the first provider, and the callback the provider kept from before the list is sent back to |
| `GET /api/settings/sso` | admin | `{providers, presets}`, no secrets (`has_secret`, `ldap.has_bind_password`) |
| `PUT/DELETE /api/settings/sso/{id}` | admin | a provider made or changed (an empty secret keeps the one there is; a new LDAP address asks for the bind password again), or removed |
| `POST /api/settings/sso/test` | admin | `{issuer}`: does it answer as an OpenID provider |
| `POST /api/settings/sso/{id}/ldap-test` | admin | `{provider, name?, password?}`: connected, bound; the person's DN, groups, role, teams, `password_ok` |
| `GET/PUT /api/settings/two-factor` | admin | `{required}`; `local_without`: who signs in with a password and has none yet |
| `GET/PUT /api/settings/appearance` | admin | the FinUI preset (`{preset}`, a FinUI create code, `""` for FinUI as it ships) |
| `POST /api/setup` | anyone with the setup token, once | `{token, name, password}`: first admin + local environment, signs in; 403 on `token` without the one `freightdock setup-token` prints (in the log too; or `FREIGHTDOCK_SETUP_TOKEN`); removes `<data>/setup-token` |
| `POST /api/login` | anyone | a local account, then each directory; signs in, or `{second_factor: totp/enrol, ticket}`; 429 after 5 misses in 15 min by address or name |
| `POST /api/login/totp` | anyone, with a ticket | `{ticket, code}` (a TOTP or recovery code): signs in; enrolling, also `recovery_codes`, once |
| `POST /api/login/totp/enrol` | anyone, with a ticket | `{ticket}` answers `{secret, uri}` when two-factor is required and the account has none |
| `POST /api/logout` | session | ends this session; `{redirect}` to the provider's end-session for SSO users |
| `GET /api/me` | session | the user (with `avatar`, the picture's address or null; `signin` `{kind: local/oidc/github/ldap, provider}`; and `sso`, signed in through a provider) and the CSRF token |
| `GET /api/users/{id}/avatar` | session | one's own picture (an administrator: anybody's), fetched from the provider's address and kept a day; PNG/JPEG/GIF/WebP only |
| `GET /api/tokens` | session | your API tokens (never the tokens): name, hint, read_only, created/expires/last used |
| `POST /api/tokens` | session, not a token | `{name, expires_days?, read_only?}` answers `{id, name, token}`, the token once |
| `DELETE /api/tokens/{id}` | session, not a token | revokes one of yours |
| `GET /api/audit` | admin | the activity log, newest first; `?action=&user=&since=&until=&before=&limit=`, `&format=csv` |
| `GET /api/auth-log` | admin | the auth log, newest first; `?event=&method=&ok=&user=&ip=&since=&until=&before=&limit=`, `&format=csv` |
| `GET/POST /api/users`, `PUT/DELETE /api/users/{id}`, `POST /api/users/{id}/password` | admin | `{id, name, role, disabled, kind: local/sso/ldap, email, teams, totp, created_at, last_login_at}`; `{name, password, role}`; `{name, role, disabled}` (never the last administrator, never oneself); a new password (local only; ends their sessions and revokes their API tokens) |
| `PUT /api/me/password` | session, not a token | `{current, password}`; ends the other sessions and revokes one's API tokens; the current one is held to the sign-in limit |
| `POST /api/me/sso-link?provider=` | session, not a token | `{redirect}`: the provider, back to Account with the account linked |
| `GET /api/me/totp` | session | `{enabled, required, recovery_left, password_signin}` |
| `POST /api/me/totp`, `POST /api/me/totp/confirm` | session, not a token | a new seed `{secret, uri}`; `{code}` turns it on and answers the `recovery_codes`, once |
| `POST /api/me/totp/off`, `POST /api/me/totp/recovery` | session, not a token | `{code}`: off (not while required); ten new recovery codes |
| `DELETE /api/users/{id}/totp` | admin | their second factor reset |
| `GET /api/teams` | session | an administrator: every team with `members`; anybody else: the teams they are in |
| `POST /api/teams`, `PUT/DELETE /api/teams/{id}` | admin | `{name, members: [user ids]}` (members given is the whole list) |
| `GET/PUT /api/environments/{id}/access`, `/api/environment-groups/{id}/access` | admin | `{users: [{id, role}], teams: [{id, role}]}`, the whole list |
| `GET/PUT /api/environments/{id}/security` | admin | GET: `{rules, own, source: environment/group/default, group: {id, name, rules}, presets: {open, locked_down}}`; PUT `{rules}` (`null`: none of its own, the group's hold); 400 on `bind_allow` for a path that is not absolute or has `..` |
| `GET/PUT /api/environment-groups/{id}/security` | admin | `{rules, presets}`; PUT `{rules}` (`null`: none) |
| `GET /api/environments`, `GET /api/environments/{id}` | session | every environment (one) the person has a role on, with `role` (`admin` for an administrator) and `actions` (what it allows), health, `group: {id, name}`, `tags`, `snapshot` (last known numbers, `at`); `agent: {enrolled, join_expires_at, connected_since, peer, last_peer, version, outdated}`, `tls: {has_cert}` |
| `POST /api/environments` | admin | `{name, kind: "agent"}` answers `join: {token, expires_at, address, image, command}` once; `{name, kind: "tls", url, tls_ca, tls_cert, tls_key}`; either with `group_id`, `tags` |
| `PUT /api/environments/{id}` | admin | `{name, group_id, tags}` (`group_id: null` is none, `tags` the whole list by name, made as needed; left out keeps); a TLS host also `url`, `tls_ca`, `tls_cert`, `tls_key` (empty keeps) |
| `GET /api/environment-groups`, `POST`, `PUT/DELETE /api/environment-groups/{id}` | session to read, admin to change | `{id, name, description, environments: [ids]}`; `environments` given is the whole membership; removing leaves its environments in none |
| `GET /api/tags`, `POST`, `PUT/DELETE /api/tags/{id}` | session to read, admin to change | `{id, name, environments: [ids]}`; renamed or removed on every environment that has it |
| `DELETE /api/environments/{id}` | admin | forgets it, cuts an agent's tunnel; `local` stays (409) |
| `POST /api/environments/{id}/join` | admin | a new join token for an agent; revokes its certificate and cuts its tunnel |
| `GET /api/registries`, `GET /api/registries/{id}` | session | every registry (one), `has_secret` and never the secret, `image_prefix` |
| `POST /api/registries`, `PUT /api/registries/{id}` | admin | `{name, kind, url, username, password, region}`, tested first (400 with the field and the registry's reason); an empty password keeps |
| `DELETE /api/registries/{id}` | admin | forgets it and its credentials |
| `GET /api/registries/{id}/repositories?namespace=` | session | `{repositories, listed, reason, image_prefix}`: the catalog, or Docker Hub's account (`namespace`, else the user's) |
| `GET /api/registries/{id}/tags?repository=` | session | `{repository, image, tags}`; pull `image:tag` |
| `GET /api/changelog` | session | the patch notes and the version running |
| `POST /api/webhooks/{token}?pull=` | anyone with the address | redeploys its stack or recreates its container; 204, 404 (logged), 429 |
| `GET /api/templates?refresh` | session | every template list (or why it could not be read) and the custom templates |
| `GET/PUT /api/settings/templates` | admin | the list addresses `{urls}` |
| `POST /api/templates/custom`, `GET/PUT/DELETE /api/templates/custom/{id}` | admin (GET: session) | custom templates |
| `POST /mcp` | API token only | MCP (Streamable HTTP): `initialize` (answers `Mcp-Session-Id`), `ping`, `tools/list`, `tools/call`; plain JSON, a notification 202 ("MCP") |
| `DELETE /mcp` | API token only | ends the session `Mcp-Session-Id` names; `GET /mcp` is 405 (no stream). With the server off, all three answer 403 |
| `GET/PUT /api/settings/mcp` | admin | `{enabled, path, ca, active, tokens: [{id, name, hint, read_only, created_at, expires_at, last_used_at, last_used_ip, owner: {id, name, disabled}, active}]}`; `{enabled}` |
| `DELETE /api/settings/mcp/tokens/{id}` | admin, not a token | revokes anybody's token |
| `GET /api/mcp/status` | session | `{enabled, path, ca}`, for Account |
| `GET /api/tls/ca.pem` | anyone | the CA that signed Freightdock's own certificate (`freightdock-ca.pem`); 404 with a certificate of one's own or plain HTTP |
| `GET/POST /api/notifications`, `PUT/DELETE /api/notifications/{id}` | admin | channels `{name, kind, enabled, events, config}` (the secret never sent back: `has_secret`); the list also names the events |
| `POST /api/notifications/{id}/test` | admin | a test message now; 502 with the reason when it does not go |
| `GET/PUT /api/settings/updates` | admin | `{enabled, check_hours (1..168), auto_update}` |
| `POST /api/updates/check` | admin | a pass now (a registry answer younger than a minute is used again); `{apply: true}` updates what opted in; answers `{environments, asked, outdated, updated, failed}` |
| `GET/PUT /api/settings/backups` | admin | the backup settings (passphrase and secret key never sent back: `has_passphrase`, `s3.has_secret`) |
| `GET /api/backups`, `POST /api/backups/run` | admin | the archives at the destination, newest first, and the last run; a backup now |
| `GET /api/backups/{name}/download` | admin | an archive as kept, sealed |
| `POST /api/backups/upload`, `POST /api/backups/restore` | admin | an archive from elsewhere (raw body, 1 GB at most); `{name or "uploaded", passphrase}` checks and stages it, answers 202, and Freightdock starts itself again |
| `GET/PUT /api/settings/history` | admin | resource history: `{enabled, every_s (10..3600), keep_days (1..365)}` |
| `GET/PUT/DELETE /api/portainer` | admin | the Portainer connection `{url, token, ca, skip_verify}` (tried first; the token never sent back; empty keeps); forget it |
| `POST /api/portainer/scan` | admin | `{mapping}`: Portainer read (GET only) and matched to Freightdock; `environments`, `freightdock`, `stacks` (each with `importable`, `checked`, `reason`, `notes`), `groups`, `tags`, `registries`, `templates`, `people` (`{users, teams}` counts), `teams` (each `{id, name, freightdock_id, outcome: new/exists/imported, importable, reason, members: [{name, freightdock_user_id, already}]}`), `errors`; changes nothing |
| `POST /api/portainer/import` | admin | `{mapping, stacks: [{id, token}], groups, tags, registries, templates, people, teams: [Portainer's team ids], deploy}`: `results`, each `{kind, id, name, outcome, reason, freightdock_id}` |

The cluster env: `FREIGHTDOCK_CLUSTER_API` (a manager's `http://127.0.0.1:7451`, or `https://<manager>:7452`, several
comma separated), `FREIGHTDOCK_CLUSTER_TOKEN_FILE` (the admin token or an API token), `FREIGHTDOCK_CLUSTER_CA_FILE` (the
cluster CA, needed for `https://`) ("Cluster" 8).

The agent endpoint, its own listener (`FREIGHTDOCK_AGENT_BIND`, 7444, TLS from Freightdock's CA): `POST /agent/v1/enrol`
`{token, csr, hostname}` answers `{cert, ca, environment}` (logged as `agent.enrol`); `GET /agent/v1/tunnel` is the
WebSocket, for a client certificate an agent environment has.

Under `/api/envs/{env}` (each route an action of "Access" 2, held to the person's role there; 404 without one; changes
logged; 503 while an agent is not connected). Lists, events, all-stats and webhooks leave out the containers, stacks,
volumes, networks and images a Standard or Read-only user cannot see ("Access" 18), and the containers using a volume
or on a network are only those they see; containers, volumes, networks and images carry `freightdock_access`, stacks
`access`. Below Environment
administrator, a create or edit of a container, a volume, a deploy of a stack (and a webhook's, as its maker), a Swarm
service or stack is held to the host's security settings, 403 with the setting on its `field` ("Access" 12 to 16),
and consoles to their switch:

| Route | What |
|---|---|
| `GET info`, `GET df`, `POST prune` | Docker's info and version; disk by kind; system prune `{all, volumes}` |
| `GET summary` | the host in numbers within 10 s (`containers {total, running, paused, stopped, unhealthy}`, `images`, `volumes`, `stacks`, `nodes`, `swarm`, `cpus`, `memory`, `docker_version`, `at`), kept as its snapshot |
| `GET stats` | one sample of every running container |
| `GET events/ws` | events, `?type=&since=` |
| `GET/POST containers`, `POST containers/prune` | list (`?all=false` for running); create `{name, config, pull, start, replace}` |
| `GET/DELETE containers/{id}` | inspect (with size); remove `?force&volumes` |
| `PUT containers/{id}/access`, `PUT stacks/{id}/access`, `PUT volumes/{name}/access`, `PUT networks/{id}/access`, `PUT images/{id}/access` | `{level: public/team/private/admins, team_id, user_id}`; a create (`POST containers`, `stacks`, `stacks/git`, `volumes`, `networks`) takes the same as `access`; a pull or build makes the person the image's owner |
| `GET containers/{id}/config`, `/top` | the create body that would make it again; processes |
| `POST containers/{id}/{start,stop,restart,kill,pause,unpause}`, `/rename`, `/recreate` | |
| `GET updates` | the verdicts by container and image id (`status, detail, reference, remote, auto, checked_at`), `opted` (names; a Standard or Read-only user's only those they see) |
| `POST containers/{id}/update` | pull its tag and make it again, put back if it does not stay up 10 s; `{id, updated}` |
| `PUT containers/{id}/auto-update` | `{enabled}`: the switch on its page, by name (the `freightdock.auto-update=true` label is the other way) |
| `GET containers/{id}/history?hours=` | the samples of a container (by name, so a removed one's too), as at most 360 chart points: cpu, mem, and network and disk as bytes a second |
| `GET containers/{id}/logs`, `/logs/ws`, `/stats/ws`, `/exec/ws`, `/attach/ws` | download; follow `?tail&timestamps&since`; stats; console `?cmd&user&cols&rows`; the main process `?cols&rows` ("Streams" 3) |
| `GET containers/{id}/stats` | one sample, as the stats WebSocket sends them |
| `POST containers/{id}/exec` (Freightdock's own container: an Administrator's alone, "Access" 20) | run a command to its end, no terminal: `{cmd: [words], user, workdir, env, timeout_s (30, at most 600), max_bytes}` answers `{exit_code, stdout, stderr, timed_out, truncated}` ("Streams" 4) |
| `GET images`, `POST images/{prune,pull,push,build,load}` | list with container counts; the four long ones stream NDJSON; pull and push sign in to a saved registry |
| `GET/DELETE images/{id}`, `POST images/{id}/tag`, `GET images/{id}/export` | export: environment admin ("Access" 19) |
| `GET/POST volumes`, `POST volumes/prune`, `GET/DELETE volumes/{name}` | |
| `GET volumes/{name}/files?path=`, `DELETE ...files?path=` | environment admin: a folder's entries (`name, kind, size, modified, mode`, at most 5000); delete a file or a folder |
| `GET volumes/{name}/files/download?path=` | environment admin: a file, or a folder as a tar, streamed (logged `volume.download`) |
| `POST volumes/{name}/files/upload?path=&name=`, `POST .../mkdir {path}` | environment admin: the body is the file (256 MB at most); a folder and those above it |
| `GET/POST networks`, `POST networks/prune`, `GET/DELETE networks/{id}`, `POST networks/{id}/{connect,disconnect}` | |
| `GET webhooks`, `POST/DELETE stacks/{id}/webhook`, `POST/DELETE containers/{id}/webhook` | every webhook (never its token); make one (its address, shown once) or remove it |
| `POST templates/deploy` | `{list, index}` or `{custom}`, `name`, `env`: a container (`{container}`) or a stack not yet deployed (`{stack}`) |
| `GET stacks`, `POST stacks` | Freightdock's stacks with their state, and every external project; make one `{name, compose, env}` (checked by Compose, not deployed) |
| `GET/PUT stacks/{id}` | the stack, its files and containers (the files even when Docker does not answer, with `docker_error`); new files `{compose, env}` (kept only when Compose takes them) |
| `GET stacks/{id}/environment` | every service's variables as the next deploy gives them (`compose config --format json`), `{services: [{service, environment: [[name, value]]}]}` |
| `POST stacks/{id}/{deploy,stop,start,pull,remove}` | NDJSON: Compose's lines, then done or error; deploy `{pull}`, remove `{volumes}` |
| `POST stacks/git`, `PUT stacks/{id}/git` | a Git stack `{name, url, reference, path, user, token, poll_minutes, env}` (asked, cloned and checked first); its settings (an empty token keeps the one there is) |
| `GET stacks/external/{name}`, `POST stacks/adopt` | a project started elsewhere with its files when readable (an administrator, the local Docker, "Stacks" 16); adopt `{name, compose?, env?}` |
| `GET swarm` | `role` (none, worker, manager), the node, and for a manager the swarm's id and spec |
| `GET swarm/nodes`, `GET/PUT/DELETE swarm/nodes/{id}` | list; one with its tasks; `{availability, role, labels}`; remove `?force` |
| `GET/POST swarm/services`, `GET/PUT/DELETE swarm/services/{id}` | list with task counts; create from Docker's spec; one with its tasks; the whole new spec |
| `POST swarm/services/{id}/{scale,rollback}`, `GET swarm/services/{id}/logs/ws`, `GET swarm/tasks` | `{replicas}`; to the spec before; every task's logs `?tail&timestamps`; `?service&node&state` |
| `GET/POST swarm/secrets`, `GET/DELETE swarm/secrets/{id}` (and `configs`) | with the services using each; `{name, data, labels}`; a secret's value never comes back, a config's `text` does |
| `GET/POST swarm/stacks`, `GET/DELETE swarm/stacks/{name}` | stacks by label and kept file; `{name, compose}` deployed (again) with `docker stack deploy`, refused if it names a file the CLI would read on Freightdock's side ("Swarm" 8); `docker stack rm` |

Services, tasks, a service's logs and Swarm stacks are the administrators' for Standard and Read-only users: their lists
answer `[]` and one service or stack 404 ("Swarm" 7).

A Swarm route on a host that is not a swarm manager answers 409 with Docker's reason.

## MCP tools

`list_environments`, `list_containers`, `inspect_container`, `container_logs`, `start_container`, `stop_container`,
`restart_container`, `kill_container`, `remove_container`, `create_container`, `exec_command`, `container_stats`,
`list_images`, `pull_image`, `remove_image`, `list_volumes`, `list_networks`, `disk_usage`, `activity_log`. Each takes
`env` (id or name, local when left out) and is one or two requests to the routes above. Connect Claude Code with
`claude mcp add --transport http freightdock https://<host>/mcp --header "Authorization: Bearer fdk_..."`; with Freightdock's
own certificate, start Claude Code with `NODE_EXTRA_CA_CERTS=freightdock-ca.pem` (the CA from Settings or Account). An
administrator turns the server off and on, and sees and revokes every token, in Settings, AI agents (MCP).

## Testing

`cargo test`: unit tests beside the code; API tests in-process (`api::tests::{app, call, signed_in}`;
`stacks::tests::app_with_compose` gives the app a stand-in Compose, a shell script that writes down its arguments;
`access::api_tests::World` an app on a fake Docker with one signed-in user per role, for every role against every
area, the users, teams and MCP tests; `access::security::api_tests` the security settings per role, through containers,
volumes, templates, stacks, webhooks, Swarm, consoles and MCP, the stand-in Compose answering `config --format json`
from a `config.json` beside it; `access::routes` reads every `.route(` in the source and fails for one without a
rule);
`tests/log_time.rs` starts the real binary in a zone and reads its first log line. `web::tests` hold the two catalogues to
each other. `qa/js-check.sh` parses every module, runs `qa/i18n-check.mjs` (every key a module asks for has English,
every English text is asked for, no words left outside `t()`, the languages agree), and makes FinUI's preset overlay again from the copied `preset.js` to see `web/assets/finui/p` is what FinUI makes.

`qa/run.sh` is the end-to-end suite: a throwaway `docker:dind` (privileged, on a network of its own, its port published
on 127.0.0.1 only), a debug Freightdock on a free port (its agent endpoint on another) with `FREIGHTDOCK_TLS=off`, a temporary data folder and
`FREIGHTDOCK_DOCKER_HOST` at the dind, then `crates/server/tests/qa.rs` with `--ignored`. It goes through the API as the UI
does (cookie, CSRF token, `Origin`): setup (once only), the CSRF and Origin refusals, pull (and a failed one), create and
start, logs (stdout and stderr), resource history sampled and answered as chart points, a console over the WebSocket (and one from another site refused), attach to a TTY
shell (typed into, left running) and to one without a TTY (watching only), attach to a stopped one refused, the volume browser (a folder made, a file uploaded,
listed, downloaded, a folder as a tar, a path out of the volume refused, deleted, no helper left), connect to a network
with an alias and disconnect (it keeps running), edit, edit rollback
(a container that cannot start, and one Docker will not make), stop (twice), remove, build and run (and a failed build),
Home's summary of the host and its kept snapshot, a group and tags placed and taken away,
a registry (`registry:2` with htpasswd, run in the dind through Freightdock on a port published at the same number on the
dind and here, so both reach it as `127.0.0.1:<port>`): a wrong password refused on save, saved, pushed to, browsed,
pulled from, a container and a stack made from it, and a pull refused once it is removed; a FinUI preset picked and in
`/assets/finui.css`; API tokens (made, a bearer sign-in, a token refused a token), MCP (initialize and its session,
tools/list, pull, create, list, logs, exec with its exit code and a timeout, stats, the lists, a read-only token
refused a stop and an exec, stop and remove, the log naming the token), image updates (a `registry:2` in the dind on
the same published port, an image built, pushed, run with the auto-update label, a newer one pushed, found and the
container updated to it); then stacks through the real Compose: deploy
with variables, a file Compose refuses, update and redeploy with pull, stop and start, remove, and a project started on
the command line listed as external, adopted, deployed from Freightdock and removed; a v3 template list the suite serves
itself, a container template deployed from it with its variables (a preset kept), and a custom template deployed as a
stack; a container's webhook (recreated, a second call at once refused, a wrong address refused) and a stack's
(redeployed with its new variables, logged as by the webhook, gone with the stack); a webhook channel tested and told
of a container that died (a listener in the test), backups to a folder (retention) and to S3 (a MinIO,
`bitnamilegacy/minio`, beside the dind; `QA_S3_IMAGE` picks another), a restore from S3 (Freightdock starts itself
again, what came after the backup is gone), a fake Portainer the suite serves (one Docker environment with the dind's
engine ID, one stack) whose project the suite starts as Portainer would (a versioned folder, `stack.env`, a relative
bind mount): connected (a wrong token refused), matched by the engine's ID, imported in place with its container
kept, not made twice, redeployed from Freightdock with its variables and its mount, and Portainer only ever asked GET;
roles (`crates/server/tests/qa/access.rs`): users made and signed in as, a read-only user who sees no host until given
a role, then sees a public container and its logs but not a private one and is refused a stop, a console and a
create; an operator who stops, starts and opens a console on everything and may not remove or create; a team given
Helpdesk on a group, reading everything and changing nothing; a standard user who does not see the administrator's
private container and sees and removes their own; the auth log and both CSV exports; sign-in
(`crates/server/tests/qa/signin.rs`): an OpenLDAP (`osixia/openldap`, `QA_LDAP_IMAGE` picks another) beside the dind
with the people of `qa/ldap/seed.ldif` and a certificate from a CA made for the run, refused without the CA, plain LDAP
refused unless allowed, tested over LDAPS and StartTLS (a person's groups, role, teams, password; `*` finding nobody),
saved with its bind password never sent back, its people signed in by directory password as their groups say and into
their teams, a wrong password and somebody in no group turned away, the local sign-in still working; TOTP set up from
Account with codes worked out in the test, then asked for at sign-in, a code never good twice, two-factor required and
set up at sign-in with recovery codes (one good once), and an administrator's reset; then, last, Swarm
(`crates/server/tests/qa/swarm.rs`, "QA" 4): the routes refusing a lone Docker, the dind made a swarm of one (at its
API, `FREIGHTDOCK_QA_DOCKER`), a service created, its logs, scaled, updated and rolled back, a secret (its value never
back) and a config, the service removed, a stack deployed, deployed again scaled and removed, the node drained,
labelled and made active, the swarm left; and the activity log. Each step prints `ok`; the first that fails stops the run with what it expected and Freightdock's
log. The dind, its network and the data folder go on exit (`QA_KEEP=1` keeps them); `QA_DIND_IMAGE` picks another
Docker. CI runs it as the `qa` job.

`environments::tests::req` makes a signed-in change for the environment tests. `agent::e2e` runs a real `freightdock-agent` against the agent endpoint
on loopback, through to a fake Docker on a unix socket, and a TLS host against a fake `dockerd --tlsverify`.

## Running an agent

```sh
git clone https://github.com/freightdock/Agent ../freightdock-agent  # the agent's repository
docker build -t freightdock-agent ../freightdock-agent               # the agent's image
FREIGHTDOCK_SERVER=host:7444 FREIGHTDOCK_JOIN_TOKEN=FDJT1-... FREIGHTDOCK_AGENT_DATA=./agent cargo run --manifest-path ../freightdock-agent/Cargo.toml -p freightdock-agent
```

Agent env: `FREIGHTDOCK_SERVER` (Freightdock's agent endpoint, `host:port`, 7444 when no port), `FREIGHTDOCK_JOIN_TOKEN` (used
once), `FREIGHTDOCK_AGENT_DATA` (`/data`: key.pem, cert.pem, ca.pem, token.sha256), `FREIGHTDOCK_AGENT_DOCKER`
(`unix:///var/run/docker.sock`). Server env for agents: `FREIGHTDOCK_AGENT_BIND` (`0.0.0.0:7444`, or `off`),
`FREIGHTDOCK_AGENT_ADDRESS` (what agents dial, for the `docker run` line), `FREIGHTDOCK_AGENT_IMAGE` (`freightdock-agent:latest`).

## Restoring a backup

From the Backups page: Restore beside an archive at the destination, or drop an archive made elsewhere; the passphrase
is asked, the archive checked and staged, and Freightdock starts itself again (the same process) with it in place. What it
replaced is moved to `<data>/before-restore-<time>/`, not deleted. With Freightdock stopped:

```sh
FREIGHTDOCK_DATA_DIR=./data FREIGHTDOCK_BACKUP_PASSPHRASE='...' freightdock restore freightdock-20261109-211409.backup
docker run --rm -v freightdock:/data -v "$PWD":/in -e FREIGHTDOCK_BACKUP_PASSPHRASE='...' freightdock restore /in/<archive>
```

then start it: the staged restore is put in place before the database opens.

## Known gaps

- Swarm: Freightdock does not make, join or leave a swarm (`docker swarm init`, `join`, join tokens), and a service is
  created without registry credentials (they come with registries).

The full list of what is left, milestone by milestone, is [`TODO.md`](TODO.md).

- Access: an image loaded from an archive is not given an owner ("Access" 18). The security settings judge a stack
  when it is deployed, not when its file is saved, and a Swarm stack only where Compose is there to resolve its file.
- Sign-in: an account is linked to one provider or directory at a time ("SSO" 13); GitHub's orgs and teams are read
  to 500 each; Active Directory's nested groups are followed only with its preset's in-chain filter (memberOf is direct
  groups only), and that filter is checked by unit tests, not against a real Active Directory.
- Registries: a registry with a certificate from a private CA cannot be reached (there is no CA field, and no switch to
  skip the check); ECR and GitLab list no catalog through the registry API (their own APIs would), so they are browsed
  by name.
- The Agent repository is private on GitHub until the release, and Cargo fetches the server's agent crates from it,
  so CI and the image's build fetch them with the secret `GIT_DEPS_TOKEN` ("Agent" 12) until it is public. The agent image is built locally
  (`freightdock-agent:latest`) until the Agent repository's release publishes `ghcr.io/freightdock/agent`; the
  server's image names that image at its own tag. Neither repository's CI has run on GitHub yet.
- Import from Portainer: Swarm stacks and templates wait for Swarm (M6); a stack with more than one compose file, and
  Portainer's webhooks, do not come; Portainer's users are not made Freightdock users (a team takes only the members
  Freightdock has a user of that name for), and their roles do not come; Edge environments' stacks come only where
  Freightdock has an environment for the host.
- Relative bind mounts of a stack written in Freightdock are read from `<data>/stacks/<id>/` ("Stacks" 7).
- The end-to-end suite covers the API, not the UI in a browser (that was checked by hand with Playwright), and not yet
  stats, events, push, load, export, volumes or network creation options.
- The UI speaks English and Norwegian Bokmål; the server's own words (an error's reason, a refusal), the patch notes
  and FinUI create's names for a preset's choices stay English.
