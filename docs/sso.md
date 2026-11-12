# Single sign-on with OpenID Connect

Freightdock signs people in through any OpenID Connect provider: Authentik, Keycloak, Microsoft Entra ID, Authelia,
Zitadel, Google and the rest. The password sign-in always stays beside it, for when the provider (or what it runs on)
is down.

How it decides who comes in: the **groups** claim. A member of an administrator group is an Administrator, a member of a
user group is a user (with the roles given to them per environment, [access.md](access.md)), and anybody else is turned
away. The role is set again at every sign-in, so taking somebody out of the group at the provider takes them out here at
their next sign-in.

## What Freightdock needs from the provider

| Freightdock field | What |
|---|---|
| **Issuer** | The provider's issuer URL, without `/.well-known/openid-configuration`. |
| **Client ID**, **Client secret** | A confidential client (the authorization-code flow; Freightdock also uses PKCE). The secret is sealed. |
| **Redirect URI** | `https://freightdock.example.com/api/auth/oidc/callback`, set exactly so at the provider (strict). Freightdock fills in its own. |
| **Scopes** | `openid email profile` by default; `openid` is required. |
| **Name claim** | Where a person's Freightdock name comes from: `preferred_username` by default. |
| **Groups claim** | `groups` by default. |
| **Administrator groups**, **User groups** | Comma-separated. The defaults are `freightdock-write` and `freightdock-read`. With both empty, everybody the provider lets through is a user. |
| **Sign in to the local account with the same name** | On by default: a person's first sign-in lands in the local account of their name (if nobody is linked to it yet) instead of a new "name (SSO)" account. Turn it off if people choose their own names at the provider. |
| **Button name** | The sign-in page says "Sign in with" this. |

Freightdock needs HTTPS for the redirect URI (or `localhost`).

## Authentik, step by step

Placeholders: Authentik at `https://auth.example.com`, Freightdock at `https://freightdock.example.com`.

1. **Groups.** Directory, Groups: make `freightdock-write` and `freightdock-read`, and put people in them.
2. **Provider.** Applications, Providers, Create, **OAuth2/OpenID Provider**:
   - Name `freightdock`; an authorization flow (the default explicit or implicit consent);
   - Client type **Confidential**; note the Client ID and Client Secret;
   - Redirect URIs: **Strict**, `https://freightdock.example.com/api/auth/oidc/callback`;
   - Signing key: choose one (a certificate). Without one Authentik signs with the client secret (HS256), which
     Freightdock also accepts;
   - Scopes: `openid`, `email`, `profile` (Authentik's `profile` scope carries `groups`).
3. **Application.** Applications, Create: name `Freightdock`, slug `freightdock`, provider `freightdock`. Bind the two groups
   (or a policy) so only they can open it.
4. **Freightdock.** Settings, **Single sign-on**:
   - Issuer `https://auth.example.com/application/o/freightdock/` (with the slug and the trailing slash), then **Test the issuer**,
     which asks the issuer's discovery document;
   - the Client ID and Client Secret;
   - leave the redirect URI, scopes and claims as they are; the groups as `freightdock-write` and `freightdock-read`;
   - switch **Single sign-on** on, and Save.
5. Sign out and use **Sign in with Authentik** on the sign-in page.

A local administrator called `alice` who signs in through Authentik as `alice` lands in that same account, which is
linked from then on (found by Authentik's subject, never again by name). Its password keeps working.

## Keycloak

- Issuer: `https://keycloak.example.com/realms/<realm>`.
- Client: OpenID Connect, **Client authentication** on (confidential), **Standard flow** on, Valid redirect URI exactly
  as above.
- Groups: Keycloak sends none by default. In the client's dedicated scope, add a mapper **Group Membership** with token
  claim name `groups`, **Full group path** off, added to the ID token and userinfo.

## Microsoft Entra ID

- Issuer: `https://login.microsoftonline.com/<tenant-id>/v2.0`.
- App registration: a **Web** redirect URI as above; a client secret under Certificates & secrets.
- Groups: Token configuration, **Add groups claim** (security groups). Entra sends group **object IDs**, not names, so
  put the IDs in Freightdock's group fields. Or use **app roles** and set Freightdock's groups claim to `roles`, with the role
  values in the group fields.
- Name claim: `preferred_username` is the sign-in name (`alice@example.com`); that is the Freightdock user's name.
- A person in more than 200 groups gets no groups claim (Entra's overage): use app roles then.

## Linking an existing account

A signed-in local user can link their account to the provider from **Account**, **Single sign-on**: they sign in at the
provider once, and from then on either way works. Linking does not change their role until their next SSO sign-in.

## Signing out

Signing out of a session that came through the provider also goes to the provider's end-session address, so the next
sign-in asks again.

## When it does not work

The sign-in page says why in words, and the **Auth log** has every refusal with its reason: not in a group, a redirect
URI the provider does not know, a clock off by more than a minute (the ID token's times are checked), a disabled
account (disabled here stays out through SSO too).
