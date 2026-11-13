# Freightdock architecture

Freightdock is the web UI of a Freightdock cluster: one Rust binary with the UI built in, over the managers' admin API.
The cluster itself (managers, workers, Raft, scheduling, recovery) is the Freightdock-Manager and Freightdock-Worker
repositories. This document explains how the pieces here work and fit; the item-by-item rules are in
[`DECISIONS.md`](DECISIONS.md), cited by section and number ("Sign-in" 2).

**Contents.** §1 The process · §2 HTTP and TLS · §3 The database · §4 Sign-in and sessions · §5 (set aside) ·
§6 The web UI · §7 Shipping · §8 The plan · §9 to §12 (set aside) · §13 The cluster

## 1. The process

One binary, `freightdock`. `main.rs` reads the configuration from the environment (`Config::from`), makes the data
folder, and, with `FREIGHTDOCK_CLUSTER_API` set, makes the cluster client (`cluster::Cluster`, §13) and loads the
database from the managers before it is opened (`replica::load`, §3). Then it opens the database, makes or loads the
TLS certificate (§2) and serves. `freightdock healthcheck` checks that the port accepts, for the image's HEALTHCHECK;
`freightdock setup-token` prints the first run's token; `freightdock --version` prints the version.

The log goes to standard output through `tracing`, filtered by `RUST_LOG` (default `freightdock=info,warn`), each line
stamped in local time with its offset (chrono's clock): the zone `TZ` names, else `/etc/localtime`.

`AppState` (`state.rs`), shared as `App` (an `Arc`), holds the database, the sign-in limiter, whether cookies are
`Secure`, the sealer, the OpenID Connect state, the sign-in tickets of the code step, the TLS CA, the setup token, the
cluster and the data folder. Beside the web server, `main` runs the auth log's retention (`authlog::watch`) and, in a
cluster, a two-second ticker that pushes what is waiting in `_replog` and stops the process with code 75 when another
Freightdock has written since (the replica is stale, "Cluster" 9); all stop with the server.

Freightdock has no Docker of its own: it neither talks to a Docker socket nor runs agents. What runs where is the
managers' to decide and the workers' to do; the UI asks and tells the managers only.

## 2. HTTP and TLS

hyper is driven directly (`main::serve`), as in FinStats: `axum::serve` sets no timer, so a request head (or a TLS
handshake) could take for ever. Here both must arrive within 15 s, a request's body may stop coming for 15 s and no
longer (`main::IdleBody`: the handler gets an error, a 400, and the connection goes), at most 1024 connections are
served at once, and on SIGTERM the requests under way get 10 s to finish. Connections are upgraded in place for WebSockets.

HTTPS is the default. Without a certificate given, `tls::prepare` opens (or makes) a CA of Freightdock's own in
`<data>/tls/` (`ca.pem`, `ca.key` 0600, `ca::Ca`) and checks the
certificate there against `tls::verdict`: none, one the CA did not sign but rcgen's old self-signed default, one whose
names do not cover `tls::names` (`localhost`, `127.0.0.1`, `::1`, the kernel's host name, the bind address or, bound to
all, every interface's address from `getifaddrs`, and `FREIGHTDOCK_TLS_NAMES`), or one with less than 30 days left is
made again (825 days, the host name as its common name, server authentication only); any other certificate at that
path is left alone with a warning. A certificate named by `FREIGHTDOCK_TLS_CERT`/`FREIGHTDOCK_TLS_KEY` is only read, and
must exist. The CA's PEM goes on `state.tls_ca` for `GET /api/tls/ca.pem` (public). `FREIGHTDOCK_TLS=off` serves plain
HTTP for a proxy.

## 3. The database

SQLite in WAL mode, `freightdock.db` in the data folder, pooled with r2d2 (6 connections). Every transaction is IMMEDIATE.
All database work goes through `Db::call`, which runs a closure on a blocking thread. `MIGRATIONS` is a list of SQL steps
applied in order, recorded in `PRAGMA user_version`; a database newer than the binary is refused, not touched.

In a cluster the database is the managers' ("Cluster" 9, `replica.rs`). Before it is opened, `replica::load` takes
their snapshot (the database file, `secret.key`, and the CA of Freightdock's own certificate, `tls/ca.pem` and
`ca.key`, so a UI the managers moved keeps the CA a browser trusted, "Cluster" 11) and replays the changes after it; with none there, the local
database becomes the first. Triggers on every replicated table (all but `replica::LOCAL_TABLES`) write each row change,
key and new values as JSON, into `_replog`; a `Db::call` that wrote (`total_changes` moved) then sends what `_replog`
holds to the managers as change `head + 1` and returns only once they took it. They take only the next number: a
Freightdock that loses (another wrote) is stale, its write fails, and it stops to be started again and load. Every 200
changes, when a migration changes the schema, and when theirs lacks the CA there is here, a snapshot replaces the
changes. `main` makes the certificate before replication starts, so the first snapshot carries its CA. A change that could not be sent
stays in `_replog` and goes with the next write, or within 2 s.

## 4. Sign-in and sessions

The first run (`POST /api/setup`) makes the first administrator, and is
refused once any user exists. It wants the setup token (`AppState::setup_token`): `FREIGHTDOCK_SETUP_TOKEN`, or 128 random
bits kept in `<data>/setup-token` and printed to the log while nobody exists (the same across restarts, shown again by `freightdock setup-token`, removed once setup is done), compared in constant time (their SHA-256s, by `auth::same`). Passwords are Argon2id. A session is 32 random bytes in an HttpOnly, SameSite=Lax cookie
(`Secure` over HTTPS); the database keeps its SHA-256 only. A session ends after 7 days unused or 30 days in all, on
sign-out, or when its user is removed or disabled.

Every session has a CSRF token, handed out by `GET /api/me`. The `Authed` extractor (`api.rs`) refuses a request without
a good session, a changing request (not GET/HEAD/OPTIONS) without `X-CSRF-Token`, and a changing request or a WebSocket
upgrade whose `Origin` is not this server. `Admin` is `Authed` and an administrator.

Sign-in misses are counted in memory by address and by name (`auth::Limiter`): five within 15 minutes and that address,
or that name from anywhere, waits out the window (429 with `Retry-After`). A try is taken (`Limiter::attempt`) under
the same lock as the check that it may be made, before the password or code is checked, and counts as a miss unless it
is given back (`release`) because it was right; so tries sent at once are held to five as well. A wrong name takes as
long as a wrong password (a decoy hash is verified).

Every change goes to `audit`, the activity log; every sign-in, failed sign-in, sign-out, SSO refusal, account linked,
second factor set up, failed or reset, team changed by a provider's groups, and API token use goes to `auth_log`, the
auth log (`authlog.rs`), which `authlog::watch` keeps to a year (hourly). Both are filtered and exported as CSV
(`?format=csv`). A sign-in with a name longer than any name can be is refused before the limiter or the auth log sees
it, and a name tried is kept to 256 characters. No route changes anything without a session.

**Access** (`access/`). Two roles: an Administrator (`users.role = 'admin'`) changes the cluster and Freightdock's own
settings, users and teams; anybody else signed in reads the cluster. `access::routes` holds the rule of every route
(`RULES`: public, signed in, or administrator) and the guard, an axum route layer on the whole router (`api::router`),
which reads the matched path, signs the person in once (`Authed`, left on the request for the handler) and holds them
to the rule. A route without a line is refused, and a test reads every `.route(` in the source and fails for one
without a rule ("Access" 5). The roles per environment, ownership and security settings of the single-host product
went with it ("Access" 23).

**Users and teams** (`users.rs`). `/api/users` and `/api/teams` are an Administrator's (anybody sees the teams they
are in); `PUT /api/me/password` is anybody's own. The last administrator cannot be taken away and nobody takes
themselves away; disabling or a new password ends the person's sessions, and a new password revokes their API tokens
("Access" 22). Teams come from sign-in providers' groups too (`signin/teams.rs`).

**API tokens** (`tokens.rs`). For programs: `fdk_` and 32 random bytes in hex, made in the Account page and shown once;
`api_tokens` keeps the SHA-256 and the last four characters. `Authed` takes `Authorization: Bearer` before the cookie:
the token is looked up with its owner as they are now (role, disabled), the request then needs no CSRF token or
`Origin`, and a read-only token is refused anything but a GET. The `User` a
token signs in carries it (`User.token`), so `audit::record` writes `via: token <name>` without any route knowing.
Tokens are made and revoked only with a session.

**Sign-in providers** (`signin/`). The providers are a list (`settings` key `sso`, `signin/providers.rs`): each has an
id, a kind (`oidc`, `github`, `ldap`), the preset it was made from, and who may come in (administrator and user groups,
e-mail domains), whether a first sign-in lands in the local account of the same name (`link_by_name`), and team rules;
client secrets and bind passwords sealed by `secret.rs`. Migration 18 turned the one OpenID Connect setting kept before
(`oidc`) into the list's first entry, id `oidc`, still sent back to `/api/auth/oidc/callback`.

*OAuth* (`oidc.rs`, `signin/github.rs`). `GET /api/auth/sso/{id}/login` (and `/api/auth/oidc/login`, the first
provider) seals a state with its provider, PKCE verifier, nonce and where the person was going into a cookie of the
browser's own (`freightdock_sso`, `Path=/api/auth`, HttpOnly, ten minutes; nothing is kept on the server), and sends them to
the provider: the authorization endpoint from the issuer's discovery document (cached ten minutes per issuer, and refused unless
every address in it is https or http to this machine), or GitHub's
`/login/oauth/authorize`. The callback (`/api/auth/sso/{id}/callback`, or the old one) opens that cookie, goes on only
when the state in it is the one in the query (so only the browser that started a sign-in finishes it), spends the
cookie whatever happens, refuses a
return on any path but that provider's own callback, swaps the code (client secret by Basic auth and in the form) and
works out who it is: for OpenID Connect the ID token checked, with userinfo when the name or groups are missing; for
GitHub `/user`, `/user/emails`, `/user/orgs` and `/user/teams` (groups `org` and `org/team`). Then `signin::admit`, which
every provider shares: the e-mail domain when the provider names some, the role from the groups (`providers::role_for`;
set again only when an administrator group is named), the user found by issuer and subject (`users.oidc_issuer`,
`oidc_sub`; for a directory `ldap:<id>`) or linked by name or made (`oidc::sso_user`), and the teams set from the groups
(`signin/teams.rs`, memberships it made marked `team_members.synced_by`, written to the auth log as `teams.sync`). Then
a session as for a password sign-in, and on to the page. A signed-in local user links from Account: `POST
/api/me/sso-link?provider=` starts the same flow with the user's id kept in the pending cookie, and the callback links
that account (only in the browser still signed in as it) instead of signing in. A failure goes back to
`/login?sso_error=...` (`/account?sso_error=...` for a link), said in words, and to the auth log.

