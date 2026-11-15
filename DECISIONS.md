# Freightdock decisions

The contracts: every rule the code keeps, with its reason, and every owner's decision. Numbered per section and cited by
section and number ("Sign-in" 3). Numbers never change: a rule that no longer holds is marked **Superseded** with a
pointer to what replaced it.

## Project

1. Freightdock replaces Portainer for Docker; it is not a drop-in (no API or database compatibility). It aims at about 90% of
   what Portainer's Business Edition does for Docker, with nothing behind a licence: no key, no node limit, no paid
   feature (owner's decision, 2026-11-09).
2. GPL-3.0-only, like FinUI and FinStats (owner's decision, 2026-11-09).
3. Kubernetes, Nomad and Azure ACI are out of scope for good. Swarm comes last (M6). Podman is best effort through its
   Docker-compatible API.
4. The fin* repositories' conventions apply: test-first, the three documents kept current, conventional commits, no LLM
   attribution, never push unless told, no em-dashes (owner's decision, 2026-11-09).
5. Patch notes as FinStats keeps them: `CHANGELOG.md`, compiled in, drawn by the app, with a dot in the menu until the
   running version's notes have been opened in this browser; a release cannot be built without its entry (owner's
   decision, 2026-11-09).
6. The repository is public, so nothing private goes in it (from 1.0.0): no secret, no token, no key or certificate, no
   address or host name of anybody's own network, no user name of a real person. Examples use the documentation
   addresses (`192.0.2.x`, RFC 5737) and `example.com` names (RFC 2606); test fixtures recorded from a real system are
   anonymised before they are committed. A versioned pre-commit hook and a CI job run gitleaks with a committed
   configuration (`.gitleaks.toml`), whose allowlist holds only third-party files and genuine test values, each with
   its reason; names that cannot be written in a public file are kept outside the repository (the untracked
   `.git/info/private-patterns` for the hook, the `PRIVATE_PATTERNS` secret for CI). History is not rewritten by an agent: what
   is found there is reported, and squashing or filtering is the owner's decision.
7. A release is a `v*` tag, built and published by CI (`release.yml`): the server's and the agent's images for amd64
   and arm64 on GHCR, and a GitHub release whose notes are the version's CHANGELOG entry, refused when the tag is not
   the crate's version or the entry is missing. The image names are written once in the workflow, so a rename is one
   edit. **Superseded** by 8.
8. A release is a `vX.Y.Z` tag that `.github/workflows/ci.yml` builds, in each repository for its own image (from
   1.0.0; **supersedes 7**): the tests, the end-to-end suite and gitleaks first, then amd64 and arm64 on runners of
   their own architecture, joined into `:X.Y.Z`, `:X.Y`, `:X` and `:latest` on GHCR, and a GitHub release with the
   version's CHANGELOG entry. `release.yml` built the same tag a second time, and the agent's image tag from a
   `tag =` the dependency never had, so it went. The CI is GitHub's alone: the Forgejo twin, kept in step by hand while
   a Forgejo stood in for GitHub, went with it. The job's `GITHUB_TOKEN` is the only credential publishing needs.
9. The screenshots in `docs/screenshots/` are made by `qa/screenshots.sh` and never by hand (from 1.0.0): a throwaway
   Freightdock with invented hosts and containers, every one of them set up through the API, each page taken in the
   light theme (`<page>.png`) and the dark one (`<page>-dark.png`), so the README shows the one its reader's theme
   asks for. A picture is refused when its page says null, undefined or NaN, names Shipshow, has a broken image, or
   does not show what it is about. Hand-made ones carried the old name and the owner's own registry into the
   repository; a generated one shows only what the tool made, and is made again whenever the UI changes.

   **Superseded** by "Cluster" 10: `qa/screenshots.sh` drove the single-host product and went with it; screenshots of the cluster UI are taken on the test cluster until a script makes them again. Back for the cluster UI with "Cluster" 33.
## Database

1. One SQLite file in WAL mode; every transaction IMMEDIATE, so a transaction that reads and then writes is never refused
   its write at commit by another commit in between.
2. Migrations are immutable once released, and only appended: a database out there has run every released step, and an
   edited step would leave it different from a new one.
3. A database whose schema is newer than the binary is refused, untouched: an older binary would not know what the newer
   columns mean, and the way back is the newer binary or a backup.

## Sign-in

1. Passwords are Argon2id, at least 12 characters. The first administrator is made by `/api/setup`, which is refused once
   any user exists, in the same transaction that checks, so two first runs at once cannot both win. **Superseded in part
   by 14**: it also wants the setup token.
2. The database holds a SHA-256 of each session token, never the token: a copy of the database (a backup) signs nobody
   in.
3. Every changing request carries the session's CSRF token in `X-CSRF-Token`. SameSite alone leaves sibling subdomains
   and older browsers; the token does not.
4. A changing request or a WebSocket upgrade whose `Origin` is not this server is refused. A WebSocket carries no CSRF
   header, and a page elsewhere must never open a console on a container.
5. Five failed sign-ins within 15 minutes lock that address, and that name from anywhere, for the rest of the window. A
   wrong name costs as long as a wrong password, so names cannot be found by timing.
6. A session ends after 7 days unused or 30 days in all.

7. The session cookie is SameSite=Lax, not Strict (from 1.0.0, with SSO): coming back from the sign-in provider is a
   navigation from its site, and a Strict cookie set on that answer is not sent with the next page, so the person would
   land signed out. Lax still keeps it off every cross-site request but a top-level GET, and 3 and 4 guard every change.
8. Webhooks are the one place a change is made without a session (from 1.0.0, M2): a CI job has no browser to sign in
   with, and Portainer's users already redeploy from CI this way. The token in the address is the whole credential,
   so it is 32 random bytes, kept only as its SHA-256 (shown once, when it is made; a new one ends the old), and
   compared by its hash, never by a guessable id. No cookie is read, so a page elsewhere gains nothing by calling it
   from a browser, and CSRF and `Origin` do not apply. Every call is in the activity log (a wrong address too, as
   `webhook.refused`), twenty wrong addresses from one address within 15 minutes wait out the window, and a webhook is
   refused while it runs and for 10 seconds after. A webhook can only redeploy what it was made for: a stack (fetched again first when it
   is from Git, images pulled again unless `?pull=false`) or a container (its image pulled, recreated with the same
   settings); a container in a Compose project gets the stack's instead.
   **Superseded** by "Cluster" 10: there are no webhooks in the cluster UI, so every change needs a session.
9. A request may come with an API token, `Authorization: Bearer fdk_...`, instead of the cookie (from 1.0.0), and then
   needs neither the CSRF token (3) nor the same `Origin` (4). Both guard against a browser sending its ambient
   credential, the cookie, on a request another site made; a browser never adds an `Authorization` header on its own,
   so a request that carries one was made by whoever holds the token. A bearer header that is not a good token is
   refused, never passed over for a cookie beside it.
10. An API token is `fdk_` and 32 random bytes in hex, shown once when it is made; the database keeps its SHA-256 (as
   2) and its last four characters to tell it by. It acts as its owner with the owner's role as it is at each use, so
   a demoted or disabled person's tokens follow at once; one made read-only may only read (GET, and no console or
   attach, which open with a GET but change things), whatever the role. It is good for 1 to 3650 days or until revoked,
   and notes when and from where it was last used. Tokens are made and revoked only with a session, never with a
   token, so a token that leaks cannot make itself a longer life or a sibling.
11. A try at a password or a code is counted as it starts, not when its answer comes (from 1.0.0, audit 2026-11-10):
   the check that one may be made and the count are one step under one lock, and a right answer gives its try back.
   Counted after the Argon2 check, every try sent at the same moment passed the check before any was counted, so one
   burst gave as many guesses as it had connections, not five (5).
12. A sign-in with a name longer than a name may be (64 characters, as 1 and "Access" 8 hold names to) is refused as a
   wrong name and password before anything is checked, counted or written (from 1.0.0, audit 2026-11-10): no account
   can have it, and the limiter's keys and the auth log would otherwise keep whatever size somebody sends, up to the
   2 MiB a body may be, for every try.
13. Changing one's own password checks the one it replaces as a sign-in checks a password (from 1.0.0, audit
   2026-11-10): the same tries by address and by name (5, 11), a 429 while locked, and every wrong one in the auth log
   (`password.failed`). Unlimited and unlogged, it let whoever held a session but not the password guess it at Argon2's
   pace, and the right guess both told them the password and locked the owner out.
14. The first run wants a setup token (owner's decision, 2026-11-10; **supersedes 1 in part**, where nobody existing
   was the only condition): while nobody exists, Freightdock makes 128 random bits at start and prints them to its log,
   or takes `FREIGHTDOCK_SETUP_TOKEN` (at least 16 characters, for automated installs and `qa/`), and `/api/setup` is
   refused without it, compared in constant time. The same `Origin` (4) is judged against the request's own `Host`,
   so a page on a name of its own, rebound to a Freightdock not yet set up that only the victim's browser can reach,
   passes it and would become the first administrator; reading the log is what proves the person is the one
   installing it.
15. A webhook's turn (one call while it runs and for ten seconds after) belongs to its address, the token's hash,
   not to its row: a webhook taken away, or whose container went ("Containers" 3), leaves its id to the next one made,
   and that new webhook must run at once rather than wait out the old one's gap. Found by the end-to-end suite, where a
   stack's new webhook was refused 429 straight after a removed container's.
16. The first run's setup token waits in `<data>/setup-token` (owner's request, 2026-11-10), readable by its owner
   alone, until the first administrator is made, when it is removed (and at any start that finds somebody exists).
   It is the same across restarts, and `freightdock setup-token` prints it (`docker exec freightdock freightdock setup-token`),
   so an installer asks for it rather than searching a log, and the token in an older log line still works. This
   keeps 14's proof: reading that file wants the same access to the server as reading its log. One given in
   `FREIGHTDOCK_SETUP_TOKEN` is never written down.
17. The binary refuses a command it does not know (2026-11-10). It used to serve whatever followed its name, so
   `docker exec freightdock freightdock setup-token` on an image older than 16 started a second Freightdock in the container,
   which printed a setup token of its own (that nobody would accept) and died on the taken port. Now it answers the
   usage and exits 1; serving is the empty command line alone.
