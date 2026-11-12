//! Remote environments as local unix sockets ("Endpoints" 1 in DECISIONS.md). For each agent tunnel, and each TLS host,
//! Freightdock listens on `<data>/run/env-<id>.sock` (the folder 0700) and joins every connection made to it to a new
//! connection to the host: a yamux stream through the agent's tunnel, or TCP with the host's client certificate. bollard
//! then speaks to the socket as to any local Docker, and so, later, will `docker compose`.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;

use super::Docker;

/// A connection to a host, however it is made.
pub trait Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Io for T {}

/// The longest a unix socket's address may be (`sun_path` is 108 bytes with its closing NUL).
const MAX_PATH: usize = 108;

pub type DialFuture = Pin<Box<dyn Future<Output = io::Result<Box<dyn Io>>> + Send>>;

/// How a connection to the host is made, each time something connects to the socket.
pub trait Dial: Send + Sync + 'static {
    fn dial(&self) -> DialFuture;
}

impl<F: Fn() -> DialFuture + Send + Sync + 'static> Dial for F {
    fn dial(&self) -> DialFuture {
        self()
    }
}

struct Live {
    generation: u64,
    docker: Docker,
    stop: CancellationToken,
    since: i64,
    peer: Option<String>,
}

/// One socket offered: its generation (a later offer for the same environment replaces it), the token that ends it, and
/// a client for it.
pub struct Offered {
    pub generation: u64,
    pub stop: CancellationToken,
    pub docker: Docker,
}

pub struct Sockets {
    dir: PathBuf,
    live: Mutex<HashMap<i64, Live>>,
    generation: AtomicU64,
    /// Only one TLS bridge is made at a time, so two first requests do not both bind the socket.
    pub(crate) making: tokio::sync::Mutex<()>,
    /// A test's folder, removed with it.
    #[cfg(test)]
    _scratch: Option<tempfile::TempDir>,
}

impl Sockets {
    /// The sockets' folder, made 0700 and emptied of the sockets an earlier run left. A unix socket's address is at
    /// most 107 bytes; when `dir` is too deep for one, a short folder of this run's own in the system's temporary folder
    /// is used instead ("Endpoints" 9).
    pub fn new(dir: &Path) -> Result<Self> {
        let probe = dir.join("env-9999999999.sock");
        let dir = if probe.as_os_str().len() < MAX_PATH {
            dir.to_path_buf()
        } else {
            let short = own_temp_dir()?;
            tracing::warn!("{} is too deep for a socket's address; the environments' sockets are in {}", dir.display(), short.display());
            short
        };
        let dir = dir.as_path();
        std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).with_context(|| format!("closing {} to others", dir.display()))?;
        }
        for e in std::fs::read_dir(dir)?.flatten() {
            if e.path().extension().is_some_and(|x| x == "sock") {
                let _ = std::fs::remove_file(e.path());
            }
        }
        Ok(Sockets {
            dir: dir.to_path_buf(),
            live: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(1),
            making: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            _scratch: None,
        })
    }

    /// Sockets in a folder of their own that goes when they do.
    #[cfg(test)]
    pub fn for_test() -> Self {
        let scratch = tempfile::tempdir().expect("a temporary folder");
        Self::new(scratch.path()).expect("the sockets' folder").keeping(scratch)
    }

    #[cfg(test)]
    fn keeping(mut self, scratch: tempfile::TempDir) -> Self {
        self._scratch = Some(scratch);
        self
    }

    pub fn path(&self, env: i64) -> PathBuf {
        self.dir.join(format!("env-{env}.sock"))
    }

    /// Listens on environment `env`'s socket and joins every connection to one `dial` makes, until the answer's `stop`
    /// is cancelled or a later offer replaces this one. `peer` is where the host is, as shown to people.
    pub async fn offer(&self, env: i64, peer: Option<String>, dial: impl Dial) -> Result<Offered> {
        let path = self.path(env);
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);
        let stop = CancellationToken::new();
        let docker = {
            let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(old) = live.remove(&env) {
                old.stop.cancel();
            }
            let _ = std::fs::remove_file(&path);
            let listener = tokio::net::UnixListener::bind(&path).with_context(|| format!("listening on {}", path.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            }
            let docker = Docker::connect(&format!("unix://{}", path.display()))?;
            live.insert(env, Live { generation, docker: docker.clone(), stop: stop.clone(), since: crate::db::now(), peer });
            tokio::spawn(accept(listener, std::sync::Arc::new(dial), stop.clone()));
            docker
        };
        Ok(Offered { generation, stop, docker })
    }

    /// A client for environment `env`'s socket, while one is offered.
    pub fn get(&self, env: i64) -> Option<Docker> {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).get(&env).map(|l| l.docker.clone())
    }

    /// Since when the socket is offered, and where the host is.
    pub fn info(&self, env: i64) -> Option<(i64, Option<String>)> {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).get(&env).map(|l| (l.since, l.peer.clone()))
    }

    /// Ends environment `env`'s socket, whichever offer it is: the tunnel is cut, connections through it end.
    pub fn close(&self, env: i64) {
        self.close_if(env, None);
    }

    /// Ends the socket only if it is still offer `generation` (a tunnel that ended must not take its successor down).
    pub fn close_generation(&self, env: i64, generation: u64) {
        self.close_if(env, Some(generation));
    }

    fn close_if(&self, env: i64, generation: Option<u64>) {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        if live.get(&env).is_some_and(|l| generation.is_none_or(|g| g == l.generation)) {
            if let Some(l) = live.remove(&env) {
                l.stop.cancel();
            }
            let _ = std::fs::remove_file(self.path(env));
        }
    }
}

