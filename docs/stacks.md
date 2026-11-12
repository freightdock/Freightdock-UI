# Stacks, webhooks and templates

A stack is a Compose project Freightdock keeps: a compose file and its `.env` variables, deployed with Docker's own
`docker compose`. So a stack behaves exactly as it would on the command line, on any environment: the local Docker, an
agent's host or a TLS host. The image carries a pinned, checksum-checked Compose; a binary run on its own finds the
plugin where Docker puts it, or at `FREIGHTDOCK_COMPOSE`.

## A stack written in Freightdock

**Stacks**, **Add a stack**:

1. A **name**. It is the Compose project's name, unique on that host.
2. Where the compose file comes from: **Written here**.
3. The **compose file**, in the editor (highlighting, search, undo), and the **Variables (.env)**, one `NAME=value` a
   line. Compose fills `${NAME}` in the file with them.
4. **Deploy it now**, or leave it off to deploy later.

Compose checks the file before Freightdock keeps it (`docker compose config`). A file Compose refuses is not saved, and
its own sentence is the error.

```yaml
services:
  web:
    image: nginx:alpine
    ports:
      - "8080:80"
    environment:
      - SITE_TITLE=${SITE_TITLE:-My site}
    restart: unless-stopped
```

On the stack's page:

| | |
|---|---|
| **Deploy** / **Update** | `docker compose up --detach --remove-orphans` |
| **Pull and redeploy** | the same, pulling every image first (`--pull always`) |
| **Pull images** | `docker compose pull` |
| **Stop**, **Start** | by the project's name, so a stack with a broken file can still be stopped |
| **Remove** | `docker compose down`; tick **Its volumes too** to remove its named volumes and their data |

Compose's own output streams onto the page as it goes. A job runs to its end even when you close the page, and a
stack runs one job at a time, so a deploy, a webhook and a Git check never cross.

The **Environment** tab shows every service's variables as the next deploy will give them (`.env`, `env_file` and the
defaults in `${...}` filled in). Values whose names look secret (pass, secret, token, key, auth) are hidden until you
press Show.

The files live in the data folder, `/data/stacks/<id>/compose.yaml` and `.env`, where you can read, diff and back them
up with your own tools.

**Relative paths.** A relative bind mount (`./config:/config`) is read from the stack's folder in the data folder.
When Freightdock runs in a container, Docker on the host reads that path on the host, where it does not exist. Use
absolute paths in bind mounts, or mount the data folder at the same path on both sides
(`-v /srv/freightdock:/srv/freightdock -e FREIGHTDOCK_DATA_DIR=/srv/freightdock`).

**Registries.** A deploy or a pull signs in to every registry saved in Freightdock ([registries.md](registries.md)), so
private images work with nothing in the compose file.

## Stacks started elsewhere

Projects Freightdock did not start (on the command line, by Portainer, by another tool) are found by their
`com.docker.compose.project` label and listed under **Stacks** as external. **Adopt** reads their files where they
were started and has Compose merge them into one, with relative paths made absolute (so they keep mounting what they
mounted) and variables left as `${...}`; their `.env` comes along. When Freightdock cannot read the files (Portainer keeps
its own in its volume), paste the compose file, or import from Portainer ([portainer.md](portainer.md)). Nothing is
redeployed by adopting; the containers stay until you next deploy.

## Git stacks

**Add a stack**, **A Git repository**:

| Field | |
|---|---|
| **Repository** | An `https://` or `http://` address. SSH is not supported yet. |
| **Branch or tag** | Empty is the default branch. |
| **Compose file** | Its path in the repository; `docker-compose.yml` unless you say. |
| **User name**, **Token** | For a private repository: a read-only access token (a GitHub fine-grained token with Contents: read, a GitLab deploy token). The token is sealed and never shown again. |
| **Check for new commits every (minutes)** | 0 checks only when you ask. |
| **Variables (.env)** | As for any stack. |

Making one asks the repository, clones it shallow and has Compose check the file; anything that does not work is not
kept. A check asks the repository what the branch points at (`git ls-remote`, one small request) and deploys again only
when that is not the commit running. The stack's page says how the last check went. A deploy fetches the branch first,
so **Deploy** always runs what the branch has now.

git runs with no system or user configuration and only the https and http transports, and the token goes to git as an
HTTP header for that one run: it is never written into the clone or seen in the process list.

## Webhooks

A webhook redeploys a stack, or recreates a container from its image pulled again, when something calls its address:
for CI, after a new image is pushed.

On the stack's (or the container's) page, the **Webhook** card: **Make a webhook**. Freightdock shows the address once:

```sh
curl -fsS -X POST https://freightdock.example.com/api/webhooks/<64 hex digits>
```

- **No sign-in**: the address is the key. Freightdock keeps only its hash; keep the address like a password, in your CI's
  secrets. **New address** ends the old one.
- A stack is redeployed with its images pulled again (a Git stack is fetched first). A container is recreated with the
  same settings from its image pulled again. Add `?pull=false` to skip the pull.
- A container that belongs to a Compose project has no webhook of its own: use its stack's.
- The answer is **204** when it is done, **502** with the reason when the deploy failed, **404** for an address that
  matches nothing, **429** while the same webhook is still running or within 10 seconds of its last call, and for a
  caller that sent 20 wrong addresses within 15 minutes.
- Every call is in the activity log as `by webhook <id>`, with the caller's address; a wrong address too.

A GitHub Actions step, after the image is pushed:

```yaml
- name: Redeploy
  run: curl -fsS -X POST "${{ secrets.FREIGHTDOCK_WEBHOOK }}"
```

With Freightdock's own certificate, the caller must trust its CA (`curl --cacert freightdock-ca.pem`,
[install.md](install.md#trust-the-ca)), or call it through a proxy with a public certificate.

## App templates

**Templates** lists apps ready to run, from template lists in Portainer's own format (v2 and v3), so every community
list works as it is. Portainer's own list is there until you set others in **Settings**, **App templates** (one
address a line):

```
https://raw.githubusercontent.com/portainer/templates/v3/templates.json
```

Choose a template, give it a name and the variables it asks for, and **Deploy**:

- A **container** template makes a container through the same path as **Add container**: pulled and started.
- A **stack** template becomes a stack, not yet deployed; its page opens and deploys it, with Compose's output. One
  from a repository becomes a Git stack on the default branch, as Portainer does it, so it can be redeployed from its
  repository later.

A list is read again every ten minutes, or at once with **Read the lists again**. A template's note, which Portainer
writes as HTML, is shown as text.

**Custom templates** are your own: **New template**, a title, a description, a logo address, the compose file and the
variables to ask for. They deploy as stacks.