18. What can take a node over wants a recent sign-in (2026-11-15): a shell on a node or in a container ("Cluster"
   35), starting an upgrade of the cluster, and a binary given for it or fetched ("Cluster" 36). Recent is a session
   started, or its password (and code, with two-factor on) given again with `POST /api/me/confirm`, within ten
   minutes (`sessions.confirmed_at`, `auth::CONFIRM_S`). Otherwise the answer is 403 with `code: "confirm"`, and the
   page asks for the password and sends the request once more (`reauth.js`); before a binary it asks first, so many
   megabytes are not sent twice. A cookie that leaked (a shared machine, a browser extension, a proxy's log) then
   reads and changes what its person can, but never runs a command as root. Somebody who came through a provider has
   no password here and signs in there again: a new session is recent. An API token is not asked, being a program's,
   made on purpose and never in a browser. The password given again is held to the sign-in limit (5) and both
   outcomes are in the auth log (`confirm`, `confirm.failed`); a wrong one is 400, not 401, which would end the
   session in the page.

## SSO

1. Single sign-on is OpenID Connect, the authorization-code flow with PKCE (S256), a state that is good once for ten
   minutes, and a nonce checked in the ID token. The ID token's signature is checked against the provider's JWKS (asked
   again at once for a key it has not seen, so a key rotation does not lock people out), or HS256 with the client
   secret for a provider set to sign that way (Authentik without a signing key). Issuer, audience and expiry are checked
   with a minute's leeway. **Superseded in part** by 14: the state is held by the browser that started the sign-in,
   not by the server.
2. Who comes in, and as what, is the groups claim: a member of an administrator group is an administrator, of a user
   group a user, and anybody else is turned away (owner's decision, 2026-11-09, as the house's Authentik does it: each
   app sent only its own `<slug>-...` groups, `freightdock-write` and `freightdock-read`). The role is set again at every SSO
   sign-in, and somebody turned away loses the sessions they had. **Superseded in part** by 15: never so as to leave
   no administrator.
3. A provider's person is a Freightdock user found again by issuer and subject, never by name: a provider whose users
   choose their names could otherwise take over a local account. A name a local account has gets " (SSO)" after it.
   **Superseded in part** by 6 (from 1.0.0, M4): the first sign-in may land in the local account of the same name;
   from then on it is found by issuer and subject, as here, and " (SSO)" is only for when that switch is off.
4. The local sign-in always stays (owner's decision, 2026-11-09): it is the way in when the provider, or what it runs
   on, is down. An SSO user has no password and cannot use it.
5. Signing out of a session that came through the provider also goes to the provider's end-session address, so the
   next sign-in asks again.
6. "Sign in to the local account with the same name" (`link_by_name`, on by default; owner's decision, 2026-11-09,
   after the owner's first Authentik sign-in gave them a second account, "<their name> (SSO)"). A person's first SSO sign-in
   lands in the local account of the same name (without regard to case) when that account is linked to nobody yet,
   and links it: its issuer and subject are written on it, and it is found by them from then on (3), its role set by
   the groups at each sign-in (2) and its password kept for the local sign-in (4). With it on, an account of that
   name already linked to somebody else turns the sign-in away; a " (SSO)" name is never made. It is safe where the
   provider decides the names (the owner's Authentik does); a provider whose users choose their own should have it off,
   which gives 3's behaviour. **Superseded in part** by 15: only the very name links, and the name is never changed to
   fit.
7. A signed-in local user links their account to the provider from Account (from 1.0.0, M4): a POST with the CSRF token
   answers the provider's address, so no other site can start it, and the way back links only in the browser still
   signed in as that account, and only to a person at the provider nobody else is linked to. Linking is not signing in:
   the groups are not asked, and the role stays until the next SSO sign-in sets it (2). It is in the auth log
   (`sso.link`) and the activity log (`account.sso-link`).
8. A disabled account stays out through SSO too (from 1.0.0, M4): the provider vouching for a person does not undo an
   administrator disabling them. Before, an SSO sign-in set `disabled` back to 0, which was harmless only while nothing
   disabled anybody.
9. Sign-in providers are a list (from 1.0.0, M4), each with an id, its own button on the sign-in page and its own
   callback, `/api/auth/sso/<id>/callback`; several may be on at once. The one OpenID Connect setting kept before the
   list became its first entry by migration 18, with the id `oidc`, its sealed secret as it was, and its callback
   `/api/auth/oidc/callback` still taken, so a provider set up before keeps working with nothing changed there. A
   sign-in's state names the provider it was started with, and it is finished only when it comes back on that
   provider's own callback: a provider that sends somebody back with another's code (a mix-up) is refused, and the
   auth log says so. The list is an administrator's; a client secret is sealed and never sent back ("Secrets" 1).
10. Presets fill in Google and Microsoft Entra ID (OpenID Connect: issuer, scopes, claims) and GitHub (OAuth 2.0
   without an ID token: the code swapped with PKCE, then who it is from its API: `/user`'s numeric id as the subject,
   since a login can be renamed and taken by somebody else, its login as the name, the verified primary address of
   `/user/emails`, and as groups each organization of `/user/orgs` and each team of `/user/teams` as `org/team`). A
   provider anybody can make an account at (Google, GitHub) cannot be turned on until it says who may come in: groups
   (organizations, teams) or e-mail domains. A domain list lets in only a person whose address the provider says it
   has verified (`email_verified`, GitHub's `verified`), so an unverified address at the right domain is not enough.
   Microsoft's issuer is its tenant's (the `common` endpoint names no one issuer to check the token against), and the
   tenant decides who is in it.
11. Teams from groups (from 1.0.0, M4; Portainer's automatic team membership): a provider may map groups to teams,
   each rule a group's name or a regular expression between slashes whose captures the team's name may use. At every
   sign-in through it (OpenID Connect, GitHub, LDAP) a person is put in the teams their groups map to, made first when
   the provider says so, and taken out of the ones this provider put them in that they no longer map to. Who put a
   person in a team is kept (`team_members.synced_by`): what an administrator gave is never taken away by a sign-in,
   and one provider never takes away what another gave. A rule is compiled when it is kept, with a size limit, since it
   runs at every sign-in. What changed is in the auth log (`teams.sync`).
12. A provider that names no administrator group leaves roles as they are (from 1.0.0, M4). **Supersedes 2 in part**:
   the role is set again at every sign-in only when the provider names an administrator group, since only then can its
   groups say who is one. Without one (Google, which sends no groups; a provider set to let everybody in), somebody new
   is a user and an account found keeps its role, so the owner linked by name to Google stays an administrator.
13. An account answers to one outside identity at a time (`users.oidc_issuer`, `oidc_sub`): one provider's, or one
   directory's (`ldap:<id>`). Two providers' groups would otherwise set its role and teams by turns at each sign-in.
   With linking by name on, the same name signing in through a second provider is turned away (6), as for somebody
   else at the first.
14. A sign-in under way is the browser's, not the server's (from 1.0.0, after the 2026-11-10 audit). Its state, PKCE
   verifier, nonce, provider, the address to go on to (at most 1 024 bytes, else `/`) and, for a link, the account are
   sealed with the key secrets are ("Secrets" 1) into a cookie, `freightdock_sso`: `Path=/api/auth`, HttpOnly, SameSite=Lax, ten minutes. The
   callback goes on only when that cookie opens and holds the state in the query, compared in constant time, and
   spends it whatever happens. Before, the server kept them in one shared map by state alone: a callback address
   captured by somebody who signed in as themselves signed in whichever browser opened it (login CSRF), and ten
   thousand sign-ins started by anybody, unauthenticated, filled the map and closed single sign-on to everybody else
   for ten minutes. The provider's code is good once, so a state is spent by its code rather than by a list kept here.
15. A provider's name is taken as it is or not at all (from 1.0.0, after the 2026-11-10 audit). A first sign-in whose
   name a local account could not have (1 to 64 characters, no control characters, no spaces at the ends: `check_name`)
   is turned away and told why, for every provider and directory alike; linking by name (6) matches only that very
   name, without regard to case. Before, the name was trimmed, stripped of control characters and cut to 56
   characters before it was looked up, so `ro\0ot`, `\tops ` or a long name sharing another's first 56 characters
   landed in `root`, `ops` or that other account. The name falls back to the e-mail address only when the provider
   says it has verified it, else to the subject. And an SSO or directory sign-in never takes the last administrator
   away: when its groups would make the only enabled administrator something less, the account stays an
   administrator and the server log says so, as an administrator cannot demote the last one either.
16. Every address a provider's discovery document names (where to sign in, the token endpoint, the keys, userinfo,
   where to sign out) is https, or http to this machine, as the issuer itself must be (from 1.0.0, after the
   2026-11-10 audit); a document that names any other is refused like one that does not answer. The browser is sent
   to two of them by script (`location.assign` in Account, to link and to sign out), and a `javascript:` address
   there would run in Freightdock's page with the session of whoever clicked. Account follows such an address only when
   it is http or https, whatever the server says, as FinUI's `h()` does for every link ("UI" 2).

## LDAP

1. LDAP and Active Directory sign people in through Freightdock's own password form (from 1.0.0, M4): the local accounts
   first (a local account's password always works, "SSO" 4), then each directory that is on, in turn. A directory that
   cannot be reached is passed over and logged, so it never locks the local sign-in out. The sign-in limiter holds
   both alike ("Sign-in" 5), and a person the directory knows but the groups turn away is told why and counts as a
   miss. Their account is found, linked by name or made as a provider's is ("SSO" 3, 6, 13), its role and teams from
   their groups ("SSO" 2, 11, 12), and it has no password of its own: only the directory checks it.
2. Only over TLS unless an administrator allows otherwise: `ldaps://`, or `ldap://` with StartTLS, against the system's
   roots or a CA given for the directory. Plain LDAP sends every password as it is typed, so it is refused unless the
   switch "Allow plain LDAP" is on, and saving it so is written to the activity log ("plain LDAP allowed") and shown
   beside the directory in Settings.
3. Freightdock binds as its service account (or anonymously), finds the one entry the user filter gives, then binds as
   that entry with the password typed: the directory alone decides whether it is right, and Freightdock never reads or
   compares a password hash. Every value put in a filter (the name typed, a DN) is escaped as RFC 4515 says (`*`, `(`,
   `)`, `\`, NUL and every byte that is not printable ASCII as `\XX`), and a filter template is one balanced filter
   with its placeholder, so a name typed cannot widen or change the search: `*` is a name, not everybody. An empty
   password is never sent (LDAP takes a name and no password as an anonymous bind, and answers success); a filter that
   finds more than one entry signs in none of them. A person is kept by `entryUUID` (`objectGUID` in Active Directory),
   so a rename or a move in the directory is still the same person.
4. The service account's password is sealed and never sent back ("Secrets" 1), and goes only to the address it was
   given for: when the address changes, saving or testing asks for it again, so an administrator's session cannot be
   used to send it to a server elsewhere and read it there.
5. Groups come from the entry's `memberOf` or from a group search (`member`, `uniqueMember`, `memberUid`); Active
   Directory's preset searches with the in-chain rule (`member:1.2.840.113556.1.4.1941:=`), which follows nested
   groups. A group counts by its name (`cn`) and by its DN, so either may be named in the group lists and team rules.
   Settings tests the connection and the service account, and looks up a person: their DN, groups, the role they would
   have and the teams, and with a password whether it is right.

## Two-factor

1. TOTP is the second factor of a sign-in with a password typed into Freightdock: local accounts and directories (from
   1.0.0, M4). A sign-in through an OAuth provider is not asked for it: the provider has its own, and asking again
   would only send people round. RFC 6238 with HMAC-SHA1, six digits and 30-second steps (what every authenticator app
   takes), tested against the RFC's vectors.
2. A right password for an account with a second factor (or when one is required) answers a ticket, not a session: 32
   random bytes, held in memory for five minutes and five wrong codes. The code step is held to the sign-in limiter,
   counting misses for the address and the name like a wrong password, and a right password no longer forgives the
   misses before it when a code is to come (else every right password would buy five more guesses at the code); only a
   whole sign-in does.
3. A code is good in its step and the one either side (a clock a little off, a code typed as it changed), and never
   twice: the step a code was taken for is kept, and only a later one is good after it, taken in one statement so that
   of two sign-ins at once with the same code one wins.
4. The seed is 160 random bits, sealed ("Secrets" 1), shown once while it is set up (as the QR code and as text) and
   never again. It is confirmed by a code from it before it counts. Ten recovery codes come with it, shown once: 80
   random bits each, beyond guessing, so a SHA-256 of each is kept (as session tokens are, "Sign-in" 2), each good once.
   New ones need a code; turning two-factor off needs a code or a recovery code.
5. An administrator may require two-factor for every sign-in with a password. Somebody without it then sets it up at
   their next sign-in, before a session is made, and cannot turn it off; an administrator resets a lost one (the
   person signs in with the password and sets up a new one). The QR code is drawn in the browser by Freightdock's own
   encoder (no library, no CDN, "UI" 1), so the seed never leaves Freightdock for a QR service. Every enrolment, wrong
   code, recovery code used, reset and change of the rule is in the auth log or the activity log.
6. An account has at most five tickets waiting at once (from 1.0.0, after the 2026-11-10 audit): a sixth takes the
   place of its oldest, and when the store (ten thousand) is full all the same, the oldest of all gives way to the new
   one. Before, a full store refused every new ticket, so one person with a right password, asked again and again,
   kept everybody else with two-factor out for five minutes at a time.

## Secrets

1. Secrets kept for others (sign-in providers' client secrets, LDAP bind passwords, TOTP seeds, Git stacks' tokens,
   registry passwords) are sealed with
   XChaCha20-Poly1305 under `secret.key` in the data folder, made on first start, 0600. A copy of the database alone
   gives none of them away. They are never sent back to the browser: a settings page says only whether there is one, and
   an empty field keeps it.

## Activity

1. Every change made through the API, and every sign-in, failed sign-in and sign-out, is written to `audit` with who,
   what, which and from where. An administrator on Docker's socket is root on the host; what they did must be findable.
2. A change made with an API token is written as its owner's, with the token named in `via` (`token <name>`), so what a
   program did can be told from what the person did by hand, and a token that misbehaved can be found and revoked.
3. Who came in, how and from where is the auth log (`auth_log`, from 1.0.0, M4), apart from the activity log: every
   password sign-in and failed one, every sign-out, every SSO sign-in and every one turned away (with why), every
   account linked to the provider, every LDAP sign-in, every second factor set up, failed, used up or reset, every
   team a provider's groups changed (from M4's sign-in), and API tokens' use, as often as a token's last use is noted (at most once a minute
   a token: noting every request would drown the rest). Each with the user (or the name tried), the address and the
   browser's user agent. **Supersedes 1 in part**: sign-ins, failures and sign-outs are written here and no longer to
   `audit`, whose entries made before stay. Both logs are an administrator's, filtered by kind, person and time, and
   exported as CSV (`?format=csv`): RFC 4180 quoting, times in UTC as RFC 3339, and a field that a spreadsheet would
   run as a formula (`=`, `+`, `-`, `@` first, and not a number) starts with `'`, since user names and targets are
   written by whoever chose them.
4. The auth log keeps a year (from 1.0.0, audit 2026-11-10): every entry older goes, at start and hourly. Anybody can
   add to it without signing in, a failed sign-in at a time, so a log that never forgets is one that fills the data
   folder in the end; a year answers who came in for any question still worth asking. The name tried is kept to 256
   characters, as the user agent is.

## Access

**Superseded in part** by 23 (G8.3): the roles per environment, ownership and security settings (1 to 4, 6 to 21) went with the single-host product; the rest still holds.

1. Roles are Portainer Business Edition's (from 1.0.0, M4). An Administrator (`users.role = 'admin'`) may do
   everything everywhere, Freightdock's own settings included. Everybody else (`role = 'user'`) has, on each environment,
   one of Environment administrator, Operator, Helpdesk, Standard user and Read-only user, given to them or to a team
   they are in, on the environment or on its group; or none, and then that environment does not exist for them (6).
   Users who were not administrators before roles could read everything and change nothing, which is Helpdesk:
   migration 11 gave them Helpdesk on every environment there was, so nobody lost what they had; somebody made since
   (an SSO user too) has no role anywhere until given one.
2. What each role may do is one table in code (`access::TABLE`), of role against action, and nothing else decides it.
   An action is what a route does in an environment: read (lists, inspect, logs, stats, events), operate (start, stop,
   restart, kill, pause, unpause a container; stop and start a stack), console (a console, an attach, a command run),
   create (a container, a stack, a volume, a network; pull, build or load an image; deploy a template), manage (edit,
   recreate, rename or remove a container or stack, redeploy, its webhook, its ownership; connect it to a network) and
   administer (prune, remove or tag or push images, remove volumes and networks, every Swarm change, adopt a project
   started elsewhere). "Own" is "only on the containers and stacks the person can see" (4); what is not owned (an
   image, a volume) is allowed.

   | | read | operate | console | create | manage | administer |
   |---|---|---|---|---|---|---|
   | Environment administrator | all | all | all | yes | all | yes |
   | Operator | all | all | all | no | no | no |
   | Helpdesk | all | no | no | no | no | no |
   | Standard user | own | own | own | yes | own | no |
   | Read-only user | own | no | no | no | no | no |

   A console is root in the container, and a create can bind the host's root and run privileged: an Operator or a
   Standard user is trusted that far until per-environment security settings (M4, not yet) can say no.
   **Superseded in part** by 12: they now can.
3. A person's role on an environment is the first of these that has any grant, and the highest within it
   (environment administrator, operator, helpdesk, standard, read-only, in that order): their own grant on the
   environment; their own grant on its group; their teams' grants on the environment; their teams' grants on its group.
   So a grant on the environment wins over the group's, a person's own wins over a team's (even a team's on the
   environment over their own on the group: a grant to one person is a deliberate exception), and of several teams the
   highest applies. An environment is in one group at most ("Environments" 5), so there are never two groups to weigh.
   Grants hang on ids, so renaming a group, a team or a person moves nothing.
4. A container or a stack is public, a team's or one person's (private), or, with no record, the administrators'
   (`resource_access`, by environment and a container's name or a stack's id; Freightdock's side, since Docker cannot
   change a running container's labels). What is made through Freightdock is private to its maker unless they say
   otherwise, and a container made through Freightdock also carries a `freightdock.owner` label with its maker's name for
   whoever looks with Docker's own tools. A container without a record of its own belongs as its Compose project's
   Freightdock stack does. What was made on the command line, or before roles, is the administrators' until an
   administrator shares it. Standard and Read-only users see (in lists, events, stats, webhooks, MCP) and reach only
   what is public, their teams' and their own; a route about anything else answers 404 for them, as if it were not
   there; externals (projects Freightdock did not start) are the administrators'. Everybody else sees all of the
   environment. A Standard user may share what they manage with the public or a team they are in, or keep it to
   themselves, never give it away to somebody else or to the administrators; an Administrator or Environment
   administrator may do any of it. The record follows a container's rename and edit, and goes when it is removed (a
   stack's when the stack goes, a team's shares when the team goes).
5. Every route is held to its rule by one guard on the whole router (`access::routes::guard`), from `RULES`: public,
   signed in, an Administrator, or an environment's action on the resource the route names (a container, a stack, an
   external project). A route with no rule is refused (403) and logged, so one added without a rule fails closed, and
   a test reads every `.route(` in the source and fails for any without a rule, and for any rule without a route. The
   guard signs the person in once and leaves who they are and their access on the request; handlers then only leave
   out of lists what the person may not see, and check the container a body names (a network's connect, an edit's
   `replace`). `/mcp` is public to the guard because it signs in itself and calls back through the same router.
6. An environment a person has no role on is not shown to them: `/api/environments` (and Home, and MCP's
   `list_environments`) leave it out, groups and tags list only the environments they see, and its routes answer 404.
   Each environment listed says the person's role there and the actions it allows, so the UI can hide what they
   cannot do; the server is the guard all the same ("Access" 5). **Supersedes "Environments" 1 in part** (everybody
   signed in saw every environment).
7. An API token acts as its owner, with their role on each environment as it is at each use ("Sign-in" 10), through
   the API and MCP alike. A read-only token is held to the read action whatever the role: a console opens with a GET,
   but is the console action, not a read.
8. Only an Administrator manages people (from 1.0.0, M4): makes a local user with a password, sets the role
   (administrator or not), renames, disables and enables, gives a new password, removes. Disabling and a new password
   end the person's sessions at once. The last administrator who is not disabled cannot be demoted, disabled or removed,
   and nobody can demote, disable or remove themselves: losing the way in is done by another administrator, on
   purpose. An SSO user has no password to reset ("SSO" 4). Anybody changes their own password in Account, with the
   one it replaces; their other sessions end. Teams are made, renamed and removed by an Administrator with their
   members; a person sees the names of the teams they are in, to share with. **Superseded in part by 22**: a new
   password, one's own or one given, revokes the person's API tokens too.
9. Containers and stacks are owned; volumes, networks and images are not (as in Portainer for Docker). Standard and
   Read-only users therefore see every volume, network and image of an environment they have a role on, and with them
   the names of the containers using each.
   **Superseded** by 18: they are owned too.
10. Routes that came with M5 are held to the table too: a container's history is a read of it; updating a container
   now and switching its automatic updates are managing it; the update badges are a read, Standard and Read-only users
   getting only those of containers they can see; the update, history, backup, notification and Portainer import
   settings and runs are an Administrator's. A volume's files (listing and downloading too, not only changing them)
   are the administer action, an Environment administrator's: a volume holds whatever its containers keep, and
   volumes are not owned (9), so reading one is reading everybody's data on that host.
11. A stack's Environment ("Stacks" 11) is a read of the stack, held to the stack's rule like its files: whoever may
   read the stack sees every value in it, secrets too, and a stack a Standard or Read-only user cannot see (4) is not
   there for them (a 404). It sends what its compose file and `.env` already hold, so no role reads it that could not
   read those.

12. Security settings per environment (from 1.0.0, M4), as Portainer's: switches that refuse everybody below
   Environment administrator on that host privileged mode, bind mounts of host paths (but those under an allow-list of
   host path prefixes), the host's PID, IPC, network and UTS namespaces, devices, extra capabilities (`cap_add`),
   security options (`security_opt`) and sysctls, and, by a switch of its own, consoles (17). They are set on an
   environment or on its group (`security_rules`); an environment's own, when it has any, hold whole over its group's,
   as a grant on the environment wins over the group's (3), so what holds on a host is always one set read in one
   place. With none, everything is allowed, as it was before there were settings, so nobody's stacks stop deploying on
   upgrade; "Locked down" is the recommended preset a click away (everything that reaches the host refused, consoles
   left open: they are an Operator's work). Administrators and Environment administrators are never held to them: the
   people who may set up the host may also make what reaches it. Only an Administrator sets them, and each change is
   in the activity log (`environment.security`, `group.security`). **Supersedes in part** the last paragraph of 2 (an
   Operator's console and a Standard user's create reach only as far as the settings let them).
13. Every way a container comes to be is held to the settings, on the server, as the person it is done for: a
   container made or edited (`POST containers`, so a container template and MCP's `create_container` too), a volume
   that is a host path underneath (a local volume with `o=bind`), a Swarm service made or changed, a stack deployed
   and a Swarm stack deployed. A stack is judged on the file Compose resolves (`docker compose config --format json`,
   with its `.env` and its `${...}` filled in), service by service, not on the text: Compose's own reading is the one
   that runs. A stack is checked when it is deployed, not when its file is saved: by hand (an editor stack before its
   job starts, so the refusal comes back on the file's field; a Git stack's after its fetch, in the job), by a webhook
   as the person who made the webhook, by the Git poller as the stack's maker, and a name no longer there (or
   disabled) is held to every rule. An API token and MCP act as their owner (7) through the same routes, so there is
   no second path to hold. Swarm changes are already an Environment administrator's (2), and are held all the same,
   so a change to the table cannot open them. An import from Portainer deploys as the Administrator who ran it.
14. An edit is held only to what it adds to the container it replaces, and a Swarm service's change to what it adds
   to the spec it replaces: what an administrator set stays when a Standard user who manages the container edits it
   (they could already reach into it). Recreating a container, updating it now, its automatic updates and a
   container's webhook make it again from its own settings, and are not judged at all.
15. What counts: a bind of an absolute host path in `Binds` or a `bind` mount (a relative source in `Binds` is a named
   volume), a volume whose driver options bind a host path (in a mount's options, a compose file's top-level volume,
   a volume made through Freightdock), and for a Swarm service a `bind` mount. A host path is compared by whole steps
   after `.` and empty steps go, so `/srv/apps` allows `/srv/apps/shop` and not `/srv/appsecret`, and a path with `..`
   in it is never allowed (it could climb out of any prefix); `/` in the allow-list allows every path. A host path at
   or under `/dev` is a device as well as a host path, refused when devices are. Devices are `Devices`,
   `DeviceRequests` (GPUs), `DeviceCgroupRules`, a compose file's `devices`, `gpus` and device reservations, and a
   service's generic resources; a namespace is refused only when it is the host's (`host`); a service's SELinux
   context and a seccomp or AppArmor mode other than the default are its security options.
16. A refusal by the settings is a 403 that says every setting the request breaks, each as "Service web: The host's
   network (network: host) is turned off for non-administrators on this host.", on the field of the first
   (`privileged`, `binds`, `network_mode`, `pid_mode`, `ipc_mode`, `uts_mode`, `devices`, `cap_add`, `security_opt`,
   `sysctls`; `compose` for a stack's file, `driver_opts` for a volume). The container form shows it under the field
   it is about where the form has one (volumes, network, privileged), the stack page on the compose file; anything
   else under the form.
17. "Consoles" off refuses the console action (a console, an attach, a command run, MCP's `exec_command`) to everybody
   below Environment administrator on that host, in the guard, before any handler; `GET /api/environments` then
   leaves `console` out of their `actions`, so the UI does not offer one. It is on by default, as consoles were
   before.

18. Volumes, networks and images are owned as containers are (from 1.0.0, M4; **supersedes 9**): public, a team's,
   private, or with no record the administrators', in `resource_access` (a volume and a network by name, an image by
   its id), changed on its page by whoever may manage it (`PUT .../volumes/{name}/access`, `networks/{id}/access`,
   `images/{id}/access`). A Standard or Read-only user sees one when its ownership lets them, or when a container they
   can see uses it (mounts the volume, is on the network, runs the image): what a container they may read shows in its
   inspect anyway, and a container they made must not lose sight of the volume Docker made for it. A volume or network
   with no record of its own belongs as its Compose project's stack does, and Docker's own networks (bridge, host,
   none, ingress, docker_gwbridge) are everybody's. Anything else is not there for them: left out of the lists, MCP's
   too, and of the events, and a route about it answers 404. Where a list or an inspect names the containers using a
   volume or on a network, it names only those they can see, so a hidden container's name never comes out through
   what it shares. A volume or network made through Freightdock is private to its maker unless the form says otherwise;
   an image belongs to whoever pulled or built it, private to them, unless it already has an owner they can see (a
   second person's pull takes it over only when they could not see it, so the image they fetched is there for them).
   The record goes when the thing is removed or pruned, so a later one of the same name is not given it. An image
   loaded from an archive is not claimed (Docker does not say which it was until it is read), nor one a container's
   create pulled (the container that runs it shows it). A volume's files stay an Environment administrator's (10),
   owned or not: they are whatever its containers keep. **Supersedes in part** 2's "what is not owned (an image, a
   volume) is allowed": a route about one is held to its owner as a container's is.
19. Exporting an image (`GET .../images/{id}/export`, `docker save`) is the administer action, an Environment
   administrator's, like a volume's files (10) (owner's decision, 2026-11-10). An export is every file of every layer:
   the application's code, what a build copied in from its context, credentials baked into a layer, an image pulled
   with an administrator's saved registry sign-in. Seeing an image's metadata is a read; taking its contents away is
   not, and a read-only token never does it.
20. A console, an attach or a command run in the container Freightdock itself runs in is a Freightdock Administrator's
   alone, whatever anybody's role on that host (owner's decision, 2026-11-10). That container holds the data folder
   (`secret.key`, the database, the agents' CA) and, as the image is run, the Docker socket, so a console there is
   every environment, not one: an Operator's or an Environment administrator's console reaches no further than their
   host. The container is found at start (`docker::own::detect`: the ids its mounts and control groups name, the first
   the local Docker knows), only ever on the local environment, and the guard refuses the console action on it, by
   name or id, with a 403, after the 404 a person who cannot see it gets. Not running in a container, nothing is
   found and nothing changes.
21. A container's webhook and its automatic-update switch are kept by its name, as its ownership record is (4), and
   live as it does (from 1.0.0, fixed 2026-11-10): `containers::forget` drops all three when the container is removed
   or pruned, and `containers::renamed` moves all three when it is renamed or edited under a new name (what the new
   name had goes). Otherwise the next container to take the name, perhaps somebody else's, would be recreated by an
   address handed out for the old one ("Sign-in" 8: a webhook redeploys only what it was made for) and updated by a
   switch nobody set for it ("Updates" 3). A container removed outside Freightdock leaves them, as it leaves its record.
22. A new password revokes the person's API tokens, whether they change it themselves or an administrator gives one
   (from 1.0.0, audit 2026-11-10; **supersedes 8 in part**, which ended only sessions). A token is made with a session
   and no password, so whoever once held the account could leave a token behind that outlived the recovery; a new
   password is how an account is taken back, and it must take everything made under the old one. The activity log says
   how many went. Disabling still only stops tokens while the account is disabled ("Sign-in" 10): it is undone on
   purpose, and the person's tokens come back with it.
23. The cluster UI has two roles (G8.3, owner's decision, 2026-11-12): an Administrator changes the cluster and
    Freightdock's own settings, users and teams; anybody else signed in reads the cluster. Every route's rule is one of
    Public, Session and Admin (`access::routes::RULES`). Finer roles (per stack, per node) come back with what needs
    them, and are not guessed at before: a role table for features that are not there would be rules that hold nothing.


## HTTPS

1. HTTPS by default, with a self-signed certificate made on first start and kept: the session cookie must never cross a
   network in the clear. `FREIGHTDOCK_TLS=off` is for a proxy that does TLS in front. **Superseded in part by 3**: the
   certificate is now signed by a CA of Freightdock's own, not by itself.
2. A request head and a TLS handshake must arrive within 15 s; at most 1024 connections at once. hyper is driven directly
   because `axum::serve` sets no timer. **Superseded in part by 6**: a body that stops coming is bounded too.
3. Without a certificate of one's own, Freightdock's certificate comes from a small CA of its own in `<data>/tls/`
   (`ca.pem`, `ca.key` 0600), made on first start and kept (from 1.0.0). A self-signed certificate cannot be trusted
   once and for good: tools that check names (Node, and so Claude Code; curl; browsers) refuse one whose subject
   alternative names do not hold the name dialled, whatever they are told to trust, and the first certificate had none.
   A person trusts the CA once (`GET /api/tls/ca.pem`, public: it is a certificate, and it is fetched before anything
   is trusted) and every certificate it signs from then on checks out. It is a CA of its own, not the agents' ("Agent"
   3): a person who trusts this one in a browser should not thereby vouch for the agent endpoint, nor the other way.
   Trusting it means trusting whatever this Freightdock's key signs, for any name; the key never leaves the data folder.
4. The certificate names `localhost`, `127.0.0.1`, `::1`, the machine's host name (its common name too), the address
   Freightdock listens on, or every interface's address but loopback and link-local when it listens on all of them
   (`0.0.0.0` the IPv4 ones, `[::]` both), and whatever `FREIGHTDOCK_TLS_NAMES` adds (comma-separated names and
   addresses: the name a proxy, a DNS record or the Docker host gives it, which Freightdock inside a container cannot
   know). It is for server authentication only and good for 825 days, the most Apple's platforms take from a CA a
   person added. It is made again at a start when its names no longer cover these (a new address, a new name; extra
   names are no reason), when it has less than 30 days left, or when its key is gone. It is checked at start only: a
   Freightdock that runs for more than its last 30 days without a restart serves an expired certificate.
5. A certificate named by `FREIGHTDOCK_TLS_CERT` or `FREIGHTDOCK_TLS_KEY` is the person's own and is never made, renewed or
   replaced; when it is not there, Freightdock says so and does not start (it used to make a self-signed one at that path).
   A certificate at the default path that the CA did not sign is left alone too, with a warning, as somebody may have
   put their own there; the one exception is the self-signed certificate Freightdock made before it had a CA (rcgen's
   default name, "rcgen self signed cert"), which is replaced at the first start (owner's decision, 2026-11-10: it is
   self-made, and nobody could trust it as it was).
6. A request's body may stop coming for 15 s and no longer (from 1.0.0, audit 2026-11-10; **supersedes 2 in part**,
   which bounded only the head and the handshake): a client that sent a head with a length and then nothing held its
   connection, and one of the 1024, for ever, so 1024 idle sockets took the UI, the API and the agent endpoint off the
   air without signing in. Only the time spent waiting on the client counts, and every piece that comes starts the 15 s
   again, so an upload of any size that keeps coming is never cut, and a handler that is slow to read is not either.
   A body trickled in a byte at a time still holds its slot; a proxy in front, or a cap per address, is for that.

## UI

1. The UI is FinUI and FinMotion, vanilla ES modules with no build step, no npm and no CDN; the look is Washi, FinUI as
   it ships (owner's decision, 2026-11-09), unless an administrator picks a preset (8).
2. Data reaches the DOM only as text, through FinUI's `h()`. Container names, labels, logs and environment variables are
   written by whoever made the container, not by Freightdock.
3. Every colour is a FinUI token, so a preset restyles everything and both themes read.
4. A terminal and a code editor are not FinUI's; they are vendored, pre-built xterm.js and CodeMirror 6 in
   `web/assets/vendor/` with their licences, each wrapped in a component that follows FinUI's contract (owner's
   decision, 2026-11-09).
5. A list that refreshes keeps what was typed in its filter: the filter lives outside what is redrawn, and a filter
   draws the last answer again without asking Docker.
6. A list does not wait for what each container uses: sampling takes Docker a second or two, so the list is drawn at
   once and the numbers when they come.
7. No file under `web/` is named the way ad and tracker blockers block: EasyPrivacy has `/log-view.js` word for word,
   uBlock Origin then refuses that module, and since `main.js` imports every page the whole app stops at its spinner.
   The output component is `output-view` for that reason; check a new file's name against the common lists.
8. The look is a FinUI preset, one for the whole instance, picked by an administrator in Settings and kept in `settings`
   (from 1.0.0). Not per person: the stylesheet is the same file for everybody, the sign-in page included, so it can be
   served before anyone signs in, cached by its ETag, and drawn from the first paint with no flash. The overlay is
   FinUI's own (`p/<axis>/<option>.css` from FinUI's `preset.js`, checked by `qa/js-check.sh`), not a second
   implementation of FinUI's presets in Rust, and a preset is only ever a code: Freightdock never takes CSS from the
   browser.
9. The code editor is CodeMirror 6 bundled once by esbuild in a throwaway `node` container into one ES module,
   `web/assets/vendor/codemirror/codemirror.js`, with every bundled package's licence and, in `VERSION`, the exact
   versions and the command that makes the same file again byte for byte. The repository keeps no npm project and the
   app no build step ("UI" 1); a newer CodeMirror is that command with the versions changed. `fd-code-editor` is a
   plain text box until the module has loaded, so it can be typed into at once and still works if it cannot load, and
   its highlighting gives classes, not colours, so its CSS colours them by token ("UI" 3).
10. Home is the landing page, and after sign-in no host is chosen (from 1.0.0): with many hosts, the first thing is
   which of them needs looking at, and a page about one host should never quietly be about the last one somebody used.
   A page about one host waits on Home until one is chosen; a row on Home chooses it (only Home, from "UI" 14).
   **Superseded** by "Cluster" 10: there are no hosts to choose; the cluster page is the landing page.
11. Home never waits on a host ("UI" 6 for hosts): it draws at once from what Freightdock keeps, asks every host on its own,
   and fills each row as its answer comes. A host that is down or slow shows its last known numbers, greyed, with when
   they are from ("Environments" 8), rather than a spinner or nothing.
   **Superseded** by "Cluster" 10: there are no hosts to choose; the cluster page is the landing page.
12. The menu lists under the chosen host's name only what is about that host (its dashboard, containers, stacks,
   templates, images, volumes, networks, events); what is Freightdock's own, the same whichever host is chosen, sits under
   Freightdock, and is there while no host is chosen: environments, registries ("Registries" 1: saved once for every
   host), the activity log, settings. The registry browser offers to pull a tag only while a host is chosen, onto that
   host; with none it only browses.
   **Superseded** by "Cluster" 10: there are no hosts to choose; the cluster page is the landing page.
13. The Swarm pages are in the menu only while the host shown is a swarm manager ("Swarm" 1): on any other host they
   would only say there is nothing there. The menu is made again when that changes, from the answer of `GET .../swarm`,
   asked at most every 30 s per host; a Swarm page reached anyway says what the host is instead. A service's or a
   stack's page goes back to its list when the host changes, as a container's does ("Environments" 4).
   **Superseded** by "Cluster" 10: there are no hosts to choose; the cluster page is the landing page.
14. The host is changed on Home and nowhere else (owner's decision, 2026-11-09): there is no switcher above the pages.
   Changing hosts is a step back to the overview, where every host's health is in front of you, not a list that can
   quietly put the page you are on onto another machine. The menu names the host shown ("Host: <name>"), and Home is
   always the first entry.
   **Superseded** by "Cluster" 10: there are no hosts to choose; the cluster page is the landing page.
15. Appearance in Settings is FinStats' (from 1.0.0): one row that takes a FinUI preset code (or the FinUI create
   address or install line holding one), what the code picks listed under it, and a link to FinUI create to make or
   change one. Freightdock offers no looks of its own to pick from: FinUI create is where a look is chosen, so there is
   one place to learn and nothing here goes stale when FinUI adds a style.
16. Every answer says who may frame it and what the page may run (from 1.0.0, audit 2026-11-10): a
   Content-Security-Policy with `frame-ancestors 'none'` and scripts from Freightdock alone (no inline script, no eval;
   theme.js, the modules and the vendored xterm.js and CodeMirror are all files here), `X-Frame-Options: DENY` for older
   browsers, `nosniff`, `Referrer-Policy: same-origin`, and `Cache-Control: no-store` on an API answer that names no
   caching of its own (the avatars do). A same-site page (a sibling subdomain, another port of the same host) gets the
   SameSite=Lax cookie in a frame, and every click inside a frame carries the CSRF token the page holds, so only
   refusing the frame stops a click made there on someone's behalf. Styles may be inline, as xterm.js sets `style`
   attributes and CodeMirror adds a sheet of its own; images may come from anywhere, as a template's logo does.
   Nothing is put on the agent endpoint, which no browser opens.
17. Every word the UI shows comes from a catalogue, the way FinStats does it (from 1.0.0): `web/assets/i18n/<tag>.json`,
   English (`en`) the source and Norwegian Bokmål (`nb`) beside it, both whole, read through `t(key, vars)` in
   `web/assets/js/i18n.js` (FinStats' `t`, `tOr`, `tParts`, `lang`). A key says where it is used (`pages.volumes.empty`,
   `common.save`); a sentence is one key with named `{placeholders}`, never glued from translated pieces, because
   Norwegian orders words otherwise; a word beside a number is a plural (`one`/`other`, picked by `Intl.PluralRules`),
   never `n === 1 ? a : b`; ids from Docker or the server (a container's state, a task's, an event's kind) are said by
   id with `tOr`, their own word where the catalogue has none. Numbers, sizes, percentages, dates and times go through
   `Intl` in the language in use (`dom.js`: `num`, `pct`, `bytes`, `ago`, `when`, `clock`, `day`, `span`), and
   `localeCompare` is given `lang()`. FinUI's own components speak through the catalogue's `finui` section (FinUI's
   `words()`), and Freightdock's `fd-` components through `t()`. The language is the browser's (Account, Language, kept as
   `freightdock.language` like the theme; nothing chosen follows `navigator.languages`, Norwegian of any kind being Bokmål,
   anything else English), not the account's: FinStats keeps it per browser too, and a phone and a desk may want
   different ones. Choosing reloads the page. `main.js` loads the catalogue before it imports `app.js`, so a module may
   say `t()` at its top level, and `theme.js` fetches it while the page loads and sets `<html lang>`. What the server
   answers in words (an error's reason) stays English, and so do the patch notes. Nothing may name a variable `t`.
   `web.rs` holds the catalogues to each other (the same keys and placeholders, plural forms, no em-dash), and
   `qa/i18n-check.mjs` (in `qa/js-check.sh`) holds the modules to them: every key asked for has English, every English
   text is asked for, and a string that looks like words outside `t()` fails unless `ON_PURPOSE` says why.

## Endpoints

**Set aside** (G8.3, 2026-11-12, "Cluster" 10): the feature these items are about is not in the cluster UI. They are kept, numbered, for when it comes back through the managers; until then they describe nothing in this repository.

1. Every environment is one Docker endpoint that bollard and the `docker compose` CLI reach alike; an agent's tunnel is
   offered as a local unix socket, so there is no second code path for remote hosts.
2. Stacks are deployed with Docker's own `docker compose`, not a reimplementation of the Compose spec, so a stack behaves
   exactly as it would on the command line.
3. A remote environment's socket (`<data>/run/env-<id>.sock`, the folder 0700, the socket 0600) is open only while its
   host can be reached: an agent's while its tunnel is up, a TLS host's from its first use until it is edited or
   removed. Anyone who can open one is root on that host, so only Freightdock's own user may. A data folder too deep for a
   socket's address (107 bytes) puts the sockets in a short folder of the system's temporary folder instead.
   **Superseded in part** by 9: that folder is a new one of the run's own, not one named after the data folder.
4. bollard settles on the API version each Docker speaks (`negotiate_version`) when it is older than bollard's own
   (1.49, Docker 28.1): for the local Docker at start, an agent's when its tunnel comes up, a TLS host's when its socket
   is made. An older daemon refuses a request in a version newer than its own.
5. Compose reaches a remote environment as bollard does, through its socket: `DOCKER_HOST=unix://<socket>` and no
   `DOCKER_TLS_VERIFY` or `DOCKER_CERT_PATH` (from 1.0.0, M2 on M3). The socket does the TLS with the key opened in
   memory, so a TLS host's key is never written to disk for Compose, and an agent's host needs nothing opened for it.
6. An environment's Docker is not trusted to end what it sends: an agent's host is root for whoever may make
   containers there, and its answer reaches the one server that manages every environment. A whole answer read by
   `docker::raw` is held to 64 MiB (`MAX_ANSWER`, far more than a swarm of thousands of tasks lists) and a refusal to
   64 KiB; a log frame to 1 MiB (`MAX_FRAME`; Docker cuts a log line at 16 KiB), refused at its header before its
   bytes are kept. What the background passes ask every environment (the summary's info, containers and volumes, the
   containers the stats history samples) is read through `raw::get`, held the same, not through bollard, which keeps
   a whole answer of any size.
7. Docker's JSON-lines streams (events, for the events page and the notifications' watch, and stats, live and
   sampled) are read by `docker::raw::follow`, not by bollard: its decoder keeps an unfinished line however long it
   gets and parses all of it again at every read, so a remote Docker sending one line that never ends costs the
   server memory without limit and time growing with its square. `raw::Lines` looks at each byte once, parses a line
   once it is whole, and ends the stream with an error once the open line passes 1 MiB (`MAX_LINE`; one of Docker's
   is a few KiB).
8. What let a remote environment's socket be made is read again once it is offered, and the socket goes when that has
   changed (from 1.0.0, after the 2026-11-10 audit): for an agent, its certificate is still the environment's ("Agent"
   9); for a TLS host, its row (kind, address, CA, certificate, key) is as it was read. Every revocation, edit and
   removal changes the database first and closes the socket after, so whichever comes second sees the other: no
   socket outlives the settings it was made from, nor serves an id given to another environment since.
9. The short folder for a data folder too deep for a socket's address is made new at each start (from 1.0.0, after
   the 2026-11-10 audit): a random name in the system's temporary folder, made 0700 by one `mkdir` that fails when
   anything is there already, a folder or a link, and then another name is tried. Before, its name came from the data
   folder's path alone and whatever was found there was taken: another user of the machine could make it first, keep
   it as theirs and put a listener of their own in place of an environment's socket, reading what Freightdock sends a
   host (registry passwords among it) and answering for it.

## Environments

**Set aside** (G8.3, 2026-11-12, "Cluster" 10): the feature these items are about is not in the cluster UI. They are kept, numbered, for when it comes back through the managers; until then they describe nothing in this repository.

1. An environment is `local` (made at setup, from `FREIGHTDOCK_DOCKER_HOST`; it cannot be removed), `agent` or `tls`. Only
   an administrator adds, edits or removes one, with the CSRF token, and each is in the activity log
   (`environment.create`, `.update`, `.remove`, `.join`); everybody signed in sees the list, to switch between hosts
   (**superseded in part** by "Access" 6: only those they have a role on).
2. Every environment is asked for its Docker version every 30 seconds: `status` up or down, `last_error`, the version,
   and `last_seen_at` the last time it answered (for an agent, also when its tunnel ended). The routes under
   `/api/envs/{env}` never care which kind it is: `docker::endpoint` answers a client for any, or 503 while an agent is
   not connected.
3. A TLS host is Docker's own TLS port (`dockerd --tlsverify`, `tcp://host:2376`) with its CA, a client certificate and
   key pasted in. All three are checked when saved, and the key is sealed ("Secrets" 1); it is opened in memory when
   the host's socket is made, never written to disk in the clear.
4. **Superseded in part** by "UI" 14: the switcher above every page is gone. The host the pages show is chosen per browser (`freightdock.env`), on Home; a container or image
   page goes back to its list when the host changes, as that container is not on the new one. (None is chosen after
   sign-in, from 1.0.0: "UI" 10.)
5. An environment is in at most one group and has any number of tags (from 1.0.0). Groups are what roles will be given
   on in M4, beside single environments: a person's role on an environment is the grant on it, else the grant on its
   group, and with one group at most there are never two group grants to choose between. Tags give no access; they are
   for finding hosts and, in M5, for deploying to many at once. Only an administrator changes either, with the CSRF
   token, each in the activity log (`environment-group.create`, `.update`, `.remove`; `tag.create`, `.update`,
   `.remove`; a group or tags given to an environment in `environment.update`'s detail). Removing a group or a tag
   leaves its environments where they are, in no group or without that tag.
6. Tags are names, 1 to 40 characters without a comma (lists of them are typed with commas), unique without regard to
   case; an environment is given its whole list by name and a name not yet a tag becomes one. Access will hang on a
   group's id, never its name, so a rename moves nothing.
7. An agent's version is what it says in its user agent as its tunnel opens, kept with the address it dialled from (so
   a host that is gone still says where it was). It is outdated when it is older than Freightdock, number by number; a
   version that cannot be read is not called old.
8. Every host that is up also gives its summary at each health check, and the last one is kept (`snapshot`, with when):
   a host that is down still shows what it had and since when, and the home page draws from it before any host
   answers. A summary that answers marks the host up, as its version would.

## Agent

**Set aside** (G8.3, 2026-11-12, "Cluster" 10): the feature these items are about is not in the cluster UI. They are kept, numbered, for when it comes back through the managers; until then they describe nothing in this repository.

1. A host Freightdock cannot reach runs `freightdock-agent`, which dials out: nothing on that host is opened to the network,
   and it works from behind NAT. One WebSocket carries yamux, and each yamux stream is one connection to that host's
   Docker socket, so bollard (and `docker compose`) see a local socket ("Endpoints" 1).
2. The agent endpoint is a listener of its own (`FREIGHTDOCK_AGENT_BIND`, `0.0.0.0:7444`; `off` to turn it off) with TLS
   from Freightdock's own CA, whatever `FREIGHTDOCK_TLS` says. A TLS-terminating proxy in front of the web UI cannot carry a
   client certificate, so the agent endpoint never goes through one: it is reached directly, or through a proxy that
   passes TCP through untouched. Where agents must dial something other than this server's name and port 7444 (NAT,
   a TCP proxy), `FREIGHTDOCK_AGENT_ADDRESS` says what, for the `docker run` line.
3. The CA (`<data>/ca/`, key 0600) is made on first start and kept. It signs the endpoint's server certificate (made
   again at each start, for the fixed name `agents.freightdock.internal`, which the agent checks whatever address it
   dialled) and each agent's client certificate, which is for client authentication only, so an agent can never pass
   for the endpoint to another agent.
4. Enrolment: an administrator adds an agent environment and gets a join token, `FDJT1-<CA fingerprint>-<secret>`,
   good once for 24 hours; Freightdock keeps only the secret's SHA-256. The agent makes its own key, trusts the endpoint
   only if its chain holds the CA the token pins (so the secret goes to no other server), and sends the token with a
   certificate request. Freightdock signs it with its own name, use and dates (only the key is taken from the request),
   and puts the certificate's SHA-256 in the token's place in one statement, so a token is spent once even when two
   agents race. The agent keeps key, certificate and CA in its volume and dials with them from then on.
5. The tunnel is opened only with a client certificate from the CA whose fingerprint is an agent environment's.
   Removing the environment or making it a new join token forgets the fingerprint and cuts the tunnel at once; the old
   certificate opens nothing more. An agent refused this way enrols again if it was given a token it has not spent,
   and otherwise keeps trying, logging why.
6. The agent dials again after any break, waiting from one second, doubling to a minute, a quarter either way at
   random, so agents do not all come back at once after Freightdock restarts. Both ends set TCP keepalive (30 s, then
   every 10 s, 3 tries), so a host that vanished is noticed within about a minute.
7. A join token in a `docker run` line stays in that container's settings; it is spent at enrolment and good for
   nothing after, which is why a token is good once.
8. A tunnel carries at most 384 connections at once (`mux::MAX_STREAMS`, from 1.0.0, after the 2026-11-10 audit), and
   one past that fails alone: its request answers an error, and every other connection and the tunnel carry on. yamux
   0.13 ends the whole connection at an open past its own limit, and at a stream from the other side past it, so the
   count is kept in `mux` before yamux is asked, and yamux's own limit is set to 4 096 at both ends, far above it,
   leaving room for the streams yamux has not yet let go and for an agent of an older Freightdock (whose limit was 512).
   Before, the 513th connection (a user with Read holding 513 log views is enough) cut the environment off for
   everybody until the agent dialled again.
9. A tunnel is offered as the environment's socket only while the certificate it was let in with is still that
   environment's, asked again once it is offered (from 1.0.0, after the 2026-11-10 audit; "Endpoints" 8). Before, a new
   join token or a removal that came between the tunnel's check and its offer closed nothing (there was nothing yet),
   and the revoked certificate's tunnel was installed after it: it served the environment, or, after a removal, the
   next environment given the same id.
10. The agent is a repository of its own, `freightdock-agent`, beside Freightdock's (owner's decision, 2026-11-10): it is
   what people install on every host, and is released and read on its own. It owns the tunnel protocol
   (`freightdock-tunnel`), which the server takes with the agent (for its end-to-end test of a real agent) as a git
   dependency at a commit (`rev`): a protocol change is made and tested there, then the server moves to its commit.
   Until a release tags both, a commit rather than a tag, since a tag is a release (the agent's CI publishes it).
   Locally, in CI and in the image's build, `.cargo/config.toml` patches both to a checkout of that commit beside
   Freightdock's (`../freightdock-agent`, `qa/agent-checkout.sh`, which reads the address and commit from
   `crates/server/Cargo.toml`), and Cargo.lock records the patched crates, so `--locked` holds everywhere the patch is.
   The agent's version moves in step with Freightdock's, since Freightdock calls an agent older than itself outdated (7);
   its image is the agent repository's to publish, at the same tags. **Superseded** by 11.
11. Freightdock and the agent are independent products (from 1.0.0, owner's decision; **supersedes 10**): the server
   takes the Agent repository's crates as ordinary git dependencies at its release tag (`tag = "v1.0.0"`), the
   protocol for the build and the agent for the in-process end-to-end test, and Cargo fetches them as it fetches any
   crate, recorded in Cargo.lock by their commit. CI and the image's build need no checkout beside this one, no patch
   and no token for the other repository; each repository's CI builds, tests and releases only itself. A protocol
   change is released in the Agent repository first (a tag), then the server moves to that tag. The example Cargo
   settings (`.cargo/config.toml.example`) patch both to a checkout beside this one, for working on both at once;
   Cargo.lock is not committed while the patch is in place. Path overrides, which would leave Cargo.lock alone, were
   tried and refused: Cargo warns they break when an overridden crate's own dependencies differ, as the agent's do.
   The image names the agent's image at its own tag (`ghcr.io/<owner>/agent`), so the two are released together.
12. A git dependency that is private on GitHub is fetched with the optional secret `GIT_DEPS_TOKEN` (from 1.0.0;
   **supersedes 11 in part**, which had no token): a read-only token, set on the repository while the Agent repository
   is private and removed once it is public. CI points git at github.com with it and lets Cargo fetch through git; the
   image's build takes it as a BuildKit secret, so it is in no layer, no build argument and no log. Without it nothing
   changes, so a fork or a public Agent repository needs nothing.

## Stacks

**Set aside** (G8.3, 2026-11-12, "Cluster" 10): the feature these items are about is not in the cluster UI. They are kept, numbered, for when it comes back through the managers; until then they describe nothing in this repository.

1. A stack's files live in the data folder, `<data>/stacks/<id>/`, not in the database: they are what Compose reads,
   and an administrator can look at them, back them up and diff them with the tools they already know. The database
   keeps only what the files do not say.
2. A compose job runs to its end even when the page that started it is closed, unlike a pull ("Streams" 1): a deploy
   stopped halfway leaves a stack half old and half new. A stack runs one job at a time (a lock), so a deploy, a
   webhook and the Git poller never cross.
3. Compose runs with an emptied environment: PATH, HOME, TZ, TMPDIR, DOCKER_CONFIG, and what reaches the environment's
   Docker, decided in one function (`compose::docker_env`, its address from `docker::cli::docker_host`, "Swarm" 5).
   Freightdock's own variables must never fill a `${...}` in somebody's compose file.
4. A compose file Compose refuses (`config --quiet`) is not kept: a new stack is not made, and an edit leaves the file as
   it was. Compose's own sentence is the error.
5. Stop, start and remove go by the project's name alone, not its file, so a stack whose file is broken can still be
   stopped and taken down.
6. A Compose project Freightdock did not start is found by its `com.docker.compose.project` label and shown as external.
   Adopting it reads its files where it was started and has Compose merge them into one with relative paths made
   absolute (so it keeps mounting what it mounted) and variables left as `${...}` (so no secret is written into the
   file); the `.env` comes along. When Freightdock cannot read them (Portainer keeps its stacks in its own volume), the
   compose file is pasted.
   **Superseded in part** by 16: its files are read only on the local Docker, for a Freightdock administrator.
7. Relative paths in a stack written in Freightdock are read from its folder in the data folder. When Freightdock runs in a
   container, Docker on the host reads the same path on the host, so bind mounts want absolute paths, or the data
   folder mounted at the same path on both sides (as with Portainer).
8. A Git stack is checked by asking its remote what its branch points at (`git ls-remote`), not by fetching: polling
   every few minutes costs one small request. It is deployed again only when that commit is not the one it runs, and
   the outcome of every check (its error, if any) is kept on the stack and shown. A check finds nothing to do when
   nothing moved, so a poll is never a redeploy for its own sake.
9. git runs with an emptied environment, no system or global configuration, no prompt, and only the https and http
   transports: a repository address cannot reach a file on the server or run a program (`ext::`). The token goes to
   git as an HTTP Basic header in its environment for that one run, so it is never written into the clone's
   configuration nor seen in the process list, and it is taken out of any error git says back. A stack's clone is
   shallow, and an update puts back only the repository's own files: what a container wrote in the folder stays.
10. A compose job that pulls (deploy, pull) signs in to every saved registry ("Registries" 3): Compose is run with
   `DOCKER_CONFIG` at a `config.json` written for that one job from `registries::docker_config`, in a private folder
   beside the stacks' folders (`<data>/stacks/.docker-<id>/`, never inside the stack's own, which a build context could
   read) and removed when Compose is done. Every registry, not only those the file names: an image can come from a
   variable, and Compose asks for what it needs. A registry whose sign-in fails is left out, said in the job's output,
   and its pulls go unsigned. With no registry saved, Compose keeps the `DOCKER_CONFIG` it had ("Stacks" 3).
11. A stack's Environment tab shows every service's variables as the next deploy gives them: `docker compose config
   --format json` over the stack's own files, so `.env`, `env_file` and the defaults in `${...}` are filled in, not the
   running containers' (owner's request, 2026-11-09). Settings kept inline in a compose file, as most imported from
   Portainer are, read as plainly as `.env`. A name that looks like a secret (pass, secret, token, key, credential,
   private, auth) has its value hidden until Show is pressed; it is still sent to anybody who may read the stack, so
   roles decide who sees it at all ("Access" 11).
