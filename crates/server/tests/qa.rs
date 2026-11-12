//! The end-to-end suite: a real Freightdock binary against a throwaway Docker (`docker:dind`), driven through its HTTP API
//! the way the UI drives it. `qa/run.sh` starts both and runs this test with `--ignored`; `FREIGHTDOCK_QA_URL` says where
//! Freightdock listens. One test, in order: each step builds on the one before, prints `ok` when it holds, and the first
//! that does not stops the run with what it expected and what came.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

#[path = "qa/access.rs"]
mod access;
#[path = "qa/signin.rs"]
mod signin;
#[path = "qa/swarm.rs"]
mod swarm;

const IMAGE: &str = "alpine:3.20";
const NAME: &str = "qa-sleeper";
const REGISTRY_IMAGE: &str = "registry:2";
const REGISTRY_PASSWORD: &str = "qa-registry-password";
/// `qa:qa-registry-password` as htpasswd keeps it (bcrypt, which is all registry:2 takes), made once with
/// `perl -e 'print crypt("qa-registry-password", "$2y$05$abcdefghijklmnopqrstuu")'` (libxcrypt).
const REGISTRY_HTPASSWD: &str = "qa:$2y$05$abcdefghijklmnopqrstuugxu5501UfI0hxMR82NbCbuiyCNzwffa";

struct Qa {
    http: reqwest::Client,
    base: String,
    cookie: String,
    csrf: String,
    env: i64,
}

/// One step: `ok` and its name when it holds, or the run stops here with the reason.
async fn step<T>(name: &str, f: impl std::future::Future<Output = Result<T>>) -> T {
    match f.await {
        Ok(v) => {
            println!("ok   {name}");
            v
        }
        Err(e) => panic!("FAILED {name}: {e:#}"),
    }
}

impl Qa {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn env(&self, path: &str) -> String {
        self.url(&format!("/api/envs/{}{path}", self.env))
    }

    /// A request as the signed-in browser sends it: the session cookie, the CSRF token and this server's `Origin`.
    fn req(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        self.http.request(method, url).header("cookie", &self.cookie).header("x-csrf-token", &self.csrf).header("origin", &self.base)
    }

    async fn get(&self, url: &str) -> Result<(StatusCode, String)> {
        let r = self.req(Method::GET, url).send().await.with_context(|| format!("GET {url}"))?;
        Ok((r.status(), r.text().await?))
    }

    async fn get_json(&self, url: &str) -> Result<Value> {
        let (s, body) = self.get(url).await?;
        ensure!(s == StatusCode::OK, "GET {url}: {s}: {body}");
        serde_json::from_str(&body).with_context(|| format!("GET {url} is not JSON: {body}"))
    }

    async fn send(&self, method: Method, url: &str, body: Option<Value>) -> Result<(StatusCode, Value)> {
        let mut r = self.req(method.clone(), url);
        if let Some(b) = body {
            r = r.json(&b);
        }
        let r = r.send().await.with_context(|| format!("{method} {url}"))?;
        let s = r.status();
        let text = r.text().await?;
        Ok((s, if text.is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::String(text)) }))
    }

    async fn post(&self, url: &str, body: Value) -> Result<(StatusCode, Value)> {
        self.send(Method::POST, url, Some(body)).await
    }

    /// A long job's NDJSON, read to its end: every line, and whether the last said done.
    async fn stream(&self, url: &str, body: Value) -> Result<(Vec<Value>, bool)> {
        let r = self.req(Method::POST, url).json(&body).send().await.with_context(|| format!("POST {url}"))?;
        let s = r.status();
        let text = r.text().await?;
        ensure!(s == StatusCode::OK, "POST {url}: {s}: {text}");
        let lines: Vec<Value> = text.lines().filter(|l| !l.trim().is_empty()).map(serde_json::from_str).collect::<Result<_, _>>().with_context(|| format!("not NDJSON: {text}"))?;
        let done = lines.last().is_some_and(|l| l["done"] == json!(true));
        Ok((lines, done))
    }

    async fn inspect(&self, id: &str) -> Result<(StatusCode, Value)> {
        self.send(Method::GET, &self.env(&format!("/containers/{id}")), None).await
    }

    async fn running(&self, id: &str) -> Result<bool> {
        let (s, c) = self.inspect(id).await?;
        ensure!(s == StatusCode::OK, "inspect {id}: {s}: {c}");
        Ok(c["State"]["Running"] == json!(true))
    }

    async fn containers(&self) -> Result<Vec<Value>> {
        Ok(self.get_json(&self.env("/containers")).await?.as_array().cloned().unwrap_or_default())
    }

    /// Waits, up to `within`, for `f` to hold; the last reason it did not is the error.
    async fn until<F, Fut>(&self, within: Duration, mut f: F) -> Result<()>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let start = Instant::now();
        loop {
            match f().await {
                Ok(()) => return Ok(()),
                Err(e) if start.elapsed() > within => return Err(e.context(format!("still not so after {within:?}"))),
                Err(_) => tokio::time::sleep(Duration::from_millis(250)).await,
            }
        }
    }

    async fn logs(&self, id: &str) -> Result<String> {
        let (s, body) = self.get(&self.env(&format!("/containers/{id}/logs?tail=all"))).await?;
        ensure!(s == StatusCode::OK, "logs of {id}: {s}: {body}");
        Ok(body)
    }

    /// A WebSocket to `path` under the environment, opened as the signed-in browser opens it.
    async fn ws(&self, path: &str) -> Result<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>> {
        let url = self.env(path).replacen("http", "ws", 1);
        let mut req = url.as_str().into_client_request()?;
        req.headers_mut().insert("cookie", self.cookie.parse()?);
        req.headers_mut().insert("origin", self.base.parse()?);
        let (ws, _) = tokio_tungstenite::connect_async(req).await.with_context(|| format!("opening {path}"))?;
        Ok(ws)
    }

    /// A stack as the list has it, by name.
    async fn stack(&self, name: &str) -> Result<Option<Value>> {
        let list = self.get_json(&self.env("/stacks")).await?;
        Ok(list.as_array().and_then(|l| l.iter().find(|s| s["name"] == json!(name)).cloned()))
    }

    /// Waits for stack `name` to be in `state` with `n` containers.
    async fn stack_is(&self, name: &str, state: &str, n: i64) -> Result<Value> {
        self.until(Duration::from_secs(30), || async {
            let s = self.stack(name).await?.with_context(|| format!("no stack {name} in the list"))?;
            ensure!(s["state"] == json!(state) && s["containers"] == json!(n), "stack {name}: {s}");
            Ok(())
        })
        .await?;
        self.stack(name).await?.with_context(|| format!("no stack {name} in the list"))
    }

    /// A stack job's NDJSON to its end, which must be done.
    async fn stack_job(&self, id: i64, word: &str, body: Value) -> Result<Vec<Value>> {
        let (lines, done) = self.stream(&self.env(&format!("/stacks/{id}/{word}")), body).await?;
        ensure!(done, "{word} of stack {id} did not end done: {lines:?}");
        Ok(lines)
    }

    async fn audit_actions(&self) -> Result<Vec<String>> {
        let a = self.get_json(&self.url("/api/audit?limit=500")).await?;
        let rows = a["entries"].as_array().cloned().with_context(|| format!("the activity log has no entries: {a}"))?;
        Ok(rows.iter().filter_map(|r| r["action"].as_str().map(str::to_string)).collect())
    }
}

/// An agent's side of `/mcp`: an API token, and the session `initialize` gave it.
struct Mcp<'a> {
    qa: &'a Qa,
    token: String,
    session: Option<String>,
    next: i64,
}

