# Notifications

Freightdock tells you when something needs you, through channels an administrator sets up under **Notifications**.

## The events

| Event | When |
|---|---|
| `container.died` | A container stopped on its own: it crashed (an exit code other than 0) or ran out of memory. A stop, restart, edit or update by somebody is no news, and neither is a job that finished. |
| `container.unhealthy` | A container's health check fails. |
| `update.available` | A newer image is out for a container ([updates.md](updates.md)). |
| `environment.down` | An environment stops answering: an agent's tunnel ends, a host goes away. |
| `deploy.failed` | A stack's deploy fails, from the page, a webhook or the Git check, with Compose's or git's reason. |

The same event about the same thing is told at most once in ten minutes, so a container crashing in a loop is one
message, not one a minute.

## Channels

**Add a channel**: a name, a kind, the events it wants (**Tell it when**), and on or off.

| Kind | What it needs |
|---|---|
| **Webhook** | An address to POST to, and a signing key if you like. |
| **E-mail** | An SMTP server, its port, security (**STARTTLS**, **TLS** or **None**), a user and password, from whom and to whom. |
| **ntfy** | The server (`https://ntfy.sh` when empty), the topic, a priority (1 to 5), and a token for a protected topic. |
| **Gotify** | The server, an application token, a priority (0 to 10). |

Each channel's secret (signing key, password, token) is sealed and never shown again. **Test** sends a
message through the channel as saved and says in words what went wrong (the server's answer, the SMTP error). The list
shows each channel's last success or error.

## A webhook's body

```json
{
  "event": "container.died",
  "title": "web died on local",
  "message": "It exited with code 1.",
  "environment": { "id": 1, "name": "local" },
  "target": "web",
  "at": 1794295800,
  "freightdock": "1.0.0"
}
```

`at` is Unix time in seconds; `freightdock` is the version that sent it.

With a signing key it carries `X-Freightdock-Signature: sha256=<hex HMAC-SHA256 of the body>`, as GitHub signs its hooks.
Check it before trusting the body:

```python
import hmac, hashlib
def genuine(body: bytes, header: str, key: bytes) -> bool:
    want = "sha256=" + hmac.new(key, body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(want, header)
```