12. A stack's id can come again once the stack is gone (`stacks.id` is a plain rowid), so nothing a stack kept on disk
   outlives it: removing a stack, or the environment it is on, takes its folder and its `.docker-<id>` folder with it, and a
   new stack's folder is emptied before its files are written, in case an older Freightdock or a crash left one there. A new
   stack never reads what another stack, live or gone, kept.
13. A Git stack's saved token is bound to the origin (scheme, host, port) of the address it was saved with. An edit that
   moves the address to another origin without a token is refused on the token's field before git is run, unless the token
   is cleared: a credential somebody else saved is never sent to a host the editor chose.
14. A stack's compose file and `.env` are used only when they lie in its own folder once every link is followed (a Git
   stack's compose file in its clone, `stacks::confine`): reading a stack, checking it and every job that reads the file
   refuse a link out, and git runs with `core.symlinks=false`, so a link committed to a repository is checked out as a
   plain file holding its target's name. What a repository or a container writes in the folder never leads Freightdock's
   reads elsewhere on the server. This holds "Stacks" 9's promise that a repository cannot reach a file on the server.
15. A stack's compose file may make Compose read only files in the stack's own folder (`confine::references`), since
   Compose runs on Freightdock's server, where every stack's files and Freightdock's own keys are: every `env_file` and
   `label_file`, the files it `include`s or `extends` (and what those name in turn), a build's context (unless it is an
   address), Dockerfile, extra contexts, SSH keys and local caches, and the `file` of its configs and secrets. Freightdock reads
   the file itself (YAML, merge keys and tags taken into account) before Compose is run over it: when it is saved, and
   before every job and every read that runs Compose, so a file changed on disk since is held to it too. A path with a
   variable in it, or `~`, is refused, because what it names is not known until Compose fills it in. Bind mounts are not
   held to it: they are read by the environment's Docker on its own host, and the security settings decide them
   ("Access" 13). Compose is still run with `--project-directory` and its working folder at the stack's.
