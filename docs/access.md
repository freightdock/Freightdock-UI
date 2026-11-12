# Users, teams, roles and ownership

Who may do what, as Portainer Business Edition has it, for free.

## Administrators and users

Everybody is an **Administrator** or a **user**:

- An Administrator may do everything everywhere, Freightdock's own settings included.
- A user has, on each environment, a role (below), or none, and then that environment does not exist for them: it is
  not on Home, not in the lists, and its pages answer "not found".

**Users** (administrators): make a local user with a name and a password (at least 12 characters), set the role, rename,
disable and enable, give a new password, remove. Disabling and a new password end that person's sessions at once.
The last administrator who is not disabled cannot be demoted, disabled or removed, and nobody can demote, disable or
remove themselves.

Anybody changes their own password in **Account**, with the one it replaces; their other sessions end.

People who sign in through single sign-on are users too ([sso.md](sso.md)); they have no password here.

## Roles on an environment

| Role | May |
|---|---|
| **Environment administrator** | everything on that environment: read, operate, console, create, manage, administer |
| **Operator** | read everything; start, stop, restart, kill, pause containers, stop and start stacks; open a console |
| **Helpdesk** | read everything, change nothing |
| **Standard user** | create containers and stacks; read, operate, open a console on and manage only what they can see (below) |
| **Read-only user** | read only what they can see |

What the actions are:

- **read**: lists, inspect, logs, stats, events.
- **operate**: start, stop, restart, kill, pause; stop and start a stack.
- **console**: a console, an attach, a command run.
- **create**: a container, a stack, a volume, a network; pull, build or load an image; deploy a template.
- **manage**: edit, recreate, rename or remove a container or stack, redeploy it, its webhook, its sharing; connect it
  to a network.
- **administer**: prune; remove, tag or push images; remove volumes and networks; browse a volume's files; every Swarm
  change; adopt a project started elsewhere.

A console is root in the container, and a create can bind the host's root and run privileged. An Operator or a
Standard user is trusted that far: there are no per-environment limits on privileged containers or bind mounts yet.

### Giving a role

On **Environments**, the people icon (**Access**) beside an environment, or beside a group: add users or teams, each
with a role. A role on a group holds for every environment in it.

A person's role on an environment is the first of these that exists, and the highest within it:

1. their own grant on the environment;
2. their own grant on its group;
3. their teams' grants on the environment;
4. their teams' grants on its group.

So a grant to one person is a deliberate exception that wins over their teams', and of several teams the highest
applies. An environment is in one group at most, so there are never two groups to weigh.

## Teams

**Users**, the teams part: make a team, name its members. Give the team a role on environments or groups. A person
sees the names of the teams they are in, to share things with them.

Teams are not taken from the single sign-on provider's groups yet: put SSO users in teams by hand.

## Ownership

Containers and stacks belong to somebody:

| | Who sees and reaches it (Standard and Read-only users) |
|---|---|
| **Public** | everybody with a role on the environment |
| **A team's** | that team's members |
| **Private** | its owner |
| **The administrators'** (no record) | nobody below Helpdesk |

- What you make through Freightdock is private to you unless you choose otherwise in the form. A container made through
  Freightdock also gets a `freightdock.owner` label with your name, for whoever looks with Docker's own tools.
- What was made on the command line, or before roles, is the administrators' until an administrator shares it.
- A container without a record of its own belongs as its stack does.
- Change it on the container's or the stack's page, in the **Access** card. A Standard user may share what they manage
  with everybody or a team they are in, or keep it; an Administrator or Environment administrator may give it to
  anybody.
- Helpdesk, Operators and Environment administrators see everything on the environment.

Volumes, networks and images are not owned (as in Portainer): a Standard or Read-only user sees all of them on an
environment they have a role on.

## Logs

- **Activity log** (administrators): every change made through Freightdock, with who, what, which and from where, and
  which API token when one was used.
- **Auth log** (administrators): every sign-in and failed one, sign-out, SSO sign-in and refusal, account linked, and
  API token use.

Both filter by kind, person and time, and export as CSV.
