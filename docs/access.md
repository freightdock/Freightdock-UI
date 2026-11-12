# Users, teams and roles

## Administrators and users

Everybody is an **Administrator** or a **user**:

- An Administrator changes the cluster (deploys, starts, stops and removes stacks; joins, labels, commands,
  authorizes and removes nodes; makes join tokens) and Freightdock's own settings, users and teams.
- A user reads the cluster: its managers, nodes, stacks, events and recoveries. Every change is refused.

**Users** (administrators): make a local user with a name and a password (at least 12 characters), set the role, rename,
disable and enable, give a new password, remove. Disabling and a new password end that person's sessions at once.
The last administrator who is not disabled cannot be demoted, disabled or removed, and nobody can demote, disable or
remove themselves.

Anybody changes their own password in **Account**, with the one it replaces; their other sessions end, and their API
tokens are revoked.

People who sign in through single sign-on or a directory are users too ([sso.md](sso.md)); they have no password here,
and their provider's groups can make them Administrators.

## Teams

**Users**, the teams part: make a team, name its members. A person sees the names of the teams they are in. A sign-in
provider can put people in teams from its groups ([sso.md](sso.md)).

Teams give no rights of their own yet: finer roles (per stack or per node) come with the features that need them.

## API tokens

In **Account**, anybody makes API tokens for scripts: shown once, good for 30, 90 or 365 days or until revoked, and
read-only if you like. A token may do what its owner may, as they are now; a read-only one only reads. See
[reference/api.md](reference/api.md).

## Logs

- **Activity log** (administrators): every change made through Freightdock, with who, what, which and from where, and
  which API token when one was used.
- **Auth log** (administrators): every sign-in and failed one, sign-out, SSO sign-in and refusal, account linked, and
  API token use.

Both filter by kind, person and time, and export as CSV.