16. A project's labels (`working_dir`, `config_files`, `environment_file`) are whatever made its containers says, so they
   are trusted as paths on Freightdock's server only when the environment is the local Docker (the host Freightdock reads is that
   one's), only for a Freightdock administrator, and never into Freightdock's own data folder (the stacks' folders, the data
   folder): an external project's page then shows the files, and adopting reads them. Anybody else, or another host,
   pastes the compose file to adopt it. **Supersedes "Stacks" 6 in part** (its files were read wherever its labels said,
   for anybody who could see the project). An adopted file whose `env_file` or build context lies outside the stack's
   folder is held to "Stacks" 15 like any other: its variables go into the stack's `.env`.
17. A new stack (from the editor, Git or a template) made by somebody who sees only some of an environment (Standard) may
   not take the name of a Compose project Docker already runs there (`stacks::unclaimed`, a 409 on the name, asked before
   anything else): a container without a record of its own belongs as its project's Freightdock stack does ("Access" 4), so
   the name alone would hand them the administrators' containers. Taking over a running project is adopting it, the
   administer action ("Access" 2). Everybody else sees the whole environment already, and may make such a stack.

## Templates

**Set aside** (G8.3, 2026-11-12, "Cluster" 10): the feature these items are about is not in the cluster UI. They are kept, numbered, for when it comes back through the managers; until then they describe nothing in this repository.

