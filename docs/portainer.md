# Coming from Portainer

Freightdock reads what Portainer has through Portainer's own API, and brings it over. Portainer, and the hosts it manages,
are left exactly as they were: Freightdock only ever sends GET requests to it. You can run both side by side for as long as
you like.

What comes: stacks (adopted in place, Git stacks as Git stacks), environment groups, tags, registries (without their
passwords) and custom templates. Portainer's users and teams are read and kept for later; nobody is made a Freightdock
user by an import.

## Before you start

1. Add every host to Freightdock first, as an agent or a TLS host ([agent.md](agent.md)), or let the import offer a new
   agent environment for a host it cannot match.
2. In Portainer, **My account**, **Access tokens**, add one (an administrator's, so it can read everything).

## Connect

**Settings**, **Moving from Portainer**, **Import from Portainer**:

- **Portainer's address**, as `https://portainer.example.com:9443`;
- the **Access token**;
- **Portainer's certificate (optional)**: Portainer makes itself a self-signed certificate, so paste it (from the
  browser, or `openssl s_client -connect portainer.example.com:9443 -showcerts`). Freightdock then trusts that certificate
  as itself, whatever name it is for. Or tick **Do not check Portainer's certificate**: an explicit choice, saved with
  the connection and written in the activity log.

**Try and save** tries the token first. It is sealed and never shown again.

## Read and match

Freightdock reads Portainer's environments and stacks and matches each environment to one of its own: by the Docker
engine's ID first (the same host, whatever either calls it), then by name, then by address. **Matched by** says how.
Change any match, or give an unmatched host a new agent environment.

Each stack says whether it can come, and why not when it cannot:

- a Swarm stack or a Kubernetes one (not yet);
- a stack with more compose files than Portainer's API gives out;
- one whose environment is gone or not matched, or whose name Freightdock already has there;
- two stacks of one name on one environment: the active one, else the newer, is ticked.

A Git stack needs its repository token again: Portainer never gives tokens out, so the page asks for it.

## Import

Tick what to bring and **Import**. Each thing says how it went (done, skipped, failed) and why.

- A Compose stack becomes a Freightdock stack of the same name, so it is the same Compose project and its containers are
  kept as they are. Its file is read through Portainer's API and merged by Compose from the folder its containers say
  they were started in, so relative paths come out as the same absolute paths Docker has been mounting. Its variables
  become the stack's `.env`, and `stack.env` beside the compose file points to it, since Portainer's files name it.
- Nothing is redeployed unless you tick **Deploy each stack from Freightdock once it is imported**.
- A registry comes without its password and is used anonymously until you give one.
- Importing again makes nothing twice.

Portainer's webhook addresses do not carry over: make a new one on the stack ([stacks.md](stacks.md#webhooks)).

## Afterwards

Check each stack's page (its Environment tab shows what the next deploy gives each service), deploy once from
Freightdock, and when all is well, stop Portainer. Removing Portainer's stacks there would take the containers down:
stop Portainer itself instead, or remove the stacks from its database only after Freightdock has redeployed them.
