use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{info, warn};

pub fn is_upstream_rate_limited(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("provider.quota")
        || lower.contains("rate limit")
        || lower.contains("too many requests")
        || lower.contains("429")
        || lower.contains("abusive traffic")
        || lower.contains("aren't available from your current network")
        || lower.contains("resource has been exhausted")
        || lower.contains("freetiererror")
        || lower.contains("quota exceeded")
        || lower.contains("usage limit")
}

const HOT_RELOAD_SCRIPT: &str = include_str!("warp_rotate.sh");
const MAX_COOLDOWN_SECS: u64 = 900;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerRuntime {
    Auto,
    Docker,
    Podman,
}

impl Default for ContainerRuntime {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarpNodeConfig {
    pub url: String,
    #[serde(default)]
    pub container: Option<String>,
    #[serde(default)]
    pub runtime: ContainerRuntime,
    #[serde(default)]
    pub socket: Option<String>,
}

fn default_nodes() -> Vec<WarpNodeConfig> {
    vec![
        WarpNodeConfig {
            url: "http://127.0.0.1:40001".to_string(),
            container: Some("test-warp-1".to_string()),
            runtime: ContainerRuntime::Auto,
            socket: None,
        },
        WarpNodeConfig {
            url: "http://127.0.0.1:40002".to_string(),
            container: Some("test-warp-2".to_string()),
            runtime: ContainerRuntime::Auto,
            socket: None,
        },
    ]
}

fn default_probe_url() -> String {
    "https://www.cloudflare.com/cdn-cgi/trace".to_string()
}

fn default_probe_interval_secs() -> u64 {
    45
}

fn default_probe_timeout_secs() -> u64 {
    6
}

fn default_cooldown_429_secs() -> u64 {
    60
}

fn default_cooldown_network_secs() -> u64 {
    15
}

fn default_max_attempts() -> usize {
    2
}

fn default_rotation_timeout_secs() -> u64 {
    90
}

fn default_acquire_wait_secs() -> u64 {
    8
}

fn default_max_requests_per_ip() -> u64 {
    30
}

fn default_true() -> bool {
    true
}

/// Controls the WARP egress pool. All fields are optional in TOML so existing
/// configurations keep working without a `[warp]` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarpConfig {
    #[serde(default = "default_probe_url")]
    pub probe_url: String,
    #[serde(default = "default_probe_interval_secs")]
    pub probe_interval_secs: u64,
    #[serde(default = "default_probe_timeout_secs")]
    pub probe_timeout_secs: u64,
    #[serde(default = "default_cooldown_429_secs")]
    pub cooldown_429_secs: u64,
    #[serde(default = "default_cooldown_network_secs")]
    pub cooldown_network_secs: u64,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: usize,
    #[serde(default = "default_rotation_timeout_secs")]
    pub rotation_timeout_secs: u64,
    /// How long a new ACP session waits for a healthy node (or an in-flight
    /// rotation) before falling back to the least-cooling node.
    #[serde(default = "default_acquire_wait_secs")]
    pub acquire_wait_secs: u64,
    /// Proactively rotate egress IP after this many successful requests to prevent rate-limits.
    #[serde(default = "default_max_requests_per_ip")]
    pub max_requests_per_ip: u64,
    #[serde(default = "default_true")]
    pub rotation_enabled: bool,
    #[serde(default = "default_true")]
    pub require_warp: bool,
    #[serde(default)]
    pub require_distinct_egress: bool,
    #[serde(default = "default_nodes")]
    pub nodes: Vec<WarpNodeConfig>,
}

impl Default for WarpConfig {
    fn default() -> Self {
        Self {
            probe_url: default_probe_url(),
            probe_interval_secs: default_probe_interval_secs(),
            probe_timeout_secs: default_probe_timeout_secs(),
            cooldown_429_secs: default_cooldown_429_secs(),
            cooldown_network_secs: default_cooldown_network_secs(),
            max_attempts: default_max_attempts(),
            rotation_timeout_secs: default_rotation_timeout_secs(),
            acquire_wait_secs: default_acquire_wait_secs(),
            max_requests_per_ip: default_max_requests_per_ip(),
            rotation_enabled: true,
            require_warp: true,
            require_distinct_egress: false,
            nodes: default_nodes(),
        }
    }
}

#[derive(Debug, Default)]
struct NodeRuntimeState {
    cooldown_until: Option<Instant>,
    failures: u32,
    probe_failures: u32,
    egress_ip: Option<String>,
    last_probe: Option<Instant>,
    last_ok: Option<Instant>,
    last_error: Option<String>,
    last_failure_kind: Option<FailureKind>,
    rotation_count: u64,
    request_count: u64,
}

