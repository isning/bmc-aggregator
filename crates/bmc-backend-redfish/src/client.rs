use std::time::Duration;

use futures_util::StreamExt;
use reqwest::{Client, Response, StatusCode, Url};
use serde_json::Value;

use crate::RedfishConfig;

const ROOT: &str = "/redfish/v1/";
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_COLLECTION_PAGES: usize = 128;

#[derive(Clone)]
pub struct RedfishClient {
    client: Client,
    base: Url,
    username: String,
    password: String,
}

impl RedfishClient {
    pub fn new(config: RedfishConfig) -> Result<Self, String> {
        if config.host.trim().is_empty() {
            return Err("Redfish host is empty".to_string());
        }
        let authority = bmc_backend_support::endpoint::authority(&config.host, config.port)?;
        let base = Url::parse(&format!("https://{authority}{ROOT}"))
            .map_err(|error| format!("invalid Redfish endpoint: {error}"))?;
        let client = Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .danger_accept_invalid_certs(config.insecure_tls)
            .connect_timeout(Duration::from_secs(45))
            .timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(4)
            .build()
            .map_err(|error| format!("create Redfish HTTP client: {error}"))?;
        Ok(Self {
            client,
            base,
            username: config.username,
            password: config.password,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(base: Url) -> Self {
        Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            base,
            username: "admin".into(),
            password: "test".into(),
        }
    }

    pub fn resolve(&self, uri: &str) -> Result<Url, String> {
        let url = self
            .base
            .join(uri)
            .map_err(|error| format!("invalid Redfish resource URI: {error}"))?;
        if url.scheme() != self.base.scheme()
            || url.host_str() != self.base.host_str()
            || url.port_or_known_default() != self.base.port_or_known_default()
        {
            return Err("Redfish resource URI points outside the configured BMC".to_string());
        }
        Ok(url)
    }

    pub async fn get(&self, uri: &str) -> Result<Value, String> {
        let response = self
            .client
            .get(self.resolve(uri)?)
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|error| format!("Redfish request failed: {error}"))?
            .error_for_status()
            .map_err(|error| format!("Redfish request failed: {error}"))?;
        let body = Self::read_bounded(response).await?;
        let resource: Value = serde_json::from_slice(&body)
            .map_err(|error| format!("Redfish JSON response invalid: {error}"))?;
        if !resource.is_object() {
            return Err("Redfish resource response must be a JSON object".into());
        }
        Ok(resource)
    }

    async fn read_bounded(response: Response) -> Result<Vec<u8>, String> {
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| format!("Redfish response read failed: {error}"))?;
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(format!(
                    "Redfish response exceeds the {MAX_RESPONSE_BYTES}-byte limit"
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    pub async fn collection_members(&self, uri: &str) -> Result<Vec<Value>, String> {
        use bmc_backend_model::pagination::{Pages, State};
        let mut pages = Pages::new(MAX_COLLECTION_PAGES);
        let mut page_uri = self.resolve(uri)?.to_string();
        let mut members = Vec::new();
        loop {
            pages = pages.enter(page_uri.clone());
            if pages.state() == State::Rejected {
                return Err("Redfish collection pagination loop or page limit exceeded".into());
            }
            let page = self.get(&page_uri).await?;
            let values = page
                .get("Members")
                .and_then(Value::as_array)
                .ok_or_else(|| "Redfish collection has invalid or missing Members".to_string())?;
            for member in values {
                let uri = member
                    .get("@odata.id")
                    .and_then(Value::as_str)
                    .filter(|uri| !uri.trim().is_empty())
                    .ok_or_else(|| {
                        "Redfish collection member has an invalid or missing resource link"
                            .to_string()
                    })?;
                self.resolve(uri)?;
            }
            members.extend(values.iter().cloned());
            let next = match page.get("Members@odata.nextLink") {
                None => None,
                Some(Value::String(next)) if !next.trim().is_empty() => {
                    Some(self.resolve(next)?.to_string())
                }
                Some(_) => return Err("Redfish collection has invalid nextLink".into()),
            };
            pages = pages.finish(next.is_some());
            if pages.state() == State::Complete {
                return Ok(members);
            }
            page_uri = next.expect("reading state has next page");
        }
    }

    pub async fn post(&self, uri: &str, body: Value) -> Result<(), String> {
        let response = self
            .client
            .post(self.resolve(uri)?)
            .basic_auth(&self.username, Some(&self.password))
            .json(&body)
            .send()
            .await
            .map_err(|error| format!("Redfish request failed: {error}"))?;
        self.finish_action(response).await
    }

    pub async fn patch(&self, uri: &str, body: Value) -> Result<(), String> {
        let response = self
            .client
            .patch(self.resolve(uri)?)
            .basic_auth(&self.username, Some(&self.password))
            .json(&body)
            .send()
            .await
            .map_err(|error| format!("Redfish request failed: {error}"))?;
        self.finish_action(response).await
    }

    pub(crate) async fn patch_verified(&self, uri: &str, body: Value) -> Result<(), String> {
        use bmc_backend_model::transaction::{Event, State};
        self.patch(uri, body.clone()).await?;
        let operation = State::Sending.step(Event::Completion(0));
        let observed = self.get(uri).await?;
        let operation = operation.step(Event::Observed(contains_fields(&observed, &body)));
        if operation != State::Confirmed {
            return Err("Redfish configuration readback mismatch; remote result unknown".into());
        }
        Ok(())
    }

    async fn finish_action(&self, response: Response) -> Result<(), String> {
        if response.status() == StatusCode::ACCEPTED {
            let header_location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body = Self::read_bounded(response).await?;
            let document = serde_json::from_slice::<Value>(&body).ok();
            let location = header_location
                .or_else(|| {
                    document.as_ref().and_then(|value| {
                        value
                            .get("@odata.id")
                            .or_else(|| value.get("TaskMonitor"))
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                })
                .ok_or_else(|| {
                    "Redfish asynchronous action returned no task location".to_string()
                })?;
            return self.wait_for_task(&location).await;
        }
        if !response.status().is_success() {
            let status = response.status();
            let body = Self::read_bounded(response).await?;
            let detail = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|value| redfish_error_message(&value))
                .unwrap_or_else(|| {
                    status
                        .canonical_reason()
                        .unwrap_or("request failed")
                        .to_string()
                });
            return Err(format!("Redfish action failed ({status}): {detail}"));
        }
        Ok(())
    }

    pub async fn wait_for_task(&self, location: &str) -> Result<(), String> {
        tokio::time::timeout(Duration::from_secs(30), self.poll_task(location))
            .await
            .map_err(|_| {
                "Redfish task timed out after 30 seconds; remote result unknown".to_string()
            })?
    }

    async fn poll_task(&self, location: &str) -> Result<(), String> {
        use bmc_backend_model::task::{classify, Observation, State, Task};
        let origin = tokio::time::Instant::now();
        let mut task = Task::new(Duration::from_secs(30));
        loop {
            task = task.step(Observation::Tick, origin.elapsed());
            if task.state() == State::TimedOut {
                return Err("Redfish task timed out; remote result unknown".into());
            }
            let response = self
                .client
                .get(self.resolve(location)?)
                .basic_auth(&self.username, Some(&self.password))
                .send()
                .await
                .map_err(|error| format!("Redfish task request failed: {error}"))?;
            let status = response.status();
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .map(|seconds| Duration::from_secs(seconds.clamp(1, 10)))
                .unwrap_or(Duration::from_secs(1));
            let body = Self::read_bounded(response).await?;
            let observation = if status == StatusCode::NO_CONTENT {
                Observation::Complete
            } else if !status.is_success() {
                Observation::Failed
            } else if body.is_empty() {
                if status == StatusCode::ACCEPTED {
                    Observation::Pending
                } else {
                    Observation::Unknown
                }
            } else {
                let document: Value = serde_json::from_slice(&body)
                    .map_err(|error| format!("Redfish task response invalid: {error}"))?;
                classify(
                    document.get("TaskState").and_then(Value::as_str),
                    document.get("TaskStatus").and_then(Value::as_str),
                    status == StatusCode::ACCEPTED,
                )
            };
            task = task.step(observation, origin.elapsed());
            match task.state() {
                State::Complete => return Ok(()),
                State::Failed => return Err(format!("Redfish task failed ({status})")),
                State::Unknown => {
                    return Err("Redfish task returned an unknown state or result".into())
                }
                State::TimedOut => {
                    return Err("Redfish task timed out; remote result unknown".into())
                }
                State::Polling => tokio::time::sleep(retry_after).await,
            }
        }
    }

    pub async fn system(&self) -> Result<(String, Value), String> {
        let root = self.get(ROOT).await?;
        let systems = root
            .pointer("/Systems/@odata.id")
            .and_then(Value::as_str)
            .ok_or_else(|| "Redfish ServiceRoot does not advertise Systems".to_string())?;
        let members = self.collection_members(systems).await?;
        if members.len() != 1 {
            return Err(format!("Redfish Systems collection contains {} members; configure a unique management endpoint", members.len()));
        }
        let uri = members
            .first()
            .and_then(|member| member.get("@odata.id"))
            .and_then(Value::as_str)
            .ok_or_else(|| "Redfish Systems collection has no members".to_string())?
            .to_string();
        Ok((uri.clone(), self.get(&uri).await?))
    }
}

fn contains_fields(observed: &Value, expected: &Value) -> bool {
    match expected {
        Value::Object(fields) => fields.iter().all(|(key, value)| {
            observed
                .get(key)
                .is_some_and(|observed| contains_fields(observed, value))
        }),
        _ => observed == expected,
    }
}

fn redfish_error_message(value: &Value) -> Option<String> {
    value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            value
                .get("Messages")
                .and_then(Value::as_array)
                .and_then(|messages| {
                    messages
                        .iter()
                        .find_map(|message| message.get("Message").and_then(Value::as_str))
                })
                .map(str::to_owned)
        })
        .or_else(|| {
            value
                .pointer("/error/@Message.ExtendedInfo")
                .and_then(Value::as_array)
                .and_then(|messages| {
                    messages
                        .iter()
                        .find_map(|message| message.get("Message").and_then(Value::as_str))
                })
                .map(str::to_owned)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, response::Response as AxumResponse, routing::get, Json, Router};

    #[test]
    fn configured_hosts_preserve_the_endpoint_and_support_ipv6() {
        for host in ["bmc.example", "127.0.0.1", "::1", "[::1]"] {
            let client = RedfishClient::new(RedfishConfig {
                host: host.into(),
                port: 8443,
                username: String::new(),
                password: String::new(),
                insecure_tls: false,
            })
            .unwrap();
            assert_eq!(client.base.port(), Some(8443));
            assert_eq!(client.base.path(), ROOT);
            assert!(client.base.username().is_empty());
        }
        for host in ["user@bmc.example", "bmc.example/path", "bmc.example?query"] {
            assert!(RedfishClient::new(RedfishConfig {
                host: host.into(),
                port: 8443,
                username: String::new(),
                password: String::new(),
                insecure_tls: false,
            })
            .is_err());
        }
    }

    async fn test_server(app: Router) -> (RedfishClient, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client =
            RedfishClient::for_test(Url::parse(&format!("http://{address}/redfish/v1/")).unwrap());
        (client, server)
    }

    #[tokio::test]
    async fn redirects_cannot_escape_the_configured_authority() {
        let target = Router::new().route(
            "/outside",
            get(|| async { Json(serde_json::json!({"escaped": true})) }),
        );
        let (other, other_server) = test_server(target).await;
        let destination = other.base.join("/outside").unwrap().to_string();
        let app = Router::new().route(
            "/redfish/v1/",
            get(move || {
                let destination = destination.clone();
                async move { axum::response::Redirect::temporary(&destination) }
            }),
        );
        let (client, server) = test_server(app).await;
        assert!(client.get("/redfish/v1/").await.is_err());
        server.abort();
        other_server.abort();
    }

    #[tokio::test]
    async fn collection_pagination_loops_are_rejected() {
        let app = Router::new().route(
            "/redfish/v1/loop",
            get(|| async {
                Json(serde_json::json!({"Members":[],"Members@odata.nextLink":"/redfish/v1/loop"}))
            }),
        );
        let (client, server) = test_server(app).await;
        assert!(client
            .collection_members("/redfish/v1/loop")
            .await
            .unwrap_err()
            .contains("loop"));
        server.abort();
    }

    #[tokio::test]
    async fn collection_members_follow_next_link() {
        let app = Router::new()
            .route("/redfish/v1/pages/1", get(|| async { Json(serde_json::json!({"Members":[{"Id":"one","@odata.id":"/one"}],"Members@odata.nextLink":"/redfish/v1/pages/2"})) }))
            .route("/redfish/v1/pages/2", get(|| async { Json(serde_json::json!({"Members":[{"Id":"two","@odata.id":"/two"}]})) }));
        let (client, server) = test_server(app).await;
        let members = client
            .collection_members("/redfish/v1/pages/1")
            .await
            .unwrap();
        assert_eq!(
            members
                .iter()
                .filter_map(|value| value["Id"].as_str())
                .collect::<Vec<_>>(),
            vec!["one", "two"]
        );
        server.abort();
    }

    #[tokio::test]
    async fn malformed_collection_members_reject_the_entire_paginated_result() {
        for invalid in [
            Value::Null,
            serde_json::json!("invalid"),
            serde_json::json!({}),
            serde_json::json!({"@odata.id": 12}),
            serde_json::json!({"@odata.id": ""}),
            serde_json::json!({"@odata.id": "  "}),
            serde_json::json!({"@odata.id": "https://other.example/member"}),
        ] {
            let app = Router::new()
                .route("/redfish/v1/pages/1", get(|| async {
                    Json(serde_json::json!({"Members":[{"@odata.id":"/valid"}],"Members@odata.nextLink":"/redfish/v1/pages/2"}))
                }))
                .route("/redfish/v1/pages/2", get(move || {
                    let invalid = invalid.clone();
                    async move { Json(serde_json::json!({"Members":[invalid]})) }
                }));
            let (client, server) = test_server(app).await;
            assert!(client
                .collection_members("/redfish/v1/pages/1")
                .await
                .is_err());
            server.abort();
        }
    }

    #[tokio::test]
    async fn oversized_response_is_rejected() {
        let app = Router::new().route(
            "/redfish/v1/large",
            get(|| async { AxumResponse::new(Body::from(vec![b'x'; MAX_RESPONSE_BYTES + 1])) }),
        );
        let (client, server) = test_server(app).await;
        assert!(client
            .get("/redfish/v1/large")
            .await
            .unwrap_err()
            .contains("exceeds"));
        server.abort();
    }

    #[tokio::test]
    async fn task_monitor_no_content_means_complete() {
        let app = Router::new().route("/redfish/v1/task", get(|| async { StatusCode::NO_CONTENT }));
        let (client, server) = test_server(app).await;
        client.wait_for_task("/redfish/v1/task").await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn unknown_and_incomplete_task_results_are_not_success() {
        for document in [
            serde_json::json!({"TaskState":"FutureState","TaskStatus":"OK"}),
            serde_json::json!({"TaskState":"Completed"}),
        ] {
            let app = Router::new().route(
                "/redfish/v1/task",
                get(move || {
                    let document = document.clone();
                    async move { Json(document) }
                }),
            );
            let (client, server) = test_server(app).await;
            assert!(client
                .wait_for_task("/redfish/v1/task")
                .await
                .unwrap_err()
                .contains("unknown"));
            server.abort();
        }
    }

    #[tokio::test]
    async fn stalled_task_request_is_bounded_by_whole_monitor_deadline() {
        let (started, mut requests) = tokio::sync::mpsc::channel(1);
        let app = Router::new().route(
            "/redfish/v1/task",
            get(move || {
                let started = started.clone();
                async move {
                    started.send(()).await.unwrap();
                    core::future::pending::<StatusCode>().await
                }
            }),
        );
        let (client, server) = test_server(app).await;
        let monitor = tokio::spawn(async move { client.wait_for_task("/redfish/v1/task").await });
        requests.recv().await.unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(31)).await;
        assert!(monitor.await.unwrap().unwrap_err().contains("timed out"));
        tokio::time::resume();
        server.abort();
    }

    #[tokio::test]
    async fn accepted_patch_does_not_hide_readback_mismatch() {
        let app = Router::new().route(
            "/redfish/v1/config",
            get(|| async { Json(serde_json::json!({"IndicatorLED":"Off"})) })
                .patch(|| async { StatusCode::NO_CONTENT }),
        );
        let (client, server) = test_server(app).await;
        assert!(client
            .patch_verified(
                "/redfish/v1/config",
                serde_json::json!({"IndicatorLED":"Lit"})
            )
            .await
            .unwrap_err()
            .contains("readback mismatch"));
        server.abort();
    }

    #[tokio::test]
    async fn collection_terminal_page_at_limit_is_accepted() {
        let app = Router::new().route(
            "/redfish/v1/pages/:page",
            get(
                |axum::extract::Path(page): axum::extract::Path<usize>| async move {
                    let mut body = serde_json::json!({"Members":[{"Id":page,"@odata.id":format!("/members/{page}")}]});
                    if page < MAX_COLLECTION_PAGES {
                        body["Members@odata.nextLink"] =
                            Value::String(format!("/redfish/v1/pages/{}", page + 1));
                    }
                    Json(body)
                },
            ),
        );
        let (client, server) = test_server(app).await;
        assert_eq!(
            client
                .collection_members("/redfish/v1/pages/1")
                .await
                .unwrap()
                .len(),
            MAX_COLLECTION_PAGES
        );
        server.abort();
    }

    #[test]
    fn extended_info_message_is_extracted() {
        let value =
            serde_json::json!({"error":{"@Message.ExtendedInfo":[{"Message":"media busy"}]}});
        assert_eq!(redfish_error_message(&value).as_deref(), Some("media busy"));
    }
}
