//! Swarm, end to end, on the same throwaway dind: it is made a swarm of one here (straight at its API, which
//! `FREIGHTDOCK_QA_DOCKER` names, as Freightdock does not make swarms), and leaves it again at the end, so the steps after
//! these see a Docker that is not in a swarm and holds nothing ("QA" 4 in DECISIONS.md).

use std::time::Duration;

use anyhow::{Context, Result, ensure};
use futures_util::StreamExt;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

use super::{IMAGE, Qa, step};

const SERVICE: &str = "qa-web";
const STACK: &str = "qa-stack";

fn compose(replicas: u32) -> String {
    format!("services:\n  app:\n    image: {IMAGE}\n    command: [\"sleep\", \"3600\"]\n    deploy:\n      replicas: {replicas}\n")
}

impl Qa {
    /// A service's running and desired tasks, as the list says them.
    async fn tasks_of(&self, name: &str) -> Result<(u64, u64)> {
        let list = self.get_json(&self.env("/swarm/services")).await?;
        let s = list.as_array().and_then(|l| l.iter().find(|s| s["Spec"]["Name"] == json!(name))).with_context(|| format!("no service {name} in {list}"))?;
        Ok((s["ServiceStatus"]["RunningTasks"].as_u64().unwrap_or(0), s["ServiceStatus"]["DesiredTasks"].as_u64().unwrap_or(0)))
    }

    async fn runs(&self, name: &str, n: u64) -> Result<()> {
        self.until(Duration::from_secs(90), || async {
            let (run, want) = self.tasks_of(name).await?;
            ensure!(run == n && want == n, "{name}: {run} of {want} running, {n} wanted");
            Ok(())
        })
        .await
    }

    async fn service(&self, name: &str) -> Result<Value> {
        self.get_json(&self.env(&format!("/swarm/services/{name}"))).await
    }
}

