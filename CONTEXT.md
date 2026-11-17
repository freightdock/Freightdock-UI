# Freightdock: context

*Last updated: 2026-11-12 (G8.3: Freightdock is the cluster's UI only. The single-host Docker pages, the agents,
stacks of its own, templates, webhooks, registries, image updates, notifications, resource history, backups, the MCP
server and the import from Portainer are gone from this repository; what of them comes back comes through the
managers, see the Manager repository's ROADMAP).*

The map. How the pieces work is in [`ARCHITECTURE.md`](ARCHITECTURE.md); the rules and their reasons in
[`DECISIONS.md`](DECISIONS.md). The cluster itself (managers, workers, the contracts between them) is in the
Freightdock-Manager, Freightdock-Worker and Freightdock-Contracts repositories; this one is the web UI over the
managers' admin API.

## Layout

```
Cargo.toml                 workspace: the server
crates/server/             the `freightdock` binary
  src/main.rs              config from the environment, the HTTPS server (hyper driven directly), the cluster and its
                           replicated database at start, `healthcheck`, `setup-token`, `--help` (anything else is refused)
  src/cluster.rs           the cluster this Freightdock is the UI of: the managers' admin API (several addresses, the
                           next tried on a connection error), asked for /api/cluster/...
  src/replica.rs           the database replicated through the managers: triggers to _replog, load, push, snapshots,
                           the stale flag that restarts a second writer ("Cluster" 9)
  src/api.rs               /api routes for sign-in, extractors Authed / Admin / Peer, the CSRF and Origin rules; puts
                           the access guard on every route
  src/access/routes.rs     RULES (every route: Public, Session, Admin, or Stack / Node: an administrator or a grant), the
                           guard; the test that every route has a rule
  src/grants.rs            operators of a stack or a node: may(), /api/cluster/grants ("Access" 24)
  src/access/api_tests.rs  tests: World, an Administrator and four plain users signed in, against the guard
  src/users.rs             /api/users, /api/teams, /api/me/password
  src/authlog.rs           the auth log, /api/auth-log; CSV (csv_file, field) for it and the activity log
  src/appearance.rs        the FinUI preset this instance wears: its code, the overlay, its settings
  src/auth.rs              Argon2id, sessions (hashed tokens), the sign-in limiter, the setup token
  src/signin/confirm.rs    the password again: recent() for what can take a node over, /api/me/confirm ("Sign-in" 18)
  src/audit.rs             the activity log
  src/avatars.rs           the copy kept of a provider's picture of a person
  src/ca.rs                Freightdock's own CA for its HTTPS certificate, made and kept in <data>/tls
  src/changelog.rs         CHANGELOG.md, compiled in and parsed for /api/changelog
  src/db.rs                SQLite pool, MIGRATIONS, Db::call (which pushes changes to the managers in a cluster)
  src/error.rs             ApiError: status, sentence, field
  src/outbound.rs          answers from hosts others name (a provider, a picture), read to a size limit
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
  src/secret.rs            Sealer: XChaCha20-Poly1305 under <data>/secret.key
  src/state.rs             AppState (db, limiter, sealer, oidc, tickets, the TLS CA, the setup token, the cluster,
                           the data folder, the MCP sessions)
  src/tls.rs               HTTPS: the certificate's names (host, interfaces, FREIGHTDOCK_TLS_NAMES), the renewal
                           rule (verdict), the acceptor; GET /api/tls/ca.pem
  src/tokens.rs            API tokens: make, parse, the bearer header, lookup; /api/tokens
  src/mcp.rs               /mcp, the MCP server: sessions, JSON-RPC, the cluster's tools as calls through the router
  src/portainer/           the import from Portainer: client.rs (GET only, the certificate as chosen), mod.rs (the
                           connection, the plan, the import into cluster stacks); fixtures/ recorded answers
  src/web.rs               the embedded UI, /assets/finui.css joined (the preset's overlay after tokens.css), the SPA fallback;
                           harden: CSP, X-Frame-Options, nosniff, Referrer-Policy on every answer, no-store on /api
  tests/log_time.rs        the real binary logs in the zone TZ names
web/
  index.html               the one page
  assets/app.css           the app's layout, in FinUI tokens
  assets/i18n/             en.json (the source) and nb.json: every word of the UI, FinUI's under `finui` ("UI" 17)
  assets/js/               main (boot: the language, then app), app (routes), i18n (t, tOr, tParts, lang, the language
                           choice), router, api, state, shell (the menus), dom (num, pct, bytes, ago, when through
                           Intl), view (loadInto, table, filterBox), confirm, editor (codeEditor: fd-code-editor in a
                           labelled field), cluster (whether the managers answer, for the menus), theme (before the
                           page draws)
  assets/js/pages/         cluster (the cluster: managers, nodes, events, commands, recoveries, join tokens; the
                           landing page), cluster-stacks (the cluster's Compose stacks and their workloads), cluster-docker (a node's Docker
                           through its worker: "Cluster" 25), integrations (shares,
                           Ceph (ceph: its setup and the dialog, "Cluster" 34), fencing provider, private registries, notifications, timings, backups, each manager's own
                           settings: "Cluster" 23, 24, 26, 27), users
                           (users and teams), activity, signins (the auth log), settings (appearance, then
                           signin-settings: providers and two-factor), account, changelog, setup, login, auth (the
                           cards both share), two-factor (the QR code and key, recovery codes), notfound
  assets/js/components/    Freightdock's own, FinUI's contract with an fd- prefix: code-editor (CodeMirror: YAML,
                           env, shell), qr-code (an SVG QR code from qr-encode.js, a QR encoder of our own: byte
                           mode, level M, versions 1 to 40)
  assets/vendor/codemirror/ CodeMirror 6 (view 6.43.15) bundled into one ES module, MIT; VERSION says how to rebuild it
  assets/finui/            FinUI, copied source (Washi, as it ships), with create/preset.js, create/presets.json and
                           the preset overlay p/<axis>/<option>.css that FinUI's site publishes
  assets/finmotion/        FinMotion, copied source
docs/                      the user's guides (README.md is the index): install, access, sso, appearance, security,
                           troubleshooting; reference/
qa/                        js-check.sh (node --check, i18n-check.mjs: the modules' words against the catalogues; in a
                           node container, or `--here` with the node on the PATH, as CI does); finui-presets.mjs makes
                           web/assets/finui/p again from the copied FinUI
Dockerfile, docker-compose.yml   the image: the binary alone, no Docker CLI and no socket
.github/workflows/ci.yml   gitleaks, tests, the image for amd64 and arm64, :edge from main, releases from tags;
                           .github/dependabot.yml (the pins, weekly)
.github/ISSUE_TEMPLATE/, PULL_REQUEST_TEMPLATE.md   issue forms (a bug, an idea) and the pull request checklist
.gitleaks.toml             gitleaks' rules: its own, Freightdock's tokens, private addresses, the owner's domain; allowlist
.githooks/pre-commit       the hook (`git config core.hooksPath .githooks`): files refused, a fast check, gitleaks
SECURITY.md, CONTRIBUTING.md, CODE_OF_CONDUCT.md   reporting a vulnerability; how work is done here; conduct
README.md                  what it is, how a cluster's UI is started, links into docs/
```

## Tables (migrations 1 to 22)

Migrations are never edited ("Database" 2), so the tables of what was set aside are still made; nothing reads them,
and the managers replicate them as they replicate the rest (empty). The ones in use:

| Table | What |
|---|---|
| `settings` | key/value; `sso` is `{providers: [...]}`, each sign-in provider (id, kind `oidc`/`github`/`ldap`, preset, groups, domains, `teams` rules, `ldap` settings; client secret and bind password sealed); `two_factor` is `{required}`; `appearance` is `{"preset": code}` |
| `users` | name (unique, case-blind), Argon2id hash (`!` for SSO and directory users), role `admin`/`user`, disabled; `oidc_issuer` + `oidc_sub` (unique; the issuer is `ldap:<provider id>` for a directory), `email`; `totp_secret` (sealed), `totp_pending` (sealed, while it is set up), `totp_last_step`; `picture` (the provider's, "Users" 1) |
| `avatars` | the copy kept of each user's picture: `content_type`, `data`, `fetched_at` |
| `recovery_codes` | user, the SHA-256 of a recovery code; deleted when used, gone with the user |
| `sessions` | SHA-256 of the token, user, CSRF token, created/expires/last seen, ip, user agent, when last confirmed ("Sign-in" 18) |
| `audit` | at, who, action, target, detail, ip, `via` (`token <name>` for a change made with an API token) |
| `auth_log` | every sign-in, failure and sign-out: at, event, method, ok, user, name tried, ip, user agent |
| `api_tokens` | user, name (unique per user, case-blind), SHA-256 of the token, `hint` (its last four), read_only, created/expires/last used, last used ip |
| `teams`, `team_members` | a team's name (unique, case-blind); who is in which (both sides cascade), and `synced_by`: the provider whose groups put them there (NULL: by hand, and a sign-in never takes them out) |
| `cluster_grants` | who besides an administrator may operate a stack or node: kind (stack, node), target, a user or a team ("Access" 24) |
| `node_names`, `manager_names`, `cluster_names` | the names nodes (by node id), managers (by raft id) and the cluster (by its id) are shown by in Freightdock only ("Cluster" 12, 14, 18) |
| `_replog` | in a cluster: each change since the last push, written by the `_rep_*` triggers ("Cluster" 9); never replicated itself |

## API

"Session" is the cookie or an API token (`Authorization: Bearer fdk_...`, no CSRF token or `Origin` needed); a
read-only token only reads.

| Route | Who | What |
|---|---|---|
| `GET /api/health` | anyone | `ok` |
| `GET /api/cluster` | session | `{connected}`, and with a cluster the manager's `/v1/cluster` as `cluster` |
| `GET /api/cluster/nodes`, `GET /api/cluster/events?after=&node=`, `GET /api/cluster/commands/{id}` | session | the manager's answer, as it is |
| `POST /api/cluster/nodes/{id}/commands` | admin | `{op, ...}` to the node; 202 and the command |
| `GET /api/cluster/nodes/{id}` | session | one node, with the containers it reports and its name here |
| `PUT /api/cluster/nodes/{id}/name` | admin | `{name}`: the name it is shown by, also set as its `name` label on the managers; empty takes both away ("Cluster" 12, 30) |
| `PUT /api/cluster/managers/{id}/name` | admin | `{name}`, by raft id: a manager's name in Freightdock only; empty takes it away ("Cluster" 14) |
| `PUT /api/cluster/name` | admin | `{name}`: the cluster's name in Freightdock only, given back as `name` by `GET /api/cluster`; empty takes it away ("Cluster" 18) |
| `DELETE /api/cluster/nodes/{id}` | admin | the node out of the cluster |
| `PUT /api/cluster/nodes/{id}/labels` | admin | `{labels}`: the node's cluster labels, all of them |
| `POST /api/cluster/nodes/{id}/authorize` | admin | a node that came back fenced runs workloads again |
| `GET /api/cluster/recoveries?stack=` | session | the managers' recovery history, newest first |
| `POST /api/cluster/join-tokens` | admin | `{kind: worker or manager, ttl_s?}`: `{token}`, shown once |
| `GET /api/cluster/stacks`, `GET /api/cluster/stacks/{name}` | session | the manager's stacks with their workloads; one with its Compose file |
| `POST /api/cluster/stacks` | admin | `{name, compose, nodes?, exposures?, description?, env?, selector?, mode?, names?}`: made or changed, on the workers chosen (`[]` any) with its exposures on the pool and its options (each left out: as it was); `{stack, warnings}` ("Cluster" 19, 22) |
| `POST /api/cluster/stack-check` | admin | the same body, nothing deployed: the services and the ports their file asks for, the warnings, where the workloads would go (`placements`) or why not (`unplaced`) ("Cluster" 19) |
| `POST /api/cluster/stacks/{name}/{start,stop,restart}`, `DELETE /api/cluster/stacks/{name}` | admin (start, stop, restart: or the stack's operators, "Access" 24) | the stack's workloads run, stop, restart or go |
| `GET /api/cluster/settings`, `PUT /api/cluster/settings` | admin | the cluster's settings, each with its source and default, and what the answering manager's environment sets; some changed (a value, or null for the managers' own; `proxmox: {url, fingerprint, token?}` or null), passed on whole, the keys on record ("Cluster" 23) |
| `GET /api/cluster/release` | session | the cluster's release (each binary's checksum and size, whether the answering manager has it), the running or last upgrade with its steps, every manager's and worker's version ("Cluster" 36) |
| `PUT /api/cluster/release/{worker,manager}?version=` | admin, recent | a binary of the release, the body as it is (up to 256 MB), passed to the managers, on record (`cluster.release.upload`) |
| `POST /api/cluster/release/fetch` | admin, recent | `{tag}`: a published release fetched and checked by a manager, on record |
| `POST /api/cluster/upgrade`, `POST /api/cluster/upgrade/stop` | admin (start: recent, "Sign-in" 18) | the upgrade to the release started (`{steps}`) or stopped, on record |
| `GET /api/cluster/nodes/{id}/shell?container=&cols=&rows=` | admin, recent ("Sign-in" 18) | a shell ("Cluster" 35): without an upgrade, whether it may be opened (204 or the manager's why not); as a WebSocket from this origin, spliced to the manager's, on record (`cluster.shell`) |
| `GET /api/cluster/nodes/{id}/docker?action=&target=&tail=&timestamps=`, `POST /api/cluster/nodes/{id}/docker` | admin or the node's operators (they change only `container.start`, `.stop`, `.restart`, "Access" 24) | a node's Docker through its worker: a look (containers, images, volumes, networks, info, a container's inspect or logs), or a change `{action, target, params}`, on record ("Cluster" 25) |
| `GET /api/cluster/registries`, `PUT`/`DELETE /api/cluster/registries/{host}`, `POST /api/cluster/registries/{host}/test` | admin | private registries' logins: listed without passwords, set `{username, password?}` (kept when left out), removed, tried from a manager ("Cluster" 26) |
| `GET /api/cluster/notifications`, `PUT`/`DELETE /api/cluster/notifications/{name}`, `POST /api/cluster/notifications/{name}/test` | admin | notification channels: listed with the events there are and without secrets, set `{kind, enabled, events, config, secret?}` whole (kept when left out, none when empty), removed, tested from a manager ("Cluster" 27) |
| `POST /api/cluster/backup` | admin | `{passphrase}`: a sealed backup of the cluster now, as a download, on record without the passphrase ("Cluster" 24) |
| `GET /api/cluster/backups`, `GET /api/cluster/backups/{name}` | admin | the answering manager's scheduled backups; one as a download ("Cluster" 24) |
| `POST /api/cluster/settings/proxmox/fingerprint`, `POST /api/cluster/settings/proxmox/test` | admin | `{url}`: the certificate the Proxmox API shows, read by a manager; `{url, fingerprint, token?}` or `{}`: the provider tried from a manager, `{version, guests}` (nothing changed) |
| `POST /api/cluster/settings/redfish/fingerprint`, `POST /api/cluster/settings/redfish/test` | admin | `{host}`: the certificate a BMC shows, read by a manager; `{username, password?}` or `{}`: the Redfish account tried on each redfish worker's BMC, `{systems}` (nothing changed) ("Cluster" 29) |
| `GET /api/cluster/floating` | session | the floating addresses and each one's holder ("Cluster" 15) |
| `PUT /api/cluster/stacks/{name}/exposures` | admin | `{exposures: [{service, container_port, protocol, address, port}]}`: what the stack exposes on the pool, all of it ("Cluster" 17) |
| `GET /api/cluster/traffic` | session | each node's latest flows, their ends named by the managers ("Cluster" 16) |
| `POST /api/cluster/floating`, `DELETE /api/cluster/floating/{address}` | admin | `{address}`, one or a range: added (`{added}`, how many were new); removed (an IPv4 address in the path only) |
| `GET /api/cluster/storage` | session | the cluster's shared storage, the volumes on each and who holds them ("Cluster" 21), and each node's health of it (`health`, "Cluster" 28) |
| `POST /api/cluster/storage`, `DELETE /api/cluster/storage/{name}` | admin | `{name, driver: nfs\|smb\|path\|cephfs, source, options}` (cephfs also `secret`, `fence_client`, `fence_secret`, `image`; smb `password`; never on record): added; removed (a storage name in the path only; refused while a stack uses it) |
| `PUT /api/cluster/storage/{name}/keys` | admin | a CephFS's keys anew ("Cluster" 34): an empty key keeps the one set, an empty `fence_client` takes eviction away |
| `PUT /api/cluster/storage/{name}/password` | admin | an SMB share's password anew ("Cluster" 37), empty for a guest share; never kept here or on record |
| `GET /api/cluster/ceph/setup` | session | the Ceph setup the managers are making or made last, each step with its node, or null ("Cluster" 34) |
| `POST /api/cluster/ceph/setup`, `POST /api/cluster/ceph/setup/retry`, `DELETE /api/cluster/ceph/setup` | admin | `{storage, fs, image, hosts: [{node_id, disks}]}`: Ceph started on them (the first bootstraps); the failed step again; the setup forgotten |
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
| `POST /api/setup` | anyone with the setup token, once | `{token, name, password}`: first admin, signs in; 403 on `token` without the one `freightdock setup-token` prints (in the log too; or `FREIGHTDOCK_SETUP_TOKEN`); removes `<data>/setup-token` |
| `POST /api/login` | anyone | a local account, then each directory; signs in, or `{second_factor: totp/enrol, ticket}`; 429 after 5 misses in 15 min by address or name |
| `POST /api/login/totp` | anyone, with a ticket | `{ticket, code}` (a TOTP or recovery code): signs in; enrolling, also `recovery_codes`, once |
| `POST /api/login/totp/enrol` | anyone, with a ticket | `{ticket}` answers `{secret, uri}` when two-factor is required and the account has none |
| `POST /api/logout` | session | ends this session; `{redirect}` to the provider's end-session for SSO users |
| `GET /api/me` | session | the user (with `avatar`, the picture's address or null; `signin` `{kind: local/oidc/github/ldap, provider}`; and `sso`, signed in through a provider), the CSRF token, and `confirmed_until` (when the session stops being recent, "Sign-in" 18; null for an API token); for anybody but an administrator `user.grants`, `[{kind, target}]` they may operate ("Access" 24) |
| `GET /api/cluster/grants?kind=&target=`, `POST /api/cluster/grants`, `DELETE /api/cluster/grants/{id}` | admin | the operators of stacks and nodes: `{kind, target, user_id | team_id}`, not an administrator, not Freightdock's own stack; on record ("Access" 24) |
| `POST /api/me/confirm` | session | `{password, code}`: the password (and code, with two-factor on) again makes the session recent (204); a wrong one 400 with `field`, the code wanted 400 `code: "totp"`, through a provider 409 `code: "sso"`; held to the sign-in limit, in the auth log |
| `GET /api/users/{id}/avatar` | session | one's own picture (an administrator: anybody's), fetched from the provider's address and kept a day; PNG/JPEG/GIF/WebP only |
| `GET /api/tokens` | session | your API tokens (never the tokens): name, hint, read_only, created/expires/last used |
| `POST /api/tokens` | session, not a token | `{name, expires_days?, read_only?}` answers `{id, name, token}`, the token once |
| `DELETE /api/tokens/{id}` | session, not a token | revokes one of yours |
| `POST /mcp`, `DELETE /mcp` | an API token (signs in itself) | the MCP server ("MCP"): one JSON-RPC message a POST, `Mcp-Session-Id` from `initialize`; DELETE ends the session; `GET /mcp` is 405 |
| `GET/PUT /api/settings/mcp`, `DELETE /api/settings/mcp/tokens/{id}` | admin | the MCP switch `{enabled}`, every API token with its owner; revoke anybody's (from a session) |
| `GET /api/mcp/status` | session | `{enabled, path, ca}`, for Account |
| `GET/PUT/DELETE /api/portainer` | admin | the Portainer connection (never its token): `{url, token?, ca, skip_verify}`, tried before it is saved |
| `POST /api/portainer/scan` | admin | Portainer's stacks, each with `target` (its stack name here) or `why` it cannot come; changes nothing |
| `POST /api/portainer/import` | admin | `{stacks: [id], start}`: each deployed as a cluster stack, stopped unless `start`; `{results}` per stack |
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
| `GET /api/changelog` | session | the patch notes and the version running |
| `GET /api/tls/ca.pem` | anyone | the CA that signed Freightdock's own certificate (`freightdock-ca.pem`); 404 with a certificate of one's own or plain HTTP |

The cluster env: `FREIGHTDOCK_CLUSTER_API` (a manager's `http://127.0.0.1:7451`, or `https://<manager>:7452`, several
comma separated), `FREIGHTDOCK_CLUSTER_TOKEN_FILE` (the admin token or an API token), `FREIGHTDOCK_CLUSTER_CA_FILE` (the
cluster CA, needed for `https://`) ("Cluster" 8); `FREIGHTDOCK_CLUSTER_TOKEN` and `FREIGHTDOCK_CLUSTER_CA` give the
values themselves, before the files, as the managers do when they run the UI ("Cluster" 11, `cluster::Given`).

## Testing

`cargo test`: unit tests beside the code; API tests in-process (`api::tests::{app, call, signed_in}`;
`access::api_tests::World` an app with an Administrator and four plain users signed in; `access::routes` reads every
`.route(` in the source and fails for one without a rule, or a rule without a route); `replica::tests` two
Freightdocks over one fake manager; `cluster::tests` a fake manager over HTTP and HTTPS. `tests/log_time.rs` starts the
real binary in a zone and reads its first log line. `web::tests` hold the two catalogues to each other.
`qa/js-check.sh` parses every module, runs `qa/i18n-check.mjs` (every key a module asks for has English, every English
text is asked for, no words left outside `t()`, the languages agree), runs the UI's own tests (`qa/*.test.mjs` with
`node --test`: the deployer's Compose edits and which workers can keep a stack's data), and makes FinUI's preset overlay again from the
copied `preset.js` to see `web/assets/finui/p` is what FinUI makes.

`qa/e2e.sh` is the end-to-end suite ("Cluster" 32): `qa/cluster.sh up` makes a throwaway one-node cluster (a manager
and a worker in a privileged `docker:dind` container, from `../dist`'s static binaries, the UI image `FD_IMAGE` beside
them, published on `127.0.0.1:17443`), `qa/e2e.mjs` drives the UI's API against it, and `qa/cluster.sh down` takes it
away (left up for a look when a step fails; `qa/cluster.sh logs` shows the three logs). It needs Docker and Node.
`qa/screenshots.sh` makes the screenshots the same way, with Chromium, into `docs/screenshots/` ("Cluster" 33).

## Known gaps

- The UI needs the managers: started without `FREIGHTDOCK_CLUSTER_API` it serves sign-in, users and settings and a
  cluster page that says it is not connected. The managers run it as a system workload later (G8.5).
- Roles are two: an Administrator changes the cluster, anybody else signed in reads it. Roles per stack or node come
  back with the features that need them.
- Back through the managers so far: a node's Docker pages ("Cluster" 25, without consoles yet), backups (24),
  registries (26) and notifications (27). The MCP server is back over the cluster ("MCP" 7), and the import from Portainer, of Compose stacks into cluster stacks ("Portainer" 11 to 13).
- Sign-in: an account is linked to one provider or directory at a time ("SSO" 13); GitHub's orgs and teams are read
  to 500 each; Active Directory's nested groups are followed only with its preset's in-chain filter (memberOf is direct
  groups only), and that filter is checked by unit tests, not against a real Active Directory.
- The UI speaks English and Norwegian Bokmål; the server's own words (an error's reason, a refusal), the patch notes
  and FinUI create's names for a preset's choices stay English.
