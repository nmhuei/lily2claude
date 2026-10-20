use crate::config::warp_pool::WarpPool;
use crate::protocol::openai::ChatCompletionsRequest;
use reqwest::{Client, Proxy};
use std::time::Duration;

#[derive(Clone)]
pub struct HttpUpstreamAdapter {
    base_url: String,
    auth_token: Option<String>,
    warp_pool: Option<WarpPool>,
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
            warp_pool,
            default_client,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn send_request(
        &self,
        request: &ChatCompletionsRequest,
    ) -> anyhow::Result<reqwest::Response> {
        self.send_request_with_session(request, None).await
    }

    pub async fn send_request_with_session(
        &self,
        request: &ChatCompletionsRequest,
        session_id: Option<&str>,
    ) -> anyhow::Result<reqwest::Response> {
        let proxy_url = self.warp_pool.as_ref().and_then(|p| p.acquire_proxy());

        let client = if let Some(ref p_url) = proxy_url {
            Client::builder()
                .timeout(Duration::from_secs(180))
                .connect_timeout(Duration::from_secs(10))
                .proxy(Proxy::all(p_url)?)
                .build()?
        } else {
            self.default_client.clone()
        };
        let url = format!("{}/chat/completions", self.base_url);

        let mut req_builder = client.post(&url).json(request);

        if let Some(ref token) = self.auth_token {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", token));
        }

        let s_id = session_id
            .map(|s| format!("ses_{}", s))
            .unwrap_or_else(|| format!("ses_{}", uuid_simple()));
        req_builder = req_builder.header("x-opencode-session", s_id);
        req_builder = req_builder.header("User-Agent", "opencode/1.19.3");

        let response = match req_builder.send().await {
            Ok(resp) => resp,
            Err(e) => {
                if let (Some(pool), Some(p_url)) = (&self.warp_pool, proxy_url) {
                    pool.mark_rate_limited(&p_url, 60);
                }
                return Err(e.into());
            }
        };

        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            if let (Some(pool), Some(p_url)) = (&self.warp_pool, proxy_url) {
                pool.mark_rate_limited(&p_url, 60);
            }
        }

        Ok(response)
    }
}

fn uuid_simple() -> String {
    use std::time::SystemTime;
    let t = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:x}", t)
}
