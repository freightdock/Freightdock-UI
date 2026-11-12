# Swarm

On a host whose Docker is a swarm manager, a **Swarm** group appears in the menu: the cluster at a glance, services,
Swarm stacks, nodes, secrets and configs. It is there only while the host shown is a manager; a worker cannot answer for
its swarm, and a lone Docker has none.

Freightdock does not make, join or leave a swarm yet. Make it on the command line, then manage it here:

```sh
docker swarm init --advertise-addr 192.0.2.10        # on the first manager
docker swarm join-token worker                       # the line to run on each worker
```

Add the manager to Freightdock as any other environment (an agent or a TLS host, [agent.md](agent.md)), or it is `local`.
Only the manager needs to be an environment; the workers are listed as its nodes.

## What you can do

| Page | |
|---|---|
| **Cluster** | Nodes, services and running tasks side by side. |
| **Services** | Every service with its tasks. Create one from Docker's own service spec; scale, edit the spec as JSON, roll back to the spec before, remove; every task's logs live. |
| **Swarm stacks** | `docker stack deploy` from a Compose file, deploy again after an edit, remove (`docker stack rm`). |
| **Nodes** | Availability (active, pause, drain), role (manager, worker), labels; remove a node that is gone. |
| **Secrets** | Make one, see which services use it, remove. A secret's value goes into the swarm once and never comes back out, not even to an administrator. |
| **Configs** | Make, read and remove configs; a config is not secret, so its text is shown on its own page. |

A change is sent at the version Docker gave with the spec, so a change made meanwhile elsewhere is refused, not
overwritten.

## Swarm stacks

**Swarm stacks**, deploy: a name and a Compose file. Freightdock runs `docker stack deploy --detach --prune` with the file
on standard input, so a service left out of the file goes. Docker keeps no Compose file for a stack, so Freightdock keeps
the one each was last deployed with, to edit and deploy again. A stack deployed elsewhere is listed by its services'
labels and can be removed; deploying it here once gives it a file.

Relative paths in a Swarm stack's file reach nothing (it is deployed from an empty folder). An absolute path, as in a
secret's `file:`, is read on Freightdock's side.

## Who may

Anybody with a role on the environment looks. Every change is the administer action: an Administrator's or an
Environment administrator's ([access.md](access.md)). Each change is in the activity log.

Not yet: registry credentials on a service, and importing Portainer's Swarm stacks.
