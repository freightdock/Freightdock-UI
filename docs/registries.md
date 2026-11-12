# Registries

A registry saved in Freightdock is a host and how to sign in there. Saved once, it is used on every environment: pulls,
pushes, new and recreated containers, stack deploys and pulls, and the image update check sign in to the registry an
image is on by themselves. You never put a password in a compose file.

## Add one

**Registries**, **Add registry** (administrators). The **Kind** fills in the address and says what the sign-in is called:

| Kind | Address | User name | Password |
|---|---|---|---|
| Docker Hub | `docker.io` | your Docker ID | an access token (Account settings, Personal access tokens) |
| GitHub (ghcr.io) | `ghcr.io` | your GitHub user | a token with `read:packages` (and `write:packages` to push) |
| GitLab | `registry.gitlab.com`, or your own | your user, or a deploy token's name | a personal or deploy token with `read_registry` |
| Amazon ECR | `<account>.dkr.ecr.<region>.amazonaws.com` | an access key id | its secret access key, and the **Region** |
| Quay | `quay.io` | your user or a robot account | its password or token |
| Another registry | `registry.example.com`, `registry.example.com:5000`, or `http://192.0.2.5:5000` for plain HTTP | | |

Saving tests the sign-in first, as a pull would. A registry that does not answer, or refuses the sign-in, is
not saved, and the registry's own reason is shown on the field it is about. The password, token or secret key is sealed
and never sent back; editing, an empty field keeps it.

Notes:

- One registry a host: Docker takes one sign-in per host.
- An image's registry is read as Docker reads it: the first part of the reference when it has a dot or a colon or is
  `localhost`, else Docker Hub. `nginx` and `docker.io/library/nginx` are both Docker Hub.
- ECR's keys are exchanged for its 12-hour token as needed (AWS Signature Version 4), kept in memory only.
- A registry is reached over HTTPS with its certificate checked. Plain HTTP only when its address says `http://`, and
  Docker on each host must also list it as an insecure registry (127.0.0.0/8 is one by default). A registry with a
  certificate from your own CA cannot be added yet.

Everybody signed in sees the list and may browse; only administrators add, change or remove.

## Browse

Choose a registry to list its repositories, then a repository's tags. With a host chosen on Home, a tag can be pulled
onto that host from there.

- Docker Hub lists no catalog, so its repositories come from Hub's own API: your account's, or the **Account or
  organisation** you name (`library` for the official images).
- GHCR, GitLab and ECR list no catalog through the registry API: type a repository's name to list its tags.
- Any other registry (`registry:2`, Harbor, Gitea) lists its catalog, page by page.