1. App templates are read in Portainer's own JSON (v2, v3, and v1's bare list) from addresses in the settings, so
   every community list works unchanged; Portainer's list is the default. An entry that cannot be read is left out, not
   the whole list. A template's note, which Portainer writes as HTML, is shown as text.
2. A template list is only ever one in the settings: a deploy names the list and the template's place in it, and
   Freightdock reads the template itself, so a deploy cannot be made to fetch an address nobody configured. Lists and
   logos are http or https addresses only.
3. A stack template from a repository is deployed as a Git stack, as Portainer does it, so it can be redeployed from
   its repository later; a container template goes through the same create path as a container made by hand.
## Registries

**Set aside** (G8.3, 2026-11-12, "Cluster" 10): the feature these items are about is not in the cluster UI. They are kept, numbered, for when it comes back through the managers; until then they describe nothing in this repository.

1. A registry is saved for the whole instance by an administrator, with the CSRF token, and each change is in the
   activity log (`registry.create`, `.update`, `.remove`); everybody signed in sees the list and browses, as they may
   pull. Its connection is tested before it is saved, signed in as a pull would be, and a registry that does not
   answer, or refuses the credentials, is not saved, with the registry's reason; an edit is tested again only when the
   address or the sign-in changed, so a rename works while the registry is down.
2. An image's registry is the first part of its reference when that has a dot or a colon or is `localhost`, as Docker
   reads it, else Docker Hub; hosts are matched whole, in one form (lower case, no scheme or path), and Docker Hub by
   any of its names (`docker.io`, `index.docker.io`, `registry-1.docker.io`). One registry a host: Docker takes one
   sign-in for a host with a pull or push, so two would only leave which one unclear.
3. The password, token or secret key is sealed ("Secrets" 1) and never sent back; the page says only whether there is
   one, and an empty field keeps it. Pulls, pushes, containers made or recreated (when their image is pulled) and,
   with stacks, compose deploys (`registries::auth_for`, `docker_config`) sign in with the saved credentials of the
   image's host by themselves, and the activity log says which registry a pull or push signed in to.
4. The browser speaks the registry's own API: a request goes without credentials first, and the 401's
   `WWW-Authenticate` says how to sign in, Basic (sent again with the user and password) or Bearer (a token for the
   scope it names, asked of the realm it names with the user and password, kept per scope). A next page is followed
   only on the same registry, so a token never goes to another host.
5. Docker Hub lists no catalog, so its repositories come from Hub's own API, an account's or organisation's (the
   user's own unless another is named). A registry that lists none (GHCR, GitLab) is browsed by typing a repository's
   name; its tags are listed as for any other.
6. ECR is signed in to with an access key and its secret, exchanged for ECR's 12-hour token by `GetAuthorizationToken`
   with AWS Signature Version 4, done by hand (HMAC-SHA256 on sha2, tested against RFC 4231 and AWS's own test vector)
   rather than with an AWS SDK for one call. The token is kept in memory only, until five minutes before it runs out.
7. A registry is reached over HTTPS with the certificate checked; plain HTTP only when its address says `http://`, as
   Docker needs it named an insecure registry (127.0.0.0/8 is one by default). There is no switch to skip the check.
8. A Forgejo or a Gitea is a kind of registry of its own, beside GitLab and Quay: its packages are a registry at the
   instance's own address, which it has to be given (there is no public one to fall back on, as `ghcr.io` is), and its
   sign-in is a user and an access token with the package scopes. It speaks the v2 API, catalog included, so it is
   browsed and pulled from as any other; the kind is there so the form asks for the right things and the list says
   what it is. A registry saved as "another registry" before stays one.

## Containers

**Set aside** (G8.3, 2026-11-12, "Cluster" 10): the feature these items are about is not in the cluster UI. They are kept, numbered, for when it comes back through the managers; until then they describe nothing in this repository.

1. Until roles come (M4), a signed-in user may look at everything and only an administrator may change anything. A
   change on Docker's socket is root on the host. **Superseded** by "Access" 2 (from 1.0.0, M4).
2. A container is made from Docker's own create body, as Docker documents it, so every option Docker has is reachable
   from the start; the editor fills it from `GET .../containers/{id}/config`.
3. Editing is replacing: the old container is stopped and renamed aside, the new one made and started under the name,
   and only then is the old one removed. If the new one cannot be made or started, the old one gets its name back and is
   started again if it ran. An edit that fails leaves the container as it was.
4. Recreating (and editing) copies a container's own settings, not what its image gave it: environment entries,
   command, entrypoint, working folder, user, health check, stop signal, labels, exposed ports and volumes equal to the
   image's are left out, and a host name Docker made up from the id is dropped. Otherwise a newer image's defaults would
   never reach a recreated container.
5. Memory in use is counted as `docker stats` counts it: without the page cache (`inactive_file`), which the kernel takes
   back when it needs it. CPU is a percentage of one CPU, so two busy CPUs are 200%.
6. A container joins and leaves networks from its own page through the networks' routes (`networks/{id}/connect`,
   `/disconnect`), not routes of its own, so one rule and one log entry cover both pages. It is offered only for a
   container with networks of its own: one on the host's network, on none, or sharing another container's has none to
   join or leave, and Docker would refuse.
7. Making a container (`containers::make`: a create, an edit, a recreate, an update) runs as a task of its own and
   ends whatever happens to whoever asked, and so does a webhook's whole redeploy, which ends its own turn
   ("Sign-in" 8). hyper drops a handler whose client hangs up, and a make stopped between setting the old container
   aside and making the new one would leave it stopped under another name (3 promises it comes back); a webhook's
   turn not ended would refuse every later call until a restart. As a compose job does ("Stacks" 2).

## Streams

**Set aside** (G8.3, 2026-11-12, "Cluster" 10): the feature these items are about is not in the cluster UI. They are kept, numbered, for when it comes back through the managers; until then they describe nothing in this repository.

1. Pulls, pushes, builds and image loads answer as NDJSON lines as they go, the last `{"done":true}` or an `error`; they
   are POSTs, so they carry the CSRF token. Each is driven by a task of its own that stops when nobody reads, so closing
   the page stops the pull.
2. Logs, stats, consoles and events are WebSockets that end when the page leaves. A console is an administrator's
   (**superseded in part** by "Access" 2: it is the console action), is logged with its command and user, and speaks binary for the terminal's bytes and text only for `{"resize":[c,r]}`.
3. Attaching to a container's main process (`attach/ws`) is an administrator's (the console action since "Access" 2) and logged, speaks the console's protocol
   after a first `{"attached":{"tty","stdin"}}`, and never stops the container by leaving: the attach is dropped, its
   input never shut, nothing sent to the process. What is typed goes in only when the container keeps its input open
   (`OpenStdin`) and not just for one attach (`StdinOnce`, which `docker run -i` in the foreground sets): Docker closes
   such an input when the attach lets go, and a shell reading it would exit and stop the container, so it is attached
   to watch only. Output without a TTY gets the carriage returns a terminal needs; Ctrl-C into a TTY reaches the
   process, as with `docker attach`, and the page says so.
4. A command can also be run without a terminal (`POST .../containers/{id}/exec`, for programs and `/mcp`): an
   administrator's (the console action since "Access" 2), logged as a console is (`container.exec`), answered once it ends with its exit code and its
   standard output and error apart, each kept to its last 64 KiB (at most 1 MiB, asked for), where errors are. It has a
   time limit (30 s, at most 600); a command past it is answered as timed out and left running, since Docker has no
   way to stop one exec but stopping the container.

5. A container's live stats come every 5 seconds unless the page asks for 1, 10 or 30 (owner's decision, 2026-11-09),
   and the choice is kept per browser (`freightdock.statsEvery`). A sample a second is more than a person reads and five
   times the traffic over an agent's tunnel. Docker still sends one a second; the server sends the first at once and
   then the newest at each tick, and the page reckons network speed over the real time between samples.

## Swarm

**Set aside** (G8.3, 2026-11-12, "Cluster" 10): the feature these items are about is not in the cluster UI. They are kept, numbered, for when it comes back through the managers; until then they describe nothing in this repository.

1. The Swarm pages and routes are for an environment whose Docker is a swarm manager, told from its `/info`
   (`Swarm.LocalNodeState` active and `ControlAvailable`); `GET .../swarm` says the role. A worker cannot answer for its
   swarm and a lone Docker has none: Docker's 503 there is answered as a 409 with its reason, since it is what the host
   is, not a failure (502 stays for a Docker that does not answer).
2. Swarm is spoken to Docker as JSON, whole (`docker::raw`), not through bollard's typed models: a node's or a service's
   spec is read and sent back on every change, and a typed model would drop the fields newer than bollard, changing
   what nobody asked to change. bollard also has no configs and no service logs. A change is sent at the version Docker
   gave with the spec, so one made meanwhile elsewhere is refused, not overwritten. A service is made and updated from
   Docker's own spec, as a container is from its create body ("Containers" 2).
3. As for containers ("Containers" 1): anybody signed in may look; only an administrator, with the CSRF token, may
   change (**superseded in part** by "Access" 2: anybody with a role on the environment looks, and every change is
   the administer action, an Environment administrator's; **superseded in part** by 7: a limited role does not read
   services, tasks or stacks), and each change is in the activity log (`swarm.node.update`, `.remove`; `swarm.service.create`, `.update`,
   `.scale`, `.rollback`, `.remove`; `swarm.secret.create`, `.remove`; `swarm.config.create`, `.remove`;
   `swarm.stack.deploy`, `.remove`).
4. A secret's value goes into the swarm once and never comes back out through Freightdock: Docker gives none back, and
   `Spec.Data` is taken out of every answer about a secret all the same. The activity log has its size, not its value.
   A config is not secret: its text is shown on its own page, never in a list.
