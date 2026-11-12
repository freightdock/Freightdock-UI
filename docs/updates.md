# Image updates

Freightdock tells you when a container's tag has a newer image in its registry, and can update the containers that ask
for it.

## How it checks

Every quarter of an hour Freightdock goes over every environment, and asks a registry about a tag at most every few hours
(six by default): what the tag points at now, a HEAD request for the manifest,
which Docker Hub does not count as a pull. That digest is compared with the image's own. A multi-arch image is looked
into for this machine's platform. Nothing is pulled to check.

- Each reference is asked once per pass, however many containers and hosts run it, and the answer is kept, so rate
  limits are never near.
- Private images are checked with the saved registry's sign-in ([registries.md](registries.md)). A registry that does
  not answer leaves the last answer with the error beside it, never a wrong badge.

The badges on containers and images:

| | |
|---|---|
| **update** | the tag points at a newer image (hover it for the detail) |
| not checked | the registry could not be asked or refused (the reason is on hover) |
| nothing | up to date, or pinned (a reference by digest has nothing newer under it) |

**Settings**, **Image updates**: on or off, how often (1 to 168 hours), and automatic updates. **Check now** asks again
at once (an answer younger than a minute is used again).

A container newly behind is told to the channels that want `update.available` ([notifications.md](notifications.md)).

## Updating

**Update now** on a container's page pulls its tag and makes the container again with the same settings. The new container
must still be up ten seconds after it started (running, not restarting, not unhealthy); if it is not, it goes and the
old one is put back and started. An update that fails leaves the container as it was, and the activity log says why.

A container in a stack is better updated with the stack's **Pull and redeploy**, which keeps Compose in charge of it.

## Automatic updates

Opt-in, per container. Nothing is updated that nobody asked for. Either:

- the label, set where the container is made, so it goes with the compose file:

  ```yaml
  services:
    web:
      image: nginx:alpine
      labels:
        - freightdock.auto-update=true
  ```

- or the switch **Update it by itself when a newer image is out** on the container's page.

With **Automatic updates** on in Settings, every container that opted in and is behind is updated after each check, the
same way as by hand, put back if the new one does not stay up. The activity log has each as `container.autoupdate`.
