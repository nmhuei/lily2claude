use crate::config::warp_pool::WarpPool;
use crate::protocol::openai::ChatCompletionsRequest;
use anyhow::anyhow;
use reqwest::{Client, Proxy, StatusCode};
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

#[derive(Clone)]
pub struct HttpUpstreamAdapter {
    base_url: String,
    auth_token: Option<String>,
    warp_pool: Option<Arc<WarpPool>>,
    default_client: Client,
}

impl HttpUpstreamAdapter {
    pub fn new(base_url: &str, auth_token: Option<&str>, warp_pool: Option<WarpPool>) -> Self {
        let default_client = Client::builder()
            .timeout(Duration::from_secs(180))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            auth_token: auth_token.map(|s| s.to_string()),
            warp_pool: warp_pool.map(Arc::new),
            default_client,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn warp_pool(&self) -> Option<Arc<WarpPool>> {
        self.warp_pool.clone()
    }

    pub async fn send_request(
        &self,
        request: &ChatCompletionsRequest,
    ) -> anyhow::Result<reqwest::Response> {
        self.send_request_with_session(request, None).await
    }

    /// Sends a request, retrying across WARP nodes before giving up.
    ///
    /// Node-level failures (connect/timeout/429) must not mark the whole
    /// provider as degraded: another node usually has a healthy egress path.
    pub async fn send_request_with_session(
        &self,
        request: &ChatCompletionsRequest,
        session_id: Option<&str>,
    ) -> anyhow::Result<reqwest::Response> {
        let Some(pool) = self.warp_pool.clone() else {
            return self.send_via(None, request, session_id).await.map_err(Into::into);
        };

        let mut candidates = pool.candidates(pool.max_attempts());
        if pool.config().rotation_enabled
            && !candidates.is_empty()
            && candidates
                .iter()
                .all(|proxy| pool.is_proxy_cooling(proxy))
        {
            // Both nodes just returned 429. Give an in-flight rotation a
            // chance to produce a fresh IP instead of immediately retrying a
            // cooling node.
            let wait = Duration::from_secs(pool.config().acquire_wait_secs.max(1));
            pool.acquire_proxy_wait(wait).await;
            candidates = pool.candidates(pool.max_attempts());
        }
        if candidates.is_empty() {
            return self.send_via(None, request, session_id).await.map_err(Into::into);
        }

        let mut last_response: Option<reqwest::Response> = None;
        let mut last_error: Option<reqwest::Error> = None;

        for proxy in candidates {
            match self
                .send_via(Some(&proxy), request, session_id)
                .await
            {
                Ok(response) => {
                    let status = response.status();

                    if status == StatusCode::TOO_MANY_REQUESTS {
                        warn!(proxy = %proxy, "WARP node returned 429; trying next proxy");
                        pool.report_rate_limited(&proxy);
                        last_response = Some(response);
                        continue;
                    }

                    if status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT {
                        // Upstream problem rather than a dead proxy. Try the
                        // other node once, but do not punish this node.
                        last_response = Some(response);
                        continue;
                    }

                    pool.report_success(&proxy);
                    return Ok(response);
                }
                Err(err) => {
                    if err.is_connect() || err.is_timeout() {
                        warn!(proxy = %proxy, error = %err, "WARP node request failed; trying next node");
                        pool.report_network_error(&proxy);
                        last_error = Some(err);
                        continue;
                    }
                    return Err(err.into());
                }
            }
        }

        // One extra attempt after waiting for a rotation. This covers the case
        // where one node was already cooling and the other one just returned
        // 429: the cooled node may now have a fresh WARP identity.
        if pool.config().rotation_enabled
            && last_response
                .as_ref()
                .is_some_and(|response| response.status() == StatusCode::TOO_MANY_REQUESTS)
        {
            let wait = Duration::from_secs(pool.config().acquire_wait_secs.max(1));
            if let Some(proxy) = pool.acquire_proxy_wait(wait).await {
                if !pool.is_proxy_cooling(&proxy) {
                    warn!(proxy = %proxy, "Retrying request through a rotated WARP node");
                    match self.send_via(Some(&proxy), request, session_id).await {
                        Ok(response) => {
                            let status = response.status();
                            if status == StatusCode::TOO_MANY_REQUESTS {
                                pool.report_rate_limited(&proxy);
                            } else if !status.is_server_error()
                                && status != StatusCode::REQUEST_TIMEOUT
                            {
                                pool.report_success(&proxy);
                            }
                            return Ok(response);
                        }
                        Err(err) => {
                            if err.is_connect() || err.is_timeout() {
                                pool.report_network_error(&proxy);
                            }
                            return Err(err.into());
                        }
                    }
                }
            }
        }

        if let Some(response) = last_response {
            if response.status() == StatusCode::TOO_MANY_REQUESTS {
                warn!("All WARP proxies are rate-limited for this request");
            }
            return Ok(response);
        }

        Err(last_error
            .map(Into::into)
            .unwrap_or_else(|| anyhow!("all WARP proxies failed")))
    }

    async fn send_via(
        &self,
        proxy_url: Option<&str>,
        request: &ChatCompletionsRequest,
        session_id: Option<&str>,
    ) -> reqwest::Result<reqwest::Response> {
        let client = match proxy_url {
            Some(proxy_url) => Client::builder()
                .timeout(Duration::from_secs(180))
                .connect_timeout(Duration::from_secs(10))
                .proxy(Proxy::all(proxy_url)?)
                .build()?,
            None => self.default_client.clone(),
        };

        let url = format!("{}/chat/completions", self.base_url);
        let mut request_builder = client.post(&url).json(request);

        if let Some(ref token) = self.auth_token {
            request_builder =
                request_builder.header("Authorization", format!("Bearer {token}"));
        }

        let s_id = session_id
            .map(|s| format!("ses_{s}"))
            .unwrap_or_else(|| format!("ses_{}", uuid_simple()));
        request_builder = request_builder.header("x-opencode-session", s_id);
        request_builder = request_builder.header("User-Agent", "opencode/1.19.3");

        request_builder.send().await
    }
}

fn uuid_simple() -> String {
    use std::time::SystemTime;
    let t = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{t:x}")
}