5. Swarm stacks are deployed with Docker's own `docker stack deploy` ("Endpoints" 2), the base CLI, from Docker's CLI
   image pinned by its digest. It runs with `--detach` and `--prune` (a service left out of the file goes), the file on
   standard input, in an empty folder of its own so a relative path in the file reaches nothing of Freightdock's. How the
   CLI reaches an environment is one function, `docker::cli::docker_host`: the endpoint's own address, so every kind
   of host is reached as bollard reaches it, with Docker's context and TLS variables cleared so nothing in Freightdock's
   own environment sends it elsewhere. An absolute path in a file (a secret's `file:`) is read on Freightdock's side; as
   with everything on Docker's socket, an administrator is root there already. **Superseded in part** by 8: such a
   file is refused, and the CLI gets an emptied environment.
6. Docker keeps no Compose file for a stack, so Freightdock keeps the one each was last deployed with, per environment
   (`swarm_stacks`, gone with the environment), to edit and deploy again. A stack is found by its services'
   `com.docker.stack.namespace` label, so one deployed elsewhere is listed and can be removed; deploying it here once
   gives it a file.
7. A swarm's services, its tasks, a service's logs and its stacks are the administrators' for a limited role
   (Standard and Read-only users), as a project started elsewhere is ("Access" 4) (owner's decision, 2026-11-10;
   **supersedes 3 in part**). They have no owner record and only an Environment administrator makes them, and a
   service's spec carries its variables, its logs what its containers print and a stack's kept file often a password:
   the same task's container answers 404 for those users, so the swarm's routes must not be a second way in. Their
   lists answer empty for them, and a route about one service or stack (`Res::Swarm` in the guard) answers 404.
   Helpdesk and up read all of it as before. The swarm itself, its nodes, and its secrets' and configs' lists stay a
   read: a secret's value never comes out ("Swarm" 4), and a node is the host's, not a workload.
8. The CLI that deploys a swarm stack runs on Freightdock's server, but whoever deploys is an Environment administrator of
   that one swarm ("Access" 2), not of Freightdock, so the server reads nothing of its own into the swarm for them. The CLI
   runs with an emptied environment, as Compose does ("Stacks" 3): PATH, HOME, TZ, TMPDIR, DOCKER_CONFIG and the
   environment's `DOCKER_HOST` (`docker::cli::command`), so no Freightdock variable fills a `${...}`. A file with a key the
   CLI would read a path from (`file`, as a secret's, a config's or `extends`', `env_file`, `label_file`) anywhere in
   it is refused before the CLI runs, after its `<<:` merge keys are applied and with keys compared without case, as
   the CLI reads them, and so is one with a YAML tag. No path is allowed, whatever it looks like: an absolute one is
   read as it stands, and a relative one climbs out of the scratch folder with `..`. A stack's secrets and configs are
   made on the Swarm pages and named `external: true`; its variables are written in the file. **Supersedes 5 in
   part**.
9. A change to a service (an update, a scale, a roll back) or a node (drain, role, labels) is sent at the version Docker
   gave when it was read, as Docker wants. Swarm moves that version by itself (a service's tasks rolling over, a
   node's status), so on a busy host a change
   meets "update out of sequence" with nobody else involved: it is then read and made again, up to five times. A
   whole new spec is sent again only while the service still has the spec it was written over; when somebody else
   changed it meanwhile, the update is refused as a conflict rather than overwriting theirs.

## QA

**Set aside** (G8.3, 2026-11-12, "Cluster" 10): the feature these items are about is not in the cluster UI. They are kept, numbered, for when it comes back through the managers; until then they describe nothing in this repository.

1. What only a real Docker can show (pull, create, logs, console, edit and its rollback, build) is checked end to end by
   `qa/run.sh`, in CI, against a throwaway `docker:dind`, never the Docker of the machine it runs on: the suite removes
   what it made, and a test that went wrong must not be able to touch somebody's containers. It refuses to go on when
   the Docker it reached already has containers.
2. The suite's steps are Rust (`crates/server/tests/qa.rs`, ignored by `cargo test`), not shell: an assertion says what
   it expected and what came, JSON is read as JSON, and the console is a real WebSocket client. The shell script only
   starts things and cleans up.
3. The suite's Freightdock listens for agents on a free port of its own (`FREIGHTDOCK_AGENT_BIND=127.0.0.1:<port>`), never
   the default 7444: that port on the machine it runs on may be somebody's Freightdock already, which made the suite's stop
   at its first start. The endpoint stays on, rather than off, so the suite's Freightdock starts as one in use does.
4. Swarm is checked on the same dind, at the end: a step makes it a swarm of one straight at its API
   (`FREIGHTDOCK_QA_DOCKER`; Freightdock makes no swarms) and the last Swarm step leaves the swarm and waits for its
   containers to go. The steps before see a lone Docker (and that the Swarm routes refuse it), the steps after a Docker
   in no swarm holding nothing. A second dind would need a second environment, an agent or a TLS host, which the suite
   does not make yet.

## Logs

1. Log lines are stamped in local time with the offset written (`2026-11-09T21:14:09.485+01:00`): the zone `TZ` names,
   else `/etc/localtime`. A log is read next to the clock on the wall, and the offset keeps every line exact when two
   zones meet. The image has tzdata, so `-e TZ=...` is all a container needs.

## MCP

**Back** (2026-11-14) over the cluster: set aside at G8.3 ("Cluster" 10), it now works on the managers' cluster through
`/api/cluster/...` (7). Where an item still speaks of Docker hosts, read the cluster's nodes and stacks.

1. Freightdock is an MCP server at `/mcp` (from 1.0.0), so an AI agent can manage its Docker hosts: the Streamable HTTP
   transport, JSON-RPC 2.0, one message a POST answered as plain JSON, `Mcp-Session-Id` from `initialize` on (a session
   is its token's and is forgotten after a day unused), protocol versions 2025-11-25, 2025-06-18 and 2025-03-26. There
   is no stream: nothing here is said unasked, so `GET /mcp` is 405, and a tool that takes time (a pull) answers once
   it is done. It is written here with serde rather than with the `rmcp` crate: the part used is a few JSON-RPC
   methods, and `rmcp`'s server brings its own session manager and service that would stand beside Freightdock's guards
   instead of going through them (2).
2. Every tool is a request to Freightdock's own API, made in process through the same router with the caller's token: it
   meets the same guards ("Sign-in" 9, 10; "Containers" 1), the same Docker code and the same activity log
   ("Activity" 2) as the UI, so a read-only token or a user who is not an administrator is refused a change through
   MCP exactly as through the API, and there is no second implementation of anything to drift. Where the API had no
   route for what a tool needs (a command run to its end, one stats sample), the route was added to the API.
3. `/mcp` signs in by API token only, never the cookie, and refuses a request whose `Origin` is not this server (as
   the spec asks, against DNS rebinding): a page a signed-in person visits must not be able to drive it.
4. A tool's failure (Docker's refusal, Freightdock's 403, a missing argument) is its result with `isError`, in
   Freightdock's own words, so the agent can read it and try otherwise; a tool that does not exist is the protocol's
   error (-32602). Results are JSON text, trimmed for a reader with a context to keep: lists as `docker ps` shows them
   (inspect gives the rest), logs to their last 32 KiB unless asked for more, any answer to 256 KiB, cut where it says
   so.
5. **Superseded by 7.** A tool names its host by `env`, an environment's id or name, and works on the local Docker when
   it is left out: an agent should not need to look up an id to do the common thing.
6. An administrator turns the MCP server on or off for the whole instance, in Settings under AI agents (MCP), kept as
   the `mcp` setting (from 1.0.0). It is on when the setting is not there, for an instance made before the switch and a
   new one alike (owner's decision, 2026-11-10: a Claude Code connection that works keeps working, and every request
   needs an API token all the same). Off, `/mcp` answers 403 with a sentence saying so for every method, before it
   looks at the token, and every MCP session ends; API tokens keep working on the API, as they are not only for MCP.
   The same card lists every API token with its owner, active or not, and an administrator revokes any of them there,
   from a session only ("Sign-in" 10); each person still makes their own in Account. The switch and each revoke are in
   the activity log (`settings.mcp`, `token.revoke` with whose it was).
7. The tools are the cluster's (2026-11-14): the cluster and its managers, nodes, stacks (check, deploy, start, stop,
   restart, remove), events, recoveries, shared storage, a node's containers and a container's logs through its
   worker ("Cluster" 25), letting a fenced node back in, and the activity log. Each is one of the UI's own
   `/api/cluster/...` routes (2), so the managers check what they check for the UI and a node's Docker stays an
   Administrator's. A node is named by its id, its name here, its hostname or its `name` label, and a stack by its
   name, so an agent can say "w1" or "web" as a person would; an unknown node is answered with the ones there are.
   Results are the managers' JSON, cut at 256 KiB (4). The sessions are the UI's, in memory: when the managers move
   the UI to another node a client is answered 404 and initializes again, as the transport has it.

## History

**Set aside** (G8.3, 2026-11-12, "Cluster" 10): the feature these items are about is not in the cluster UI. They are kept, numbered, for when it comes back through the managers; until then they describe nothing in this repository.

1. Each running container's use is sampled into SQLite every `every_s` seconds (60 by default, 10 to 3600), for every
   environment that answers, and kept `keep_days` (7 by default, 1 to 365); an administrator sets both, or turns it
   off, in Settings. The rows are the database's own, so history needs nothing else installed and goes with a backup.
2. A sample is kept by the container's name, not its id: an edit, a recreate or an update makes a new container under
   the same name, and its history must go on. A removed container's history stays until it is old enough to go.
3. Network and disk are kept as Docker's counters and turned into rates only when asked, between neighbouring samples:
   a counter that went back (the container started again) or a gap of more than ten minutes counts nothing, rather
   than a spike or a made-up line. An answer has at most 360 points, averaged over equal stretches of time.

## Notifications

**Set aside** (G8.3, 2026-11-12, "Cluster" 10): the feature these items are about is not in the cluster UI. They are kept, numbered, for when it comes back through the managers; until then they describe nothing in this repository.
**Back through the managers** (2026-11-13, "Cluster" 27): items 1, 3, 4 and 5 hold there; item 2 as "Cluster" 27 says (from the workers' reports, not Docker's events); item 6's deploy is the cluster's failed run.

1. Freightdock tells through channels an administrator sets up: a webhook, e-mail over SMTP, an ntfy topic, a Gotify
   server, each with the events it wants. Each channel has at most one secret (a webhook's signing key, an ntfy or
   Gotify token, the SMTP password), sealed like every other ("Secrets" 1) and never sent back; every change to a
   channel and every test is in the activity log.
2. A container "died" when it stopped on its own: Docker's `die` with an exit code other than 0, not within a minute
   of a signal sent to it (`kill`, which a stop, a restart, an edit and an update all send first), or after running
   out of memory. A job that finished (exit 0) did not die, and a stop is no news.
3. The same event about the same thing (kind, environment, container or host) is told at most once in ten minutes, so
   a container crashing in a loop is one message, not one a minute. The window is in memory: a restart may tell once
   more.
4. A webhook's body is JSON (`event, title, message, environment, target, at, freightdock`), and with a signing key it
   carries `X-Freightdock-Signature: sha256=<hex HMAC-SHA256 of the body>`, as GitHub signs its hooks, so the receiver can
   tell Freightdock from anyone else who knows the address.
5. The test button sends through the channel as saved and answers what went wrong in words (the server's answer, the
   SMTP error), without the address's query, which may hold a token.
6. A stack's deploy that fails is told (`deploy.failed`, with Compose's or git's reason) wherever it was asked from:
   the page, a webhook or the Git poller, as all three go through `stacks::run`. Only a deploy: a stop, a pull or a
   remove that fails is said on the page that asked and in the activity log, and nobody waits on it later.

## Updates

**Set aside** (G8.3, 2026-11-12, "Cluster" 10): the feature these items are about is not in the cluster UI. They are kept, numbered, for when it comes back through the managers; until then they describe nothing in this repository.

1. A newer image is found by digest, never by pulling: the tag's digest in its registry (a HEAD on the manifest,
   which Docker Hub does not count as a pull) against the image's own registry digests. An index (a multi-arch image)
   is fetched only when its digest is not the local one, once per digest, to see whether the local platform's own
   manifest in it is the local one (an image pulled for one platform, or pushed from here, records that one).
2. A registry is asked about a reference at most every `check_hours` (6 by default, 1 to 168), whatever the number of
   environments and containers using it; its answer is kept in the database, and a failed question keeps the last
   answer with the error beside it. A check asked for by hand asks again what is older than a minute. Rate limits are
   never near, and a registry that is down does not turn every badge to an error.
3. Updating is opt-in per container: the label `freightdock.auto-update=true` (set where the container is made, so it
   goes with a compose file), or a switch on the container's page kept by its name. Freightdock does not update anything
   nobody asked it to; everything else only gets a badge, an `update.available` notification and an Update button.
4. An update is a recreate with the image pulled again ("Containers" 3, 4), and one more check: the new container must
   still be up ten seconds after it started (running, not restarting, not unhealthy), or it goes and the old one is put
   back and started. An update that fails leaves the container as it was, and says why in the activity log
   (`container.autoupdate`, by nobody, or `container.update` by whoever pressed the button).
5. **Superseded** by 6 (the registries feature keeps credentials now). Registry credentials come from one function,
   `updates::registry::credentials`, used by the check and by the pull an update makes; until the registries feature
   (M3) keeps credentials, it answers none and every registry is asked anonymously, so a private image is "not
   checked" rather than wrong.
6. `updates::registry::credentials` answers the saved registry's sign-in for the reference's host
   (`registries::auth_for`, "Registries" 2, 3), for the check and for the pull an update makes, so a private image
   is checked and updated like any other. A registry with nothing saved is asked anonymously, and one whose sign-in
   fails (ECR refusing its keys) too, with the reason in the log: its image is then "not checked", never wrong.

## Volume files

**Set aside** (G8.3, 2026-11-12, "Cluster" 10): the feature these items are about is not in the cluster UI. They are kept, numbered, for when it comes back through the managers; until then they describe nothing in this repository.

1. A volume's files are reached through a short-lived helper container that mounts it, through Docker's own API (the
   archive endpoints and a command in the helper), not a second kind of stream through the agent's tunnel as the plan
   first said: the helper works the same on the local Docker, a TLS host and an agent's host, needs nothing new in the
   agent or its protocol, and runs with the host's own Docker semantics for the volume's driver. Its cost is a helper
   image (`busybox:1.37`) on each host, pulled the first time.
2. The helper mounts the volume and nothing else, without a network, read-only for listing and downloading; a path is
   refused if it has `..` or a control character. A symbolic link in the volume points inside the helper, never at
   the host. The volume must exist: Docker would quietly make a new one for an unknown name.
3. Only an administrator browses (**superseded in part** by "Access" 10: an Environment administrator too), since a
   volume holds whatever its containers keep (keys, databases); every upload,
   download, new folder and delete is in the activity log (`volume.upload`, `.download`, `.mkdir`, `.delete_path`).
4. An upload is at most 256 MB and is held in memory while it is put into the volume; a download is streamed and has
   no limit. A listing shows at most 5000 entries and says when there were more.

## Backups

**Set aside** (G8.3, 2026-11-12, "Cluster" 10): the feature these items are about is not in the cluster UI. They are kept, numbered, for when it comes back through the managers; until then they describe nothing in this repository.

1. A backup is Freightdock's own data, all that is needed to stand it up again: the database (copied with SQLite's online
   backup API, so nothing stops), `secret.key` (without it no sealed secret in the database opens), the agents' CA
   (without it no agent's certificate is good), the TLS certificate, and the stacks' files when there are any. Not the
   sockets, older backups, or the containers and volumes, which are Docker's and backed up as Docker's.
2. Archives go to a folder on Freightdock's side (a mounted share) or to S3, through a small client of its own signing
   with Signature Version 4 (tested against AWS's published examples), so any S3-compatible store works and no SDK is
   pulled in. Only Freightdock's own names (`freightdock-YYYYMMDD-HHMMSS.backup`) are counted for `keep` and removed.
3. Every archive is sealed with a passphrase the administrator sets, at least 12 characters: an Argon2id key (64 MiB,
   three passes, a random salt) and XChaCha20-Poly1305 over chunks of 1 MiB, each nonce a random prefix with the
   chunk's number and whether it is the last, the header (magic, cost, salt, prefix) its associated data. A changed,
   reordered, dropped or cut-off chunk does not open, and a wrong passphrase looks the same as damage. The archive holds
   `secret.key`, so the passphrase is all that guards everything in it; Freightdock keeps it sealed for the schedule, but
   a restore asks for it typed.
4. A restore is never applied to a running database: it is checked (only Freightdock's names in it, nothing outside its
   folder, the database whole and not newer than this binary) and staged, then Freightdock starts itself again in the same
   process and puts it in place before the database opens. What it replaces is moved aside, not deleted, so a restore
   can be undone by hand. The same staging is `freightdock restore <archive>` with Freightdock stopped.
5. A header's Argon2 cost is bounded before any work (at most 1 GiB, ten passes, eight lanes), so a made-up archive
   cannot make a restore take all the memory or an hour.
6. A restore makes what it put in place private (folders 0700, files 0600) without ever following a link (from 1.0.0,
   audit 2026-11-10): a Git stack's clone keeps the links its repository commits, so a person with Create on any
   environment could commit one to `/usr`, and the next restore would have made Freightdock's own system unusable. A link
   is left as it is, and what it points at is never touched.

## Portainer

**Back** (2026-11-14) for the cluster, as 11 to 13 say. Of 1 to 10, what still holds: 1 (administrators, the CSRF
token, the activity log; the token sealed, tried before it is saved, kept when the field is left empty for the same
address), 2 (GET only, no redirect, read again at the import) and 3 (the certificate checked, pasted, or not checked by
an explicit choice). 4 to 10 were about the single-host Freightdock (environments, adoption in place, groups, tags,
registries, templates, teams) and describe nothing here.

1. Importing from Portainer is for administrators, with the CSRF token, and each step is in the activity log
   (`portainer.connect`, `.forget`, `.import`, `.people`, and each thing made under its own action, `stack.adopt` with
   `from Portainer stack <id>`). The access token is sealed ("Secrets" 1) and never sent back; it is tried before it is
   saved (Portainer's status, then its environments with the token), and an empty field keeps it for the same address.
2. Freightdock only reads Portainer: its client has GET and nothing else, follows no redirect (the token goes to the
   address given and nowhere else), and an import reads Portainer again rather than taking the page's word for what is
   there. Portainer and the hosts it manages are left exactly as they were; what moves is Freightdock's side.
3. Portainer's certificate is checked. Portainer makes itself a self-signed one (the real one has an empty subject), so
   a certificate can be pasted: Portainer's own is then trusted as itself, pinned, for whatever name it is for, or a CA
   that signed it is used as one. Not checking at all is an explicit choice, never a default: it is saved with the
   connection, shown on the page, and written in the activity log.
4. Portainer's environments are matched to Freightdock's by the Docker engine's ID first (Portainer's Docker proxy
   `/info` against each environment's own), since it is the same host whatever either calls it. A cloned machine keeps
   its engine's ID (two of the real Portainer's hosts share one), so an ID that two environments on either side share
   matches only with the host name too. Then by name, then by address (a TLS host's, where an agent dials from). The
   administrator sees how each was matched and can change it; one with no match can be given a new agent environment
   through the join token that already exists.
5. A Portainer stack becomes a Freightdock stack adopted in place: the same name is the same Compose project, so its
   containers are kept and nothing is deployed unless asked. Its file is read through the API and merged by Compose from
   the folder its containers' labels say it was started in (Portainer's is versioned, `/data/compose/<id>/v<n>`), so
   relative paths come out as the same absolute paths Docker has been mounting ("Stacks" 6); its `stack.env` and `.env`
   stay the stack's own. Its variables are written as Portainer writes `stack.env` (`name=value`, as it is), and
   `stack.env` beside the compose file is a link to the stack's `.env`, since Portainer's compose files name it. A Git
   stack becomes a Git stack (branch from `refs/heads/...`, path, polling interval rounded up to minutes); Portainer
   never gives out the repository's token, so the page asks for it.
6. What cannot come says why, and is not ticked: a Swarm stack (not brought over yet, though Freightdock has Swarm stacks
   now: TODO), Kubernetes, a stack with more compose files than the API gives out, one whose environment is gone or not
   matched, one whose name Freightdock has there already. Two Portainer stacks of one name for one environment (the real
   one has them) cannot both be one project: the active one, else the newer, is ticked. Portainer's webhook address does
   not carry over; a new one is made on the stack.
7. A registry from Portainer comes without its password, which Portainer never gives out: it is saved untested, its
   user kept, and is used anonymously until an administrator gives the password ("Registries" 1 still holds for
   every registry added or changed by hand).
8. Importing twice makes nothing twice: each thing made is written down (`portainer_imports`, by Portainer's
   InstanceID, kind and id), and is skipped as imported already while it is still there; one removed from Freightdock can
   come again. Groups and tags are found by name before they are made; an environment already in another group stays
   in it.
9. Portainer's users, teams and who is in which are kept in the settings (`portainer_people`) for when Freightdock has
   teams (M4). Nobody is made a Freightdock user by an import. **Superseded in part** by 10: the teams are now made.
10. Portainer's teams become Freightdock teams when ticked (from 1.0.0, M4). A team is the one an earlier import made
   (`portainer_imports`, kind `team`) while it is still there, whatever it is called now; else Freightdock's team of the
   same name, case-blind as team names are; else a new one. Its members are the Freightdock users with the names of
   Portainer's members, case-blind as user names are: a name is the only thing the two share, and Portainer's
   users have no password Freightdock could take, so nobody is made a user, and a member Freightdock has no user for is
   named and left out (the preview says who before anything is made). An import only adds: a team is never made
   twice, members already in it stay, and nobody is taken out, since somebody may have been put in by hand in
   Freightdock since. Portainer's team leader, and the roles its teams had on environments, do not come: Freightdock's
   roles are given per environment or group ("Access" 1), by an administrator. The copy in `portainer_people` is
   still kept when asked for. Each team made is `team.create` and each member added `team.update`, both `from
   Portainer`, in the activity log.
11. A Portainer Compose stack comes into the cluster as a cluster stack (2026-11-14): its file as Portainer gives it,
   its variables as the stack's `.env` (written as Portainer writes `stack.env`; a name Compose would not take is left
   out and said), a description naming where it came from, and its name made a stack name (lower case, `-` for
   anything else, at most 40). Nothing is adopted: the cluster places it on its own nodes, wherever Portainer ran it.
   A Git stack comes as its file, not followed. What cannot come says why and is not ticked: Swarm and Kubernetes
   stacks, one with more Compose files than the API gives out, one whose name the cluster has; of two that would have
   one name, the active one, else the one changed last.
12. An imported stack is made stopped unless the Administrator asks to start it (the managers' `"stopped": true`):
   Portainer's copy still runs where it was, and the same stack started twice would take its ports and volumes twice.
   The page says to stop it in Portainer first, then start it here.
13. An import is one deploy per stack through the managers, so they check the file as they check one typed in, and a
   stack they refuse says their sentence and leaves the others. Each one brought in is `portainer.import` in the
   activity log, with Portainer's stack id and environment and whether it was started.

## Users

1. A person's picture is the one their sign-in provider gives (the OpenID `picture` claim, https only), kept on the
   account at each sign-in and forgotten when the provider stops giving it (owner's request, 2026-11-10). Freightdock
   fetches it itself and serves it from `/api/users/{id}/avatar`, keeping a copy for a day and serving the copy while
   the host is down: the browser never calls the picture's host, so Gravatar and the like learn nothing about who uses
   Freightdock and no third-party address is needed in the page. Only PNG, JPEG, GIF and WebP up to 1 MB come back, never
   SVG, which can carry script. One sees one's own; an administrator anybody's. Without a picture, initials.
2. An account linked to a sign-in provider shows no password or two-factor card in Account (owner's decision,
   2026-11-10): the provider gives its sign-in and its two-factor, and two places to manage them is one too many. The
   local sign-in stays as the way back in ("SSO" 4); an administrator resets its password from Users.
3. `/api/me` says how the account signs in, `{kind, provider}`: `local` (a password kept here), `oidc` or `github` (a
   provider) or `ldap` (a password a directory checks), with the provider's name (owner's request, 2026-11-10).
   Account shows it beside the role as a badge, `SSO: Authentik` with a key or `Directory: <name>`. It narrows "Users"
   2: the password card is for `local` alone (a directory's password is changed in the directory); two-factor is for
   `local` and `ldap`, which sign in with a password; and the card that links to a provider is only for a `local`
   account that is not linked yet, since it does nothing else.

## Outbound

1. Every answer the server reads from a host somebody else names (a registry an image reference says, the realm its
   challenge names, a sign-in provider's picture) is read through `outbound::capped`, to a limit fitting what it is:
   4 MiB for a manifest or an index, 64 KiB for a registry's token answer, 1 MB for a picture. An answer that says it
   is larger, or turns out to be, is refused at the limit, before more is kept. Anybody who can pull or build names a
   registry, and the one server that manages every environment must not keep and parse whatever that registry sends.
2. Where such a host may send the server is held too. The update check follows a redirect only on the host first
   asked, never from https down to http (`outbound::same_host`); a registry may move a path, not point the server at
   another machine. Its token realm is followed only over https (plain http for a registry on this machine, as the
   registry itself is spoken to), and not when any address its host has is inside (loopback, private, link-local and
   the metadata service, CGNAT, `outbound::internal_ip`) unless the registry itself is inside too; the realm is then
   asked at the addresses checked, so a second lookup cannot answer elsewhere. A repository's path parts are a
   registry's (`.`, `..`, `?`, `#`, `%` refused), so a reference cannot step out of `/v2/`. Anybody who can pull or
   build names a registry, and the update check runs on the server, not on the environment's host: without this, a
   registry on the internet could have the server ask this machine and its network, and read back the status in the
   badge. A sign-in picture's redirect is followed only over https, and not to an address written inside unless the
   picture's own was (`outbound::outward`): a picture host may hand off to a CDN.

## Cluster

1. Freightdock becomes the UI of a Freightdock cluster (Freightdock-Manager, Freightdock-Worker), and only its UI: an
   image of its own that the managers run (owner's decision, 2026-11-11). The cluster's state is the managers', kept
   by them with Raft; Freightdock keeps none of it and asks a manager every time (`cluster.rs`), so it can run on any
   node or move. While the cluster pages are built, Freightdock keeps its own sign-in and users, and reaches the
   manager's admin API on the host it runs on (`FREIGHTDOCK_CLUSTER_API`, the token in
   `FREIGHTDOCK_CLUSTER_TOKEN_FILE`); the users move into the managers' state later, and the single-host pages go when
   Freightdock is trimmed to the cluster's UI.
2. Reading the cluster is anybody's who is signed in; a command to a node, removing a node and a join token are an
   Administrator's, written to the activity log (`cluster.command` with the operation and the command's id,
   `cluster.node.remove`, `cluster.join_token` with its kind and never the token). Only an id the manager made (hex and
   `-`) goes into the manager's path, so nothing a person types reaches another of its routes. A refusal from the
   manager comes through with its status and sentence; a 401 from it is Freightdock's token, answered as 502, so the
   person is not signed out for it.
3. One node runs the whole stack by itself, and that is how a cluster starts (owner's decision, 2026-11-11): manager,
   worker and this UI on one host, more nodes joining later. Nothing here may assume more than one node.
4. A cluster stack is a Compose file the managers keep and run (Freightdock-Manager's G4): Freightdock sends the file
   and shows what the manager says of each workload, and reads, places and runs nothing itself. Reading the stacks is
   anybody's who is signed in; deploying, starting, stopping, restarting and removing one are an Administrator's,
   written to the activity log (`cluster.stack.deploy`, `.start`, `.stop`, `.restart`, `.remove`, with the stack's
   name). Only a name the manager allows (a-z, 0-9, `-`, at most 40) and one of the three actions go into the
   manager's path.
5. A node's labels as the cluster sets them are an Administrator's to change, all at once (the manager's
   `NodeLabelsSet`; they go over the worker's own labels of the same key), written to the activity log
   (`cluster.node.labels` with the labels). The manager checks keys and values; Freightdock only passes them on.
6. Authorizing a node that came back fenced is an Administrator's, written to the activity log
   (`cluster.node.authorize` with the node's id): it lets the node run workloads again, so the person is asked first
   and told to do it only when nothing the node ran was started elsewhere meanwhile. Freightdock shows the fencing
   state the managers keep and drives none of it; the managers may authorize by themselves when set to.
7. Recovery is the managers' alone (Freightdock-Manager's G7): they move a fenced node's stateless workloads and leave
   pinned ones and ones with volumes, and keep the history. Freightdock shows that history and each workload's kind
   to anybody who is signed in, and changes nothing; only a stack's name goes into the manager's path.
8. Freightdock may reach the managers over the network (Freightdock-Manager's G8.2): `https://` addresses, several
   comma separated, with an API token instead of the admin token. It trusts only the cluster CA
   (`FREIGHTDOCK_CLUSTER_CA_FILE`; there is no HTTPS without it) and checks for the managers' fixed name, so no other
   certificate a public CA would sign passes. It goes on to the next manager only when a connection could not be made:
   a request that reached one is never sent to another, since a change might then be made twice.
9. In a cluster Freightdock's database is replicated by the managers (owner's choice, 2026-11-12; Freightdock-Manager
   decision 16), so the managers can run the UI on any node. Freightdock keeps its SQLite and its SQL; triggers record
   every row written, and a write counts once the managers took it, numbered so only one Freightdock writes (one that
   lost is stale and starts again, loading theirs). Changes are rows, not SQL, and replayed as an update else an insert,
   never `INSERT OR REPLACE` (it would cascade deletes). The key that seals secrets travels with the snapshot: the
   managers hold both, readable only with an admin token. The tables of one host (statistics, the single-host
   environments) are not replicated. A write the managers did not take fails, though it stays here and is sent again.
10. Freightdock is the cluster's UI and nothing else (G8.3, owner's decision, 2026-11-12): no Docker of its own, no
    agents, no stacks, templates, webhooks, registries, image updates, notifications, resource history, backups, MCP
    server or import from Portainer of its own. A one-node cluster does everything a single host did ("Cluster" 3), so
    there is one product, not two to keep in step. What is set aside comes back, if it does, through the managers:
    notifications, backups of the managers' state, the MCP server, the import from Portainer (last), registries, and a
    node's Docker pages through its worker. Their DECISIONS sections stay, marked set aside, so their numbers and
    reasons are there when they come back.
11. The managers run the UI (Freightdock-Manager's G8.5): `freightdock-manager ui <image>` places it on a node as the
    managers' own stack, moves it when that node fails, and it is reached on an address of the pool like any stack
    (at first every manager answered for it at 7443; that went, owner, 2026-11-13: see 19). So the UI is
    started with its token and the cluster CA as values (`FREIGHTDOCK_CLUSTER_TOKEN`, `FREIGHTDOCK_CLUSTER_CA`, taken
    before their `_FILE`s), which the manager that starts it fills in and the cluster's state never holds; a value is
    never shown in a log. It has no volume, so the CA of its own HTTPS certificate travels in the database snapshot
    with `secret.key`, made before the first snapshot: a browser that trusted the UI once still trusts it on another
    node. A snapshot from before that carried none is replaced at start, and a certificate from another CA is made
    again. Run by hand beside a manager, as before, still works.
12. A node can be given a name in Freightdock (owner's wish, 2026-11-12), shown instead of its hostname wherever a
    node is named here; the managers and the node keep its hostname, so nothing in the cluster depends on it. It is an
    Administrator's to give (`PUT /api/cluster/nodes/{id}/name`, at most 64 characters on one line, empty takes it
    away), kept in Freightdock's database (`node_names`, replicated with the rest) and written to the activity log
    (`cluster.node.rename` with the name). Each node has a page of its own (`/cluster/nodes/{id}`): what the managers
    know of it, the stacks' workloads placed on it and the containers it reports.
13. The dashboard (`/dashboard`, owner's wish, 2026-11-12) is where Freightdock opens: managers online, whether the
    cluster is quorate and which node leads (the managers' `online` and `quorate`, from the reports they exchange each
    second), workers healthy, then CPU (weighted by each node's CPUs) and memory over every node heard, then a row per
    node with its CPU, memory, disk and workloads. It reads only what the cluster pages read, and changes nothing. The
    managers' gauge is green with all online, yellow while one more could go and quorum hold, orange when one more
    would lose it, red without it; the workers' green with all healthy, yellow with some down, orange with one left,
    red with none (FinUI has no orange: its warning and critical mixed).
14. A manager can be given a name in Freightdock too (owner's wish, 2026-11-12), by its raft id, the one thing about
    it that never changes (its address can): shown instead of its address wherever a manager is named here, the
    managers keeping their raft ids and addresses. As 12: an Administrator's (`PUT /api/cluster/managers/{id}/name`,
    the id digits only), 64 characters on one line, empty takes it away, kept in `manager_names` and written to the
    activity log (`cluster.manager.rename`). Each manager has a page (`/cluster/managers/{id}`): its address, raft id,
    role, whether it is heard, the worker on its host if any and the workers whose stream it holds.
15. Floating addresses (G10.3) are the managers': Freightdock lists them with each one's holder by name, from
    `GET /api/cluster/floating` (anybody signed in), and an Administrator adds one (`POST /api/cluster/floating`,
    `{address}`) or removes one (`DELETE /api/cluster/floating/{address}`, the path an IPv4 address only, else 404),
    both written to the activity log (`cluster.floating.add`, `cluster.floating.remove`). What makes an address good
    and which worker holds it is the managers' to decide, so Freightdock checks nothing more and passes their refusal
    on as it is. The card sits on the Cluster page under the workers.
16. The network is drawn live (owner's wish, G9.8, `/cluster/network`, under Overview): outside the cluster on the
    left, a column per node with its workloads, and a moving line per pair of ends that talked since each node's last
    heartbeat, as thick as its traffic, from `GET /api/cluster/traffic` (anybody signed in) every three seconds. The
    owner chose flows (who talks to whom) over node links or counters per workload. Each connection is counted by one
    node, so a line is never drawn twice; a client outside the cluster is never named. Pointing at an end shows only
    its lines and a click keeps that, across redraws. The same flows are a table under it, for exact numbers and for
    screen readers; the lines stand still for whoever asks for reduced motion.
17. A stack says how each service is reached from outside the cluster (owner, 2026-11-13, G10.5): on a pool address
    (an exposure, set on the Cluster stacks page by an Administrator, `PUT /api/cluster/stacks/{name}/exposures`,
    written to the activity log as `cluster.stack.expose`, never in the Compose file), on every worker's own address
    (the Compose file's `ports:`), or not at all ("Internal only"). A pool address answers only its exposures and a
    port is its own on each address, so two stacks may use one port on two addresses; the managers check that and
    Freightdock passes their refusal on. The pool card on the Cluster page lists what each address exposes. Saying
    which is which is the point: what leaves the cluster is only what somebody meant to let out. **Superseded** in
    part by 19: a Compose port no longer answers on the workers' own addresses.
18. The cluster has a name (owner, 2026-11-13), given by an Administrator on the Cluster page
    (`PUT /api/cluster/name`, `cluster.rename` in the activity log) and kept in Freightdock only, in `cluster_names` by
    the cluster's id, like a node's or a manager's ("Cluster" 12, 14); it heads the Cluster page and the dashboard.
    The managers know the cluster by its id alone, so a name never has to agree with them. The pool takes a range
    (`192.0.2.10-20`, `192.0.2.8/29`, at most 1024) as well as one address; an address no stack exposes on shows
    "Not in use", since the managers announce it on the LAN only once one does.
19. A stack is reached from outside the cluster only on the pool, and otherwise only inside it (owner, 2026-11-13):
    a Compose file's `ports:` open nothing, on a worker's own address or anywhere. They are what the file asks for,
    which the deployer offers as exposures on a free pool address (kept, moved, or set to Internal only before it
    deploys), so a ready-made Compose file still works and nothing leaves by accident. The managers' own stack, the UI,
    too: it is on a pool address at 443 (`freightdock-manager ui <image> [address]`), never on a worker's or a
    manager's own address, so a service keeps one address of its own that nobody has to change; it cannot be edited or
    removed here.
    A stack is deployed and changed on its own page, the deployer (`/cluster/stacks/new`, `/cluster/stacks/<name>/edit`),
    in three parts the managers check as they are written (`POST /api/cluster/stack-check`, Admin, changing nothing so
    not on record): the Compose file with the services found and the keys set aside; where it runs, any healthy worker
    or the workers chosen (`nodes`, kept by the managers, its workloads placed only there and moved there when the
    choice changes), with where its new workloads would go or why they cannot; and its exposures. The deploy sends all
    three at once (`POST /api/cluster/stacks` with `nodes` and `exposures`), checked whole by the managers.
20. The stacks page has a search (stack, service, image, workload, worker, address and exposure, ignoring case) and
    filters by state (running, needs attention, stopped), by reach (on the pool, internal only) and by worker, kept
    while Freightdock is open; it says how many of the stacks it shows. The bar is drawn again only when the workers
    change, so a reload every few seconds never takes the typing away.
21. Shared storage (the managers' G11) is shown on the Integrations page (23; on the Cluster page under the pool before): each with its kind (NFS, which the
    workers mount, or a shared directory the host already has), where it is, and the volumes kept on it with the worker
    holding each volume for one writer (`GET /api/cluster/storage`, any session). An Administrator adds one or
    removes one no stack uses (`POST /api/cluster/storage`, `DELETE /api/cluster/storage/{name}`, `cluster.storage.add`
    and `.remove` on record; only a storage name goes into the managers' path). A stack puts a volume on it from its
    Compose file (`x-freightdock: {storage, access}`), so the deployer lists the volumes on storage the managers found
    and the stack's card says where each is and who holds it; its workloads move when their worker fails ("Moves
    (shared storage)").
22. A stack is given its options beside its Compose file when it is deployed (owner, 2026-11-13: a label selector, a
    choice of how it survives its worker, a name of one's own for its workloads, a description), and the managers keep
    them with it (`StackOptions`): a description shown on its card; an env file that fills the file's variables (every
    form Compose has) and environment names given with no value, shown only in the stack's detail; a node label
    selector (`zone=a`, `disk!=hdd`) added to every service's constraints, its workloads moved when a worker's labels
    change; a mode over what the file says (stateless: every workload moves, named volumes local; pinned: every
    workload waits for its worker; shared: every workload moves with its named volumes on the cluster's storage, one
    writer); and container names by service, each one workload's in the cluster and also a name for it inside the
    stack. Each option left out of a deploy stays as it was; a null mode goes back to the file. The deployer sends
    them with its check too, so a selector no worker meets or a name taken is said before anything is deployed. The UI
    server passes on only these five keys beside the name, file, nodes and exposures.
23. Every setting of the cluster is an Administrator's to see and change from the UI (owner, 2026-11-13: "every setting
    exposed via the UI, an integrations tab where I can do everything I need"). The Integrations page has the shares
    (21, moved there), the fencing provider (Proxmox: its address, an API token and the certificate's fingerprint; the
    fingerprint read from the server by a manager, to be compared with Proxmox's own before it is pinned; tried from a
    manager, which answers Proxmox's version and each worker's guest as Proxmox sees it; how each worker is fenced, as
    its labels), the timings (heartbeat, suspect, unreachable, the self-fencing timers, recovery delay; fencing and
    automatic authorization as switches that save at once) and what each manager's environment sets, shown and not
    changed, since a node needs it before it can reach the cluster. The values are the managers' replicated settings:
    one change is every manager's, and each says where it comes from (the cluster, this manager's environment, the
    default); one the cluster set can go back to the managers' own. The token is sent once and never comes back, so the
    activity log names the settings changed (`cluster.settings.change`), never their values. All four routes are an
    Administrator's (`GET`/`PUT /api/cluster/settings`, `POST /api/cluster/settings/proxmox/fingerprint` and `/test`);
    the last two change nothing and only ask from a manager, which is the one that talks to Proxmox.
24. Backups of the cluster are the managers' and are taken from the Integrations page (owner's roadmap, 2026-11-13):
    the replicated state (this UI's database with it) and the cluster CA, sealed by a manager with a passphrase. One
    now is a download sealed with a passphrase typed twice and sent once (`POST /api/cluster/backup`), kept nowhere,
    never cached (`Cache-Control: no-store`). Scheduled ones are each manager's: how often and how many are settings
    (23), and their passphrase is a write-only setting, shown only as set or not. The stored ones listed are the
    answering manager's (`GET /api/cluster/backups`), each downloaded as it is (`GET /api/cluster/backups/{name}`,
    only a name the managers make goes into a manager's path). All three are an Administrator's, even to list, since
    a backup with its passphrase is the whole cluster; each download is on record (`cluster.backup.download`), never
    with the passphrase. Bringing one back is a manager's command on a new manager (`freightdock-manager restore`),
    not this UI's: there is no cluster for it to talk to then.
25. A node's Docker comes back as a page of the node, through its worker (owner, 2026-11-12: "kept, rebuilt to go
    through the workers"; the direct-Docker pages went with the standalone mode, 10). Its containers, images, volumes
    and networks are asked of the worker when the page is drawn (the managers' `DockerRequest`, never in the cluster's
    log), with a container's logs (at most 5000 lines) and inspect. The changes are start, stop, restart and remove a
    container, pull, remove and prune images, remove a volume or a network. The cluster's own (a workload's container,
    a stack's network or volume on the cluster's storage) are shown and marked but never changed here: the worker
    refuses, since they belong to their stack and the reconcile would undo it. Docker's own networks stay. Both routes
    are an Administrator's, a look too, since a container's inspect and logs may hold its secrets
    (`GET`/`POST /api/cluster/nodes/{id}/docker`); only the keys known go on to the manager, and each change is on
    record (`cluster.docker`, the action and what it was on). A change runs to its end in a task of its own, on the
    managers and here: a browser that leaves before the answer would otherwise leave it done on the node and nowhere
    on record.
26. Private registries are the cluster's, set on the Integrations page (owner's roadmap, 2026-11-13): a login per
    registry, named as images name it (`ghcr.io`, `registry.example:5000`, `docker.io`), kept in the managers'
    replicated state. The password or token goes in once and never comes back, like the Proxmox token (23); a change
    may leave it out to keep it. The manager that sends a run or a page's pull fills the login in for the image's
    registry, so it is never kept with a command. A login can be tried from a manager first (it signs in as Docker
    would and pulls nothing). All four routes are an Administrator's, a look too
    (`GET /api/cluster/registries`, `PUT`/`DELETE /api/cluster/registries/{host}`, `POST .../test`); only a registry's
    name goes into a manager's path, and the log names the registry and the user, never the password
    (`cluster.registry.set`, `cluster.registry.remove`).
27. Notifications come back through the managers, set on the Integrations page (owner's roadmap, 2026-11-13), and
    not in the UI: the UI is a workload itself and would be gone exactly when its node goes down. A channel (a
    webhook, e-mail over SMTP, an ntfy topic, a Gotify server) is kept in the managers' replicated state with the
    events it wants; its one secret (signing key, token, SMTP password) goes in once and never comes back (a change may
    leave it out to keep it, or take it away). The Raft leader watches node health, fencing, recoveries, workload
    containers, failed runs and the managers, and tells each change once; a manager tells its own failed backup. The
    same event about the same thing is told at most once in ten minutes ("Notifications" 3, in memory, so a new leader
    may tell once more). A container "died" when it stopped with a code other than 0 while its workload should run
    there with that spec: a stop, a replaced generation or a stopped stack is no news ("Notifications" 2 without
    Docker's events, which the managers do not see). A webhook is signed as "Notifications" 4 says. A test is sent
    through the channel as saved, and a failure says why without the address's query ("Notifications" 5). All four
    routes are an Administrator's, a look too (`GET /api/cluster/notifications`, `PUT`/`DELETE
    /api/cluster/notifications/{name}`, `POST .../test`); only a channel's name goes into a manager's path, and the log
    names the channel and its kind, never its secret (`cluster.notification.set`, `.remove`, `.test` with whether it
    went through, as a test reaches outside). Not yet told: a failing health check, which the workers do not report.
28. Shared storage shows its health per node on the Shares card (G11.8, 2026-11-13): each node that made a storage
    ready probes it (an NFS export still mounted, its space, a file written and read back; a probe stuck past 10 s is
    said as such) and the managers pass it on in `GET /api/cluster/storage` (`health`). Only the nodes that use a
    storage answer for it, so one no workload has used yet says so instead of a guess. The managers place no new
    workload with volumes on a storage on a node where it fails, and tell `storage.unhealthy` and `storage.back`
    through the notification channels (27).
29. A bare-metal worker is fenced through its BMC with Redfish (G12.5, 2026-11-14), set on the Integrations page
    beside Proxmox: one account for every BMC (the cluster's `redfish` setting, the password never shown) and, per
    worker, its BMC's address and pinned certificate as labels (`redfish.host`, `redfish.fingerprint`,
    `redfish.system`), read from the BMC by a manager like Proxmox's. Why one account: a cluster's BMCs are set up
    alike, and a password per node would be a secret in a label, which every reader of the nodes sees. The managers
    power the system off and call the worker fenced only once the BMC says its power is off.
30. A node's name given here is the managers' too (2026-11-14): renaming a node also sets its `name` label (its other
    labels kept), and an empty name takes the label away. Why: the managers call a node by its `name` label in their
    notifications, CLI, metrics and errors, and two names for one node (one here, another there) read as two nodes.
    The managers are told first, so a name they refuse is not given here either. It amends 12: the name is no
    longer Freightdock's alone, and a stack placed by `node.labels.name` follows the new name at its next deploy.
31. A new stack can be made without being started (2026-11-14): "Make it without starting it" when it is deployed, the
    managers' `"stopped": true`. It is placed and kept, and runs once started on the Stacks page. Why: to look a stack
    over before it runs, and for one whose old copy still runs elsewhere (the Portainer import, "Portainer" 12); the
    end-to-end suite found the option was dropped on its way to the managers.
32. The end-to-end suite runs against a real cluster made for it (2026-11-14): `qa/cluster.sh` starts a manager and a
    worker in one privileged `docker:dind` container, from the static binaries, with the UI beside them on the
    manager's admin token; `qa/e2e.mjs` drives the UI's API as the browser does (setup, sign-in, the node, stacks,
    roles, MCP, the activity log) and takes away what it made. Why one throwaway container: it needs no test cluster,
    no LAN address and nothing of the host but Docker, so it runs anywhere the old `docker:dind` suite ran. The UI runs
    beside the manager rather than as its system workload, which needs an address of a LAN's pool.
33. Screenshots are made by a script again (2026-11-14; "Project" 9 back): `qa/screenshots.sh` makes a throwaway
    cluster as the suite does (its own name and port), gives it invented stacks, a node name and a user through the
    API, and takes each page with headless Chromium in the light and the dark theme into `docs/screenshots/`, refusing
    a page that says null, undefined or NaN, shows an error or has a broken image. They are not committed yet: the
    node's figures (CPUs, memory, disk) are those of the machine that made them, and putting a picture of somebody's
    own machine in the public repository is the owner's choice ("Project" 6).

34. Ceph is set up, and a CephFS's keys given, from the Integrations page (2026-11-14; the managers' G12.6): a Ceph
    card under the shares shows the setup the managers are making or made last (`/api/cluster/ceph/setup`), each step
    with its node and what it said, asked again every three seconds while it runs; an administrator runs a failed step
    again or forgets the setup (what it made stays). "Set up Ceph" asks every node that is heard, not fenced and runs
    Docker for its disks (the node Docker read `disks`) and offers only those the node says are free; a node with none
    says why in one line (a container sees its host's disks). The nodes go in the order their first disk was ticked,
    the first bootstrapping, and starting asks once more, naming every disk, since each is erased. The managers check
    again that each disk is free. The storage dialog takes a `cephfs` storage with its mount key and the optional
    evicting client and key, and a CephFS row has a Keys dialog (`PUT /api/cluster/storage/{name}/keys`): password
    fields, a key left empty keeping the one set, passed to the managers and never kept here, sent back or put in the
    activity log, where the setup is on record with its filesystem, nodes and disks. The setup is said to be started by
    the user who started it.

35. Shells on a node and in its containers (2026-11-14): an administrator opens one from the node's page (the host's,
    a login shell as root) or from a running container (the node's page or its Docker page), on a page of its own,
    `/cluster/nodes/{id}/shell?container=`, drawn by xterm.js (vendored like CodeMirror, MIT, pre-built) in FinUI's
    tokens read as the page draws them. Freightdock first asks whether it may be opened, since a browser is never told
    why a WebSocket failed, then opens one from its own origin only (a WebSocket is not held to CORS, so a missing or
    foreign `Origin` is refused) and splices it, bytes as they are, to the manager's: Freightdock frames nothing and
    keeps nothing typed or written. The opening is in the activity log (`cluster.shell`, the node and what on it) and
    is an event on the managers. The cluster's `shells` setting (on by default, in Integrations beside fencing) turns
    them off for everybody; a node's owner turns the host's off on the node itself (`FREIGHTDOCK_WORKER_HOST_SHELL=off`),
    which no setting overrides. Reason: a shell is what an administrator needs to look at a node or a container
    without leaving the UI, and an administrator can already run any container, so it adds no power, only a way in
    that is on record.

36. Upgrades from the UI (2026-11-15): an administrator gives the cluster its release on the Upgrades page
    (`/cluster/upgrades`), a worker and a manager binary uploaded as they are (passed to a manager, never kept here) or
    a published release fetched by its tag, which the manager checks against the release's SHA256SUMS; then starts the
    upgrade and follows it. The managers roll it out a node at a time, workers first and the leader last, each back on
    the new version and healthy before the next, and stop at the first that is not; it can be stopped. Every manager's
    and worker's version is listed, those behind marked, and a worker too old to take an upgrade is said to be
    upgraded by hand. Giving a release, fetching one, starting and stopping are in the activity log
    (`cluster.release.upload`, `cluster.release.fetch`, `cluster.upgrade.start`, `cluster.upgrade.stop`). Reason: an
    upgrade by hand is a login on every node, and a node forgotten is a node behind; one from here is the same binary
    everywhere, checked, in an order that keeps a majority of managers and every container running.
37. SMB storage (2026-11-15, G11.9): an `smb` storage is `//host/share` with `username=` (or `guest`) in its options
    and the password in a field of its own, typed in the add dialog or set anew with **Password**
    (`PUT /api/cluster/storage/{name}/password`). As with a CephFS's keys ("Cluster" 34) the password goes straight
    to the managers, is never kept here, never shown (the list says only "password set" or "guest") and never on
    record (`cluster.storage.password` names the storage only). Reason: a NAS account usually reaches more than the
    share; it belongs where the CephFS keys are, in the managers' replicated state, handed to a worker only in the
    mount call.
