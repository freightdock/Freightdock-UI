# The HTTP API

The UI is built on the same API a program uses. Every route, with who may call it and what it takes, is listed in
[CONTEXT.md](../../CONTEXT.md#api); this page is how to use it.

## Signing in

With an API token (made in Account, [access.md](../access.md#api-tokens)):

```sh
curl -fsS -H "Authorization: Bearer fdk_..." https://freightdock.example.com/api/me
```

A token needs no CSRF token and no `Origin`. A read-only token may only read.

A browser session is a cookie from `POST /api/login` (`{"name", "password"}`), and then every change needs the CSRF
token from `GET /api/me` in `X-CSRF-Token`, and an `Origin` that is Freightdock's own address. That is for the UI; programs
use tokens.

## The shape of it

| | |
|---|---|
| `/api/status`, `/api/health` | Anyone: the version and whether setup is done; `ok`. |
| `/api/cluster/...` | The cluster, as the managers answer: `nodes`, `events`, `commands/{id}`, `recoveries`, `stacks`, `join-tokens`. Reading is anybody's signed in, a change an Administrator's. |
| `/api/settings/...`, `/api/users`, `/api/teams`, `/api/audit`, `/api/auth-log` | Freightdock's own things. |
| `/api/tls/ca.pem` | Anyone: the CA that signed Freightdock's own certificate. |

Answers are JSON. An error is `{"error": "a sentence", "field": "name"}` with a fitting status: 400 for a field that
is wrong (named in `field`), 401, 403, 404, 409, 429 with `Retry-After`, 502 when the managers refused or could not be
reached, with their reason.

## Examples

```sh
H='Authorization: Bearer fdk_...'; S=https://freightdock.example.com

curl -fsS -H "$H" "$S/api/cluster/nodes"                                  # the nodes
curl -fsS -H "$H" "$S/api/cluster/stacks"                                 # the stacks and their workloads
curl -fsS -H "$H" -X POST "$S/api/cluster/stacks/web/restart"             # restart one (an Administrator's token)
curl -fsS -H "$H" "$S/api/audit?limit=20"                                 # the activity log (administrators)
```

Every change made through the API is in the activity log, with the token named.
