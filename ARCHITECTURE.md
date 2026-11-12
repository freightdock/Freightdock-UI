# Freightdock architecture

Freightdock manages Docker hosts from one Rust binary with its web UI built in. This document explains how the pieces work
and fit; the item-by-item rules are in [`DECISIONS.md`](DECISIONS.md), cited by section and number ("Sign-in" 2).

**Contents.** §1 The process · §2 HTTP and TLS · §3 The database · §4 Sign-in and sessions · §5 Docker endpoints ·
§6 The web UI · §7 Shipping · §8 The plan · §9 Stacks · §10 App templates · §11 Webhooks

## 1. The process

One binary, `freightdock`. `main.rs` reads the configuration from the environment (`Config::from`), makes the data folder,
opens the database (§3), connects to Docker (§5; a Docker that does not answer yet is a warning, not a stop), makes or
loads the TLS certificate (§2) and serves. `freightdock healthcheck` checks that the port accepts, for the image's
HEALTHCHECK; `freightdock --version` prints the version.

The log goes to standard output through `tracing`, filtered by `RUST_LOG` (default `freightdock=info,warn`), each line
stamped in local time with its offset (chrono's clock): the zone `TZ` names, else `/etc/localtime`.

`AppState` (`state.rs`), shared as `App` (an `Arc`), holds the database, the sign-in limiter, whether cookies are
`Secure`, the local Docker endpoint, the sealer, the remote environments' sockets (`docker::sockets`) and the agents'
CA and settings (`agent::Agents`). Beside the web server, `main` runs the agent endpoint (§5), the health check of
every environment (`environments::watch`, every 30 s), the resource history's sampler (`history::watch`) and the
notifications' events watch (`notify::watch`), the image update check (`updates::watch`) and the backup schedule
(`backup::watch`); all stop with the server. A restore asks `main` to stop serving and start the binary again
(`backup::Backups::restart`).

**Backups** (`backup/`). A backup is SQLite's online backup API copying the live database to a file beside it (a
consistent copy while Freightdock goes on writing), then a tar.gz of a manifest, that copy, `secret.key`, `ca/`, `tls/`
and `stacks/` when there, sealed (`backup::crypt`: Argon2id from the passphrase, XChaCha20-Poly1305 in 1 MiB chunks)
and put in a folder (written beside, then renamed) or in S3 (`backup::s3`, Signature Version 4, path-style addresses
unless asked, the bucket made when it is not there). Then the oldest of Freightdock's archives beyond `keep` go. The watch
makes one every `every_hours` while the schedule is on. A restore opens the archive, refuses anything in it but those
names, checks the database (`integrity_check`, a schema not newer than this binary's) and stages it in
`<data>/restore/` with a marker; then Freightdock stops serving and runs itself again in the same process (`exec` of
`/proc/self/exe`), and `backup::apply_staged`, before the database opens, moves what is there aside and the staged
files in place, and the activity log gets `backup.restored`. `freightdock restore <archive>` stages one the same way while
Freightdock is stopped.

**Image updates** (`updates/`). Every quarter of an hour a pass goes over every environment that answers. Each
container's reference (from its inspect: the list shows an image id once the tag has moved) and each tagged image's is
split as a registry reads it (`registry::Reference`: Docker Hub's `library/`, a registry on this machine over plain HTTP
as Docker does), and the registry asked what the tag is now: a HEAD on `/v2/<repo>/manifests/<tag>` with every
manifest type in `Accept`, signed in as its `WWW-Authenticate` says (a bearer token from its realm, with the saved registry's sign-in that
`registry::credentials` takes from `registries::auth_for`, anonymous when none is saved; the realm only over https and, unless the registry is itself inside, never at an address inside, and redirects only on the host first asked, "Outbound" 2). Every answer is read to a size limit (`outbound::capped`). The answer is kept in `image_checks` and used again
until it is `check_hours` old (six by default), and once per pass for every environment that runs the same reference.
The verdict compares the image's own `RepoDigests` for that repository with the tag's digest; a multi-arch index whose
digest is not the local one is fetched once (the one GET), and the local platform's manifest in it decides. A
container whose image has no registry digest but whose tag points at another image here is behind too. The verdicts go
to `update_status`, which the badges read (`GET .../updates`); a container newly behind is told (`update.available`).
A container that opted in (`freightdock.auto-update=true`, or its page's switch in `auto_updates`) and is behind is
updated: its create body worked out against the image it has (so the new image's defaults reach it), the tag pulled,
and `containers::make` with `settle`: the old container stepped aside, the new one started, and if it is not still up
ten seconds later (stopped, restarting, unhealthy) it goes and the old one is back, as when an edit fails.

**Notifications** (`notify.rs`). `notify::notify(app, Event)` is the one way to tell something: it drops an event told
about the same thing within ten minutes (`Notifier`, in memory), then, in the background, sends it to every enabled
channel that wants that kind and writes down each channel's last success or error. A webhook gets the event as JSON
(signed with HMAC-SHA256 when it has a key), ntfy a JSON publish to its server, Gotify its `/message`, e-mail a plain
text mail through lettre over SMTP. The events come from: `notify::watch`, one task per environment following Docker's
container events (read as JSON lines by `docker::raw::follow`, as the events and stats WebSockets and the stats
samples are, so a line that never ends is refused at 1 MiB, "Endpoints" 7): `die`, `kill`, `oom`, `health_status`; `Deaths` tells a death nobody asked for from a stop, and a
dropped stream picks up from the last event's time); `environments::check` and the agent endpoint, when an environment
that was up is not; the image update check (an update out); and `notify::deploy_failed`, which `stacks::run` calls
when a deploy fails (by hand, a webhook or the Git poller).

**Resource history** (`history.rs`). Every `every_s` (a minute unless Settings says otherwise; the watch looks at the
clock and the settings every 5 s, so a change holds at once) the sampler asks every environment for one sample of each
running container, all at once (`streams::sample_running`, the same arithmetic as the live Stats tab), and writes a row
per container to `container_stats`, by the container's name. Rows older than `keep_days` go after each pass. The
container page's History tab asks `.../history?hours=`; `history::series` turns the counters into rates between
neighbouring samples (none across a gap of ten minutes or a counter that went back) and averages them into at most 360
points, drawn with FinUI's line chart.

## 2. HTTP and TLS

hyper is driven directly (`main::serve`), as in FinStats: `axum::serve` sets no timer, so a request head (or a TLS
handshake) could take for ever. Here both must arrive within 15 s, a request's body may stop coming for 15 s and no
longer (`main::IdleBody`: the handler gets an error, a 400, and the connection goes), at most 1024 connections are
served at once, and on SIGTERM the requests under way get 10 s to finish. Connections are upgraded in place for WebSockets.

HTTPS is the default. Without a certificate given, `tls::prepare` opens (or makes) a CA of Freightdock's own in
`<data>/tls/` (`ca.pem`, `ca.key` 0600, `agent::ca::Ca` by another name, apart from the agents' CA) and checks the
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
their snapshot (the database file and `secret.key`) and replays the changes after it; with none there, the local
database becomes the first. Triggers on every replicated table (all but `replica::LOCAL_TABLES`) write each row change,
key and new values as JSON, into `_replog`; a `Db::call` that wrote (`total_changes` moved) then sends what `_replog`
holds to the managers as change `head + 1` and returns only once they took it. They take only the next number: a
Freightdock that loses (another wrote) is stale, its write fails, and it stops to be started again and load. Every 200
changes, and when a migration changes the schema, a snapshot replaces the changes. A change that could not be sent
stays in `_replog` and goes with the next write, or within 2 s.

## 4. Sign-in and sessions

The first run (`POST /api/setup`) makes the first administrator and the `local` environment in one transaction, and is
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
it, and a name tried is kept to 256 characters. The one route that changes something without a session is a webhook's (§11, "Sign-in" 8).

**Access** (`access/`). Who may do what is one table, `access::TABLE`: for each role on an environment (Environment
administrator, Operator, Helpdesk, Standard user, Read-only user) and each action a route does there (read, operate,
console, create, manage, administer), all, none, or only on the containers and stacks the person can see. An
Administrator (`users.role = 'admin'`) is above the table. Roles are `grants`: to a user or a team (`teams`,
`team_members`), on an environment or on its group; `access::resolve` picks the person's own grant on the environment,
else theirs on the group, else the highest of their teams' on the environment, else on the group. `access::routes`
holds the rule of every route (`RULES`: public, signed in, administrator, or an environment's action on the container
or stack its `{id}` names) and the guard, an axum route layer on the whole router (`api::router`), which reads the
route the router matched (`MatchedPath`), signs the person in, loads their `Access` for the `{env}` and refuses what
the table does not allow; a route with no rule is refused. It leaves the `Authed` and the `Access` on the request, so
`Authed` is looked up once and handlers take `Allowed` (who and their access, for a change) or `Access` (for a list).
For Standard and Read-only users the guard also checks that the container (asked of Docker) or stack the route names
is one they can see, else 404. Ownership (`access/ownership.rs`) is `resource_access`, by environment and a container's
name or a stack's id: public, a team's, private, or no record (the administrators'); a container without one belongs
as its Compose project's stack does. `Owners::load` reads an environment's records once for a list, which then leaves
out what the person cannot see (containers, stacks, all-stats, events, webhooks) and says whom each belongs to
(`freightdock_access`, `access`). A create records its maker (private unless asked otherwise) and labels the container
`freightdock.owner`; a rename, an edit and a remove carry the record along, with the container's webhook and its
automatic-update switch (`containers::renamed`, `containers::forget`, which a prune calls too, "Access" 21). Volumes, networks and images are in `resource_access` too ("Access" 18):
`routes::sight` gives a limited person's `ownership::Seen` (what the containers they can see mount, are on and run),
by which the volume, network and image lists, their inspects and the events leave out what they may not see, and
the guard checks a route's `{name}` or `{id}` (`Res::Volume`, `Res::Network`, `Res::Image`, asked of Docker).
`Res::Swarm` (a swarm service or stack) and `Res::External` are never visible to a limited person ("Swarm" 7).
The console action on the container Freightdock runs in (`docker::own`, found at start, `AppState::own_container`) is
refused to all but an Administrator ("Access" 20).
`images::claim` makes the puller or builder an image's owner. `/api/environments` gives only the
environments the person has a role on, each with `role` and `actions`, which the UI hides buttons by.

**Security settings** (`access/security.rs`). `security_rules` holds an environment's or a group's `Rules` as JSON
(the switches of "Access" 12 and the host paths allowed); `security::found` reads what holds on an environment (its own,
else its group's, else everything allowed). Each way a container is made turns what it asks for into one `Spec`
(`of_container` from Docker's create body, `of_compose` from `docker compose config --format json`, `of_service` from a
Swarm service's spec, `of_volume` from a volume's create body), and `judge` lists what the rules refuse of it as
`Breach`es (`added` keeps only what an edit adds). `security::applies` answers the rules a `Judge` is held to on an
environment (none for an Administrator or Environment administrator, or when no rule is about specs): `Judge::of(access)`
for a person at a page, `Judge::Named` for a webhook's owner or a Git stack's maker; `check` turns breaches into the 403
on its field. The callers are `containers::owned` (create and edit), `volumes::create`, `stacks::hold` (the `act`
handler before an editor stack's job, and `run_held` for every deploy), the Swarm service and stack handlers. Consoles
are refused in the guard (`consoles_closed`), and `environments::seen` leaves `console` out of `actions` there. The
dialog is `web/assets/js/security.js`, from Environments and Groups.

**Users and teams** (`users.rs`). `/api/users`, `/api/teams` and the grants of an environment or a group
(`/api/environments/{id}/access`, `/api/environment-groups/{id}/access`, the whole list each time) are an
Administrator's; `PUT /api/me/password` is anybody's own. The last administrator cannot be taken away and nobody takes
themselves away; disabling or a new password ends the person's sessions, and a new password revokes their API tokens
("Access" 22).

**API tokens** (`tokens.rs`). For programs: `fdk_` and 32 random bytes in hex, made in the Account page and shown once;
`api_tokens` keeps the SHA-256 and the last four characters. `Authed` takes `Authorization: Bearer` before the cookie:
the token is looked up with its owner as they are now (role, disabled), the request then needs no CSRF token or
`Origin`, and a read-only token is refused anything but a GET (and, by `Admin`, a console or attach). The `User` a
token signs in carries it (`User.token`), so `audit::record` writes `via: token <name>` without any route knowing.
Tokens are made and revoked only with a session.

**MCP** (`mcp.rs`). `/mcp` is a Model Context Protocol server over Streamable HTTP, for AI agents: each POST is one
JSON-RPC message, answered as plain JSON (`initialize`, `ping`, `tools/list`, `tools/call`; a notification gets 202).
It takes an API token only, refuses another site's `Origin`, and keeps sessions in memory (`state.mcp`), each its
token's. A tool call is turned into one or two requests to Freightdock's own API, sent in process through `api::router`
with the caller's token, peer address and `Host`; so every tool meets the API's guards, Docker code and activity log,
and Freightdock's refusal becomes the tool's `isError` result in Freightdock's words. What comes back is trimmed for an agent
(lists as `docker ps` shows them, logs to their end, at most 256 KiB). The `mcp` setting (`{enabled}`, on when there
is none) is read on every `/mcp` request before the token: off, all three methods answer 403 and the sessions are
dropped. `/api/settings/mcp` is the Settings card's data (every token joined to its owner), and its
`tokens/{id}` revokes anybody's; `/api/mcp/status` tells Account whether it is on and whether there is a CA.

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

## 5. Docker endpoints

`docker::Docker` is one Docker endpoint: an address (`unix://`, `tcp://`, `http://`) and a bollard client for it.
`docker::endpoint(app, env)` answers one for any environment: the local one from `FREIGHTDOCK_DOCKER_HOST`, or the unix
socket of a remote one. Routes under `/api/envs/{env}` call it and never care which kind they got.

What Compose is handed to reach environment X is decided in `stacks::compose::docker_env(app, env)`: `DOCKER_HOST`
set to `docker::cli::docker_host(app, env)`, the address `docker::endpoint` answers (the local Docker's own, or
`unix://` and a remote one's socket), and nothing else. Docker's CLI (`docker stack deploy`) takes the same function, so
every kind of environment is wired in one place for every Docker program Freightdock runs, and a stack deploys to an
agent's host or a TLS host as it does to the local one.

**Remote sockets** (`docker/sockets.rs`). For an agent's tunnel or a TLS host, Freightdock listens on
`<data>/run/env-<id>.sock` and joins every connection to it to a new connection to the host, made by the socket's
`Dial`: a yamux stream through the tunnel, or TCP and a TLS handshake with the host's client certificate
(`docker/tls_host.rs`, the key opened from its seal in memory). bollard then speaks plain HTTP to the socket, as it would
to a local Docker, and `docker compose` is pointed at the same socket. An offer has a generation: a newer offer for
the environment (an agent dialling again) replaces the old, and the old tunnel's end cannot close its successor. What
let a socket be made is read again once it is offered (the agent's certificate still the environment's, a TLS host's
row unchanged), and the socket goes when it is not: a revocation or an edit that lands while it is being made changes
the database before it closes the socket, so one of the two always sees it.

**The agent** (the `freightdock-agent` binary and image, a repository of its own with the tunnel protocol, `freightdock-tunnel`;
the server takes both as ordinary git dependencies at the Agent repository's release tag ("Agent" 11)). It dials Freightdock's agent endpoint
(`agent/endpoint.rs`, its own listener, `FREIGHTDOCK_AGENT_BIND`, port 7444) over TLS from Freightdock's own CA
(`agent/ca.rs`). Without a certificate it enrols first: it makes a key, checks the endpoint against the CA its join token
pins, and posts the token with a certificate request to `/agent/v1/enrol`; Freightdock signs it, keeps its fingerprint in the
environment's row and spends the token. With the certificate it opens a WebSocket at `/agent/v1/tunnel`; the listener
puts the fingerprint of the certificate shown in the handshake on the request (`ClientCert`), and the route finds the
environment by it. Both ends then run yamux over the WebSocket's binary messages (`freightdock-tunnel`, in the agent's repository: `ws::WsIo` makes the
messages a byte stream, `mux::connection` drives yamux and opens streams, at most `MAX_STREAMS` (384) at once, an open
past that failing alone), and the endpoint offers the tunnel as the
environment's socket. Every stream Freightdock opens, the agent joins to its Docker socket. When the tunnel ends the
socket goes; the agent dials again with backoff. Removing the environment or a new join token cuts the tunnel through
the socket's cancel token.

```
browser → /api/envs/2/... → bollard → <data>/run/env-2.sock → yamux stream → WebSocket over mTLS (7444)
        → freightdock-agent → /var/run/docker.sock on the far host
```

**Registries** (`registries/`). A saved registry is a host and how to sign in there; its secret is sealed. Docker is
told who to be with each pull and push (bollard's `DockerCredentials`, Docker's `X-Registry-Auth`), so
`images::pull`, `images::push` and `containers::pull` (create and recreate) ask `registries::auth_for_image`, which
reads the reference's host as Docker does (`reference.rs`) and answers the credentials saved for it, for ECR the token
its keys were exchanged for (`ecr.rs`, kept until it nearly runs out). **For compose deploys**, `registries::auth_for(app,
host)` answers the same for one host, and `registries::docker_config(&[Auth])` writes them as a Docker client
`config.json`: a stack's deploy or pull (`stacks::compose::registry_config`) writes one with every saved registry in
`<data>/stacks/.docker-<id>/` and runs `docker compose` with `DOCKER_CONFIG` there, the folder gone when it is done.
Freightdock itself talks to a registry only to test a sign-in on saving and to browse (`v2.rs`): the `WWW-Authenticate` challenge, Basic or a
Bearer token per scope, the catalog and tags page by page; Docker Hub's repositories come from Hub's API (`hub.rs`).

**Swarm** (`docker/swarm/`). An environment whose Docker is a swarm manager (its `/info`: `LocalNodeState` active and
`ControlAvailable`) has routes under `/api/envs/{env}/swarm` for nodes, services, tasks, secrets, configs and stacks.
They speak to Docker through `docker::raw`, the Engine API as JSON in and JSON out over the same endpoint bollard uses
(hyper's client on the unix socket or TCP): bollard has no configs and no service logs, and a spec that is read,
changed in one field and sent back (a node drained, a service scaled) must keep the fields bollard's models do not know.
Changes are sent at the version Docker gave with the spec. A service's logs are Docker's framed stream, taken apart by
`raw::Demux` into the container logs' messages. Every answer is held to a size (`raw::MAX_ANSWER`, 64 MiB; a log
frame to `raw::MAX_FRAME`, 1 MiB), since a remote environment's Docker is not trusted to end one ("Endpoints" 6). Swarm stacks are `docker stack deploy` run by `docker::cli` with
`DOCKER_HOST` from `cli::docker_host(app, env)`, the endpoint's own address, so the CLI reaches every kind of host as
bollard does, with an emptied environment; the file goes in on standard input (refused first if it has a key the CLI
would read a file from on Freightdock's side, `stacks::reads_no_files`, "Swarm" 8) and is kept in `swarm_stacks` to be
edited and deployed again.

**Volume files** (`docker/volume_files.rs`). A volume's files are reached through a helper container per request,
made from `busybox` (pulled on a host the first time) with the volume at `/volume`, no network and no other mount, and
removed when the request is done (a guard removes it however the request ends). Listing runs `stat` over the folder in
it and reads back one line and one NUL-ended name per entry, so a name may hold anything; making a folder and deleting
run `mkdir -p` and `rm -rf` on `/volume/<path>`; a download is Docker's archive API on the helper, created and never
started: the first entry of the tar says whether it is a file (streamed as itself, the tar's headers dropped) or a
folder (the tar as it comes); an upload is a tar of the one file put with the same API. Every path is cleaned first
(`clean`: no `..`, no control characters), so nothing outside the volume is named, and the helper can see nothing else
of the host anyway. It is all Docker's API, so it works through an agent's tunnel or to a TLS host as it does locally.

**Health** (`environments.rs`). Every 30 s each environment is asked its version; the row keeps up or down, the error,
the version and when it last answered. An agent is also checked as its tunnel comes up and marked down as it ends.
A host that is up is then asked for its summary (`docker/summary.rs`: info, every container and the volumes at once,
within 10 s, read through `raw::get` so each answer is held to `raw::MAX_ANSWER`), kept in the row as its `snapshot`; `GET /api/envs/{env}/summary` takes one on demand the same way. An
agent's version is read from its user agent (`freightdock-agent/1.0.0`) as its tunnel opens and kept with the address it
dialled from; `outdated` compares it with Freightdock's, number by number.

**Groups and tags** (`groups.rs`). An environment is in at most one group (`environments.group_id`, set null when the
group goes) and has any number of tags (`environment_tags`), both case-blind names. `groups::Placement` is what a create
or edit of an environment says about them (left out, a group of null, a whole list of tag names), checked before and
written in the same transaction as the environment; tags it names that do not exist yet are made and logged as
`tag.create`. Roles are given on an environment or on a group (§4, Access), and an environment's role for a person
is the grant on it, else the grant on its one group, so there is never a choice between two groups. Access hangs on
`environment_groups.id`, which a rename does not change.

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
redrawn. Long operations read NDJSON as it comes (`stream.postStream`); a pull adds up its layers into one bar.
Logs, stats, consoles and events are WebSockets the page closes when it is left or redrawn. The console tab runs a
command (exec) or attaches to the main process; an attach that would close the process's input only watches.
Its Networks card connects it to another network (aliases, an address) and disconnects it. Its History tab
draws what it used over an hour to a month from the resource history (§1), stopped or not.

The host the Docker pages show is `state.env`, kept per browser; `docker.base()` builds every Docker path from it. It
is none after sign-in: the landing page is Home (`/home`), and a route marked `host` (the dashboard at `/`,
containers, stacks, templates, images, volumes, networks, events, the Swarm pages) goes to Home while none is chosen.
The menus list the chosen host's pages under its name ("No host chosen" otherwise) and are made again in place when
it changes. The shell asks `/api/environments` again on each navigation, so the menu's host name and Home stay fresh;
the host is changed only on Home ("UI" 14).

**Home** (`pages/home.js`) draws every host at once from `/api/environments`: health, Docker version, address (an
agent's last address when it is gone), group, tags, agent version (marked when outdated) and the last snapshot's
numbers. Then it asks each host's `summary` on its own, never waiting for them together, and draws each row again as
its answer comes; a host that does not answer keeps its last numbers, greyed, with when they are from. Above, the hosts
counted (all, up, down, outdated, in no group), each a quick filter; a toolbar sorts (age, group, kind, health), filters
by group and tag and searches, outside what is redrawn; pages of 10, 25 or 50 once there are more than ten. A row is a
link to the dashboard that chooses its host first. Every 30 s it asks again while the tab is seen. `pages/environments.js` lists the hosts with their health, group and tags, the groups
and tags under them (`pages/env-groups.js`), and, for an administrator, adds an agent (and shows its join token and `docker run` line once), adds a TLS host, edits,
makes a new join token and removes.
`pages/registries.js` lists the registries and, for an administrator, adds (the kind picks the address and what the
sign-in is called; the save is the connection test, its refusal shown on the field it is about), edits and removes;
its browser lists a registry's repositories (Docker Hub's for an account named on the page) or takes a repository's
name, then its tags, each pulled onto the host shown through the images page's pull dialog.

The Swarm pages (`/swarm`, its services, stacks, nodes, secrets and configs) are a group of their own in the menu, there
only while the host shown is a swarm manager: `swarm.js` asks `GET .../swarm` when the host changes and on navigation,
at most every 30 s per host, and the shell makes its menus again when the answer changes. Each Swarm page is wrapped in
`swarmPage`, which draws why there is nothing to show on a host that is not a manager (opened from a link or an old tab).
The cluster page puts nodes, services and running tasks side by side; a service's page has its tasks, its logs (every
task's, over the WebSocket), its spec to edit as JSON and Docker's inspect; a stack's page has its services and its
Compose file in the code editor, deployed again from there.

**Roles in the UI.** Each environment in `/api/environments` carries the person's `role` there and the `actions` it
allows; `state.can(action)` answers for the host shown (an Administrator may everything), and pages draw a button only
when its action is allowed: start and stop for operate, Console for console, Add for create, Edit and Remove for
manage, prune and Swarm changes for administer. The server refuses the rest all the same. A container's and a stack's
page has an Access card (`ownership.js`) to share it; the forms that make one have the same picker. Users and teams are
`/users`, the auth log `/auth-log`; who has which role on a host or a group is a dialog on Environments
(`grants.js`). Account has the person's own password and, with SSO on, the link to the provider.

Freightdock's own components follow FinUI's contract (data-free, tokens only, an exported `meta`) under an `fd-` prefix,
so they can move to FinUI later: `terminal` wraps the vendored xterm.js and takes its colours from the tokens when it
opens; `code-editor` wraps the vendored CodeMirror 6 bundle (a plain text box until it loads, its highlighting
coloured by token in its CSS, a small API: value, language, read-only, onChange) and is the build page's Dockerfile;
`output-view` keeps the last 5000 lines, strips terminal codes, marks standard error, filters, and follows the end
until the person scrolls up. The container form edits Docker's create body itself: fields for the everyday settings,
and the whole body as JSON in a second tab, so nothing it does not show is lost on an edit.

## 7. Shipping

The image (`Dockerfile`) builds the binary, then puts it on Debian slim with Docker's static CLI (from Docker's own CLI
image pinned by its digest; `docker stack` for Swarm stacks, M6), the Compose plugin (a pinned release, its SHA-256
checked for each architecture, at `/usr/local/libexec/docker/cli-plugins/docker-compose` and named by
`FREIGHTDOCK_COMPOSE`), git (Git stacks) and tzdata, so `-e TZ=Europe/Oslo` sets the log's zone. `/data` is the volume.
CI (`.github/workflows/ci.yml`) runs gitleaks, the tests, clippy with warnings as errors, the UI's JS checks
(`qa/js-check.sh --here`) and the end-to-end suite, then builds the image, as FinStats does: on GitHub to
`ghcr.io/<owner>/freightdock`, amd64 and arm64 each on a runner of its own architecture, pushed by digest and joined into
one tag: `:edge` from `main`; `:X.Y.Z`, `:X.Y`, `:X` (from 1.0) and `:latest` from a tag `vX.Y.Z`, refused unless it is
the workspace's version with a patch-notes entry, and a release with that entry (`gh release create`). Docker's steps
are plain `docker buildx`, and the job's own `GITHUB_TOKEN` is the only credential publishing needs.

The agent's crates come to every job and to the image's build the way any dependency does: Cargo fetches them from the
Agent repository at the tag `crates/server/Cargo.toml` names. The image is built with `AGENT_IMAGE` naming the agent's image of the same tag (made `FREIGHTDOCK_AGENT_IMAGE`), so the line it shows for a
new host pulls the matching agent. That image is the agent repository's own: its CI, the same shape, puts the
`freightdock-agent` binary alone on Debian slim, its key and certificate in the `/data` volume. The Dockerfiles keep
Cargo's registry, its git checkouts and `target` in BuildKit cache mounts, so a builder that keeps its cache (a self-hosted runner's)
compiles only what changed.

The `secrets` job runs gitleaks (`.gitleaks.toml`) over the tree and over the commits a push or pull request brings,
copied into its container rather than mounted, and greps the tree for the names in the repository secret
`PRIVATE_PATTERNS`. The pre-commit hook (`.githooks/pre-commit`) does the same before a commit: refuses data, keys and
`.env` files, checks the added lines for tokens, keys, private addresses and the names in the untracked
`.git/info/private-patterns`, then runs gitleaks (or its Docker image) on the staged files.

The end-to-end suite (`qa/run.sh`) is a real binary against a real Docker that is thrown away after: a privileged
`docker:dind` on a network of its own, reached over plain TCP on a port published to 127.0.0.1 only, and Freightdock on
plain HTTP with a temporary data folder. The script only starts and cleans up (a trap removes the dind, its network and
the folder whatever happened); the steps and their assertions are Rust, `crates/server/tests/qa.rs`, an `#[ignore]`d
integration test that speaks HTTP with reqwest and the console's WebSocket with tokio-tungstenite, as a browser would:
session cookie, CSRF token, same `Origin`. It is one test in order, since each step builds on the last.
Freightdock there runs on the machine, not in the image, so the script fetches the same pinned Compose release into
`~/.cache/freightdock-qa` (checked by its SHA-256) and hands it over as `FREIGHTDOCK_COMPOSE`; the suite runs that binary
itself once, at the dind, to start a project "elsewhere" for adopting.

## 8. The plan

| | Milestone | What |
|---|---|---|
| M0 | Foundation | this document so far |
| M1 | One host | dashboard; containers (all actions, create and edit, logs, console, stats, inspect); images (pull, tag, push, build, import/export, prune); volumes; networks; events |
| M2 | Stacks and templates | compose editor and `.env`, deploy and redeploy through `docker compose`, adopting external projects; Git stacks with polling and webhooks; app templates (Portainer's v2/v3 JSON) and custom templates |
| M3 | Many hosts | environments: TCP+TLS, and the agent (outbound WebSocket, yamux, mTLS from Freightdock's own CA), groups and tags; registries |
| M4 | Access | users, teams, roles per environment (Portainer BE's set), ownership, OIDC, LDAP, TOTP, API tokens and the MCP server, activity and auth logs, per-environment security settings |
| M5 | The paid-only extras | multi-environment stacks, image update detection and auto-update, volume browser, backups, notifications, stats history, the Portainer migration helper |
| M6 | Swarm | nodes, services, Swarm stacks, secrets, configs |

## 9. Stacks

A stack is a Compose project Freightdock keeps (`stacks/`). It is deployed with Docker's own Compose, run as a program
(`stacks/compose.rs`): the plugin binary on its own, found at `FREIGHTDOCK_COMPOSE` or where Docker's packages put it.
Freightdock builds its arguments (`--ansi never --progress plain -p <name> -f <file> --project-directory <dir>
--env-file <.env>` and the job), runs it with an emptied environment plus what reaches the environment's Docker
(`docker_env`), and reads standard output and standard error line by line.

**The files** live in the data folder, one folder a stack, by its id:

```
<data>/stacks/<id>/
  compose.yaml     the compose file (an editor stack; an adopted one)
  .env             its variables, NAME=value, which Compose fills ${NAME} with
  repo/            a Git stack's clone (shallow), its compose file at the stack's path inside it
```

The project directory, where relative paths in the file are read from, is the compose file's own folder. The database
(`stacks`) keeps the environment, the name (the Compose project name, unique in its environment), where it came from
(`editor` or `git`), who made it, and when it was changed and deployed.
Stack ids can come again, so a stack's folder goes when the stack goes, or when its environment is removed
(`stacks::forget_folders`), and a new stack's folder is emptied before its files are written ("Stacks" 12).

**Jobs.** Deploy and update are `up --detach --remove-orphans` (with `--pull always` for pull and redeploy); stop,
start and remove (`down --remove-orphans`, with `--volumes` when asked) go by the project name alone, so a stack with a
broken file can still be taken down; pull is `pull`. Each job holds its stack's lock, runs in a task of its own to the
end whether or not the page still reads it, and answers NDJSON as images do (Compose's lines as `stream`, then
`{"done":true}` or `{"error":...}` with Compose's last words on standard error). The outcome goes to the activity log,
a failure with its reason. A file is checked with `config --quiet` before it is kept: a stack Compose refuses is not made,
and an edit it refuses leaves the old file. Before Compose is run over a file at all (saving it, every job that reads
it, the Environment tab), `stacks/confine.rs` reads it as YAML and refuses any file it names (`env_file`, `include`,
`extends`, a build's context and the rest) that lies outside the stack's folder once every link and `..` is followed
("Stacks" 15).

**External projects.** The stacks list groups every container by its `com.docker.compose.project` label
(`stacks/projects.rs`); a project with no stack by its name is external, with its services, state, working folder and
files from Compose's labels (Portainer's are under `/data/compose/<id>`). For a Freightdock administrator on the local
Docker (`stacks::project_files`, never into the data folder, "Stacks" 16), adopting one reads its files where it was
started and has Compose merge them into one (`config --no-interpolate --no-env-resolution`, from its working folder):
relative paths come out absolute, variables stay `${...}`, and its `.env` comes along. When Freightdock cannot read them,
the file is pasted, or, for Portainer's, read through Portainer's API by an import (§12). Its containers are left as they are until the stack is next deployed. A Standard user's new stack may
not take a running project's name (`stacks::unclaimed`, "Stacks" 17): that would make its containers theirs.

**Git stacks** (`stacks/git.rs`) name a repository (https or http), a branch or tag (empty: the default branch), the
compose file's path inside it (`docker-compose.yml` unless said), a user name and a token (sealed), and how often to
check, in minutes (0: only when asked). Making one asks the remote (`ls-remote`), clones it shallow into `repo/`, and
has Compose check the file there; anything that does not work is not kept. A deploy fetches the branch first
(`fetch --depth 1`, `reset --hard FETCH_HEAD`) and keeps the commit it deployed. The poller (`spawn_poller`, from
`main.rs`) wakes every 30 seconds, and each Git stack whose interval has passed asks its remote what its branch points
at; when that is not the commit it runs, it is deployed again (logged as by the Git poller). How the last check went
is kept on the stack (`git_checked_at`, `git_error`) and shown on its page. git runs as Compose does, with an emptied
environment, and is handed the token as an HTTP header in its environment ("Stacks" 9). The token is kept on an edit
only while the address stays on the origin it was saved for (`git::same_origin`, "Stacks" 13). git checks links out as
plain files (`core.symlinks=false`), and the compose file is used only when it is a file in the clone once every link is
followed (`stacks/confine.rs`, "Stacks" 14).

**The UI** (`pages/stacks.js`, `pages/stack.js`, `pages/stack-git.js`) writes compose files and variables in `editor.js`'s `codeEditor`: the
`fd-code-editor` component (CodeMirror; `yaml` for a compose file, `env` for a .env) in a labelled field with its help
and error. Every page asks for an editor only through it.

## 10. App templates

`templates/` reads template lists in Portainer's JSON, so the community's lists work as they are: v2 and v3
(`{"version": ..., "templates": [...]}`, v3 adding an `id`) and v1's bare list. Each entry is read on its own, so one it
cannot read is left out, not the list. Type 1 is a container, 3 (and v2's 4, a compose file by its address) a stack,
2 a Swarm stack (listed, not deployable until M6). The list addresses are the `templates` setting (Portainer's own v3
list until one is set); a list is fetched (at most 8 MB) and kept ten minutes, or asked again with `?refresh`.

Deploying a container template makes Docker's create body from it (`container_body`: image, command split as a shell
would, variables, ports with the host's port Docker picks when none is named, anonymous volumes or the binds it names,
labels, privileged, network, restart policy, unless-stopped when it says none) and goes through the container create
path, pulled and started. A stack template becomes a stack, not yet deployed (the page deploys it next, with Compose's
output): from its repository and stack file as a Git stack on the default branch (as Portainer deploys it), from its
compose file's address, or from a custom template's file. Its variables become the stack's `.env`. A variable marked
`preset` is the template's and is not asked; a `select` is asked as a choice.

Custom templates (`templates` table) are Freightdock's own: a title, words, a logo address, a compose file and the
variables it asks for, in Portainer's `env` shape. The page shows Portainer's notes, which are HTML, as text ("UI" 2).

## 11. Webhooks

`webhooks.rs`. An administrator makes a webhook for a stack (`POST .../stacks/{id}/webhook`) or a container
(`POST .../containers/{id}/webhook`, kept by the container's name, since a recreate changes its id); the answer is its
address, `/api/webhooks/<token>`, the one time it is shown. The `webhooks` table keeps the token's SHA-256, the
environment, the kind and target, who made it and when it was last called; a new one for the same target replaces it,
and a stack's goes when the stack does.

`POST /api/webhooks/{token}` needs no session. A token that is not 64 hex digits is not looked up; one that matches
nothing is a 404, logged as `webhook.refused`, and counts against the caller's address (`Gate`: twenty in 15 minutes,
then 429 with `Retry-After`). A match takes the webhook's turn (none while it runs, nor within 10 seconds of its last call ending), then does the job before it
answers, in a task of its own that ends the turn itself, so a caller that hangs up stops nothing ("Containers" 7): a stack is deployed (`stacks::run`, so it holds the stack's lock and a Git stack is fetched first) with its
images pulled again; a container is recreated (`containers::recreate_as`) from its image pulled again; `?pull=false`
skips the pull. The answer is 204, or the job's reason with a 502. The activity log says `by webhook <id>`, with the
caller's address and no user.

## 12. Import from Portainer

`portainer/` (M5's migration helper). `client.rs` is Portainer's API with one method, GET, the token in `X-API-Key`,
no redirects, and the certificate trusted as the connection says (`Trust`: a pasted certificate pinned or used as a CA,
or no check by choice; rustls with ring, given to reqwest as its TLS); it reads `/api/system/status`, `/api/endpoints`,
`/api/endpoints/{id}/docker/info`, `/api/stacks` and `/api/stacks/{id}/file`, `/api/custom_templates` and their files,
`/api/registries`, `/api/endpoint_groups`, `/api/tags`, `/api/users`, `/api/teams` and `/api/team_memberships`, in
Portainer CE's and Business 2.x's shapes (recorded from a Business 2.45.3, anonymised, in `portainer/fixtures/`).
`plan.rs` is pure: `map_all` matches environments ("Portainer" 4), `stacks` makes each stack's line (what it is, where
it goes, whether it can come and why not, what is running of it there), `teams` (each of Portainer's teams as a
Freightdock team: by the import's record, else by name, else new; its members as the Freightdock users of their names),
`env_text`, `reference`, `poll_minutes`, `registry_kind`.

`mod.rs` keeps the connection in the `portainer` setting (the token sealed) and has three routes. `PUT /api/portainer`
tries and saves it. `POST /api/portainer/scan` reads both sides (Portainer's lists and each Docker environment's
engine through its proxy; each of Freightdock's environments' `/info` and containers, eight seconds each, at once), matches
them (the page's choices first), and answers the plan; it changes nothing. `POST /api/portainer/import` reads again and
makes what is ticked, in order: groups, tags, registries (`registries::import`, no password, untested), custom
templates (`templates::insert_custom`), stacks, people (the copy in `portainer_people`), teams (`import_team`: the
team found again or made, the members Freightdock has added with `INSERT OR IGNORE`, none removed). A compose stack's file is merged by Compose from its project's
working folder into one, then made with `stacks::make_with` (`Extra`: `stack.env` linked to `.env`, the log's
detail); a Git stack with `stacks::make_git` (`GitForm::from_portainer`). Each thing made is written in
`portainer_imports`; the answer is every thing's outcome (`done`, `skipped`, `failed`) with its reason. One import runs
at a time (`AppState::importing`).

The page is `pages/portainer.js` (`/portainer`, from Settings): the connection, then the environments with a choice of
Freightdock environment for each (or a new agent environment, with `environments.js`'s join dialog), the stacks with their
notes and a token field for a Git stack that needs one, groups, tags, registries, templates, teams (each with the
members who join and those left out), people, and Import; then
how each thing went.

## 13. The cluster

`cluster.rs` makes Freightdock the UI of a Freightdock cluster ("Cluster" 1). `Cluster` (in `AppState::cluster`, set at
start when `FREIGHTDOCK_CLUSTER_API` is) is one or more managers' API addresses and the file with the bearer token,
read at each call; a reqwest client per manager, 15 s, no redirects. Over HTTPS (port 7452, an API token) the client
trusts only the cluster CA in `FREIGHTDOCK_CLUSTER_CA_FILE` and checks the certificate for the managers' fixed name,
which it resolves to the address given; a call starts at the manager that answered last and goes on to the next only
when the connection could not be made, so a change is never sent twice ("Cluster" 8). The routes under `/api/cluster` each make one call to the
manager (`/v1/cluster`, `/v1/nodes`, `/v1/events`, `/v1/nodes/{id}/commands`, `/v1/commands/{id}`, `DELETE
/v1/nodes/{id}`, `/v1/join-tokens` or `/v1/manager-tokens`) and answer what it answered; its refusals keep their
status and sentence ("Cluster" 2). `GET /api/cluster` answers `{connected: false}` without a cluster, so the UI can ask
it on any Freightdock.

The page is `pages/cluster.js` (`/cluster`, under Overview when `cluster.js` says the cluster is connected): the
managers and which leads, the nodes with their health as the managers agree on it and which manager hears each, the
latest events, asked again every three seconds; an administrator pings a node, runs, stops or removes a container on
it, sets its labels (`PUT /api/cluster/nodes/{id}/labels`, "Cluster" 5; the labels column shows the worker's own and
the cluster's merged), authorizes one that came back fenced (`POST /api/cluster/nodes/{id}/authorize`, "Cluster" 6;
the Fencing column shows the managers' fencing state with its message as a tooltip), removes it, or makes a join token for a worker or a manager. A command is followed until it ends and its outcome
is a toast.

`pages/cluster-stacks.js` (`/cluster/stacks`, beside it under Overview) lists the cluster's stacks
(`/api/cluster/stacks`, which asks `/v1/stacks`), each with its workloads: the node it is placed on, its status as
the manager words it (running, stopped, pending, failed, no node, unknown), its container as the worker last reported
it, and its last command. An administrator deploys a stack from a Compose file in the code editor, changes one by
deploying it again (`GET /api/cluster/stacks/{name}` gives the file back), starts, stops, restarts or removes it
("Cluster" 4). The manager's warnings on a file (keys it set aside) are toasts.

Below the stacks, the same page shows the managers' recovery history (`/api/cluster/recoveries`, "Cluster" 7): a
workload moved off a fenced node and where to, or left there and why; each workload's row says what happens to it if
its node fails (moves, waits for its volumes, waits pinned), and its status is Fenced while its node is in fencing.

