# The HTTP API

The UI is built on the same API a program uses. Every route, with who may call it and what it takes, is listed in
[CONTEXT.md](../../CONTEXT.md#api); this page is how to use it.

## Signing in

With an API token ([api-and-mcp.md](../api-and-mcp.md)):

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
| `/api/environments` | The hosts you have a role on, with your role and the actions it allows. |
| `/api/envs/{env}/...` | Everything about one host: `containers`, `images`, `volumes`, `networks`, `stacks`, `swarm/...`, `events/ws`, `summary`, `info`, `df`. `{env}` is the environment's id. |
| `/api/registries`, `/api/templates`, `/api/notifications`, `/api/backups`, `/api/settings/...`, `/api/users`, `/api/teams`, `/api/audit`, `/api/auth-log` | Freightdock's own things. |
| `/api/webhooks/{token}` | A webhook's call: no sign-in, the token is the key. |
| `/api/tls/ca.pem` | Anyone: the CA that signed Freightdock's own certificate. |
| `/mcp` | The MCP server. |

Answers are JSON. An error is `{"error": "a sentence", "field": "name"}` with a fitting status: 400 for a field that
is wrong (named in `field`), 403, 404 (also for what you may not see), 409, 429 with `Retry-After`, 502 when Docker or
another service refused, with its reason, 503 while an agent is not connected.

Long jobs (pull, push, build, load, a stack's deploy) answer NDJSON as they go, one JSON object a line, ending with
`{"done":true}` or `{"error":"..."}`. Live views (logs, stats, consoles, events) are WebSockets.

## Examples

```sh
H='Authorization: Bearer fdk_...'; S=https://freightdock.example.com

curl -fsS -H "$H" "$S/api/environments"                                   # the hosts
curl -fsS -H "$H" "$S/api/envs/1/containers?all=true"                     # every container on host 1
curl -fsS -H "$H" -X POST "$S/api/envs/1/containers/web/restart"          # restart one
curl -fsS -H "$H" -H 'Content-Type: application/json' -X POST "$S/api/envs/1/images/pull" \
  -d '{"image":"nginx:alpine"}'                                           # pull, as NDJSON
curl -fsS -H "$H" -H 'Content-Type: application/json' -X POST "$S/api/envs/1/containers/web/exec" \
  -d '{"cmd":["nginx","-t"]}'                                             # a command to its end
curl -fsS -H "$H" "$S/api/audit?limit=20"                                 # the activity log (administrators)
```

Every change made through the API is in the activity log, with the token named.