impl Mcp<'_> {
    /// One JSON-RPC request, as an MCP client sends it: the token and no cookie, no CSRF token, no `Origin`.
    async fn rpc(&mut self, method: &str, params: Value) -> Result<(StatusCode, reqwest::header::HeaderMap, Value)> {
        self.next += 1;
        let mut r = self.qa.http.post(self.qa.url("/mcp")).bearer_auth(&self.token).header("accept", "application/json, text/event-stream");
        if let Some(s) = &self.session {
            r = r.header("mcp-session-id", s).header("mcp-protocol-version", "2025-06-18");
        }
        let r = r.json(&json!({ "jsonrpc": "2.0", "id": self.next, "method": method, "params": params })).send().await.with_context(|| format!("MCP {method}"))?;
        let (s, h) = (r.status(), r.headers().clone());
        let text = r.text().await?;
        let v = serde_json::from_str(&text).with_context(|| format!("MCP {method} answered what is not JSON: {s}: {text}"))?;
        Ok((s, h, v))
    }

    /// A tool's answer: whether it is an error, and its text.
    async fn tool(&mut self, name: &str, args: Value) -> Result<(bool, String)> {
        let (s, _, b) = self.rpc("tools/call", json!({ "name": name, "arguments": args })).await?;
        ensure!(s == StatusCode::OK && b["result"].is_object(), "tools/call {name}: {s}: {b}");
        Ok((b["result"]["isError"] == json!(true), b["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string()))
    }

    /// A tool that must work, its text as JSON.
    async fn ok(&mut self, name: &str, args: Value) -> Result<Value> {
        let (error, text) = self.tool(name, args).await?;
        ensure!(!error, "{name} failed: {text}");
        serde_json::from_str(&text).with_context(|| format!("{name} answered what is not JSON: {text}"))
    }
}

fn names(list: &[Value]) -> Vec<String> {
    list.iter().flat_map(|c| c["Names"].as_array().cloned().unwrap_or_default()).filter_map(|n| n.as_str().map(|s| s.trim_start_matches('/').to_string())).collect()
}

fn env_of(c: &Value) -> Vec<String> {
    c["Config"]["Env"].as_array().into_iter().flatten().filter_map(|e| e.as_str().map(str::to_string)).collect()
}

#[tokio::test]
#[ignore = "end to end: run by qa/run.sh against a throwaway docker:dind"]
async fn freightdock_manages_a_real_docker_end_to_end() {
    let base = std::env::var("FREIGHTDOCK_QA_URL").expect("FREIGHTDOCK_QA_URL: where the Freightdock under test listens (qa/run.sh sets it)");
    let base = base.trim_end_matches('/').to_string();
    let http = reqwest::Client::builder().timeout(Duration::from_secs(300)).build().unwrap();
    let mut qa = Qa { http, base, cookie: String::new(), csrf: String::new(), env: 0 };

    step("Freightdock answers on /api/health", async {
        let start = Instant::now();
        loop {
            match qa.http.get(qa.url("/api/health")).send().await {
                Ok(r) if r.status() == StatusCode::OK => return Ok(()),
                _ if start.elapsed() > Duration::from_secs(60) => bail!("no answer from {} within a minute", qa.base),
                _ => tokio::time::sleep(Duration::from_millis(250)).await,
            }
        }
    })
    .await;

    step("a new Freightdock says it is not set up", async {
        let s: Value = qa.http.get(qa.url("/api/status")).send().await?.json().await?;
        ensure!(s["configured"] == json!(false), "status: {s}");
        Ok(())
    })
    .await;

    let setup_token = std::env::var("FREIGHTDOCK_QA_SETUP_TOKEN").expect("FREIGHTDOCK_QA_SETUP_TOKEN: the setup token Freightdock was started with (qa/run.sh sets it)");
    step("setup without the setup token is refused", async {
        let r = qa.http.post(qa.url("/api/setup")).header("origin", &qa.base).json(&json!({ "name": "qa-admin", "password": "correct horse battery", "token": "not the token" })).send().await?;
        ensure!(r.status() == StatusCode::FORBIDDEN, "setup with a wrong token: {}", r.status());
        Ok(())
    })
    .await;

    let cookie = step("setup makes the first administrator and signs them in", async {
        let r = qa.http.post(qa.url("/api/setup")).header("origin", &qa.base).json(&json!({ "name": "qa-admin", "password": "correct horse battery", "token": setup_token })).send().await?;
        let s = r.status();
        let set = r.headers().get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()).find(|v| v.starts_with("freightdock_session=")).map(|v| v.split(';').next().unwrap_or_default().to_string());
        let body = r.text().await?;
        ensure!(s == StatusCode::OK, "setup: {s}: {body}");
        set.context("setup set no session cookie")
    })
    .await;
    qa.cookie = cookie;

    step("setup is refused once there is an administrator", async {
        let r = qa.http.post(qa.url("/api/setup")).json(&json!({ "name": "qa-intruder", "password": "correct horse battery", "token": setup_token })).send().await?;
        ensure!(r.status() == StatusCode::CONFLICT, "a second setup: {}", r.status());
        Ok(())
    })
    .await;

    let (csrf, env) = step("the session has a CSRF token and the local environment", async {
        let me = qa.get_json(&qa.url("/api/me")).await?;
        let csrf = me["csrf"].as_str().context("no csrf in /api/me")?.to_string();
        let envs = qa.get_json(&qa.url("/api/environments")).await?;
        let local = envs.as_array().and_then(|e| e.iter().find(|e| e["name"] == json!("local"))).context("no local environment")?;
        Ok((csrf, local["id"].as_i64().context("environment id")?))
    })
    .await;
    qa.csrf = csrf;
    qa.env = env;

    step("the Docker behind it is the empty throwaway one", async {
        let info = qa.get_json(&qa.env("/info")).await?;
        let n = info["info"]["Containers"].as_i64().with_context(|| format!("no container count in info: {info}"))?;
        ensure!(n == 0, "the Docker under test already has {n} containers: is this the host's Docker and not the dind?");
        Ok(())
    })
    .await;

    step("a change without the CSRF token, or from another site, is refused", async {
        let url = qa.env("/containers/prune");
        let r = qa.http.post(&url).header("cookie", &qa.cookie).send().await?;
        ensure!(r.status() == StatusCode::FORBIDDEN, "no token: {}", r.status());
        let r = qa.http.post(&url).header("cookie", &qa.cookie).header("x-csrf-token", &qa.csrf).header("origin", "https://evil.example").send().await?;
        ensure!(r.status() == StatusCode::FORBIDDEN, "another origin: {}", r.status());
        Ok(())
    })
    .await;

    step(&format!("pull {IMAGE} streams progress and ends done"), async {
        let (lines, done) = qa.stream(&qa.env("/images/pull"), json!({ "image": IMAGE })).await?;
        ensure!(done, "the pull did not end done: {lines:?}");
        ensure!(lines.iter().all(|l| l.get("error").is_none()), "an error line: {lines:?}");
        let images = qa.get_json(&qa.env("/images")).await?;
        let tags: Vec<&str> = images.as_array().into_iter().flatten().flat_map(|i| i["RepoTags"].as_array().into_iter().flatten()).filter_map(Value::as_str).collect();
        ensure!(tags.contains(&IMAGE), "{IMAGE} is not in the image list: {tags:?}");
        Ok(())
    })
    .await;

    step("a pull of an image that does not exist ends with an error line", async {
        let (lines, done) = qa.stream(&qa.env("/images/pull"), json!({ "image": "freightdock-qa/no-such-image:never" })).await?;
        ensure!(!done, "a failed pull ended done: {lines:?}");
        ensure!(lines.last().is_some_and(|l| l["error"].is_string()), "no error line at the end: {lines:?}");
        Ok(())
    })
    .await;

    let id = step("create and start a container", async {
        let body = json!({
            "name": NAME,
            "config": { "Image": IMAGE, "Cmd": ["sh", "-c", "echo hello-from-qa; echo oops-from-qa >&2; exec sleep 3600"] },
        });
        let (s, made) = qa.post(&qa.env("/containers"), body).await?;
        ensure!(s == StatusCode::CREATED, "create: {s}: {made}");
        let id = made["id"].as_str().context("no id")?.to_string();
        ensure!(qa.running(&id).await?, "it is not running");
        ensure!(names(&qa.containers().await?).contains(&NAME.to_string()), "{NAME} is not in the list");
        Ok(id)
    })
    .await;

    step("its logs have what it wrote to standard output and standard error", async {
        qa.until(Duration::from_secs(15), || async {
            let logs = qa.logs(&id).await?;
            ensure!(logs.contains("hello-from-qa") && logs.contains("oops-from-qa"), "logs so far: {logs:?}");
            Ok(())
        })
        .await
    })
    .await;

    step("its live stats come at the interval asked for, and an interval not offered is refused", async {
        let mut ws = qa.ws(&format!("/containers/{id}/stats/ws?every=1")).await?;
        let mut next = async || -> Result<Value> {
            let m = tokio::time::timeout(Duration::from_secs(10), ws.next()).await.context("no sample")?.context("closed")??;
            Ok(serde_json::from_str(m.to_text()?)?)
        };
        let first = next().await?;
        ensure!(first["cpu"].is_number() && first["mem"].is_number(), "the first sample: {first}");
        let t = std::time::Instant::now();
        let second = next().await?;
        ensure!(second["cpu"].is_number(), "the second sample: {second}");
        ensure!(t.elapsed() >= Duration::from_millis(700), "a second sample came {:?} after the first, not about a second", t.elapsed());
        ensure!(qa.ws(&format!("/containers/{id}/stats/ws?every=2")).await.is_err(), "a sample every 2 seconds was offered");
        Ok(())
    })
    .await;

    step("Home's summary counts the running container, and the host keeps it as its last known numbers", async {
        let sum = qa.get_json(&qa.env("/summary")).await?;
        ensure!(sum["containers"]["running"].as_i64() >= Some(1) && sum["containers"]["total"].as_i64() >= Some(1), "containers: {sum}");
        ensure!(sum["images"].as_i64() >= Some(1), "images: {sum}");
        ensure!(sum["cpus"].as_i64() > Some(0) && sum["memory"].as_i64() > Some(0), "cpus and memory: {sum}");
        ensure!(sum["nodes"] == json!(1) && sum["docker_version"].is_string(), "nodes and version: {sum}");
        let me = qa.get_json(&qa.url(&format!("/api/environments/{}", qa.env))).await?;
        ensure!(me["snapshot"]["containers"] == sum["containers"] && me["snapshot"]["at"].is_i64(), "the snapshot kept: {me}");
        ensure!(me["status"] == json!("up"), "it answered, so it is up: {me}");
        Ok(())
    })
    .await;

    step("a group and tags placed on the host, then taken away again", async {
        let (s, g) = qa.post(&qa.url("/api/environment-groups"), json!({ "name": "qa-group", "environments": [qa.env] })).await?;
        ensure!(s == StatusCode::CREATED, "group: {s}: {g}");
        let gid = g["id"].as_i64().context("group id")?;
        let path = qa.url(&format!("/api/environments/{}", qa.env));
        let (s, e) = qa.send(Method::PUT, &path, Some(json!({ "tags": ["qa", "dind"] }))).await?;
        ensure!(s == StatusCode::OK, "tags: {s}: {e}");
        ensure!(e["group"]["id"] == json!(gid) && e["tags"] == json!(["dind", "qa"]), "placed: {e}");
        let tags = qa.get_json(&qa.url("/api/tags")).await?;
        let qa_tag = tags.as_array().and_then(|t| t.iter().find(|t| t["name"] == json!("qa"))).with_context(|| format!("no qa tag: {tags}"))?;
        let (s, b) = qa.send(Method::DELETE, &qa.url(&format!("/api/tags/{}", qa_tag["id"])), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove the tag: {s}: {b}");
        let (s, b) = qa.send(Method::DELETE, &qa.url(&format!("/api/environment-groups/{gid}")), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove the group: {s}: {b}");
        let e = qa.get_json(&path).await?;
        ensure!(e["group"].is_null() && e["tags"] == json!(["dind"]), "what is left: {e}");
        let (s, b) = qa.send(Method::PUT, &path, Some(json!({ "tags": [] }))).await?;
        ensure!(s == StatusCode::OK, "no tags: {s}: {b}");
        Ok(())
    })
    .await;

    step("resource history samples it and answers the samples as chart points", async {
        let (s, b) = qa.send(Method::PUT, &qa.url("/api/settings/history"), Some(json!({ "enabled": true, "every_s": 10, "keep_days": 1 }))).await?;
        ensure!(s == StatusCode::NO_CONTENT, "PUT the history settings: {s}: {b}");
        qa.until(Duration::from_secs(60), || async {
            let h = qa.get_json(&qa.env(&format!("/containers/{id}/history?hours=1"))).await?;
            ensure!(h["container"] == json!(NAME), "the history is not by its name: {h}");
            let points = h["points"].as_array().cloned().unwrap_or_default();
            ensure!(!points.is_empty(), "no samples yet: {h}");
            ensure!(points[0]["mem"].as_u64().is_some_and(|m| m > 0), "a sample without its memory: {}", points[0]);
            Ok(())
        })
        .await
    })
    .await;

    step("a console runs a command inside it", async {
        let url = qa.env(&format!("/containers/{id}/exec/ws?cmd=/bin/sh&cols=80&rows=24")).replacen("http", "ws", 1);
        let mut req = url.as_str().into_client_request()?;
        req.headers_mut().insert("cookie", qa.cookie.parse()?);
        req.headers_mut().insert("origin", qa.base.parse()?);
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.context("opening the console")?;
        ws.send(Message::Text(json!({ "resize": [100, 30] }).to_string().into())).await?;
        ws.send(Message::Binary(b"echo qa-$((40+2)); exit\n".to_vec().into())).await?;
        let (mut seen, mut ended) = (String::new(), false);
        let read = async {
            while let Some(m) = ws.next().await {
                match m? {
                    Message::Binary(b) => seen.push_str(&String::from_utf8_lossy(&b)),
                    Message::Text(t) if t.contains("\"end\"") => {
                        ended = true;
                        break;
                    }
                    Message::Text(t) => bail!("the console said {t}"),
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(20), read).await.context("the console did not finish")??;
        ensure!(seen.contains("qa-42"), "the command's output did not come back: {seen:?}");
        ensure!(ended, "the console did not say it ended when the shell exited");
        Ok(())
    })
    .await;

    step("a console from another site is refused", async {
        let url = qa.env(&format!("/containers/{id}/exec/ws")).replacen("http", "ws", 1);
        let mut req = url.as_str().into_client_request()?;
        req.headers_mut().insert("cookie", qa.cookie.parse()?);
        req.headers_mut().insert("origin", "https://evil.example".parse()?);
        ensure!(tokio_tungstenite::connect_async(req).await.is_err(), "a console opened from another origin");
        Ok(())
    })
    .await;

    step("attaching to a TTY container's shell types into it, and leaving does not stop it", async {
        let body = json!({ "name": "qa-attached", "config": { "Image": IMAGE, "Cmd": ["sh"], "Tty": true, "OpenStdin": true, "AttachStdin": true } });
        let (s, made) = qa.post(&qa.env("/containers"), body).await?;
        ensure!(s == StatusCode::CREATED, "create: {s}: {made}");
        let attached = made["id"].as_str().context("no id")?.to_string();
        let mut ws = qa.ws(&format!("/containers/{attached}/attach/ws?cols=80&rows=24")).await?;
        let first = tokio::time::timeout(Duration::from_secs(10), ws.next()).await.context("no first message")?.context("closed at once")??;
        let first: Value = serde_json::from_str(first.to_text()?).with_context(|| format!("the first message is not JSON: {first:?}"))?;
        ensure!(first["attached"] == json!({ "tty": true, "stdin": true }), "the first message: {first}");
        ws.send(Message::Text(json!({ "resize": [100, 30] }).to_string().into())).await?;
        ws.send(Message::Binary(b"echo qa-attach-$((6*7))\n".to_vec().into())).await?;
        let mut seen = String::new();
        let read = async {
            while let Some(m) = ws.next().await {
                if let Message::Binary(b) = m? {
                    seen.push_str(&String::from_utf8_lossy(&b));
                    if seen.contains("qa-attach-42") { break; }
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(20), read).await.with_context(|| format!("the shell did not answer: {seen:?}"))??;
        ws.close(None).await?;
        drop(ws);
        tokio::time::sleep(Duration::from_secs(2)).await;
        ensure!(qa.running(&attached).await?, "leaving the attach stopped the container");
        let (s, b) = qa.send(Method::DELETE, &qa.env(&format!("/containers/{attached}?force=true")), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove: {s}: {b}");
        Ok(())
    })
    .await;

    step("attaching without a TTY, or to an input that closes after, only watches", async {
        let body = json!({ "name": "qa-ticker", "config": { "Image": IMAGE, "Cmd": ["sh", "-c", "while true; do echo tick; sleep 1; done"], "OpenStdin": true, "StdinOnce": true } });
        let (s, made) = qa.post(&qa.env("/containers"), body).await?;
        ensure!(s == StatusCode::CREATED, "create: {s}: {made}");
        let ticker = made["id"].as_str().context("no id")?.to_string();
        let mut ws = qa.ws(&format!("/containers/{ticker}/attach/ws")).await?;
        let first = tokio::time::timeout(Duration::from_secs(10), ws.next()).await.context("no first message")?.context("closed at once")??;
        let first: Value = serde_json::from_str(first.to_text()?)?;
        ensure!(first["attached"] == json!({ "tty": false, "stdin": false }), "the first message: {first}");
        let mut seen = String::new();
        let read = async {
            while let Some(m) = ws.next().await {
                if let Message::Binary(b) = m? {
                    seen.push_str(&String::from_utf8_lossy(&b));
                    if seen.contains("tick\r\n") { break; }
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(20), read).await.with_context(|| format!("no tick came: {seen:?}"))??;
        drop(ws);
        tokio::time::sleep(Duration::from_secs(2)).await;
        ensure!(qa.running(&ticker).await?, "leaving the attach stopped the container");
        let (s, b) = qa.send(Method::DELETE, &qa.env(&format!("/containers/{ticker}?force=true")), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove: {s}: {b}");
        Ok(())
    })
    .await;

    step("attaching to a container that is not running is refused with the reason", async {
        let body = json!({ "name": "qa-idle", "start": false, "config": { "Image": IMAGE, "Cmd": ["true"] } });
        let (s, made) = qa.post(&qa.env("/containers"), body).await?;
        ensure!(s == StatusCode::CREATED, "create: {s}: {made}");
        let idle = made["id"].as_str().context("no id")?.to_string();
        ensure!(qa.ws(&format!("/containers/{idle}/attach/ws")).await.is_err(), "an attach to a stopped container opened");
        let (s, b) = qa.send(Method::DELETE, &qa.env(&format!("/containers/{idle}?force=true")), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove: {s}: {b}");
        Ok(())
    })
    .await;

    step("connect it to a network with an alias, and disconnect it again", async {
        let (s, b) = qa.post(&qa.env("/networks"), json!({ "Name": "qa-net" })).await?;
        ensure!(s == StatusCode::CREATED, "create the network: {s}: {b}");
        let (s, b) = qa.post(&qa.env("/networks/qa-net/connect"), json!({ "container": id, "aliases": ["qa-alias"] })).await?;
        ensure!(s == StatusCode::NO_CONTENT, "connect: {s}: {b}");
        let (_, c) = qa.inspect(&id).await?;
        let on = &c["NetworkSettings"]["Networks"]["qa-net"];
        ensure!(on.is_object(), "it is not on qa-net: {}", c["NetworkSettings"]["Networks"]);
        ensure!(on["Aliases"].as_array().is_some_and(|a| a.contains(&json!("qa-alias"))), "the alias is missing: {on}");
        ensure!(qa.running(&id).await?, "connecting stopped it");
        let (s, b) = qa.post(&qa.env("/networks/qa-net/disconnect"), json!({ "container": id })).await?;
        ensure!(s == StatusCode::NO_CONTENT, "disconnect: {s}: {b}");
        let (_, c) = qa.inspect(&id).await?;
        ensure!(c["NetworkSettings"]["Networks"]["qa-net"].is_null(), "it is still on qa-net");
        ensure!(qa.running(&id).await?, "disconnecting stopped it");
        let (s, b) = qa.send(Method::DELETE, &qa.env("/networks/qa-net"), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove the network: {s}: {b}");
        Ok(())
    })
    .await;

    step("the volume browser makes a folder, uploads, lists, downloads and deletes inside a volume, and nowhere else", async {
        let (s, b) = qa.post(&qa.env("/volumes"), json!({ "Name": "qa-files" })).await?;
        ensure!(s == StatusCode::CREATED, "create the volume: {s}: {b}");
        let files = |rest: &str| qa.env(&format!("/volumes/qa-files/files{rest}"));
        let (s, b) = qa.post(&files("/mkdir"), json!({ "path": "docs/deep" })).await?;
        ensure!(s == StatusCode::CREATED, "mkdir: {s}: {b}");
        let text = "hello from the volume browser\n";
        let r = qa.req(Method::POST, &files("/upload?path=docs&name=hello%20qa.txt")).header("content-type", "application/octet-stream").body(text).send().await?;
        ensure!(r.status() == StatusCode::CREATED, "upload: {}: {}", r.status(), r.text().await?);
        let l = qa.get_json(&files("?path=docs")).await?;
        let entries = l["entries"].as_array().cloned().unwrap_or_default();
        ensure!(entries.len() == 2, "docs holds {entries:?}");
        ensure!(entries[0]["name"] == json!("deep") && entries[0]["kind"] == json!("dir"), "folders first: {entries:?}");
        ensure!(entries[1]["name"] == json!("hello qa.txt") && entries[1]["size"] == json!(text.len()), "the file: {}", entries[1]);
        let r = qa.req(Method::GET, &files("/download?path=docs/hello%20qa.txt")).send().await?;
        ensure!(r.status() == StatusCode::OK, "download: {}", r.status());
        let disposition = r.headers().get("content-disposition").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
        ensure!(disposition.contains("hello%20qa.txt"), "its name: {disposition}");
        let got = r.text().await?;
        ensure!(got == text, "the file came back as {got:?}");
        let r = qa.req(Method::GET, &files("/download?path=docs")).send().await?;
        ensure!(r.headers().get("content-type").is_some_and(|v| v == "application/x-tar"), "a folder comes as a tar: {:?}", r.headers());
        ensure!(String::from_utf8_lossy(&r.bytes().await?).contains("hello from the volume browser"), "the folder's tar lacks the file");
        let (s, b) = qa.send(Method::GET, &files("?path=../../etc"), None).await?;
        ensure!(s == StatusCode::BAD_REQUEST, "a path out of the volume: {s}: {b}");
        let (s, b) = qa.send(Method::DELETE, &files("?path=docs/hello%20qa.txt"), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "delete the file: {s}: {b}");
        let l = qa.get_json(&files("?path=docs")).await?;
        ensure!(l["entries"].as_array().is_some_and(|e| e.len() == 1), "after the delete: {l}");
        let (s, b) = qa.send(Method::DELETE, &files("?path=docs"), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "delete the folder: {s}: {b}");
        ensure!(qa.get_json(&files("")).await?["entries"].as_array().is_some_and(Vec::is_empty), "the volume is not empty again");
        // The helpers go once each request is done; then the volume can go too.
        qa.until(Duration::from_secs(20), || async {
            let all = names(&qa.containers().await?);
            ensure!(!all.iter().any(|n| n.starts_with("freightdock-files-")), "helpers are left: {all:?}");
            let (s, b) = qa.send(Method::DELETE, &qa.env("/volumes/qa-files"), None).await?;
            ensure!(s == StatusCode::NO_CONTENT, "remove the volume: {s}: {b}");
            Ok(())
        })
        .await
    })
    .await;

    let id = step("edit replaces it: a new container under the same name, the old one gone", async {
        let body = json!({
            "name": NAME,
            "replace": id,
            "config": { "Image": IMAGE, "Cmd": ["sleep", "3600"], "Env": ["QA_EDIT=1"] },
        });
        let (s, made) = qa.post(&qa.env("/containers"), body).await?;
        ensure!(s == StatusCode::CREATED, "edit: {s}: {made}");
        let new = made["id"].as_str().context("no id")?.to_string();
        ensure!(new != id, "the edit kept the old id");
        let (s, c) = qa.inspect(NAME).await?;
        ensure!(s == StatusCode::OK && c["Id"] == json!(new), "{NAME} is not the new container: {s}: {}", c["Id"]);
        ensure!(env_of(&c).contains(&"QA_EDIT=1".to_string()), "the edit's environment is missing: {:?}", env_of(&c));
        ensure!(c["State"]["Running"] == json!(true), "the edited container is not running");
        let (s, _) = qa.inspect(&id).await?;
        ensure!(s == StatusCode::NOT_FOUND, "the old container is still there: {s}");
        Ok(new)
    })
    .await;

    for (what, config) in [
        ("one that cannot start", json!({ "Image": IMAGE, "Cmd": ["/no/such/program"] })),
        ("one Docker will not make", json!({ "Image": IMAGE, "Cmd": ["sleep", "1"], "HostConfig": { "Memory": 1 } })),
    ] {
        step(&format!("an edit to {what} fails and leaves the container as it was"), async {
            let (s, e) = qa.post(&qa.env("/containers"), json!({ "name": NAME, "replace": id, "config": config })).await?;
            ensure!(!s.is_success(), "the bad edit succeeded: {s}: {e}");
            ensure!(e["error"].as_str().is_some_and(|m| !m.is_empty()), "no reason given: {e}");
            let (s, c) = qa.inspect(NAME).await?;
            ensure!(s == StatusCode::OK && c["Id"] == json!(id), "{NAME} is not the container it was: {s}: {}", c["Id"]);
            ensure!(c["State"]["Running"] == json!(true), "it was running and was not started again");
            ensure!(env_of(&c).contains(&"QA_EDIT=1".to_string()), "its settings changed");
            let all = names(&qa.containers().await?);
            ensure!(all == vec![NAME.to_string()], "something was left behind: {all:?}");
            Ok(())
        })
        .await;
    }

    step("stop it, and stopping it again is no error", async {
        let url = qa.env(&format!("/containers/{id}/stop"));
        let (s, b) = qa.post(&url, json!({})).await?;
        ensure!(s == StatusCode::NO_CONTENT, "stop: {s}: {b}");
        ensure!(!qa.running(&id).await?, "it still runs");
        let (s, b) = qa.post(&url, json!({})).await?;
        ensure!(s == StatusCode::NO_CONTENT, "stop again: {s}: {b}");
        Ok(())
    })
    .await;

    step("remove it", async {
        let (s, b) = qa.send(Method::DELETE, &qa.env(&format!("/containers/{id}")), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove: {s}: {b}");
        let (s, _) = qa.inspect(&id).await?;
        ensure!(s == StatusCode::NOT_FOUND, "it is still there: {s}");
        ensure!(qa.containers().await?.is_empty(), "containers are left");
        Ok(())
    })
    .await;

    step("build an image from a Dockerfile, and run it", async {
        let dockerfile = format!("FROM {IMAGE}\nRUN echo built-by-qa > /qa\nCMD [\"cat\", \"/qa\"]\n");
        let (lines, done) = qa.stream(&qa.env("/images/build?tag=qa-built:1"), json!({ "dockerfile": dockerfile })).await?;
        ensure!(done, "the build did not end done: {lines:?}");
        let (s, made) = qa.post(&qa.env("/containers"), json!({ "name": "qa-built", "config": { "Image": "qa-built:1" } })).await?;
        ensure!(s == StatusCode::CREATED, "running the built image: {s}: {made}");
        let built = made["id"].as_str().context("no id")?.to_string();
        qa.until(Duration::from_secs(15), || async {
            let logs = qa.logs(&built).await?;
            ensure!(logs.contains("built-by-qa"), "logs so far: {logs:?}");
            Ok(())
        })
        .await?;
        let (s, b) = qa.send(Method::DELETE, &qa.env(&format!("/containers/{built}?force=true")), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove: {s}: {b}");
        let (s, b) = qa.send(Method::DELETE, &qa.env("/images/qa-built:1"), None).await?;
        ensure!(s == StatusCode::OK, "remove the built image: {s}: {b}");
        Ok(())
    })
    .await;

    step("a build that fails ends with an error line", async {
        let (lines, done) = qa.stream(&qa.env("/images/build?tag=qa-broken:1"), json!({ "dockerfile": format!("FROM {IMAGE}\nRUN exit 3\n") })).await?;
        ensure!(!done, "a failed build ended done: {lines:?}");
        ensure!(lines.last().is_some_and(|l| l["error"].is_string()), "no error line at the end: {lines:?}");
        Ok(())
    })
    .await;

    // A registry of our own, with a password, run in the throwaway Docker through Freightdock itself. Its port is published
    // on the dind and from the dind to this machine at the same number, so the dind's Docker and Freightdock both reach it
    // as 127.0.0.1:<port> (which Docker takes over plain HTTP), and an image's name is the same for both.
    let rport = std::env::var("FREIGHTDOCK_QA_REGISTRY_PORT").expect("FREIGHTDOCK_QA_REGISTRY_PORT: the registry's port (qa/run.sh sets it)");
    let reg = format!("127.0.0.1:{rport}");
    let pushed = format!("{reg}/qa/alpine:1");

    let registry_ct = step("a registry:2 with a password runs in the throwaway Docker", async {
        let body = json!({
            "name": "qa-registry",
            "pull": true,
            "config": {
                "Image": REGISTRY_IMAGE,
                "Env": ["REGISTRY_AUTH=htpasswd", "REGISTRY_AUTH_HTPASSWD_REALM=qa", "REGISTRY_AUTH_HTPASSWD_PATH=/auth/htpasswd", format!("QA_HTPASSWD={REGISTRY_HTPASSWD}")],
                "Entrypoint": ["/bin/sh", "-c", "mkdir -p /auth && printf '%s\\n' \"$QA_HTPASSWD\" > /auth/htpasswd && exec registry serve /etc/docker/registry/config.yml"],
                "ExposedPorts": { "5000/tcp": {} },
                "HostConfig": { "PortBindings": { "5000/tcp": [{ "HostPort": rport }] } },
            },
        });
        let (s, made) = qa.post(&qa.env("/containers"), body).await?;
        ensure!(s == StatusCode::CREATED, "create the registry: {s}: {made}");
        let id = made["id"].as_str().context("no id")?.to_string();
        qa.until(Duration::from_secs(30), || async {
            let r = qa.http.get(format!("http://{reg}/v2/")).send().await?;
            let challenge = r.headers().get("www-authenticate").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
            ensure!(r.status() == StatusCode::UNAUTHORIZED && challenge.starts_with("Basic"), "the registry answered {} {challenge:?}", r.status());
            Ok(())
        })
        .await?;
        Ok(id)
    })
    .await;

    step("adding it with a wrong password is refused when it is tested, with the reason", async {
        let (s, b) = qa.post(&qa.url("/api/registries"), json!({ "name": "qa", "kind": "custom", "url": format!("http://{reg}"), "username": "qa", "password": "not-the-password" })).await?;
        ensure!(s == StatusCode::BAD_REQUEST && b["field"] == json!("password"), "a wrong password: {s}: {b}");
        Ok(())
    })
    .await;

    let registry = step("adding it with the right password tests and saves it, and the password is never sent back", async {
        let (s, b) = qa.post(&qa.url("/api/registries"), json!({ "name": "qa", "kind": "custom", "url": format!("http://{reg}"), "username": "qa", "password": REGISTRY_PASSWORD })).await?;
        ensure!(s == StatusCode::CREATED, "add the registry: {s}: {b}");
        ensure!(b["host"] == json!(reg) && b["has_secret"] == json!(true), "{b}");
        let list = qa.get_json(&qa.url("/api/registries")).await?;
        ensure!(!list.to_string().contains(REGISTRY_PASSWORD), "the password came back: {list}");
        b["id"].as_i64().context("registry id")
    })
    .await;

    step("a push to it signs in with the saved credentials", async {
        let (s, b) = qa.post(&qa.env(&format!("/images/{IMAGE}/tag")), json!({ "reference": pushed })).await?;
        ensure!(s == StatusCode::NO_CONTENT, "tag: {s}: {b}");
        let (lines, done) = qa.stream(&qa.env("/images/push"), json!({ "image": pushed })).await?;
        ensure!(done, "the push did not end done: {lines:?}");
        Ok(())
    })
    .await;

    step("the registry browser lists its repositories and a repository's tags", async {
        let r = qa.get_json(&qa.url(&format!("/api/registries/{registry}/repositories"))).await?;
        ensure!(r["listed"] == json!(true) && r["repositories"].as_array().is_some_and(|a| a.contains(&json!("qa/alpine"))), "repositories: {r}");
        let t = qa.get_json(&qa.url(&format!("/api/registries/{registry}/tags?repository=qa/alpine"))).await?;
        ensure!(t["tags"] == json!(["1"]), "tags: {t}");
        ensure!(t["image"] == json!(format!("{reg}/qa/alpine")), "the image to pull: {t}");
        Ok(())
    })
    .await;

    // The image goes, every tag of it, so the next pull has to come from the registry.
    let forget = || async {
        let images = qa.get_json(&qa.env("/images")).await?;
        let found = images.as_array().into_iter().flatten().find(|i| i["RepoTags"].as_array().is_some_and(|t| t.contains(&json!(pushed))));
        if let Some(id) = found.and_then(|i| i["Id"].as_str()) {
            let (s, b) = qa.send(Method::DELETE, &qa.env(&format!("/images/{id}?force=true")), None).await?;
            ensure!(s == StatusCode::OK, "remove {pushed}: {s}: {b}");
        }
        Ok::<_, anyhow::Error>(())
    };

    step("a pull of a tag the browser listed signs in with the saved credentials", async {
        forget().await?;
        let (lines, done) = qa.stream(&qa.env("/images/pull"), json!({ "image": pushed })).await?;
        ensure!(done, "the pull did not end done: {lines:?}");
        Ok(())
    })
    .await;

    step("a container made from an image on it pulls with the saved credentials", async {
        forget().await?;
        let (s, made) = qa.post(&qa.env("/containers"), json!({ "name": "qa-from-registry", "start": false, "config": { "Image": pushed, "Cmd": ["true"] } })).await?;
        ensure!(s == StatusCode::CREATED, "create from the registry: {s}: {made}");
        let id = made["id"].as_str().context("no id")?;
        let (s, b) = qa.send(Method::DELETE, &qa.env(&format!("/containers/{id}?force=true")), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove: {s}: {b}");
        Ok(())
    })
    .await;

    step("a stack whose image is on it deploys, Compose signing in with the saved credentials", async {
        forget().await?;
        let (s, b) = qa.post(&qa.env("/stacks"), json!({ "name": "qa-private", "compose": greeter(&pushed) })).await?;
        ensure!(s == StatusCode::CREATED, "create: {s}: {b}");
        let id = b["id"].as_i64().context("no id")?;
        qa.stack_job(id, "deploy", json!({})).await?;
        qa.stack_is("qa-private", "running", 1).await?;
        qa.stack_job(id, "remove", json!({})).await?;
        Ok(())
    })
    .await;

    step("without the saved credentials a pull from it is refused", async {
        let (s, b) = qa.send(Method::DELETE, &qa.url(&format!("/api/registries/{registry}")), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove the registry: {s}: {b}");
        forget().await?;
        let (lines, done) = qa.stream(&qa.env("/images/pull"), json!({ "image": pushed })).await?;
        ensure!(!done, "a pull with no credentials ended done: {lines:?}");
        let last = lines.last().and_then(|l| l["error"].as_str()).unwrap_or_default().to_lowercase();
        ensure!(last.contains("auth") || last.contains("unauthorized"), "not refused for want of credentials: {lines:?}");
        let (s, b) = qa.send(Method::DELETE, &qa.env(&format!("/containers/{registry_ct}?force=true")), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove the registry's container: {s}: {b}");
        Ok(())
    })
    .await;

    step("a FinUI preset code pasted in Settings is the stylesheet everybody gets, signed in or not", async {
        let css = || async {
            let r = qa.http.get(qa.url("/assets/finui.css")).send().await?;
            ensure!(r.status() == StatusCode::OK, "finui.css: {}", r.status());
            Ok::<_, anyhow::Error>(r.text().await?)
        };
        ensure!(!css().await?.contains("FinUI preset"), "a new Freightdock wears Washi, FinUI as it ships");
        let a = qa.get_json(&qa.url("/api/settings/appearance")).await?;
        ensure!(a == json!({ "preset": "" }), "only the code is kept and sent: {a}");
        // FinUI create's Studio, pasted as its code: Geist, small corners, ink buttons.
        let code = "59121000001040301201".to_string();
        let (s, b) = qa.send(Method::PUT, &qa.url("/api/settings/appearance"), Some(json!({ "preset": code }))).await?;
        ensure!(s == StatusCode::NO_CONTENT, "PUT the preset: {s}: {b}");
        let studio_css = css().await?;
        ensure!(studio_css.contains(&format!("/* FinUI preset {code},")), "Studio's overlay is not in finui.css");
        ensure!(studio_css.contains("Geist"), "Studio's font is not named");
        let (s, b) = qa.send(Method::PUT, &qa.url("/api/settings/appearance"), Some(json!({ "preset": "" }))).await?;
        ensure!(s == StatusCode::NO_CONTENT, "back to Washi: {s}: {b}");
        ensure!(!css().await?.contains("FinUI preset"), "Washi again");
        Ok(())
    })
    .await;

    let (admin_token, read_token) = step("API tokens are made, shown once, and sign in without a cookie", async {
        let mut made = vec![];
        for (name, read_only) in [("qa-agent", false), ("qa-look", true)] {
            let (s, b) = qa.post(&qa.url("/api/tokens"), json!({ "name": name, "read_only": read_only, "expires_days": 1 })).await?;
            ensure!(s == StatusCode::CREATED, "make {name}: {s}: {b}");
            made.push(b["token"].as_str().context("no token")?.to_string());
        }
        let list = qa.get_json(&qa.url("/api/tokens")).await?;
        ensure!(!list.to_string().contains(&made[0][4..]), "a token is sent back: {list}");
        let r = qa.http.get(qa.url("/api/me")).bearer_auth(&made[0]).send().await?;
        let s = r.status();
        let me: Value = r.json().await?;
        ensure!(s == StatusCode::OK && me["user"]["role"] == json!("admin") && me["csrf"] == json!(""), "the token signs in as its owner: {s}: {me}");
        let r = qa.http.post(qa.url("/api/tokens")).bearer_auth(&made[0]).json(&json!({ "name": "child" })).send().await?;
        ensure!(r.status() == StatusCode::FORBIDDEN, "a token made a token: {}", r.status());
        Ok((made[0].clone(), made[1].clone()))
    })
    .await;

    let mut mcp = Mcp { qa: &qa, token: admin_token, session: None, next: 0 };
    step("MCP initialize answers a session, and tools/list has every tool", async {
        let r = qa.http.post(qa.url("/mcp")).json(&json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {} })).send().await?;
        ensure!(r.status() == StatusCode::UNAUTHORIZED, "MCP without a token: {}", r.status());
        let (s, h, b) = mcp.rpc("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "qa", "version": "1" } })).await?;
        ensure!(s == StatusCode::OK && b["result"]["protocolVersion"] == json!("2025-06-18"), "initialize: {s}: {b}");
        mcp.session = Some(h.get("mcp-session-id").context("no Mcp-Session-Id")?.to_str()?.to_string());
        let r = qa.http.post(qa.url("/mcp")).bearer_auth(&mcp.token).header("mcp-session-id", mcp.session.as_deref().unwrap_or_default()).json(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).send().await?;
        ensure!(r.status() == StatusCode::ACCEPTED, "notifications/initialized: {}", r.status());
        let (s, _, b) = mcp.rpc("tools/list", json!({})).await?;
        let tools: Vec<String> = b["result"]["tools"].as_array().into_iter().flatten().filter_map(|t| t["name"].as_str().map(str::to_string)).collect();
        ensure!(s == StatusCode::OK, "tools/list: {s}: {b}");
        for want in ["list_environments", "list_containers", "container_logs", "stop_container", "create_container", "exec_command", "pull_image", "activity_log"] {
            ensure!(tools.iter().any(|t| t == want), "no {want} in {tools:?}");
        }
        let envs = mcp.ok("list_environments", json!({})).await?;
        ensure!(envs[0]["name"] == json!("local") && envs[0]["status"] == json!("up"), "list_environments: {envs}");
        Ok(())
    })
    .await;

    step("through MCP: pull, run a container, see it listed, its logs, its stats and a command in it", async {
        let pulled = mcp.ok("pull_image", json!({ "image": IMAGE })).await?;
        ensure!(pulled["done"] == json!(true), "pull_image: {pulled}");
        let (error, text) = mcp.tool("pull_image", json!({ "image": "freightdock-qa/does-not-exist:never" })).await?;
        ensure!(error, "a pull of nothing went well: {text}");
        let made = mcp.ok("create_container", json!({ "env": "local", "name": "qa-mcp", "config": { "Image": IMAGE, "Cmd": ["sh", "-c", "echo hello-from-mcp; exec sleep 300"] } })).await?;
        ensure!(made["id"].is_string(), "create_container: {made}");
        let list = mcp.ok("list_containers", json!({ "env": qa.env, "name": "qa-mcp" })).await?;
        ensure!(list[0]["name"] == json!("qa-mcp") && list[0]["state"] == json!("running"), "list_containers: {list}");
        mcp.qa.until(Duration::from_secs(15), || async {
            let logs = Mcp { qa: &qa, token: mcp.token.clone(), session: None, next: 0 }.ok("container_logs", json!({ "container": "qa-mcp", "tail": 10 })).await?;
            ensure!(logs["logs"].as_str().unwrap_or_default().contains("hello-from-mcp"), "container_logs: {logs}");
            Ok(())
        })
        .await?;
        let ran = mcp.ok("exec_command", json!({ "container": "qa-mcp", "cmd": ["sh", "-c", "echo out; echo err >&2; exit 4"] })).await?;
        ensure!(ran["exit_code"] == json!(4) && ran["stdout"] == json!("out\n") && ran["stderr"] == json!("err\n") && ran["timed_out"] == json!(false), "exec_command: {ran}");
        let slow = mcp.ok("exec_command", json!({ "container": "qa-mcp", "cmd": ["sleep", "30"], "timeout_s": 1 })).await?;
        ensure!(slow["timed_out"] == json!(true) && slow["exit_code"].is_null(), "a command past its time: {slow}");
        let stats = mcp.ok("container_stats", json!({ "container": "qa-mcp" })).await?;
        ensure!(stats["pids"].as_u64().unwrap_or(0) >= 1 && stats["mem"].is_u64(), "container_stats: {stats}");
        for (tool, key) in [("list_images", "tags"), ("list_networks", "driver"), ("list_volumes", "")] {
            let v = mcp.ok(tool, json!({})).await?;
            ensure!(v.is_array() && (key.is_empty() || v[0].get(key).is_some()), "{tool}: {v}");
        }
        let df = mcp.ok("disk_usage", json!({})).await?;
        ensure!(df.is_object(), "disk_usage: {df}");
        Ok(())
    })
    .await;

    step("a read-only token is refused a stop through MCP, and the container runs on", async {
        let mut look = Mcp { qa: &qa, token: read_token.clone(), session: None, next: 0 };
        let list = look.ok("list_containers", json!({})).await?;
        ensure!(list.as_array().is_some_and(|l| !l.is_empty()), "a read-only token reads: {list}");
        let (error, text) = look.tool("stop_container", json!({ "container": "qa-mcp" })).await?;
        ensure!(error && text.contains("403"), "a read-only token stopped a container: {text}");
        let (error, text) = look.tool("exec_command", json!({ "container": "qa-mcp", "cmd": ["true"] })).await?;
        ensure!(error && text.contains("403"), "a read-only token ran a command: {text}");
        ensure!(qa.running("qa-mcp").await?, "it was stopped");
        Ok(())
    })
    .await;

    step("through MCP: stop it, remove it, and the activity log says which token did", async {
        let done = mcp.ok("stop_container", json!({ "container": "qa-mcp" })).await?;
        ensure!(done["ok"] == json!(true), "stop_container: {done}");
        ensure!(!qa.running("qa-mcp").await?, "it still runs");
        mcp.ok("remove_container", json!({ "container": "qa-mcp" })).await?;
        let (s, _) = qa.inspect("qa-mcp").await?;
        ensure!(s == StatusCode::NOT_FOUND, "it is still there: {s}");
        let log = mcp.ok("activity_log", json!({ "action": "container", "limit": 20 })).await?;
        let by_token: Vec<&Value> = log.as_array().into_iter().flatten().filter(|e| e["via"] == json!("token qa-agent")).collect();
        for a in ["container.create", "container.exec", "container.stop", "container.remove"] {
            ensure!(by_token.iter().any(|e| e["action"] == json!(a)), "no {a} via the token in {log}");
        }
        let r = qa.http.delete(qa.url("/mcp")).bearer_auth(&mcp.token).header("mcp-session-id", mcp.session.as_deref().unwrap_or_default()).send().await?;
        ensure!(r.status() == StatusCode::NO_CONTENT, "end the session: {}", r.status());
        Ok(())
    })
    .await;

    step("the MCP switch in Settings: off refuses the agent, the card lists its token, on lets it back in", async {
        let card = qa.get_json(&qa.url("/api/settings/mcp")).await?;
        ensure!(card["enabled"] == json!(true), "on by default: {card}");
        ensure!(card["tokens"].as_array().into_iter().flatten().any(|t| t["name"] == json!("qa-agent") && t["owner"]["name"].is_string() && t["last_used_at"].is_i64()), "the agent's token, whose, last used: {card}");
        let (s, b) = qa.send(Method::PUT, &qa.url("/api/settings/mcp"), Some(json!({ "enabled": false }))).await?;
        ensure!(s == StatusCode::NO_CONTENT, "turn it off: {s}: {b}");
        let (s, _, b) = Mcp { qa: &qa, token: mcp.token.clone(), session: None, next: 0 }.rpc("tools/list", json!({})).await?;
        ensure!(s == StatusCode::FORBIDDEN && b.to_string().contains("turned off"), "off: {s}: {b}");
        let (s, b) = qa.send(Method::PUT, &qa.url("/api/settings/mcp"), Some(json!({ "enabled": true }))).await?;
        ensure!(s == StatusCode::NO_CONTENT, "turn it on: {s}: {b}");
        let envs = Mcp { qa: &qa, token: mcp.token.clone(), session: None, next: 0 }.ok("list_environments", json!({})).await?;
        ensure!(envs.as_array().is_some_and(|e| !e.is_empty()), "back on: {envs}");
        let (s, _) = qa.get(&qa.url("/api/tls/ca.pem")).await?;
        ensure!(s == StatusCode::NOT_FOUND, "plain HTTP here, so no CA to hand out: {s}");
        Ok(())
    })
    .await;

    let rport: u16 = std::env::var("FREIGHTDOCK_QA_REGISTRY_PORT").ok().and_then(|p| p.parse().ok()).expect("FREIGHTDOCK_QA_REGISTRY_PORT: the registry's port (qa/run.sh sets it)");
    let app_ref = format!("localhost:{rport}/qa/app:1");
    let registry = step("a registry runs in the dind, published where Freightdock reaches it too", async {
        let (lines, done) = qa.stream(&qa.env("/images/pull"), json!({ "image": "registry:2" })).await?;
        ensure!(done, "pulling registry:2: {lines:?}");
        let port = format!("{rport}");
        let body = json!({ "name": "qa-registry", "config": { "Image": "registry:2", "ExposedPorts": { "5000/tcp": {} }, "HostConfig": { "PortBindings": { "5000/tcp": [{ "HostPort": port }] } } } });
        let (s, made) = qa.post(&qa.env("/containers"), body).await?;
        ensure!(s == StatusCode::CREATED, "create the registry: {s}: {made}");
        qa.until(Duration::from_secs(30), || async {
            let r = qa.http.get(format!("http://127.0.0.1:{rport}/v2/")).send().await?;
            ensure!(r.status() == StatusCode::OK, "the registry answers {}", r.status());
            Ok(())
        })
        .await?;
        made["id"].as_str().map(str::to_string).context("no id")
    })
    .await;

    // Builds the app's image as `version` under the one tag, and pushes it.
    let build_push = |version: &'static str| {
        let (qa, app_ref) = (&qa, app_ref.clone());
        async move {
            let dockerfile = format!("FROM {IMAGE}\nRUN echo {version} > /version\nCMD [\"sleep\", \"3600\"]\n");
            let tag = app_ref.replace(':', "%3A").replace('/', "%2F");
            let (lines, done) = qa.stream(&qa.env(&format!("/images/build?tag={tag}")), json!({ "dockerfile": dockerfile })).await?;
            ensure!(done, "building {version}: {lines:?}");
            let (lines, done) = qa.stream(&qa.env("/images/push"), json!({ "image": app_ref })).await?;
            ensure!(done, "pushing {version}: {lines:?}");
            let images = qa.get_json(&qa.env("/images")).await?;
            let mine = images.as_array().into_iter().flatten().find(|i| i["RepoTags"].as_array().is_some_and(|t| t.contains(&json!(app_ref))));
            mine.and_then(|i| i["Id"].as_str()).map(str::to_string).with_context(|| format!("{app_ref} is not in the image list"))
        }
    };
    let status_of = |id: String| {
        let qa = &qa;
        async move {
            let u = qa.get_json(&qa.env("/updates")).await?;
            Ok::<_, anyhow::Error>(u["containers"][&id].clone())
        }
    };

    let (app_id, v1) = step("an image built here, pushed, and run with automatic updates on is current", async {
        let v1 = build_push("one").await?;
        let (s, made) = qa.post(&qa.env("/containers"), json!({ "name": "qa-app", "config": { "Image": app_ref, "Labels": { "freightdock.auto-update": "true" } } })).await?;
        ensure!(s == StatusCode::CREATED, "create: {s}: {made}");
        let id = made["id"].as_str().context("no id")?.to_string();
        let (s, sum) = qa.post(&qa.url("/api/updates/check"), json!({})).await?;
        ensure!(s == StatusCode::OK, "check: {s}: {sum}");
        let st = status_of(id.clone()).await?;
        ensure!(st["status"] == json!("current") && st["auto"] == json!(true), "the verdict: {st} (the pass: {sum})");
        Ok((id, v1))
    })
    .await;

    step("a newer image pushed under its tag is found, and the container is updated to it", async {
        let v2 = build_push("two").await?;
        ensure!(v2 != v1, "the second build is the same image");
        let (s, sum) = qa.post(&qa.url("/api/updates/check"), json!({})).await?;
        ensure!(s == StatusCode::OK, "check: {s}: {sum}");
        let st = status_of(app_id.clone()).await?;
        ensure!(st["status"] == json!("outdated"), "the verdict after a newer push: {st} (the pass: {sum})");
        let (s, sum) = qa.post(&qa.url("/api/updates/check"), json!({ "apply": true })).await?;
        ensure!(s == StatusCode::OK, "check and apply: {s}: {sum}");
        ensure!(sum["updated"] == json!(["qa-app"]), "it was not updated: {sum}");
        let (s, c) = qa.inspect("qa-app").await?;
        ensure!(s == StatusCode::OK, "inspect: {s}: {c}");
        ensure!(c["Id"] != json!(app_id), "qa-app is still the old container");
        ensure!(c["Image"] == json!(v2), "qa-app runs {} and not the new image {v2}", c["Image"]);
        ensure!(c["State"]["Running"] == json!(true), "the updated container is not running");
        ensure!(c["Config"]["Labels"]["freightdock.auto-update"] == json!("true"), "its settings were not kept");
        let (s, b) = qa.send(Method::DELETE, &qa.env("/containers/qa-app?force=true"), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove: {s}: {b}");
        let (s, b) = qa.send(Method::DELETE, &qa.env(&format!("/containers/{registry}?force=true")), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove the registry: {s}: {b}");
        Ok(())
    })
    .await;

    stacks(&qa).await;
    templates(&qa).await;
    webhooks(&qa).await;
    portainer(&qa).await;

    step("a webhook channel gets its test, and a container that dies is told to it", async {
        // A tiny listener on loopback, where Freightdock (on this machine too) can reach it: each request's head and body.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/qa-hook", listener.local_addr()?);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let hook = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut s, _)) = listener.accept().await {
                let mut got = Vec::new();
                let mut buf = [0u8; 8192];
                while let Ok(n) = s.read(&mut buf).await {
                    got.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&got).to_string();
                    let len = text.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
                    if n == 0 || text.find("\r\n\r\n").is_some_and(|i| got.len() >= i + 4 + len) { break; }
                }
                let _ = s.write_all(b"HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n").await;
                let _ = tx.send(String::from_utf8_lossy(&got).to_string());
            }
        });
        let (s, ch) = qa.post(&qa.url("/api/notifications"), json!({ "name": "qa-hook", "kind": "webhook", "events": ["container.died"], "config": { "url": url, "secret": "qa-signing-key" } })).await?;
        ensure!(s == StatusCode::CREATED, "add the channel: {s}: {ch}");
        let channel = ch["id"].as_i64().context("channel id")?;
        let (s, b) = qa.post(&qa.url(&format!("/api/notifications/{channel}/test")), json!({})).await?;
        ensure!(s == StatusCode::NO_CONTENT, "the test: {s}: {b}");
        let got = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.context("the test did not arrive")?.context("the listener ended")?;
        ensure!(got.starts_with("POST /qa-hook") && got.contains("\"event\":\"test\"") && got.contains("x-freightdock-signature: sha256="), "the test as it came: {got}");

        // The events watch follows each environment; a moment for it to be listening.
        tokio::time::sleep(Duration::from_secs(3)).await;
        let (s, made) = qa.post(&qa.env("/containers"), json!({ "name": "qa-doomed", "config": { "Image": IMAGE, "Cmd": ["sh", "-c", "sleep 1; exit 3"] } })).await?;
        ensure!(s == StatusCode::CREATED, "create: {s}: {made}");
        let doomed = made["id"].as_str().context("no id")?.to_string();
        let told = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let m = rx.recv().await.context("the listener ended")?;
                if m.contains("container.died") { return Ok::<_, anyhow::Error>(m); }
            }
        }).await.context("no notification came within 30 s of the container dying")??;
        ensure!(told.contains("qa-doomed died on local") && told.contains("exited with code 3"), "the notification: {told}");
        let (s, b) = qa.send(Method::DELETE, &qa.env(&format!("/containers/{doomed}?force=true")), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove: {s}: {b}");
        let (_, list) = qa.send(Method::GET, &qa.url("/api/notifications"), None).await?;
        ensure!(list["channels"][0]["last_sent_at"].is_i64(), "the channel does not say it sent: {list}");
        hook.abort();
        Ok(())
    })
    .await;

    step("backups to a folder are made and the oldest beyond keep go", async {
        let body = json!({ "enabled": false, "every_hours": 24, "keep": 2, "destination": "folder", "passphrase": "qa backup passphrase" });
        let (s, b) = qa.send(Method::PUT, &qa.url("/api/settings/backups"), Some(body)).await?;
        ensure!(s == StatusCode::NO_CONTENT, "the backup settings: {s}: {b}");
        for _ in 0..3 {
            let (s, b) = qa.post(&qa.url("/api/backups/run"), json!({})).await?;
            ensure!(s == StatusCode::OK, "a backup: {s}: {b}");
            // Archives are named by the second.
            tokio::time::sleep(Duration::from_millis(1100)).await;
        }
        let l = qa.get_json(&qa.url("/api/backups")).await?;
        ensure!(l["archives"].as_array().is_some_and(|a| a.len() == 2), "keep 2: {l}");
        Ok(())
    })
    .await;

    let s3 = std::env::var("FREIGHTDOCK_QA_S3").expect("FREIGHTDOCK_QA_S3: the S3 store's address (qa/run.sh sets it)");
    let archive = step("a backup to S3 (MinIO, signed with Signature Version 4) makes its bucket and is listed", async {
        let body = json!({ "enabled": false, "every_hours": 24, "keep": 2, "destination": "s3",
            "s3": { "endpoint": s3, "region": "us-east-1", "bucket": "qa-backups", "prefix": "freightdock/", "access_key": "qa-access-key", "secret_key": "qa-secret-key-1234" } });
        let (s, b) = qa.send(Method::PUT, &qa.url("/api/settings/backups"), Some(body)).await?;
        ensure!(s == StatusCode::NO_CONTENT, "the S3 settings: {s}: {b}");
        // MinIO may still be starting.
        qa.until(Duration::from_secs(60), || async {
            let (s, b) = qa.post(&qa.url("/api/backups/run"), json!({})).await?;
            ensure!(s == StatusCode::OK, "a backup to S3: {s}: {b}");
            Ok(())
        })
        .await?;
        let l = qa.get_json(&qa.url("/api/backups")).await?;
        ensure!(l["destination"] == json!("s3") && l["error"].is_null(), "the S3 listing: {l}");
        l["archives"][0]["name"].as_str().map(str::to_string).with_context(|| format!("no archive in S3: {l}"))
    })
    .await;

    step("a restore from S3 puts the backup back: Freightdock starts itself again and what came after is gone", async {
        let (s, ch) = qa.post(&qa.url("/api/notifications"), json!({ "name": "made-after-the-backup", "kind": "webhook", "events": [], "config": { "url": "http://127.0.0.1:1/x" } })).await?;
        ensure!(s == StatusCode::CREATED, "a channel after the backup: {s}: {ch}");
        let (s, b) = qa.post(&qa.url("/api/backups/restore"), json!({ "name": archive, "passphrase": "not the passphrase" })).await?;
        ensure!(s == StatusCode::BAD_REQUEST, "a wrong passphrase: {s}: {b}");
        let (s, b) = qa.post(&qa.url("/api/backups/restore"), json!({ "name": archive, "passphrase": "qa backup passphrase" })).await?;
        ensure!(s == StatusCode::ACCEPTED && b["restarting"] == json!(true), "the restore: {s}: {b}");
        tokio::time::sleep(Duration::from_secs(2)).await;
        qa.until(Duration::from_secs(60), || async {
            let names: Vec<String> = qa.get_json(&qa.url("/api/notifications")).await?["channels"].as_array().into_iter().flatten().filter_map(|c| c["name"].as_str().map(str::to_string)).collect();
            ensure!(names.contains(&"qa-hook".to_string()) && !names.contains(&"made-after-the-backup".to_string()), "the channels after the restore: {names:?}");
            Ok(())
        })
        .await?;
        let actions = qa.audit_actions().await?;
        ensure!(actions.first().is_some_and(|a| a == "backup.restored"), "the restore is not the newest entry: {:?}", &actions[..actions.len().min(5)]);
        Ok(())
    })
    .await;

    access::steps(&qa).await;
    signin::steps(&qa).await;

    // Swarm last: it makes the dind a swarm and leaves it again at the end ("QA" 4).
    swarm::steps(&qa).await;

    step("every change is in the activity log", async {
        let actions = qa.audit_actions().await?;
        for a in ["setup", "image.pull", "container.create", "container.exec", "container.attach", "network.create", "network.connect", "network.disconnect", "network.remove", "container.edit", "container.stop", "container.remove", "image.build", "image.remove", "image.push", "registry.create", "registry.remove", "settings.appearance",
                  "environment-group.create", "environment-group.remove", "environment.update", "tag.create", "tag.remove",
                  "stack.create", "stack.deploy", "stack.update", "stack.stop", "stack.start", "stack.remove", "stack.adopt",
                  "settings.templates", "template.create", "webhook.create", "webhook.refused", "token.create", "portainer.connect", "portainer.import",
                  "settings.history", "notification.create", "notification.test", "updates.check", "container.autoupdate", "volume.mkdir", "volume.upload", "volume.download", "volume.delete_path", "settings.backups", "backup.make", "backup.restored",
                  "user.create", "user.remove", "team.create", "team.remove", "access.update",
                  "settings.sso", "settings.sso.remove", "account.totp-on", "settings.two-factor", "user.totp-reset"] {
            ensure!(actions.iter().any(|x| x == a), "no {a} in the activity log: {actions:?}");
        }
        Ok(())
    })
    .await;
}

/// A compose file whose one service says `$GREETING` (from the stack's variables) and then waits, stopping at once.
fn greeter(image: &str) -> String {
    format!("services:\n  app:\n    image: {image}\n    environment:\n      GREETING: ${{GREETING:-nobody}}\n    command: [\"sh\", \"-c\", \"echo greeting=$$GREETING; exec sleep 3600\"]\n    stop_signal: SIGKILL\n")
}

/// Stacks through `docker compose`: deploy, update, stop, start, remove; and a project started elsewhere, adopted.
async fn stacks(qa: &Qa) {
    let id = step("a stack is made and deployed with docker compose, its variables filled in", async {
        let (s, b) = qa.post(&qa.env("/stacks"), json!({ "name": "qa-stack", "compose": greeter(IMAGE), "env": "GREETING=hello\n" })).await?;
        ensure!(s == StatusCode::CREATED, "create: {s}: {b}");
        let id = b["id"].as_i64().context("no id")?;
        let lines = qa.stack_job(id, "deploy", json!({})).await?;
        ensure!(lines.iter().any(|l| l["stream"].as_str().is_some_and(|t| t.contains("qa-stack-app-1"))), "Compose's own output streams: {lines:?}");
        qa.stack_is("qa-stack", "running", 1).await?;
        qa.until(Duration::from_secs(15), || async {
            let logs = qa.logs("qa-stack-app-1").await?;
            ensure!(logs.contains("greeting=hello"), "logs so far: {logs:?}");
            Ok(())
        })
        .await?;
        Ok(id)
    })
    .await;

    step("a stack's environment is each service's variables as Compose fills them in", async {
        let v = qa.get_json(&qa.env(&format!("/stacks/{id}/environment"))).await?;
        ensure!(v["services"] == json!([{ "service": "app", "environment": [["GREETING", "hello"]] }]), "the .env value, not the default: {v}");
        Ok(())
    })
    .await;

    step("a stack whose file Compose refuses is not made", async {
        let (s, b) = qa.post(&qa.env("/stacks"), json!({ "name": "qa-broken", "compose": "services:\n  app:\n    imag: nope\n" })).await?;
        ensure!(s == StatusCode::BAD_REQUEST && b["field"] == json!("compose"), "create: {s}: {b}");
        ensure!(qa.stack("qa-broken").await?.is_none(), "it was kept");
        Ok(())
    })
    .await;

    step("an update of its variables reaches the container on the next deploy", async {
        let (s, b) = qa.send(Method::PUT, &qa.env(&format!("/stacks/{id}")), Some(json!({ "compose": greeter(IMAGE), "env": "GREETING=updated\n" }))).await?;
        ensure!(s == StatusCode::NO_CONTENT, "update: {s}: {b}");
        qa.stack_job(id, "deploy", json!({ "pull": true })).await?;
        qa.until(Duration::from_secs(15), || async {
            let logs = qa.logs("qa-stack-app-1").await?;
            ensure!(logs.contains("greeting=updated"), "logs so far: {logs:?}");
            Ok(())
        })
        .await
    })
    .await;

    step("stop the stack, and start it again", async {
        qa.stack_job(id, "stop", json!({})).await?;
        qa.stack_is("qa-stack", "stopped", 1).await?;
        qa.stack_job(id, "start", json!({})).await?;
        qa.stack_is("qa-stack", "running", 1).await?;
        Ok(())
    })
    .await;

    step("remove the stack: its containers and its place in Freightdock go", async {
        qa.stack_job(id, "remove", json!({ "volumes": true })).await?;
        ensure!(qa.stack("qa-stack").await?.is_none(), "it is still listed");
        let (s, _) = qa.get(&qa.env(&format!("/stacks/{id}"))).await?;
        ensure!(s == StatusCode::NOT_FOUND, "the stack is still there: {s}");
        ensure!(qa.containers().await?.is_empty(), "containers are left: {:?}", names(&qa.containers().await?));
        Ok(())
    })
    .await;

    let elsewhere = step("a project started on the command line is listed as external", async {
        let compose = std::env::var("FREIGHTDOCK_QA_COMPOSE").context("FREIGHTDOCK_QA_COMPOSE: the Compose binary (qa/run.sh sets it)")?;
        let docker = std::env::var("FREIGHTDOCK_QA_DOCKER").context("FREIGHTDOCK_QA_DOCKER: the dind's address")?;
        let dir = std::path::PathBuf::from(std::env::var("FREIGHTDOCK_QA_SCRATCH").context("FREIGHTDOCK_QA_SCRATCH")?);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("compose.yaml"), greeter(IMAGE))?;
        std::fs::write(dir.join(".env"), "GREETING=from-elsewhere\n")?;
        let out = tokio::process::Command::new(&compose)
            .args(["-p", "qa-elsewhere", "up", "--detach"])
            .current_dir(&dir)
            .env("DOCKER_HOST", &docker)
            .output()
            .await
            .context("running Compose")?;
        ensure!(out.status.success(), "compose up elsewhere: {}", String::from_utf8_lossy(&out.stderr));
        let s = qa.stack_is("qa-elsewhere", "running", 1).await?;
        ensure!(s["external"] == json!(true) && s["id"].is_null(), "not external: {s}");
        Ok(dir)
    })
    .await;

    let adopted = step("adopting it keeps its file, read where it was started, and its variables", async {
        let x = qa.get_json(&qa.env("/stacks/external/qa-elsewhere")).await?;
        let file = x["compose"].as_str().with_context(|| format!("Freightdock could not read its file: {x}"))?;
        ensure!(file.contains("GREETING: ${GREETING:-nobody}"), "variables must stay ${{...}}, not be filled in: {file}");
        ensure!(x["env"] == json!("GREETING=from-elsewhere\n"), "its .env: {x}");
        let (s, b) = qa.post(&qa.env("/stacks/adopt"), json!({ "name": "qa-elsewhere" })).await?;
        ensure!(s == StatusCode::CREATED, "adopt: {s}: {b}");
        let id = b["id"].as_i64().context("no id")?;
        let st = qa.stack_is("qa-elsewhere", "running", 1).await?;
        ensure!(st["external"] == json!(false) && st["id"] == json!(id), "still external: {st}");
        let (s, b) = qa.post(&qa.env("/stacks/adopt"), json!({ "name": "qa-elsewhere" })).await?;
        ensure!(s == StatusCode::CONFLICT, "adopted twice: {s}: {b}");
        Ok(id)
    })
    .await;

    step("the adopted stack deploys from Freightdock, and is removed", async {
        qa.stack_job(adopted, "deploy", json!({})).await?;
        qa.stack_is("qa-elsewhere", "running", 1).await?;
        let logs = qa.logs("qa-elsewhere-app-1").await?;
        ensure!(logs.contains("greeting=from-elsewhere"), "its variables came along: {logs:?}");
        qa.stack_job(adopted, "remove", json!({})).await?;
        ensure!(qa.containers().await?.is_empty(), "containers are left");
        std::fs::remove_dir_all(&elsewhere)?;
        Ok(())
    })
    .await;
}

/// A template list in Portainer's v3 format, served by the suite itself on a port of its own: one container template.
async fn serve_templates() -> String {
    let list = json!({ "version": "3", "templates": [
        { "id": 1, "type": 1, "title": "QA sleeper", "description": "Waits", "image": IMAGE, "command": "sh -c 'echo template=$GREETING; exec sleep 3600'",
          "env": [{ "name": "GREETING", "label": "Greeting", "default": "unset" }, { "name": "FIXED", "default": "kept", "preset": true }] },
        { "id": 2, "type": 2, "title": "QA swarm", "description": "Needs Swarm", "repository": { "url": "https://example.com/r", "stackfile": "s.yml" } },
    ] })
    .to_string();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a port for the template list");
    let addr = l.local_addr().unwrap();
    let r = axum::Router::new().route("/templates.json", axum::routing::get(move || async move { ([("content-type", "application/json")], list) }));
    tokio::spawn(async move { axum::serve(l, r).await.unwrap() });
    format!("http://{addr}/templates.json")
}

/// App templates: a list by its address, a container template deployed from it, and a custom template as a stack.
async fn templates(qa: &Qa) {
    let url = serve_templates().await;
    step("a template list in Portainer's v3 format is read from its address", async {
        let (s, b) = qa.send(Method::PUT, &qa.url("/api/settings/templates"), Some(json!({ "urls": [url] }))).await?;
        ensure!(s == StatusCode::NO_CONTENT, "settings: {s}: {b}");
        let l = qa.get_json(&qa.url("/api/templates")).await?;
        let titles: Vec<&str> = l["lists"][0]["templates"].as_array().context("no templates")?.iter().filter_map(|t| t["title"].as_str()).collect();
        ensure!(titles == ["QA sleeper", "QA swarm"], "templates: {l}");
        ensure!(l["lists"][0]["templates"][1]["deployable"] == json!(false), "a Swarm template is not deployable here: {l}");
        Ok(())
    })
    .await;

    step("a container template is deployed with the variables given", async {
        let (s, b) = qa.post(&qa.env("/templates/deploy"), json!({ "list": url, "index": 0, "name": "qa-from-template", "env": { "GREETING": "hi", "FIXED": "changed" } })).await?;
        ensure!(s == StatusCode::CREATED, "deploy: {s}: {b}");
        ensure!(qa.running("qa-from-template").await?, "it does not run");
        let (_, c) = qa.inspect("qa-from-template").await?;
        let env = env_of(&c);
        ensure!(env.contains(&"GREETING=hi".to_string()) && env.contains(&"FIXED=kept".to_string()), "a preset stays the template's: {env:?}");
        qa.until(Duration::from_secs(15), || async {
            let logs = qa.logs("qa-from-template").await?;
            ensure!(logs.contains("template=hi"), "logs so far: {logs:?}");
            Ok(())
        })
        .await?;
        let (s, b) = qa.send(Method::DELETE, &qa.env("/containers/qa-from-template?force=true"), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove: {s}: {b}");
        Ok(())
    })
    .await;

    step("a custom template is deployed as a stack", async {
        let (s, b) = qa.post(&qa.url("/api/templates/custom"), json!({ "title": "QA greeter", "compose": greeter(IMAGE), "env": [{ "name": "GREETING", "default": "custom" }] })).await?;
        ensure!(s == StatusCode::CREATED, "custom: {s}: {b}");
        let (s, b) = qa.post(&qa.env("/templates/deploy"), json!({ "custom": b["id"], "name": "qa-custom" })).await?;
        ensure!(s == StatusCode::CREATED, "deploy: {s}: {b}");
        let id = b["stack"].as_i64().context("no stack")?;
        qa.stack_job(id, "deploy", json!({})).await?;
        qa.stack_is("qa-custom", "running", 1).await?;
        qa.until(Duration::from_secs(15), || async {
            let logs = qa.logs("qa-custom-app-1").await?;
            ensure!(logs.contains("greeting=custom"), "logs so far: {logs:?}");
            Ok(())
        })
        .await?;
        qa.stack_job(id, "remove", json!({})).await?;
        ensure!(qa.containers().await?.is_empty(), "containers are left");
        Ok(())
    })
    .await;
}

/// Webhooks, called as a CI job calls them: no cookie, no CSRF token, no Origin.
async fn webhooks(qa: &Qa) {
    let hook = |url: &str| {
        let url = format!("{}{url}", qa.base);
        async move { Ok::<_, anyhow::Error>(reqwest::Client::new().post(&url).send().await.with_context(|| format!("POST {url}"))?.status()) }
    };

    step("a container's webhook pulls its image and recreates it", async {
        let (s, b) = qa.post(&qa.env("/containers"), json!({ "name": "qa-hooked", "config": { "Image": IMAGE, "Cmd": ["sleep", "3600"] } })).await?;
        ensure!(s == StatusCode::CREATED, "create: {s}: {b}");
        let before = b["id"].as_str().context("no id")?.to_string();
        let (s, b) = qa.post(&qa.env("/containers/qa-hooked/webhook"), json!({})).await?;
        ensure!(s == StatusCode::CREATED, "make the webhook: {s}: {b}");
        let url = b["url"].as_str().context("no url")?.to_string();
        let s = hook(&url).await?;
        ensure!(s == StatusCode::NO_CONTENT, "the webhook: {s}");
        let (_, c) = qa.inspect("qa-hooked").await?;
        ensure!(c["Id"].as_str() != Some(before.as_str()), "it was not recreated: {}", c["Id"]);
        ensure!(qa.running("qa-hooked").await?, "it does not run after");
        let s = hook(&url).await?;
        ensure!(s == StatusCode::TOO_MANY_REQUESTS, "a second call at once: {s}");
        let s = hook(&format!("/api/webhooks/{}", "0".repeat(64))).await?;
        ensure!(s == StatusCode::NOT_FOUND, "a wrong address: {s}");
        let (s, b) = qa.send(Method::DELETE, &qa.env("/containers/qa-hooked?force=true"), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove: {s}: {b}");
        Ok(())
    })
    .await;

    step("a stack's webhook deploys it again with what changed", async {
        let (s, b) = qa.post(&qa.env("/stacks"), json!({ "name": "qa-hooked-stack", "compose": greeter(IMAGE), "env": "GREETING=before\n" })).await?;
        ensure!(s == StatusCode::CREATED, "create: {s}: {b}");
        let id = b["id"].as_i64().context("no id")?;
        qa.stack_job(id, "deploy", json!({})).await?;
        let (s, b) = qa.post(&qa.env(&format!("/stacks/{id}/webhook")), json!({})).await?;
        ensure!(s == StatusCode::CREATED, "make the webhook: {s}: {b}");
        let url = b["url"].as_str().context("no url")?.to_string();
        let (s, b) = qa.send(Method::PUT, &qa.env(&format!("/stacks/{id}")), Some(json!({ "compose": greeter(IMAGE), "env": "GREETING=from-ci\n" }))).await?;
        ensure!(s == StatusCode::NO_CONTENT, "update: {s}: {b}");
        let s = hook(&url).await?;
        ensure!(s == StatusCode::NO_CONTENT, "the webhook: {s}");
        qa.until(Duration::from_secs(15), || async {
            let logs = qa.logs("qa-hooked-stack-app-1").await?;
            ensure!(logs.contains("greeting=from-ci"), "logs so far: {logs:?}");
            Ok(())
        })
        .await?;
        let entries = qa.get_json(&qa.url("/api/audit?action=stack.deploy")).await?;
        ensure!(entries["entries"][0]["detail"].as_str().is_some_and(|d| d.starts_with("by webhook")), "not logged as by the webhook: {entries}");
        qa.stack_job(id, "remove", json!({})).await?;
        let s = hook(&url).await?;
        ensure!(s == StatusCode::NOT_FOUND, "the webhook outlived its stack: {s}");
        ensure!(qa.containers().await?.is_empty(), "containers are left");
        Ok(())
    })
    .await;
}

/// A fake Portainer, served by the suite on a port of its own: one Docker environment (whose engine's ID is the dind's,
/// as Portainer's Docker proxy would say), and one Compose stack whose project the suite has started in the dind as
/// Portainer would have. Everything it is asked is written down, to see Freightdock only ever reads.
async fn serve_portainer(engine: String, dir: String, file: String) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let a = asked.clone();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a port for the fake Portainer");
    let addr = l.local_addr().unwrap();
    let r = axum::Router::new().fallback(move |m: axum::http::Method, u: axum::http::Uri, h: axum::http::HeaderMap| {
        let (a, engine, dir, file) = (a.clone(), engine.clone(), dir.clone(), file.clone());
        async move {
            a.lock().unwrap().push(format!("{m} {}", u.path()));
            let ok = |v: Value| (axum::http::StatusCode::OK, axum::Json(v));
            if m != axum::http::Method::GET {
                return (axum::http::StatusCode::METHOD_NOT_ALLOWED, axum::Json(json!({ "message": "read only" })));
            }
            if u.path() == "/api/system/status" {
                return ok(json!({ "Version": "2.45.3", "InstanceID": "qa-portainer" }));
            }
            if h.get("x-api-key").and_then(|v| v.to_str().ok()) != Some("ptr_qa") {
                return (axum::http::StatusCode::UNAUTHORIZED, axum::Json(json!({ "message": "A valid authorization token is missing" })));
            }
            match u.path() {
                "/api/endpoints" => ok(json!([{ "Id": 1, "Name": "qa-portainer-host", "Type": 1, "URL": "unix:///var/run/docker.sock", "GroupId": 1, "Status": 1 }])),
                "/api/endpoints/1/docker/info" => ok(json!({ "ID": engine, "Name": "dind" })),
                "/api/stacks" => ok(json!([{ "Id": 77, "Name": "qa-portainer", "Type": 2, "EndpointId": 1, "EntryPoint": "docker-compose.yml", "Status": 1,
                                            "Env": [{ "name": "GREETING", "value": "from-portainer" }], "ProjectPath": dir, "AdditionalFiles": null, "GitConfig": null }])),
                "/api/stacks/77/file" => ok(json!({ "StackFileContent": file })),
                "/api/custom_templates" | "/api/registries" | "/api/tags" | "/api/users" | "/api/teams" | "/api/team_memberships" => ok(json!([])),
                "/api/endpoint_groups" => ok(json!([{ "Id": 1, "Name": "Unassigned" }])),
                _ => (axum::http::StatusCode::NOT_FOUND, axum::Json(json!({ "message": "not found" }))),
            }
        }
    });
    tokio::spawn(async move { axum::serve(l, r).await.unwrap() });
    (format!("http://{addr}"), asked)
}

/// Import from Portainer: a project Portainer started (its compose file in a versioned folder, its variables in
/// `stack.env`, a relative bind mount) is found through Portainer's API, adopted in place with its container kept
/// running, not made twice, then redeployed from Freightdock with the same variables and mount.
async fn portainer(qa: &Qa) {
    let file = "services:\n  app:\n    image: alpine:3.20\n    env_file: stack.env\n    volumes:\n      - ./data:/data\n    command: [\"sh\", \"-c\", \"echo greeting=$$GREETING; exec sleep 3600\"]\n    stop_signal: SIGKILL\n";
    let dir = step("a project Portainer started runs in the dind, from its folder and with its stack.env", async {
        let compose = std::env::var("FREIGHTDOCK_QA_COMPOSE").context("FREIGHTDOCK_QA_COMPOSE")?;
        let docker = std::env::var("FREIGHTDOCK_QA_DOCKER").context("FREIGHTDOCK_QA_DOCKER")?;
        let dir = std::path::PathBuf::from(std::env::var("FREIGHTDOCK_QA_SCRATCH").context("FREIGHTDOCK_QA_SCRATCH")?).join("portainer/compose/77/v1");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("docker-compose.yml"), file)?;
        std::fs::write(dir.join("stack.env"), "GREETING=from-portainer\n")?;
        let out = tokio::process::Command::new(&compose)
            .args(["-p", "qa-portainer", "-f", "docker-compose.yml", "--env-file", "stack.env", "up", "--detach"])
            .current_dir(&dir)
            .env("DOCKER_HOST", &docker)
            .output()
            .await
            .context("running Compose")?;
        ensure!(out.status.success(), "compose up as Portainer: {}", String::from_utf8_lossy(&out.stderr));
        qa.stack_is("qa-portainer", "running", 1).await?;
        Ok(dir)
    })
    .await;
    let engine = qa.get_json(&qa.env("/info")).await.expect("the dind's info")["info"]["ID"].as_str().unwrap_or_default().to_string();
    let (purl, asked) = serve_portainer(engine.clone(), dir.to_string_lossy().to_string(), file.to_string()).await;

    step("Freightdock connects to Portainer and matches its environment by the Docker engine's ID", async {
        ensure!(!engine.is_empty(), "the dind's engine ID is not in /info");
        let (s, b) = qa.send(Method::PUT, &qa.url("/api/portainer"), Some(json!({ "url": purl, "token": "ptr_wrong" }))).await?;
        ensure!(s == StatusCode::BAD_REQUEST && b["field"] == json!("token"), "a wrong token: {s}: {b}");
        let (s, b) = qa.send(Method::PUT, &qa.url("/api/portainer"), Some(json!({ "url": purl, "token": "ptr_qa" }))).await?;
        ensure!(s == StatusCode::OK && b["connected"] == json!(true), "connect: {s}: {b}");
        ensure!(!b.to_string().contains("ptr_qa"), "the token was sent back: {b}");
        let (s, b) = qa.post(&qa.url("/api/portainer/scan"), json!({})).await?;
        ensure!(s == StatusCode::OK, "scan: {s}: {b}");
        let e = &b["environments"][0];
        ensure!(e["env"] == json!(qa.env) && e["match"]["by"] == json!("engine ID"), "not matched by the engine ID: {e}");
        let st = &b["stacks"][0];
        ensure!(st["importable"] == json!(true) && st["checked"] == json!(true) && st["running"] == json!(1), "the stack: {st}");
        ensure!(st["working_dir"] == json!(dir.to_string_lossy()), "where it was started: {st}");
        Ok(())
    })
    .await;

    let id = step("importing it adopts it in place: the same project, its container kept running", async {
        let before = qa.inspect("qa-portainer-app-1").await?.1["Id"].clone();
        let (s, b) = qa.post(&qa.url("/api/portainer/import"), json!({ "stacks": [{ "id": 77 }] })).await?;
        ensure!(s == StatusCode::OK, "import: {s}: {b}");
        let r = &b["results"][0];
        ensure!(r["outcome"] == json!("done"), "the stack: {r}");
        let id = r["freightdock_id"].as_i64().context("no stack id")?;
        let st = qa.stack_is("qa-portainer", "running", 1).await?;
        ensure!(st["external"] == json!(false) && st["id"] == json!(id), "still external: {st}");
        ensure!(qa.inspect("qa-portainer-app-1").await?.1["Id"] == before, "the container was replaced");
        let x = qa.get_json(&qa.env(&format!("/stacks/{id}"))).await?;
        let merged = x["compose"].as_str().unwrap_or_default();
        ensure!(merged.contains(&format!("{}/data", dir.display())), "the relative mount is not made absolute from where Portainer ran it: {merged}");
        ensure!(merged.contains("stack.env") && !merged.contains(&format!("{}/stack.env", dir.display())), "stack.env is not the stack's own: {merged}");
        ensure!(x["env"] == json!("GREETING=from-portainer\n"), "its variables: {x}");
        Ok(id)
    })
    .await;

    step("a second import makes nothing twice", async {
        let (s, b) = qa.post(&qa.url("/api/portainer/import"), json!({ "stacks": [{ "id": 77 }] })).await?;
        ensure!(s == StatusCode::OK && b["results"][0]["outcome"] == json!("skipped") && b["results"][0]["reason"] == json!("Imported already."), "again: {s}: {b}");
        let all = qa.get_json(&qa.env("/stacks")).await?;
        ensure!(all.as_array().map_or(0, |l| l.iter().filter(|s| s["name"] == json!("qa-portainer")).count()) == 1, "two of it: {all}");
        Ok(())
    })
    .await;

    step("the imported stack redeploys from Freightdock with its stack.env and its mount, and is removed", async {
        qa.stack_job(id, "deploy", json!({})).await?;
        qa.stack_is("qa-portainer", "running", 1).await?;
        let logs = qa.logs("qa-portainer-app-1").await?;
        ensure!(logs.contains("greeting=from-portainer"), "its variables came along: {logs:?}");
        let c = qa.inspect("qa-portainer-app-1").await?.1;
        let mounts = c["Mounts"].to_string();
        ensure!(mounts.contains(&format!("{}/data", dir.display())), "the mount moved: {mounts}");
        qa.stack_job(id, "remove", json!({})).await?;
        ensure!(qa.containers().await?.is_empty(), "containers are left");
        Ok(())
    })
    .await;

    step("Portainer was only ever read", async {
        let asked = asked.lock().unwrap().clone();
        ensure!(asked.iter().all(|a| a.starts_with("GET ")), "Freightdock asked Portainer for more than reading: {asked:?}");
        let (s, _) = qa.send(Method::DELETE, &qa.url("/api/portainer"), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "forget: {s}");
        Ok(())
    })
    .await;
}
