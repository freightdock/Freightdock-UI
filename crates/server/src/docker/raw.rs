//! Docker's Engine API spoken directly, as JSON in and JSON out. bollard has no configs and no service logs, and its
//! typed models keep only the fields it knows: a node's or a service's spec read through them and sent back would
//! lose whatever a newer Docker added. Swarm's specs go to Docker whole ("Swarm" 2 in DECISIONS.md).

use axum::body::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use serde_json::Value;

use super::{Docker, TIMEOUT_S, api_error};
use crate::error::{ApiError, ApiResult};

/// A request to the endpoint's Docker: `path` is the API path with its query (`/services?status=true`), unversioned,
/// so Docker answers in its own version.
async fn send(d: &Docker, method: Method, path: &str, body: Option<&Value>) -> ApiResult<hyper::Response<Incoming>> {
    let mut req = Request::builder().method(method).uri(path).header(hyper::header::HOST, "docker");
    let body = match body {
        Some(v) => {
            req = req.header(hyper::header::CONTENT_TYPE, "application/json");
            Full::new(Bytes::from(v.to_string()))
        }
        None => Full::new(Bytes::new()),
    };
    let req = req.body(body).map_err(|e| ApiError::bad(format!("That is not a request Docker takes: {e}")))?;
    let down = |e: &dyn std::fmt::Display| {
        tracing::warn!("Docker at {}: {e}", d.url());
        ApiError::new(hyper::StatusCode::BAD_GATEWAY, format!("Docker did not answer: {e}"))
    };
    let url = d.url();
    if let Some(path) = url.strip_prefix("unix://") {
        let io = tokio::net::UnixStream::connect(path).await.map_err(|e| down(&e))?;
        over(io, req).await.map_err(|e| down(&e))
    } else {
        let host = url.trim_start_matches("tcp://").trim_start_matches("http://").trim_end_matches('/');
        let io = tokio::net::TcpStream::connect(host).await.map_err(|e| down(&e))?;
        over(io, req).await.map_err(|e| down(&e))
    }
}

async fn over<S>(io: S, req: Request<Full<Bytes>>) -> Result<hyper::Response<Incoming>, hyper::Error>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io)).await?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    sender.send_request(req).await
}

/// The most of one answer that is kept: far more than a swarm of thousands of tasks lists, and a Docker that sends more
/// is not answering as one ("Endpoints" 6). A refusal is a sentence, and kept to far less.
pub const MAX_ANSWER: usize = 64 * 1024 * 1024;
const MAX_REFUSAL: usize = 64 * 1024;

/// The body whole, refused once it passes `max` bytes: a remote environment's Docker is not trusted to end it.
async fn read(body: Incoming, max: usize) -> ApiResult<Bytes> {
    match http_body_util::Limited::new(body, max).collect().await {
        Ok(b) => Ok(b.to_bytes()),
        Err(e) if e.is::<http_body_util::LengthLimitError>() => Err(ApiError::new(hyper::StatusCode::BAD_GATEWAY, format!("Docker's answer is too big (over {} MiB).", max / 1024 / 1024))),
        Err(e) => Err(ApiError::new(hyper::StatusCode::BAD_GATEWAY, format!("Docker's answer broke off: {e}"))),
    }
}

/// Docker's refusal as the API's: its status and its own sentence, as `api_error` says it for bollard.
fn refusal(status: u16, body: &[u8]) -> ApiError {
    let message = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| String::from_utf8_lossy(body).trim().to_string());
    api_error(bollard::errors::Error::DockerResponseServerError { status_code: status, message })
}

/// One call's answer whole, at most `MAX_ANSWER` bytes, once Docker said yes. Held to Docker's usual time limit.
async fn answer(d: &Docker, method: Method, path: &str, body: Option<&Value>) -> ApiResult<Bytes> {
    let work = async {
        let r = send(d, method, path, body).await?;
        let status = r.status();
        let bytes = read(r.into_body(), if status.is_success() { MAX_ANSWER } else { MAX_REFUSAL }).await?;
        if !status.is_success() {
            return Err(refusal(status.as_u16(), &bytes));
        }
        Ok(bytes)
    };
    tokio::time::timeout(std::time::Duration::from_secs(TIMEOUT_S), work).await.unwrap_or_else(|_| Err(ApiError::new(hyper::StatusCode::GATEWAY_TIMEOUT, "Docker took too long to answer.")))
}

fn not_json(e: serde_json::Error) -> ApiError {
    ApiError::new(hyper::StatusCode::BAD_GATEWAY, format!("Docker's answer is not JSON: {e}"))
}

