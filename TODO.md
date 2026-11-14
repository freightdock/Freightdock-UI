# Freightdock: what is left to do

Freightdock is the cluster's UI (G8.3, "Cluster" 10). The cluster's plan, goal by goal, is the Freightdock-Manager
repository's ROADMAP.md, with what has been done in its PROGRESS.md; this list is only what is left in this repository.
Tick an item off in the same commit that does it, and move what it changes into CONTEXT, ARCHITECTURE and DECISIONS.
The single-host milestones M0 to M6 are history (git log of the old Freightdock repository).

## Next

- [x] G8.5: the managers run the UI as a system workload, reachable on its pool address; one UI writing at a time.
- [x] An end-to-end suite against a real cluster (managers and workers): `qa/e2e.sh` ("Cluster" 32).
- [x] Screenshots of the cluster UI, made by a script and never by hand: `qa/screenshots.sh` ("Cluster" 33).
- [x] Ceph set up from the Integrations page, and a CephFS's keys given there ("Cluster" 34).
- [ ] Commit the screenshots and show them in the README, once the owner says pictures of the machine that made them
      may go in ("Cluster" 33).

## Set aside, to come back through the managers

In the owner's order, external integrations last:

- [x] A node's Docker pages (containers, images, volumes, networks, logs) through its worker. Consoles not yet.
- [x] Registries with saved sign-ins, for the managers to pull with.
- [x] Notifications from the managers' events.
- [x] Backups of the managers' state.
- [x] The MCP server, over the cluster ("MCP" 7).
- [x] Import from Portainer, Compose stacks into cluster stacks ("Portainer" 11 to 13).
- [ ] Finer roles (per stack, per node) with the features that need them.

## Loose ends

- [ ] Set up the Authentik side for SSO (application and provider `freightdock`, groups `freightdock-read` and
      `freightdock-write`), then turn SSO on in Settings. It changes the live sign-in system, so it waits for a yes.
- [ ] `replica::LOCAL_TABLES` still names the single-host `environments` and `container_stats`; harmless (they are
      empty), to drop when a migration next touches them.