/// A new folder in the shared temporary folder, made by this process alone: a random name, made 0700 by a `mkdir` that
/// fails when anything (a folder, a link) is there already, so nobody can make it first and keep it.
fn own_temp_dir() -> Result<PathBuf> {
    for _ in 0..16 {
        let path = std::env::temp_dir().join(format!("freightdock-{:016x}", rand::random::<u64>()));
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        match b.create(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("making {}", path.display())),
        }
    }
    anyhow::bail!("could not make a folder of Freightdock's own in {}", std::env::temp_dir().display())
}

async fn accept(listener: tokio::net::UnixListener, dial: std::sync::Arc<impl Dial>, stop: CancellationToken) {
    loop {
        let conn = tokio::select! {
            c = listener.accept() => c,
            _ = stop.cancelled() => return,
        };
        let (local, _) = match conn {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("a remote environment's socket could not accept: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                continue;
            }
        };
        let (dial, stop) = (dial.clone(), stop.clone());
        tokio::spawn(async move {
            let remote = tokio::select! {
                r = dial.dial() => r,
                _ = stop.cancelled() => return,
            };
            match remote {
                Ok(remote) => {
                    tokio::select! {
                        _ = freightdock_tunnel::mux::join(local, remote) => {}
                        _ = stop.cancelled() => {}
                    }
                }
                Err(e) => tracing::debug!("could not reach a remote environment: {e}"),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A dial to an echo: what goes in comes back.
    fn echo() -> impl Dial {
        || -> DialFuture {
            Box::pin(async {
                let (a, mut b) = tokio::io::duplex(1024);
                tokio::spawn(async move {
                    let mut buf = [0u8; 64];
                    while let Ok(n) = b.read(&mut buf).await {
                        if n == 0 || b.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
                Ok(Box::new(a) as Box<dyn Io>)
            })
        }
    }

    #[tokio::test]
    async fn a_socket_joins_each_connection_to_the_host_until_it_is_closed() {
        let dir = tempfile::tempdir().unwrap();
        let s = Sockets::new(dir.path()).unwrap();
        let o = s.offer(4, Some("192.0.2.4".into()), echo()).await.unwrap();
        assert_eq!(o.docker.url(), format!("unix://{}", s.path(4).display()));
        assert!(s.get(4).is_some());
        assert_eq!(s.info(4).unwrap().1.as_deref(), Some("192.0.2.4"));

        let mut c = tokio::net::UnixStream::connect(s.path(4)).await.unwrap();
        c.write_all(b"ping").await.unwrap();
        let mut back = [0u8; 4];
        c.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"ping");

        s.close(4);
        assert!(s.get(4).is_none());
        assert!(!s.path(4).exists());
        let mut rest = Vec::new();
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(5), c.read_to_end(&mut rest)).await.unwrap().unwrap(), 0, "a cut tunnel ends what went through it");
    }

    #[tokio::test]
    async fn a_new_offer_replaces_the_old_and_the_old_cannot_close_the_new() {
        let dir = tempfile::tempdir().unwrap();
        let s = Sockets::new(dir.path()).unwrap();
        let first = s.offer(1, None, echo()).await.unwrap();
        let second = s.offer(1, None, echo()).await.unwrap();
        assert!(first.stop.is_cancelled());
        s.close_generation(1, first.generation);
        assert!(s.get(1).is_some() && s.path(1).exists(), "the old tunnel ending leaves the new one");
        s.close_generation(1, second.generation);
        assert!(s.get(1).is_none());
    }

    #[tokio::test]
    async fn a_folder_too_deep_for_a_socket_s_address_gives_way_to_a_short_one() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("x".repeat(120));
        let s = Sockets::new(&deep).unwrap();
        assert!(s.path(1_000_000_000).as_os_str().len() < MAX_PATH);
        assert!(s.offer(1, None, echo()).await.is_ok(), "a socket can be offered");
        let again = Sockets::new(&deep).unwrap();
        assert_ne!(again.dir, s.dir, "a new folder each start, so nobody can make it first");
        let _ = std::fs::remove_dir_all(&again.dir);
        let shallow = dir.path().join("run");
        assert_eq!(Sockets::new(&shallow).unwrap().dir, shallow, "a folder that fits is used as it is");
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_short_folder_somebody_made_first_is_never_taken() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("y".repeat(120));
        // The name the short folder once had, made beforehand by somebody else, with a socket of theirs in it.
        let predicted = std::env::temp_dir().join(format!("freightdock-{}", &freightdock_tunnel::sha256_hex(deep.as_os_str().as_encoded_bytes())[..16]));
        std::fs::create_dir_all(&predicted).unwrap();
        std::fs::write(predicted.join("theirs.sock"), b"").unwrap();
        let s = Sockets::new(&deep).unwrap();
        assert_ne!(s.dir, predicted);
        assert!(predicted.join("theirs.sock").exists(), "nothing in it is touched");
        let m = std::fs::symlink_metadata(&s.dir).unwrap();
        assert!(m.is_dir() && !m.file_type().is_symlink());
        assert_eq!(m.permissions().mode() & 0o777, 0o700);
        assert_eq!(m.uid(), std::fs::metadata(dir.path()).unwrap().uid(), "made by Freightdock itself");
        let _ = std::fs::remove_dir_all(&predicted);
        let _ = std::fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn sockets_left_by_an_earlier_run_are_cleared_and_the_folder_is_closed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("env-9.sock"), b"").unwrap();
        Sockets::new(dir.path()).unwrap();
        assert!(!dir.path().join("env-9.sock").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777, 0o700);
        }
    }
}