/// One call, its answer as JSON (`null` for an empty one).
pub async fn call(d: &Docker, method: Method, path: &str, body: Option<&Value>) -> ApiResult<Value> {
    let bytes = answer(d, method, path, body).await?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).map_err(not_json)
}

/// One GET, its answer read into `T` (bollard's models, for what is only read): what the background passes ask every
/// environment, so a remote one's answer is held to `MAX_ANSWER` as `call`'s is ("Endpoints" 6).
pub async fn get<T: serde::de::DeserializeOwned>(d: &Docker, path: &str) -> ApiResult<T> {
    serde_json::from_slice(&answer(d, Method::GET, path, None).await?).map_err(not_json)
}

/// A GET whose answer flows for as long as it lasts (a service's logs, followed): its body, once Docker said yes.
pub async fn stream(d: &Docker, path: &str) -> ApiResult<Incoming> {
    let r = send(d, Method::GET, path, None).await?;
    let status = r.status();
    if !status.is_success() {
        let bytes = read(r.into_body(), MAX_REFUSAL).await.unwrap_or_default();
        return Err(refusal(status.as_u16(), &bytes));
    }
    Ok(r.into_body())
}

/// The longest line of a JSON-lines stream (events, stats) waited for: one of Docker's is a few KiB, and one longer is
/// a Docker not speaking Docker ("Endpoints" 7).
pub const MAX_LINE: usize = 1024 * 1024;

/// A JSON-lines body taken apart at its newlines, each byte looked at once, so a line is parsed once it is whole and
/// never before. bollard's decoder instead parses the whole unfinished line again at every read and keeps it however
/// long it gets, which a remote Docker can turn into time growing with the square of what it sends.
#[derive(Default)]
pub struct Lines {
    buf: Vec<u8>,
    /// How much of `buf` is known to hold no newline.
    scanned: usize,
}

impl Lines {
    /// The lines `bytes` completes (blank ones left out), or a refusal once the line still open passes `MAX_LINE`.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, ApiError> {
        self.buf.extend_from_slice(bytes);
        let (mut out, mut from) = (vec![], 0);
        while let Some(i) = self.buf[self.scanned..].iter().position(|b| *b == b'\n') {
            let end = self.scanned + i;
            let line = &self.buf[from..end];
            if !line.iter().all(u8::is_ascii_whitespace) {
                out.push(line.to_vec());
            }
            from = end + 1;
            self.scanned = from;
        }
        self.buf.drain(..from);
        self.scanned = self.buf.len();
        if self.buf.len() > MAX_LINE {
            self.buf = Vec::new();
            return Err(ApiError::new(hyper::StatusCode::BAD_GATEWAY, format!("Docker sent a line too long to be one of its own (over {} MiB).", MAX_LINE / 1024 / 1024)));
        }
        Ok(out)
    }

    /// The last line, when the body ended without a newline after it (a single answer, as stats without `stream`).
    pub fn finish(self) -> Option<Vec<u8>> {
        (!self.buf.iter().all(u8::is_ascii_whitespace)).then_some(self.buf)
    }
}

/// A GET whose answer is JSON lines (events, stats), followed for as long as it lasts: each line read into `T`, and
/// the stream ends after an error (a line over `MAX_LINE`, one that is not JSON, the body broken off).
pub async fn follow<T: serde::de::DeserializeOwned + Send + 'static>(d: &Docker, path: &str) -> ApiResult<impl futures_util::Stream<Item = ApiResult<T>> + Send + use<T>> {
    use futures_util::StreamExt;
    let frames = stream(d, path).await?.into_data_stream();
    let parse = |line: Vec<u8>| serde_json::from_slice::<T>(&line).map_err(not_json);
    Ok(futures_util::stream::unfold((frames, Some(Lines::default()), std::collections::VecDeque::new()), move |(mut frames, mut lines, mut queue)| async move {
        loop {
            if let Some(item) = queue.pop_front() {
                return Some((item, (frames, lines, queue)));
            }
            let open = lines.as_mut()?;
            match frames.next().await {
                Some(Ok(bytes)) => match open.push(&bytes) {
                    Ok(done) => queue.extend(done.into_iter().map(parse)),
                    Err(e) => {
                        queue.push_back(Err(e));
                        lines = None;
                    }
                },
                Some(Err(e)) => {
                    queue.push_back(Err(ApiError::new(hyper::StatusCode::BAD_GATEWAY, format!("Docker's answer broke off: {e}"))));
                    lines = None;
                }
                None => {
                    queue.extend(lines.take().and_then(Lines::finish).map(parse));
                }
            }
        }
    }))
}