#[derive(Debug)]
struct WarpNode {
    proxy_url: String,
    container: String,
    runtime: ContainerRuntime,
    socket: Option<String>,
    state: Mutex<NodeRuntimeState>,
    rotation_inflight: AtomicBool,
}

impl WarpNode {
    fn is_cooling(&self, now: Instant) -> bool {
        let state = self.state.lock().unwrap();
        state.cooldown_until.is_some_and(|until| until > now)
    }

    fn runtime_variants(&self) -> Vec<ContainerRuntime> {
        match self.runtime {
            ContainerRuntime::Auto => vec![ContainerRuntime::Podman, ContainerRuntime::Docker],
            other => vec![other],
        }
    }

    fn command_for(&self, runtime: ContainerRuntime) -> tokio::process::Command {
        match runtime {
            ContainerRuntime::Podman => {
                let mut cmd = tokio::process::Command::new("podman");
                // Never let an inherited DOCKER_HOST redirect the rootless podman CLI.
                cmd.env_remove("DOCKER_HOST").env_remove("CONTAINER_HOST");
                if let Some(ref socket) = self.socket {
                    cmd.arg("--url").arg(socket);
                }
                cmd
            }
            ContainerRuntime::Docker => {
                let mut cmd = tokio::process::Command::new("docker");
                // The user shell may point DOCKER_HOST at the rootless podman
                // socket; force the real Docker daemon unless configured otherwise.
                cmd.env_remove("DOCKER_HOST");
                cmd.arg("--host")
                    .arg(self.socket.as_deref().unwrap_or("unix:///var/run/docker.sock"));
                cmd
            }
            ContainerRuntime::Auto => unreachable!("Auto is expanded by runtime_variants"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeStatus {
    pub proxy_url: String,
    pub container: String,
    pub runtime: String,
    pub state: String,
    pub healthy: bool,
    pub cooldown_remaining_secs: u64,
    pub rotating: bool,
    pub egress_ip: Option<String>,
    pub last_ok_secs_ago: Option<u64>,
    pub failures: u32,
    pub rotation_count: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    RateLimited,
    Network,
    Probe,
}

pub struct WarpPool {
    nodes: Vec<Arc<WarpNode>>,
    cursor: AtomicUsize,
    config: WarpConfig,
    prober_started: Arc<AtomicBool>,
}

impl std::fmt::Debug for WarpPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WarpPool")
            .field("nodes", &self.nodes)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Clone for WarpPool {
    fn clone(&self) -> Self {
        Self {
            nodes: self.nodes.clone(),
            cursor: AtomicUsize::new(self.cursor.load(Ordering::Relaxed)),
            config: self.config.clone(),
            prober_started: self.prober_started.clone(),
        }
    }
}

impl WarpPool {
    /// Backwards-compatible constructor used by provider configuration and tests.
    /// Container names/runtimes are inferred from the proxy URL.
    pub fn new(proxies: Vec<String>) -> Self {
        let nodes = proxies
            .into_iter()
            .map(|url| WarpNodeConfig {
                container: Some(default_container_for(&url)),
                url,
                runtime: ContainerRuntime::Auto,
                socket: None,
            })
            .collect();

        let config = WarpConfig {
            nodes,
            ..WarpConfig::default()
        };
        Self::from_config(&config)
    }

    /// Test constructor: no background rotation, no probing side effects.
    pub fn for_test(proxies: Vec<String>) -> Self {
        let nodes = proxies
            .into_iter()
            .map(|url| WarpNodeConfig {
                container: Some(default_container_for(&url)),
                url,
                runtime: ContainerRuntime::Auto,
                socket: None,
            })
            .collect();

        let config = WarpConfig {
            nodes,
            rotation_enabled: false,
            probe_interval_secs: 3600,
            ..WarpConfig::default()
        };
        Self::from_config(&config)
    }

    pub fn default_pool() -> Self {
        Self::from_config(&WarpConfig::default())
    }

    pub fn from_config(config: &WarpConfig) -> Self {
        let nodes = config
            .nodes
            .iter()
            .map(|node| {
                let container = node
                    .container
                    .clone()
                    .unwrap_or_else(|| default_container_for(&node.url));
                Arc::new(WarpNode {
                    proxy_url: node.url.clone(),
                    container,
                    runtime: node.runtime,
                    socket: node.socket.clone(),
                    state: Mutex::new(NodeRuntimeState::default()),
                    rotation_inflight: AtomicBool::new(false),
                })
            })
            .collect();

        Self {
            nodes,
            cursor: AtomicUsize::new(0),
            config: config.clone(),
            prober_started: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn config(&self) -> &WarpConfig {
        &self.config
    }

    pub fn max_attempts(&self) -> usize {
        self.config.max_attempts.max(1)
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Healthy nodes in round-robin order. When every node is cooling down, the
    /// node whose cooldown expires first is returned as a last resort and a
    /// background heal is scheduled. Never returns more than `max` entries.
    pub fn candidates(&self, max: usize) -> Vec<String> {
        if self.nodes.is_empty() {
            return Vec::new();
        }

        let now = Instant::now();
        let total = self.nodes.len();
        let start = self.cursor.fetch_add(1, Ordering::Relaxed);

        let mut healthy = Vec::new();
        for offset in 0..total {
            let node = &self.nodes[(start + offset) % total];
            if !node.is_cooling(now) && !node.rotation_inflight.load(Ordering::Relaxed) {
                healthy.push(node.proxy_url.clone());
            }
        }

        if healthy.is_empty() {
            if let Some(node) = self.earliest_expiry_node() {
                healthy.push(node.proxy_url.clone());
            }
            self.trigger_auto_heal();
        }

        healthy.truncate(max.max(1));
        healthy
    }

    /// Kept for API compatibility. Prefer `candidates()` in retry loops.
    pub fn acquire_proxy(&self) -> Option<String> {
        self.candidates(1).into_iter().next()
    }

    /// True when the given proxy URL is currently in cooldown.
    pub fn is_proxy_cooling(&self, proxy: &str) -> bool {
        self.find_node(proxy)
            .map(|node| node.is_cooling(Instant::now()))
            .unwrap_or(false)
    }

    /// Acquire a proxy for a new ACP session, waiting briefly for a cooling
    /// node's background rotation to finish instead of immediately reusing the
    /// IP that just returned 429. This is what makes "try the other proxy"
    /// actually work when both nodes are cooling at the same time.
    pub async fn acquire_proxy_wait(&self, timeout: Duration) -> Option<String> {
        if self.nodes.is_empty() {
            return None;
        }

        if let Some(proxy) = self.acquire_proxy() {
            if !self.is_proxy_cooling(&proxy) {
                return Some(proxy);
            }
        }

        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if let Some(proxy) = self.acquire_proxy() {
                if !self.is_proxy_cooling(&proxy) {
                    info!(
                        proxy = %proxy,
                        "WARP node became healthy while waiting for proxy failover"
                    );
                    return Some(proxy);
                }
            }
        }

        warn!("No WARP node became healthy before the failover wait timed out");
        self.acquire_proxy()
    }

    pub fn mark_rate_limited(&self, proxy: &str, cooldown_secs: u64) {
        self.report_failure_with(proxy, FailureKind::RateLimited, Some(cooldown_secs));
    }

    pub fn report_rate_limited(&self, proxy: &str) {
        self.report_failure_with(proxy, FailureKind::RateLimited, None);
    }

    pub fn report_network_error(&self, proxy: &str) {
        self.report_failure_with(proxy, FailureKind::Network, None);
    }

    pub fn report_success(&self, proxy: &str) {
        let Some(node) = self.find_node(proxy) else {
            return;
        };
        let should_rotate = {
            let mut state = node.state.lock().unwrap();
            state.failures = 0;
            state.probe_failures = 0;
            state.cooldown_until = None;
            state.last_error = None;
            state.last_failure_kind = None;
            state.last_ok = Some(Instant::now());
            state.request_count = state.request_count.saturating_add(1);

            self.config.rotation_enabled
                && self.config.max_requests_per_ip > 0
                && state.request_count >= self.config.max_requests_per_ip
        };

        if should_rotate {
            info!(
                container = %node.container,
                proxy = %node.proxy_url,
                "Pre-emptive WARP rotation triggered after serving request limit"
            );
            self.spawn_rotation(node);
        }
    }

    fn report_failure_with(&self, proxy: &str, kind: FailureKind, base_override: Option<u64>) {
        let Some(node) = self.find_node(proxy) else {
            return;
        };

        let base = base_override.unwrap_or(match kind {
            FailureKind::RateLimited => self.config.cooldown_429_secs,
            FailureKind::Network | FailureKind::Probe => self.config.cooldown_network_secs,
        });

        let (failures, cooldown_secs) = {
            let mut state = node.state.lock().unwrap();
            state.failures = state.failures.saturating_add(1);
            let secs = compute_cooldown(base, state.failures);
            state.cooldown_until = Some(Instant::now() + Duration::from_secs(secs));
            state.last_error = Some(format!("{kind:?}"));
            state.last_failure_kind = Some(kind);
            (state.failures, secs)
        };

        warn!(
            proxy,
            ?kind,
            failures,
            cooldown_secs,
            "WARP node failed; entering cooldown"
        );

        let should_rotate = self.config.rotation_enabled
            && (matches!(kind, FailureKind::RateLimited) || failures >= 3);
        if should_rotate {
            self.spawn_rotation(node);
        }
    }

    pub fn is_exhausted(&self) -> bool {
        if self.nodes.is_empty() {
            return true;
        }
        let now = Instant::now();
        self.nodes.iter().all(|node| node.is_cooling(now))
    }

    pub fn distinct_egress_ips(&self) -> usize {
        self.nodes
            .iter()
            .filter_map(|node| node.state.lock().unwrap().egress_ip.clone())
            .collect::<HashSet<_>>()
            .len()
    }

    /// Old tuple shape retained for compatibility with existing callers.
    pub fn status_report(&self) -> Vec<(String, String, bool)> {
        let now = Instant::now();
        self.nodes
            .iter()
            .map(|node| {
                let state = node.state.lock().unwrap();
                let cooling = state.cooldown_until.is_some_and(|until| until > now);
                (node.proxy_url.clone(), node.container.clone(), !cooling)
            })
            .collect()
    }

    pub fn node_statuses(&self) -> Vec<NodeStatus> {
        let now = Instant::now();
        self.nodes
            .iter()
            .map(|node| {
                let state = node.state.lock().unwrap();
                let cooling = state.cooldown_until.is_some_and(|until| until > now);
                let rotating = node.rotation_inflight.load(Ordering::Relaxed);
                NodeStatus {
                    proxy_url: node.proxy_url.clone(),
                    container: node.container.clone(),
                    runtime: format!("{:?}", node.runtime).to_lowercase(),
                    state: if rotating {
                        "rotating".to_string()
                    } else if cooling {
                        "cooling".to_string()
                    } else {
                        "healthy".to_string()
                    },
                    healthy: !cooling && !rotating,
                    cooldown_remaining_secs: state
                        .cooldown_until
                        .map(|until| until.saturating_duration_since(now).as_secs())
                        .unwrap_or(0),
                    rotating,
                    egress_ip: state.egress_ip.clone(),
                    last_ok_secs_ago: state
                        .last_ok
                        .map(|last| now.saturating_duration_since(last).as_secs()),
                    failures: state.failures,
                    rotation_count: state.rotation_count,
                    last_error: state.last_error.clone(),
                }
            })
            .collect()
    }

    fn find_node(&self, proxy: &str) -> Option<Arc<WarpNode>> {
        self.nodes
            .iter()
            .find(|node| node.proxy_url == proxy)
            .cloned()
    }

    fn earliest_expiry_node(&self) -> Option<Arc<WarpNode>> {
        let now = Instant::now();
        self.nodes
            .iter()
            .min_by_key(|node| {
                node.state
                    .lock()
                    .unwrap()
                    .cooldown_until
                    .unwrap_or(now)
            })
            .cloned()
    }

    fn trigger_auto_heal(&self) {
        if !self.config.rotation_enabled || self.nodes.is_empty() {
            return;
        }

        let now = Instant::now();
        let mut cooling: Vec<(Instant, Arc<WarpNode>)> = self
            .nodes
            .iter()
            .filter_map(|node| {
                let until = node.state.lock().unwrap().cooldown_until?;
                if until > now {
                    Some((until, node.clone()))
                } else {
                    None
                }
            })
            .collect();

        cooling.sort_by_key(|(until, _)| *until);
        for (_, node) in cooling.into_iter().take(2) {
            self.spawn_rotation(node);
        }
    }

    fn spawn_rotation(&self, node: Arc<WarpNode>) {
        if !self.config.rotation_enabled {
            return;
        }
        if node.rotation_inflight.swap(true, Ordering::AcqRel) {
            return;
        }

        let config = self.config.clone();
        let node_for_job = node.clone();
        let timeout = Duration::from_secs(self.config.rotation_timeout_secs.max(10));

        info!(
            container = %node.container,
            proxy = %node.proxy_url,
            "Scheduling WARP IP rotation"
        );

        let job = async move {
            let result = tokio::time::timeout(
                timeout,
                Self::perform_rotation(&node_for_job, &config),
            )
            .await;

            match result {
                Ok(true) => info!(
                    container = %node_for_job.container,
                    "WARP rotation completed and passed real egress probe"
                ),
                Ok(false) => warn!(
                    container = %node_for_job.container,
                    "WARP rotation finished but egress probe still failing"
                ),
                Err(_) => warn!(
                    container = %node_for_job.container,
                    "WARP rotation timed out"
                ),
            }

            node_for_job
                .rotation_inflight
                .store(false, Ordering::Release);
        };

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(job);
        } else {
            std::thread::spawn(move || {
                if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    runtime.block_on(job);
                }
            });
        }
    }

    async fn perform_rotation(node: &WarpNode, config: &WarpConfig) -> bool {
        info!(
            container = %node.container,
            "Rotating WARP identity with in-container hot reload"
        );

        let hot = exec_in_container(
            node,
            &["sh", "-c", HOT_RELOAD_SCRIPT],
        )
        .await;

        let mut hot_ok = false;
        match hot {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stdout.contains("HOT_RELOAD_SUCCESS") {
                    hot_ok = true;
                    info!(container = %node.container, "WARP hot reload succeeded");
                } else {
                    warn!(
                        container = %node.container,
                        stdout = %stdout.trim(),
                        stderr = %stderr.trim(),
                        "WARP hot reload did not report success"
                    );
                }
            }
            Err(err) => warn!(
                container = %node.container,
                error = %err,
                "WARP hot reload could not run"
            ),
        }

        if !hot_ok {
            warn!(
                container = %node.container,
                "Falling back to container restart"
            );
            let _ = exec_in_container(
                node,
                &[
                    "rm",
                    "-f",
                    "/etc/sing-box/config.json",
                    "/etc/sing-box/cache.db",
                ],
            )
            .await;
            let _ = restart_container(node).await;
        }

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut last_error = None;
        while Instant::now() < deadline {
            match probe_node(node, config).await {
                Ok(egress_ip) => {
                    let mut state = node.state.lock().unwrap();
                    state.cooldown_until = None;
                    state.failures = 0;
                    state.probe_failures = 0;
                    state.egress_ip = Some(egress_ip.clone());
                    state.last_probe = Some(Instant::now());
                    state.last_ok = Some(Instant::now());
                    state.last_error = None;
                    state.last_failure_kind = None;
                    state.rotation_count = state.rotation_count.saturating_add(1);
                    state.request_count = 0;
                    info!(
                        container = %node.container,
                        egress_ip,
                        "WARP node verified with real egress probe"
                    );
                    return true;
                }
                Err(err) => {
                    last_error = Some(err.to_string());
                    tokio::time::sleep(Duration::from_millis(350)).await;
                }
            }
        }

        if let Some(err) = last_error {
            warn!(
                container = %node.container,
                error = %err,
                "WARP node did not pass egress probe after rotation"
            );
            let mut state = node.state.lock().unwrap();
            state.last_error = Some(format!("probe after rotation: {err}"));
        }
        false
    }

    /// Probe every node with a real HTTP request through its proxy and verify
    /// that the egress goes through WARP. The old TCP-connect check produced
    /// false positives because the published host port stays open even when the
    /// tunnel is dead.
    pub async fn probe_all(&self) {
        for node in &self.nodes {
            match probe_node(node, &self.config).await {
                Ok(egress_ip) => {
                    let mut state = node.state.lock().unwrap();
                    let now = Instant::now();
                    // A healthy tunnel does not clear an upstream 429: the IP
                    // is still rate-limited until its cooldown expires.
                    let rate_limited = state.last_failure_kind == Some(FailureKind::RateLimited)
                        && state.cooldown_until.is_some_and(|until| until > now);

                    state.egress_ip = Some(egress_ip.clone());
                    state.last_probe = Some(now);
                    state.last_ok = Some(now);
                    state.probe_failures = 0;
                    if !rate_limited {
                        state.failures = 0;
                        state.cooldown_until = None;
                        state.last_error = None;
                        state.last_failure_kind = None;
                    }
                    info!(
                        container = %node.container,
                        egress_ip,
                        rate_limited,
                        "WARP node probe OK"
                    );
                }
                Err(err) => {
                    let probe_failures = {
                        let mut state = node.state.lock().unwrap();
                        state.probe_failures = state.probe_failures.saturating_add(1);
                        state.last_probe = Some(Instant::now());
                        state.last_error = Some(format!("probe: {err}"));
                        state.probe_failures
                    };
                    warn!(
                        container = %node.container,
                        probe_failures,
                        error = %err,
                        "WARP node probe failed"
                    );

                    if probe_failures >= 3 {
                        self.report_failure_with(
                            &node.proxy_url,
                            FailureKind::Probe,
                            None,
                        );
                    }
                }
            }
        }
    }

    /// Starts one background prober per pool. Safe to call multiple times.
    pub fn spawn_prober(self: &Arc<Self>) {
        if self.prober_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            this.probe_all().await;
            let mut interval = tokio::time::interval(Duration::from_secs(
                this.config.probe_interval_secs.max(5),
            ));
            interval.tick().await;
            loop {
                interval.tick().await;
                this.probe_all().await;
            }
        });
    }
}

fn compute_cooldown(base_secs: u64, failures: u32) -> u64 {
    let exponent = failures.saturating_sub(1).min(5);
    base_secs
        .saturating_mul(1u64 << exponent)
        .min(MAX_COOLDOWN_SECS)
}

fn default_container_for(url: &str) -> String {
    if url.contains("40001") {
        "test-warp-1".to_string()
    } else if url.contains("40002") {
        "test-warp-2".to_string()
    } else {
        let port = url.rsplit(':').next().unwrap_or("1");
        format!("test-warp-{port}")
    }
}

fn runtime_binary(runtime: ContainerRuntime) -> &'static str {
    match runtime {
        ContainerRuntime::Docker => "docker",
        ContainerRuntime::Podman | ContainerRuntime::Auto => "podman",
    }
}

