<p align="center"><img src="web/assets/logo.svg" width="88" height="88" alt=""></p>
<h1 align="center">Freightdock</h1>

<p align="center">
  The web UI of a Freightdock cluster.<br>
  One node is a whole cluster; add nodes when you want more.
</p>

<p align="center">
  <a href="docs/install.md"><b>Install it</b></a>: start the UI beside a manager, the setup token and trusting its certificate.<br>
  <a href="docs/README.md"><b>Read the docs</b></a>: roles, single sign-on, two-factor and the API.
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-GPL--3.0-8a5cf5" alt="License: GPL-3.0"></a>
</p>

> **How Freightdock is built: with AI assistance.** Most of the code here was written by Claude Code, working from the
> maintainer's design decisions and reviewed before it landed. Every change is written test-first, and the tests run
> in CI on every push. Said plainly because you should know what you are running; the responsibility for this code is
> the maintainer's, not a model's.

## What it is

A Freightdock cluster is managers (the control plane, replicated with Raft) and workers (one on every node, running
the containers), each a small binary under systemd. This repository is the UI over them: one Rust binary with its web
UI built in, that signs people in and talks to the managers' admin API. It has no Docker of its own; what runs where
is the managers' to decide and the workers' to do.

The repositories, side by side: **Freightdock-Contracts** (the gRPC contract between managers and workers),
**Freightdock-Manager**, **Freightdock-Worker**, and **Freightdock-UI** (this).

## What you get

- **The cluster at a glance**: the managers and which leads, every node with its health, labels and fencing state,
  the events, and the recoveries the managers made when a node failed.
- **Stacks**: Compose files the managers keep and place on the nodes, deployed, started, stopped and removed here.
- **Nodes**: join tokens for workers and managers, labels, commands, authorizing a node that came back fenced.
- **Sign-in** with local accounts, OpenID Connect (Authentik, Keycloak, Entra ID, Google, Microsoft), GitHub or LDAP
  and Active Directory; teams from groups; two-factor with TOTP.
- **Two roles**: an Administrator changes the cluster, anybody else signed in reads it.
- **The activity log** (every change, who made it and from where) and the **auth log**, both as CSV too.
- **API tokens** for scripts, and **HTTPS from the first start** with a certificate of its own, or yours.
- **Highly available**: the managers run the UI and move it when its node fails; its database is replicated through
  them, so it comes back with the same people, sessions and certificate.

## Get started

You need a Freightdock cluster: a manager and a worker on one node is enough (the Manager repository's README has
`install-node.sh first <host>`). Then let the managers run the UI:

```sh
freightdock-manager ui ghcr.io/freightdock/freightdock:latest
```

It runs on a node of the cluster, moves when that node fails, and opens at **https://any-manager:7443**. Or run it
yourself beside a manager: an API token for the UI and the cluster CA first.

```sh
mkdir -p /var/lib/freightdock-ui
freightdock-manager api-token create ui admin > /var/lib/freightdock-ui/api.token
freightdock-manager ca > /var/lib/freightdock-ui/ca.pem
chmod 600 /var/lib/freightdock-ui/api.token

docker run -d --name freightdock --restart unless-stopped --network host \
  -e TZ=Europe/London \
  -e FREIGHTDOCK_CLUSTER_API=https://127.0.0.1:7452 \
  -e FREIGHTDOCK_CLUSTER_TOKEN_FILE=/run/freightdock/api.token \
  -e FREIGHTDOCK_CLUSTER_CA_FILE=/run/freightdock/ca.pem \
  -v /var/lib/freightdock-ui:/run/freightdock:ro \
  -v freightdock:/data \
  ghcr.io/freightdock/freightdock:latest

docker exec freightdock freightdock setup-token
```

Open **https://your-node:7443**, paste the setup token and make the first administrator. [Install](docs/install.md)
has more managers, a reverse proxy, your own certificate and upgrading.

## Documentation

All of it is in [`docs/`](docs/README.md):

- [Install](docs/install.md), [running it safely](docs/security.md), [troubleshooting](docs/troubleshooting.md)
- [Users, teams and roles](docs/access.md), [single sign-on](docs/sso.md), [appearance](docs/appearance.md)
- [Environment variables and commands](docs/reference/environment.md), [the HTTP API](docs/reference/api.md), and
  the [patch notes](CHANGELOG.md)

Found a security problem? [Report it privately](SECURITY.md). Working on Freightdock itself:
[CONTRIBUTING.md](CONTRIBUTING.md). Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md).

## Licence

Freightdock is free software under the [GNU General Public License v3.0](LICENSE) only. You may use, study, share and change
it; if you distribute a changed version, it has to stay under the same licence with its source available. It comes with
no warranty.

Its UI is built on [FinUI](https://github.com/finstats/finui) and [FinMotion](https://github.com/finstats/finmotion)
(GPL-3.0-only); the fonts are under the SIL Open Font License 1.1, their licences in `web/assets/finui/fonts/`.