/// A path with its query, each value escaped.
pub fn path(base: &str, query: &[(&str, &str)]) -> String {
    if query.is_empty() {
        return base.to_string();
    }
    let mut s = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in query {
        s.append_pair(k, v);
    }
    format!("{base}?{}", s.finish())
}

/// Docker's log stream without a TTY is framed: eight bytes (the stream, three zeros, the length as a big-endian u32)
/// before each piece. With a TTY it is the bytes alone. `push` takes what came and answers the pieces it completes,
/// or why the stream cannot go on (a frame over `MAX_FRAME`).
/// The longest frame waited for: Docker frames each write of a log (a line, cut at 16 KiB by its log drivers), so one
/// that says it is longer is a Docker not speaking Docker, and its bytes are not kept.
pub const MAX_FRAME: usize = 1024 * 1024;

#[derive(Default)]
pub struct Demux {
    tty: bool,
    buf: Vec<u8>,
}

impl Demux {
    pub fn new(tty: bool) -> Self {
        Demux { tty, buf: Vec::new() }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<(&'static str, Vec<u8>)>, String> {
        if self.tty {
            return Ok(if bytes.is_empty() { vec![] } else { vec![("out", bytes.to_vec())] });
        }
        self.buf.extend_from_slice(bytes);
        let mut out = vec![];
        loop {
            if self.buf.len() < 8 {
                break;
            }
            let len = u32::from_be_bytes([self.buf[4], self.buf[5], self.buf[6], self.buf[7]]) as usize;
            if len > MAX_FRAME {
                self.buf.clear();
                return Err(format!("Docker sent a log frame of {len} bytes, more than any it sends."));
            }
            if self.buf.len() < 8 + len {
                break;
            }
            let stream = match self.buf[0] {
                2 => "err",
                0 => "in",
                _ => "out",
            };
            let piece: Vec<u8> = self.buf[8..8 + len].to_vec();
            self.buf.drain(..8 + len);
            if stream != "in" {
                out.push((stream, piece));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn frame(stream: u8, text: &str) -> Vec<u8> {
        let mut f = vec![stream, 0, 0, 0];
        f.extend_from_slice(&(text.len() as u32).to_be_bytes());
        f.extend_from_slice(text.as_bytes());
        f
    }

    #[test]
    fn framed_logs_come_apart_by_stream_even_when_a_frame_is_split() {
        let mut d = Demux::new(false);
        let mut all = frame(1, "hello\n");
        all.extend(frame(2, "oops\n"));
        let (a, b) = all.split_at(10);
        assert!(d.push(a).unwrap().is_empty(), "nothing whole yet");
        let got = d.push(b).unwrap();
        assert_eq!(got, vec![("out", b"hello\n".to_vec()), ("err", b"oops\n".to_vec())]);
        assert!(d.push(&[]).unwrap().is_empty());
    }

    #[test]
    fn a_frame_longer_than_any_docker_sends_ends_the_logs() {
        let mut d = Demux::new(false);
        let mut header = vec![1, 0, 0, 0];
        header.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(d.push(&header).is_err(), "refused at its header, before its bytes are kept");
        let mut d = Demux::new(false);
        let mut f = vec![1, 0, 0, 0];
        f.extend_from_slice(&(MAX_FRAME as u32).to_be_bytes());
        assert!(d.push(&f).unwrap().is_empty(), "the largest frame Docker may send is waited for");
    }

    /// A Docker on a unix socket that answers every request with an endless chunked JSON array.
    pub(crate) fn endless_docker(dir: &std::path::Path) -> Docker {
        use tokio::io::AsyncWriteExt;
        let sock = dir.join("d.sock");
        let l = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    use tokio::io::AsyncReadExt;
                    let mut req = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                        match s.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => req.extend_from_slice(&buf[..n]),
                        }
                    }
                    let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n1\r\n[\r\n").await;
                    let chunk = format!("{:x}\r\n{}\r\n", 64 * 1024, "1,".repeat(32 * 1024));
                    while s.write_all(chunk.as_bytes()).await.is_ok() {}
                });
            }
        });
        Docker::connect(&format!("unix://{}", sock.display())).unwrap()
    }

    #[tokio::test]
    async fn an_answer_larger_than_any_docker_gives_is_refused_at_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let d = endless_docker(dir.path());
        let e = tokio::time::timeout(std::time::Duration::from_secs(20), call(&d, Method::GET, "/tasks", None)).await.expect("refused at the limit, not when the time ran out").unwrap_err();
        assert_eq!(e.status, hyper::StatusCode::BAD_GATEWAY);
        assert!(e.message.contains("too big"), "{}", e.message);
    }

    #[test]
    fn json_lines_come_apart_where_their_newlines_are() {
        let mut l = Lines::default();
        assert!(l.push(b"{\"a\":").unwrap().is_empty());
        assert_eq!(l.push(b"1}\n{\"b\":2}\n\n{\"c\"").unwrap(), vec![b"{\"a\":1}".to_vec(), b"{\"b\":2}".to_vec()], "a blank line is no line");
        assert_eq!(l.push(b":3}").unwrap(), Vec::<Vec<u8>>::new());
        assert_eq!(l.finish(), Some(b"{\"c\":3}".to_vec()), "the last line needs no newline");
    }

    #[test]
    fn a_line_that_never_ends_is_refused_once_it_passes_the_limit() {
        let mut l = Lines::default();
        let chunk = vec![b'x'; 1024];
        let mut sent = 0;
        let e = loop {
            match l.push(&chunk) {
                Ok(_) => sent += chunk.len(),
                Err(e) => break e,
            }
            assert!(sent <= 2 * MAX_LINE, "still kept after {sent} bytes");
        };
        assert_eq!(e.status, hyper::StatusCode::BAD_GATEWAY);
        assert!(sent <= MAX_LINE, "refused at the limit: {sent}");
    }

    #[tokio::test]
    async fn a_followed_stream_gives_each_line_then_ends_with_the_one_too_long() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("d.sock");
        let l = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = vec![0; 4096];
            let n = s.read(&mut buf).await.unwrap();
            assert!(String::from_utf8_lossy(&buf[..n]).starts_with("GET /events?since=1 HTTP/1.1"));
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n").await;
            let first = "{\"Action\":\"die\"}\n{\"Action\":\"start\"}\n{\"Action\":\"";
            let _ = s.write_all(format!("{:x}\r\n{first}\r\n", first.len()).as_bytes()).await;
            let chunk = format!("{:x}\r\n{}\r\n", 1024, "x".repeat(1024));
            while s.write_all(chunk.as_bytes()).await.is_ok() {}
        });
        let d = Docker::connect(&format!("unix://{}", sock.display())).unwrap();
        let s = follow::<Value>(&d, "/events?since=1").await.unwrap();
        futures_util::pin_mut!(s);
        use futures_util::StreamExt;
        assert_eq!(s.next().await.unwrap().unwrap()["Action"], "die");
        assert_eq!(s.next().await.unwrap().unwrap()["Action"], "start");
        let e = tokio::time::timeout(std::time::Duration::from_secs(20), s.next()).await.expect("refused at the limit").unwrap().unwrap_err();
        assert!(e.message.contains("too long"), "{}", e.message);
        assert!(s.next().await.is_none(), "and then it ends");
    }

    #[test]
    fn a_tty_s_logs_are_the_bytes_alone() {
        let mut d = Demux::new(true);
        assert_eq!(d.push(b"\x01\x00raw").unwrap(), vec![("out", b"\x01\x00raw".to_vec())]);
    }

    #[test]
    fn a_query_is_escaped() {
        assert_eq!(path("/services", &[]), "/services");
        assert_eq!(path("/tasks", &[("filters", r#"{"service":["a b"]}"#)]), "/tasks?filters=%7B%22service%22%3A%5B%22a+b%22%5D%7D");
    }

    #[test]
    fn docker_s_refusal_keeps_its_words() {
        let e = refusal(404, br#"{"message":"service x not found"}"#);
        assert_eq!(e.status, hyper::StatusCode::NOT_FOUND);
        assert_eq!(e.message, "Service x not found");
        let e = refusal(400, b"plain words");
        assert_eq!(e.message, "Plain words");
    }

    #[tokio::test]
    async fn a_docker_that_does_not_answer_is_a_502() {
        let d = Docker::connect("tcp://127.0.0.1:1").unwrap();
        let e = call(&d, Method::GET, "/info", None).await.unwrap_err();
        assert_eq!(e.status, hyper::StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn json_goes_to_docker_and_its_answer_comes_back() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("d.sock");
        let l = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = vec![0; 4096];
            let n = s.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(req.starts_with("POST /configs/create HTTP/1.1"), "{req}");
            assert!(req.contains(r#"{"Name":"c"}"#), "{req}");
            let body = r#"{"ID":"abc"}"#;
            s.write_all(format!("HTTP/1.1 201 Created\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        });
        let d = Docker::connect(&format!("unix://{}", sock.display())).unwrap();
        let v = call(&d, Method::POST, "/configs/create", Some(&serde_json::json!({ "Name": "c" }))).await.unwrap();
        assert_eq!(v["ID"], "abc");
    }
}