async fn exec_in_container(
    node: &WarpNode,
    args: &[&str],
) -> anyhow::Result<std::process::Output> {
    let mut last_error = None;

    for runtime in node.runtime_variants() {
        let mut cmd = node.command_for(runtime);
        cmd.args(["exec", node.container.as_str()]);
        cmd.args(args);

        match cmd.output().await {
            Ok(output) if output.status.success() => return Ok(output),
            Ok(output) => {
                last_error = Some(format!(
                    "{} exited {}: {}",
                    runtime_binary(runtime),
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            Err(err) => {
                last_error = Some(format!("{}: {}", runtime_binary(runtime), err));
            }
        }
    }

    Err(anyhow::anyhow!(
        "container exec failed for {}: {}",
        node.container,
        last_error.unwrap_or_else(|| "no supported container runtime".to_string())
    ))
}

async fn restart_container(node: &WarpNode) -> anyhow::Result<()> {
    let mut last_error = None;

    for runtime in node.runtime_variants() {
        let mut cmd = node.command_for(runtime);
        cmd.args(["restart", node.container.as_str()]);

        match cmd.output().await {
            Ok(output) if output.status.success() => return Ok(()),
            Ok(output) => {
                last_error = Some(format!(
                    "{} exited {}: {}",
                    runtime_binary(runtime),
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            Err(err) => {
                last_error = Some(format!("{}: {}", runtime_binary(runtime), err));
            }
        }
    }

    Err(anyhow::anyhow!(
        "container restart failed for {}: {}",
        node.container,
        last_error.unwrap_or_else(|| "no supported container runtime".to_string())
    ))
}

async fn probe_node(node: &WarpNode, config: &WarpConfig) -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(config.probe_timeout_secs.max(1)))
        .proxy(reqwest::Proxy::all(&node.proxy_url)?)
        .build()?;

    let response = client.get(&config.probe_url).send().await?;
    let status = response.status();
    let body = response.text().await?;

    if !status.is_success() {
        anyhow::bail!("probe HTTP {}", status);
    }

    match parse_warp_trace(&body) {
        Some(ip) => Ok(ip),
        None if config.require_warp => anyhow::bail!("egress is not WARP (warp=off)"),
        None => Ok("unknown".to_string()),
    }
}

/// Parses `https://www.cloudflare.com/cdn-cgi/trace`.
/// Returns the egress IP only when the response reports `warp=on`.
pub fn parse_warp_trace(body: &str) -> Option<String> {
    let mut egress_ip = None;
    let mut warp_on = false;

    for line in body.lines() {
        let line = line.trim();
        if let Some(ip) = line.strip_prefix("ip=") {
            if !ip.is_empty() {
                egress_ip = Some(ip.to_string());
            }
        } else if line == "warp=on" {
            warp_on = true;
        }
    }

    if warp_on {
        egress_ip
    } else {
        None
    }
}
