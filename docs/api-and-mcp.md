# API tokens and the MCP server

Everything the UI does goes through Freightdock's HTTP API, and a program can use the same API with a token. The MCP
server at `/mcp` lets an AI agent such as Claude Code manage your hosts through it.

## API tokens

**Account**, **API tokens**, **New token**: a name (what it is for, so you know which to revoke), how long it is good
for (30 days, 90 days, a year, or until revoked), and whether it may only read.

The token (`fdk_` and 64 hex digits) is shown once. Freightdock keeps only its hash and its last four characters.

```sh
curl -fsS -H "Authorization: Bearer fdk_..." https://freightdock.example.com/api/environments
```

- A token acts as you, with your role as it is at each use: demoted or disabled, your tokens follow at once.
- A **read-only** token may only read: no change, and no console or attach (they open with a GET but change things).
- A request with a token needs no CSRF token and no `Origin`, since a browser never sends an `Authorization` header on
  its own.
- Every change made with a token is in the activity log as yours, with the token named (`via token claude-code`); its
  last use and the address are on the Account page.
- Tokens are made and revoked only from a signed-in session, never with a token, so a token that leaks cannot make
  itself a longer life or a sibling. Revoke one with the bin beside it.

The routes are listed in [reference/api.md](reference/api.md).

## The MCP server

Freightdock is a Model Context Protocol server over Streamable HTTP at `https://freightdock.example.com/mcp`. Its tools:

`list_environments`, `list_containers`, `inspect_container`, `container_logs`, `start_container`, `stop_container`,
`restart_container`, `kill_container`, `remove_container`, `create_container`, `exec_command`, `container_stats`,
`list_images`, `pull_image`, `remove_image`, `list_volumes`, `list_networks`, `disk_usage`, `activity_log`.

Each takes `env` (an environment's id or name; the local one when left out) and is one or two requests to the API, with
your token: so it meets the same roles, the same checks and the same activity log as the UI. A read-only token can
look but not touch.

### Claude Code

Make a token first (read-only, if the agent should only look), then:

```sh
claude mcp add --transport http freightdock https://freightdock.example.com/mcp \
  --header "Authorization: Bearer fdk_..."
```

The Account page has the line with your address and the token filled in, under **Connect an AI agent**.

Then ask, for example: "Which containers on node-1 restarted today, and what do their logs say?"

### Freightdock's certificate

Claude Code is a Node program and checks certificates and their names. With Freightdock's own certificate, trust its CA
([install.md](install.md#trust-the-ca)) for Node:

```sh
curl -fsSk -o ~/freightdock-ca.pem https://freightdock.example.com:7443/api/tls/ca.pem
NODE_EXTRA_CA_CERTS="$HOME/freightdock-ca.pem" claude
```

and use an address the certificate names: `localhost`, the machine's name and addresses, and whatever
`FREIGHTDOCK_TLS_NAMES` adds (in a container, add the Docker host's name). Or give Freightdock a certificate of your own, or
put it behind a proxy with a public one.

Do not turn certificate checks off for Node as a whole (`NODE_TLS_REJECT_UNAUTHORIZED=0`): it turns them off for
everything Claude Code talks to.

### Turning it off, and every token

An administrator has **Settings**, **AI agents (MCP)**:

- **MCP server**: on or off for the whole instance (on unless turned off). Off, `/mcp` refuses everybody with a
  sentence saying so, and every MCP session ends; API tokens keep working for the API.
- the address and the line that connects Claude Code;
- every API token, with its owner, whether it is read-only, when it expires and when it was last used, and **Revoke**
  for any of them. Each person still makes their own in Account.

The switch and each revoke are in the activity log.

### Other MCP clients

Any client that speaks Streamable HTTP with a bearer header works. Freightdock answers each POST as plain JSON (no event
stream), protocol versions 2025-11-25, 2025-06-18 and 2025-03-26. `/mcp` takes an API token only, never the browser's
cookie, and refuses a request from another site's `Origin`.
