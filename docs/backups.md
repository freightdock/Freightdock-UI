# Backups and restore

A backup is Freightdock's own data, all it needs to stand up again: the database (copied while Freightdock runs, nothing
stops), `secret.key` (without it no sealed secret opens), the agents' CA (without it no agent's certificate is good),
the TLS certificate, and the stacks' files.

It is not your containers or their volumes: those are Docker's, and are backed up as Docker's.

## Set it up

**Backups** (administrators):

| | |
|---|---|
| **Back up on a schedule** | On, and every how many hours. |
| **Keep** | How many of Freightdock's archives to keep; the oldest beyond that go. Only Freightdock's own names (`freightdock-YYYYMMDD-HHMMSS.backup`) are counted or removed. |
| **Destination** | **A folder** on Freightdock's side (mount one: a disk, an NFS share), or **S3**. |
| **Passphrase** | At least 12 characters. Every archive is sealed with it. |

A folder must be mounted into the container:

```sh
docker run ... -v /srv/backups/freightdock:/backups ghcr.io/freightdock/freightdock:latest
```

and the folder field says `/backups`.

**S3** is any S3-compatible store (AWS, MinIO, Backblaze B2, Cloudflare R2, Garage): the endpoint
(`https://s3.eu-west-1.amazonaws.com`, `https://minio.example.com`), the region, the bucket (made when it is not
there), a prefix if you like, an access key and its secret. Addresses are path-style unless you tick
**Virtual-hosted addresses**. The secret is sealed and never shown again.

**Back up now** makes one at once. The page lists the archives at the destination and how the last run went.

**Keep the passphrase somewhere else.** An archive holds `secret.key`, so the passphrase is all that guards everything
in it, and without it the archive is useless. Freightdock keeps it sealed for the schedule, but a restore asks for it
typed.

How it is sealed: an Argon2id key from the passphrase (64 MiB, three passes, a random salt) and XChaCha20-Poly1305 over
1 MiB chunks. A changed, reordered or cut-off archive does not open; a wrong passphrase looks the same as damage.

## Restore from the page

On **Backups**, **Restore** beside an archive at the destination, or drop an archive made elsewhere onto the page
(1 GB at most). Type the passphrase. Freightdock checks the archive (only its own files in it, the database whole and not
newer than this Freightdock), stages it, and starts itself again with it in place. Sign in again afterwards.

What it replaced is moved to `/data/before-restore-<time>/`, not deleted, so a restore can be undone by hand.

## Restore with Freightdock stopped

For a new machine, or when Freightdock will not start:

```sh
docker stop freightdock
docker run --rm \
  -v freightdock:/data \
  -v /srv/backups/freightdock:/in:ro \
  -e FREIGHTDOCK_BACKUP_PASSPHRASE='your passphrase' \
  ghcr.io/freightdock/freightdock:latest restore /in/freightdock-20261110-072859.backup
docker start freightdock
```

`freightdock restore` checks and stages the archive:

```
... it is put in place when Freightdock next starts with FREIGHTDOCK_DATA_DIR=/data.
```

and the next start puts it in place before the database opens:

```
INFO freightdock: restored a backup from freightdock-20261110-072859.backup, made 2026-11-10 07:28:59 UTC, by freightdock restore; what it replaced is in /data/before-restore-20261110-072913
```

Without Docker: `FREIGHTDOCK_DATA_DIR=./data FREIGHTDOCK_BACKUP_PASSPHRASE='...' freightdock restore <archive>`, then start it.

## Moving Freightdock to another machine

1. Make a backup and copy the archive over.
2. On the new machine, restore it into a new volume as above, then start Freightdock there.
3. If its address changed, agents dial the old one: start each agent again with the new `FREIGHTDOCK_SERVER` (their
   certificates stay good, since the CA came along). A TLS host needs nothing.
4. Single sign-on: change the redirect URI at the provider and in Settings, if the address changed.

## Before an upgrade

An older Freightdock refuses a database a newer one has changed. Make a backup before every upgrade; going back is the
older image and that backup.