*LDAP* (`signin/ldap.rs`, the `ldap3` crate over rustls). `POST /api/login` tries the local accounts, then each
directory that is on (`ldap::sign_in`): connect (LDAPS, or StartTLS; plain only when allowed), bind as the service
account, find the one entry of `user_filter` with the name escaped (RFC 4515), read its groups (`memberOf`, or a group
search with `{dn}` and `{username}` escaped), bind as the entry with the password, then `signin::admit` as above. A
directory that cannot be reached is logged and passed over. `POST /api/settings/sso/{id}/ldap-test` runs the same steps
from Settings on the form's values, a person's lookup and password check included.

*Two-factor* (`signin/totp.rs`). A right password (local or a directory's) for an account with a TOTP seed, or when the
`two_factor` setting requires one, answers `{second_factor: "totp" | "enrol", ticket}` instead of a session; tickets are
in memory (`state.tickets`, five minutes, five wrong codes, five waiting per account with the oldest giving way). `POST /api/login/totp` takes the ticket and a code (or a
recovery code) and makes the session (`api::signed_in`, which also forgives the limiter's misses and writes the auth
log); with `enrol`, `POST /api/login/totp/enrol` gives the seed first, and the code step confirms it and answers the
recovery codes. Codes: RFC 6238, HMAC-SHA1, six digits, a step either side, never a step at or before
`users.totp_last_step`. Seeds are sealed in `users.totp_secret` (`totp_pending` while being set up from Account);
recovery codes are SHA-256 hashes in `recovery_codes`, deleted as they are used. The QR code is drawn in the browser
(`web/assets/js/components/qr-code/`, an encoder of our own).

## 5. Docker endpoints (set aside)

The single-host product's Docker endpoints (the local socket, TLS hosts, agents over a yamux tunnel) are gone. A
node's Docker is the worker's; a node's pages come back through it, asked of the managers ("Cluster" 10).

## 6. The web UI

`web/` is embedded with rust-embed (`web.rs`); a debug build reads it from disk. `/assets/finui.css` is FinUI's
foundation and component CSS joined in `registry.json`'s order, with an ETag, and the preset an administrator picked
right after `tokens.css`; FinUI's fonts, which that CSS names
relatively, are served at `/assets/fonts/`. Any other address that is not a file or `/api` is `index.html`.
`web::harden`, the outermost layer in `main.rs`, puts on every answer a Content-Security-Policy (`web::CSP`: scripts
only from here, `frame-ancestors 'none'`), `X-Frame-Options: DENY`, `nosniff` and `Referrer-Policy: same-origin`, and
`Cache-Control: no-store` on an API answer that names no caching of its own ("UI" 16).

**Presets** (`appearance.rs`). A FinUI preset is a code, one base-36 digit per axis of FinUI's `create/presets.json`
(base colour, accent, charts, radius, font and the rest); FinUI create makes them, and Freightdock
only takes the code ("UI" 15). Its overlay is FinUI's own: a small file of tokens per option, `p/<axis>/<option>.css`, as
FinUI's site publishes them for its installer, joined in axis order so a later axis sets a token last. The code is kept
in `settings` (`appearance`) for the whole instance; `web.rs` asks for it on every `/assets/finui.css` (a release build
keeps the last stylesheet made, so that is one small read), and the ETag changes with it. Since the overlay is inside
the one blocking stylesheet, the page is drawn in the preset from the first paint, the sign-in page too. Saving in
Settings loads the new stylesheet beside the old and drops the old once it is in.

The UI is vanilla ES modules on FinUI and FinMotion. `main.js` puts the language in use first (`i18n.js` fetches
`/assets/i18n/<tag>.json`, which `theme.js` began fetching in `<head>`, and hands FinUI its part), then imports `app.js`,
which starts FinMotion (`motion()`) and the
moving icons (`animateWithin`), asks `/api/status` and `/api/me`, and starts the router. `router.js` maps addresses to
pages; each page gets an AbortSignal, `cleanup()` and `every()` that end when it is left. The guard sends everybody to
`/setup` until there is an administrator, and to `/login` until they sign in. `shell.js` builds FinUI's desktop menu
(Grouped) and mobile menu (Tab bar) once per person and keeps them across pages.

A page loads with `view.loadInto` (a skeleton the first time, FinUI's error with Try again on failure, the old drawing
kept while a refresh is on its way, and `redraw()` to draw the last answer again when only a filter changed). Lists are
FinUI data tables, refreshed on a timer only while the tab is seen (`every`); their filter box lives outside what is
redrawn.

The landing page is the dashboard (`/dashboard`, `pages/dashboard.js`, "Cluster" 13; `/` and the old `/home` go there),
and beside it the cluster (`/cluster`): its managers, nodes with their labels
and state, the floating addresses with their holders, recoveries, events and the commands sent, and for an
administrator the join tokens, a node's commands and adding or removing a floating address ("Cluster" 15).
Each stack on `/cluster/stacks` has a "Reached from outside" table (its exposures on the pool, its Compose ports on
every worker, or internal only) and, for an administrator, an editor of its exposures ("Cluster" 17).
`/cluster/network` (`pages/cluster-network.js`, "Cluster" 16) draws the traffic live as an SVG: outside, a column per
node with its workloads, a line per pair of ends sized by its bytes per second, and the flows as a table under it.
`/cluster/stacks` lists the cluster's Compose stacks with their workloads, and an administrator writes, deploys,
starts, stops and removes them in the code editor. `cluster.js` asks `/api/cluster` whether the managers answer, at
most once a minute, and the shell makes its menus again when that changes.

**Roles in the UI.** `state.isAdmin()` decides whether a change's button is drawn; the server refuses the rest all
the same. Users and teams are `/users`, the activity log `/activity`, the auth log `/auth-log`, Settings (appearance,
sign-in providers, two-factor) `/settings`. Account has the person's own password, two-factor, the link to a provider,
API tokens and the CA to trust.

Freightdock's own components follow FinUI's contract (data-free, tokens only, an exported `meta`) under an `fd-` prefix,
so they can move to FinUI later: `code-editor` wraps the vendored CodeMirror 6 bundle (a plain text box until it
loads, its highlighting coloured by token in its CSS, a small API: value, language, read-only, onChange) and is the
cluster stack's Compose file; `qr-code` draws the two-factor key.

## 7. Shipping

The image (`Dockerfile`) builds the binary, then puts it on Debian slim with tzdata alone, so `-e TZ=Europe/Oslo` sets
the log's zone. `/data` is the volume; no Docker socket is mounted. CI (`.github/workflows/ci.yml`) runs gitleaks, the
tests, clippy with warnings as errors and the UI's JS checks (`qa/js-check.sh --here`), then builds the image, as
FinStats does: on GitHub to `ghcr.io/<owner>/freightdock`, amd64 and arm64 each on a runner of its own architecture,
pushed by digest and joined into one tag: `:edge` from `main`; `:X.Y.Z`, `:X.Y`, `:X` (from 1.0) and `:latest` from a
tag `vX.Y.Z`, refused unless it is the workspace's version with a patch-notes entry, and a release with that entry
(`gh release create`). Docker's steps are plain `docker buildx`, and the job's own `GITHUB_TOKEN` is the only
credential publishing needs. The Dockerfile keeps Cargo's registry, its git checkouts and `target` in BuildKit cache
mounts, so a builder that keeps its cache compiles only what changed.

The `secrets` job runs gitleaks (`.gitleaks.toml`) over the tree and over the commits a push or pull request brings,
copied into its container rather than mounted, and greps the tree for the names in the repository secret
`PRIVATE_PATTERNS`. The pre-commit hook (`.githooks/pre-commit`) does the same before a commit: refuses data, keys and
`.env` files, checks the added lines for tokens, keys, private addresses and the names in the untracked
`.git/info/private-patterns`, then runs gitleaks (or its Docker image) on the staged files.

## 8. The plan

The plan is the cluster's, kept in the Freightdock-Manager repository (ROADMAP.md, PROGRESS.md). The single-host
milestones M0 to M6 are history; what of them comes back comes through the managers.

## 9. to 12. Stacks, app templates, webhooks, import from Portainer (set aside)

Freightdock's own Compose stacks, app templates, webhooks and the import from Portainer went with the single-host
product. The cluster's stacks are the managers' (§13); the import from Portainer comes back last ("Cluster" 10).

## 13. The cluster

`cluster.rs` makes Freightdock the UI of a Freightdock cluster ("Cluster" 1). `Cluster` (in `AppState::cluster`, set at
start when `FREIGHTDOCK_CLUSTER_API` is) is one or more managers' API addresses and the bearer token, a file read at
each call or the value itself (`Given`, what the managers hand the UI they run, "Cluster" 11); a reqwest client per manager, 15 s, no redirects. Over HTTPS (port 7452, an API token) the client
trusts only the cluster CA in `FREIGHTDOCK_CLUSTER_CA_FILE` and checks the certificate for the managers' fixed name,
which it resolves to the address given; a call starts at the manager that answered last and goes on to the next only
when the connection could not be made, so a change is never sent twice ("Cluster" 8). The routes under `/api/cluster` each make one call to the
manager (`/v1/cluster`, `/v1/nodes`, `/v1/events`, `/v1/nodes/{id}/commands`, `/v1/commands/{id}`, `DELETE
/v1/nodes/{id}`, `/v1/join-tokens` or `/v1/manager-tokens`) and answer what it answered; its refusals keep their
status and sentence ("Cluster" 2). `GET /api/cluster` answers `{connected: false}` without a cluster, so the UI can ask
it on any Freightdock.

The page is `pages/cluster.js` (`/cluster`, under Overview after the dashboard): the
managers (each opening `pages/cluster-manager.js` at `/cluster/managers/{raft id}`: its details and the workers it
hears; an administrator renames it there, "Cluster" 14), which leads and the node each runs on (matched by host), the workers (every node, with whether it runs a
manager too, each opening `pages/cluster-node.js` at `/cluster/nodes/{id}`: its details, the workloads placed on it
and the containers it reports, the addresses of its workloads and its overlay as it last said, `network` in the
managers' answer; an administrator renames it there, "Cluster" 12) with their health as the managers agree on it and which manager hears each, the
latest events, asked again every three seconds; an administrator pings a node, runs, stops or removes a container on
it, sets its labels (`PUT /api/cluster/nodes/{id}/labels`, "Cluster" 5; the labels column shows the worker's own and
the cluster's merged), authorizes one that came back fenced (`POST /api/cluster/nodes/{id}/authorize`, "Cluster" 6;
the Fencing column shows the managers' fencing state with its message as a tooltip), removes it, or makes a join token for a worker or a manager.
Under the pool the cluster's shared storage (`/api/cluster/storage`, "Cluster" 21), added or removed by an
administrator. A node is shown by its name in Freightdock (`node_names`,
merged into the managers' answers by `cluster.rs`), else its hostname; a manager by its (`manager_names`, merged into
`GET /api/cluster`'s `raft`), else its host. A command is followed until it ends and its outcome
is a toast.

`pages/cluster-stacks.js` (`/cluster/stacks`, beside it under Overview) lists the cluster's stacks
(`/api/cluster/stacks`, which asks `/v1/stacks`), each with its workloads: the node it is placed on, its status as
the manager words it (running, stopped, pending, failed, no node, unknown), its container as the worker last reported
it, and its last command, under a search and filters ("Cluster" 20). An administrator starts, stops, restarts or
removes a stack ("Cluster" 4), and deploys or changes one on the deployer, `pages/cluster-deploy.js`
(`/cluster/stacks/new`, `/cluster/stacks/<name>/edit`, "Cluster" 19): the Compose file in the code editor, checked by
the managers a moment after each edit (`/api/cluster/stack-check`), where it runs, and its exposures on the pool
(`exposureEditor`, which the stacks page's Exposures dialog uses too), all sent with the deploy. The manager's warnings
on a file (keys it set aside) are listed under it and toasts after the deploy.

Below the stacks, the same page shows the managers' recovery history (`/api/cluster/recoveries`, "Cluster" 7): a
workload moved off a fenced node and where to, or left there and why; each workload's row says what happens to it if
its node fails (moves, waits for its volumes, waits pinned), and its status is Fenced while its node is in fencing.