pub async fn steps(qa: &Qa) {
    let docker = std::env::var("FREIGHTDOCK_QA_DOCKER").expect("FREIGHTDOCK_QA_DOCKER: the dind's own API, to make it a swarm (qa/run.sh sets it)");
    // qa/run.sh gives the dind's address as Docker writes it (`tcp://`, which Compose takes too); spoken to as HTTP here.
    let docker = docker.trim_end_matches('/').replacen("tcp://", "http://", 1);

    step("a Docker in no swarm says so, and its Swarm routes say why", async {
        let s = qa.get_json(&qa.env("/swarm")).await?;
        ensure!(s["role"] == json!("none"), "swarm: {s}");
        let (st, b) = qa.send(Method::GET, &qa.env("/swarm/services"), None).await?;
        ensure!(st == StatusCode::CONFLICT, "services on a lone Docker: {st}: {b}");
        ensure!(b["error"].as_str().is_some_and(|e| e.contains("not a swarm manager")), "{b}");
        let (st, b) = qa.post(&qa.env("/swarm/stacks"), json!({ "name": STACK, "compose": compose(1) })).await?;
        ensure!(st == StatusCode::CONFLICT, "a stack on a lone Docker: {st}: {b}");
        Ok(())
    })
    .await;

    let node = step("made a swarm, the host is its manager and its one node", async {
        let r = qa.http.post(format!("{docker}/swarm/init")).json(&json!({ "ListenAddr": "0.0.0.0:2377", "AdvertiseAddr": "eth0" })).send().await?;
        let s = r.status();
        ensure!(s.is_success(), "swarm init: {s}: {}", r.text().await?);
        let st = qa.get_json(&qa.env("/swarm")).await?;
        ensure!(st["role"] == json!("manager"), "swarm: {st}");
        ensure!(st["swarm"]["id"].is_string(), "no swarm id: {st}");
        let nodes = qa.get_json(&qa.env("/swarm/nodes")).await?;
        let list = nodes.as_array().context("nodes")?;
        ensure!(list.len() == 1, "nodes: {nodes}");
        ensure!(list[0]["Spec"]["Role"] == json!("manager"), "{nodes}");
        Ok(list[0]["ID"].as_str().context("node id")?.to_string())
    })
    .await;

    step("create a service, and its task runs", async {
        let spec = json!({
            "Name": SERVICE,
            "TaskTemplate": { "ContainerSpec": { "Image": IMAGE, "Command": ["sh", "-c", "echo hello-from-swarm; echo oops-from-swarm >&2; exec sleep 3600"] } },
            "Mode": { "Replicated": { "Replicas": 1 } },
        });
        let (s, b) = qa.post(&qa.env("/swarm/services"), spec).await?;
        ensure!(s == StatusCode::CREATED, "create: {s}: {b}");
        ensure!(b["ID"].is_string(), "no id: {b}");
        qa.runs(SERVICE, 1).await
    })
    .await;

    step("its logs come over the WebSocket, standard output and standard error apart", async {
        let mut ws = qa.ws(&format!("/swarm/services/{SERVICE}/logs/ws?tail=all")).await?;
        let (mut out, mut err) = (false, false);
        let read = async {
            while let Some(m) = ws.next().await {
                if let Message::Text(t) = m? {
                    let v: Value = serde_json::from_str(&t)?;
                    let text = v["t"].as_str().unwrap_or("");
                    out |= v["s"] == json!("out") && text.contains("hello-from-swarm");
                    err |= v["s"] == json!("err") && text.contains("oops-from-swarm");
                    if out && err {
                        return Ok(());
                    }
                    ensure!(v["error"].is_null(), "logs: {v}");
                }
            }
            anyhow::bail!("the logs ended first")
        };
        tokio::time::timeout(Duration::from_secs(30), read).await.context("no logs within 30 s")??;
        let _ = ws.close(None).await;
        Ok(())
    })
    .await;

    step("scale it to two", async {
        let (s, b) = qa.post(&qa.env(&format!("/swarm/services/{SERVICE}/scale")), json!({ "replicas": 2 })).await?;
        ensure!(s == StatusCode::OK, "scale: {s}: {b}");
        qa.runs(SERVICE, 2).await?;
        let tasks = qa.get_json(&qa.env(&format!("/swarm/tasks?service={SERVICE}&state=running"))).await?;
        ensure!(tasks.as_array().map_or(0, Vec::len) == 2, "running tasks: {tasks}");
        Ok(())
    })
    .await;

    step("update it, then roll it back to what it was", async {
        let before = qa.service(SERVICE).await?;
        let mut spec = before["service"]["Spec"].clone();
        spec["TaskTemplate"]["ContainerSpec"]["Env"] = json!(["QA_UPDATED=1"]);
        let (s, b) = qa.send(Method::PUT, &qa.env(&format!("/swarm/services/{SERVICE}")), Some(spec)).await?;
        ensure!(s == StatusCode::OK, "update: {s}: {b}");
        let after = qa.service(SERVICE).await?;
        ensure!(after["service"]["Spec"]["TaskTemplate"]["ContainerSpec"]["Env"] == json!(["QA_UPDATED=1"]), "not updated: {after}");
        ensure!(after["service"]["PreviousSpec"].is_object(), "no previous spec to roll back to: {after}");
        let (s, b) = qa.post(&qa.env(&format!("/swarm/services/{SERVICE}/rollback")), json!({})).await?;
        ensure!(s == StatusCode::OK, "rollback: {s}: {b}");
        qa.until(Duration::from_secs(30), || async {
            let back = qa.service(SERVICE).await?;
            ensure!(back["service"]["Spec"]["TaskTemplate"]["ContainerSpec"]["Env"].is_null(), "not rolled back: {}", back["service"]["Spec"]);
            Ok(())
        })
        .await?;
        qa.runs(SERVICE, 2).await
    })
    .await;

    step("a secret goes in and its value never comes back out", async {
        let (s, b) = qa.post(&qa.env("/swarm/secrets"), json!({ "name": "qa-secret", "data": "hunter2-qa" })).await?;
        ensure!(s == StatusCode::CREATED, "secret: {s}: {b}");
        let id = b["id"].as_str().context("secret id")?.to_string();
        let (_, list) = qa.get(&qa.env("/swarm/secrets")).await?;
        let (_, one) = qa.get(&qa.env(&format!("/swarm/secrets/{id}"))).await?;
        for text in [&list, &one] {
            ensure!(text.contains("qa-secret"), "not listed: {text}");
            ensure!(!text.contains("hunter2-qa") && !text.contains("aHVudGVyMi1xYQ"), "the value came back: {text}");
        }
        let (s, b) = qa.send(Method::DELETE, &qa.env(&format!("/swarm/secrets/{id}")), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove: {s}: {b}");
        Ok(())
    })
    .await;

    step("a config goes in, is read back as text, and is removed", async {
        let (s, b) = qa.post(&qa.env("/swarm/configs"), json!({ "name": "qa-config", "data": "listen 80\n", "labels": { "qa": "yes" } })).await?;
        ensure!(s == StatusCode::CREATED, "config: {s}: {b}");
        let id = b["id"].as_str().context("config id")?.to_string();
        let c = qa.get_json(&qa.env(&format!("/swarm/configs/{id}"))).await?;
        ensure!(c["text"] == json!("listen 80\n"), "config: {c}");
        ensure!(c["Spec"]["Labels"]["qa"] == json!("yes"), "labels: {c}");
        let (s, b) = qa.send(Method::DELETE, &qa.env(&format!("/swarm/configs/{id}")), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove: {s}: {b}");
        Ok(())
    })
    .await;

    step("remove the service", async {
        let (s, b) = qa.send(Method::DELETE, &qa.env(&format!("/swarm/services/{SERVICE}")), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove: {s}: {b}");
        let (s, _) = qa.send(Method::GET, &qa.env(&format!("/swarm/services/{SERVICE}")), None).await?;
        ensure!(s == StatusCode::NOT_FOUND, "still there: {s}");
        Ok(())
    })
    .await;

    step("deploy a stack with docker stack deploy, deploy it again scaled, and remove it", async {
        let (s, b) = qa.post(&qa.env("/swarm/stacks"), json!({ "name": STACK, "compose": compose(1) })).await?;
        ensure!(s == StatusCode::OK, "deploy: {s}: {b}");
        let svc = format!("{STACK}_app");
        qa.runs(&svc, 1).await?;
        let list = qa.get_json(&qa.env("/swarm/stacks")).await?;
        let row = list.as_array().and_then(|l| l.iter().find(|r| r["name"] == json!(STACK))).with_context(|| format!("not listed: {list}"))?;
        ensure!(row["services"] == json!(1) && row["file"] == json!(true), "{row}");
        let (s, b) = qa.post(&qa.env("/swarm/stacks"), json!({ "name": STACK, "compose": compose(2) })).await?;
        ensure!(s == StatusCode::OK, "deploy again: {s}: {b}");
        qa.runs(&svc, 2).await?;
        let one = qa.get_json(&qa.env(&format!("/swarm/stacks/{STACK}"))).await?;
        ensure!(one["compose"] == json!(compose(2)), "the file kept is the last one deployed: {one}");
        let (s, b) = qa.post(&qa.env("/swarm/stacks"), json!({ "name": "qa-broken", "compose": "services:\n  x:\n    image: [\n" })).await?;
        ensure!(s == StatusCode::BAD_REQUEST && b["field"] == json!("compose"), "a broken file: {s}: {b}");
        let (s, b) = qa.send(Method::DELETE, &qa.env(&format!("/swarm/stacks/{STACK}")), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "remove: {s}: {b}");
        let (s, _) = qa.send(Method::GET, &qa.env(&format!("/swarm/stacks/{STACK}")), None).await?;
        ensure!(s == StatusCode::NOT_FOUND, "still there: {s}");
        Ok(())
    })
    .await;

    step("drain the node, label it, and make it active again", async {
        let url = qa.env(&format!("/swarm/nodes/{node}"));
        let (s, b) = qa.send(Method::PUT, &url, Some(json!({ "availability": "drain", "labels": { "qa": "drained" } }))).await?;
        ensure!(s == StatusCode::NO_CONTENT, "drain: {s}: {b}");
        let n = qa.get_json(&url).await?;
        ensure!(n["node"]["Spec"]["Availability"] == json!("drain") && n["node"]["Spec"]["Labels"]["qa"] == json!("drained"), "{n}");
        ensure!(n["node"]["Spec"]["Role"] == json!("manager"), "the role is kept: {n}");
        let (s, b) = qa.send(Method::PUT, &url, Some(json!({ "availability": "active" }))).await?;
        ensure!(s == StatusCode::NO_CONTENT, "active: {s}: {b}");
        let n = qa.get_json(&url).await?;
        ensure!(n["node"]["Spec"]["Availability"] == json!("active") && n["node"]["Spec"]["Labels"]["qa"] == json!("drained"), "{n}");
        let (s, b) = qa.send(Method::PUT, &url, Some(json!({ "availability": "asleep" }))).await?;
        ensure!(s == StatusCode::BAD_REQUEST, "a wrong availability: {s}: {b}");
        Ok(())
    })
    .await;

    step("every Swarm change is in the activity log", async {
        let actions = qa.audit_actions().await?;
        for a in ["swarm.service.create", "swarm.service.scale", "swarm.service.update", "swarm.service.rollback", "swarm.service.remove", "swarm.secret.create", "swarm.secret.remove", "swarm.config.create", "swarm.config.remove", "swarm.stack.deploy", "swarm.stack.remove", "swarm.node.update"] {
            ensure!(actions.iter().any(|x| x == a), "no {a} in the activity log: {actions:?}");
        }
        Ok(())
    })
    .await;

    step("the dind leaves the swarm, and what the stack left is gone", async {
        let r = qa.http.post(format!("{docker}/swarm/leave?force=true")).send().await?;
        let s = r.status();
        ensure!(s.is_success(), "swarm leave: {s}: {}", r.text().await?);
        ensure!(qa.get_json(&qa.env("/swarm")).await?["role"] == json!("none"), "still in a swarm");
        qa.until(Duration::from_secs(60), || async {
            let left = qa.containers().await?;
            ensure!(left.is_empty(), "containers left: {:?}", super::names(&left));
            Ok(())
        })
        .await
    })
    .await;
}
