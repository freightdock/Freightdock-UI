# Resource history and the volume browser

## Resource history

Freightdock samples every running container's CPU, memory, network and disk into its own database, on every environment
that answers. A container's **History** tab draws the last hour, 6 hours, a day, a week or 30 days, whether it is
running now or not.

**Settings**, **Resource history**: on or off, how often to sample (10 to 3600 seconds; a minute by default) and how
many days to keep (1 to 365; a week by default). A change holds at once.

- Samples are kept by the container's name, so its history goes on across an edit, a recreate or an update. A removed
  container's history stays until it is old enough to go.
- CPU is a percentage of one CPU (two busy CPUs are 200%). Memory is counted as `docker stats` counts it, without the
  page cache.
- Network and disk are kept as Docker's counters and drawn as rates. A restart (a counter that went back) or a gap of
  more than ten minutes draws nothing there, rather than a spike.
- It needs nothing else installed, and it goes with a backup.

The live **Stats** tab is apart from this: every 5 seconds, or 1, 10 or 30 if you pick it.

## The volume browser

**Volumes**, a volume, **Files**: its folders and files, and download a file (or a folder as a tar), upload files, make
folders, delete.

It works on every kind of environment the same way: each request starts a short-lived helper container (`busybox:1.37`,
pulled on a host the first time) with the volume at `/volume`, no network and no other mount, and removes it when the
request is done. Nothing new runs on the host, and nothing is needed in the agent.

- Only an Administrator or an Environment administrator may browse, reading too: a volume holds whatever its containers
  keep (keys, databases), and volumes are nobody's in particular.
- A path with `..` or a control character is refused, and a symbolic link in the volume points inside the helper,
  never at the host.
- An upload is at most 256 MB. A download is streamed, with no limit. A listing shows at most 5000 entries and says when
  there were more.
- Every upload, download, new folder and delete is in the activity log.

Writing into a running database's volume can break the database: stop its container first.
