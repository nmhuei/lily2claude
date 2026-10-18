use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct WarpNode {
    pub proxy_url: String,
    pub container_name: String,
}

#[derive(Debug, Clone)]
pub struct WarpPool {
    nodes: Vec<WarpNode>,
    cursor: Arc<AtomicUsize>,
    cooldowns: Arc<Mutex<HashMap<String, Instant>>>,
}

impl WarpPool {
    pub fn new(proxies: Vec<String>) -> Self {
        let nodes = proxies
            .into_iter()
            .map(|p| {
                let container = if p.contains("40001") {
                    "test-warp-1".to_string()
                } else if p.contains("40002") {
                    "test-warp-2".to_string()
                } else if p.contains("40010") || p.contains("40003") {
                    "test-warp-3".to_string()
                } else {
                    let port = p.split(':').last().unwrap_or("1");
                    format!("test-warp-{}", port)
                };
                WarpNode {
                    proxy_url: p,
                    container_name: container,
                }
            })
            .collect();

        Self {
            nodes,
            cursor: Arc::new(AtomicUsize::new(0)),
            cooldowns: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn default_pool() -> Self {
        Self::new(vec![
            "http://127.0.0.1:40001".to_string(),
            "http://127.0.0.1:40002".to_string(),
            "http://127.0.0.1:40010".to_string(),
        ])
    }

    pub fn acquire_proxy(&self) -> Option<String> {
        if self.nodes.is_empty() {
            return None;
        }

        let now = Instant::now();
        let mut cooldowns = self.cooldowns.lock().unwrap();
        cooldowns.retain(|_, expiry| *expiry > now);

        let total = self.nodes.len();
        for _ in 0..total {
            let idx = self.cursor.fetch_add(1, Ordering::Relaxed) % total;
            let node = &self.nodes[idx];
            if !cooldowns.contains_key(&node.proxy_url) {
                // Verify port connectivity before handing out proxy
                if let Some(port) = parse_port_from_url(&node.proxy_url) {
                    if std::net::TcpStream::connect_timeout(
                        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                        Duration::from_millis(60),
                    )
                    .is_ok()
                    {
                        return Some(node.proxy_url.clone());
                    } else {
                        // Port unresponsive, mark cooldown
                        cooldowns.insert(node.proxy_url.clone(), now + Duration::from_secs(60));
                        continue;
                    }
                }
                return Some(node.proxy_url.clone());
            }
        }

        // All proxies rate-limited or in cooldown! Trigger auto-healing on first node
        drop(cooldowns);
        self.trigger_auto_heal();
        self.nodes.first().map(|n| n.proxy_url.clone())
    }

    pub fn mark_rate_limited(&self, proxy: &str, cooldown_secs: u64) {
        warn!(proxy, cooldown_secs, "Marking WARP proxy rate-limited, scheduling cooldown & IP rotation");
        let mut cooldowns = self.cooldowns.lock().unwrap();
        let expiry = Instant::now() + Duration::from_secs(cooldown_secs);
        cooldowns.insert(proxy.to_string(), expiry);

        // Find associated container name and trigger IP rotation in background
        if let Some(node) = self.nodes.iter().find(|n| n.proxy_url == proxy) {
            let container = node.container_name.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    info!(container = %container, "Rotating WARP container IP due to rate limit...");
                    let _ = tokio::process::Command::new("docker")
                        .args(["exec", &container, "rm", "-f", "/etc/sing-box/config.json", "/etc/sing-box/cache.db"])
                        .output()
                        .await;
                    let _ = tokio::process::Command::new("docker")
                        .args(["restart", &container])
                        .output()
                        .await;
                    info!(container = %container, "WARP container rotated with fresh Cloudflare IP!");
                });
            } else {
                std::thread::spawn(move || {
                    let _ = std::process::Command::new("docker")
                        .args(["exec", &container, "rm", "-f", "/etc/sing-box/config.json", "/etc/sing-box/cache.db"])
                        .output();
                    let _ = std::process::Command::new("docker")
                        .args(["restart", &container])
                        .output();
                });
            }
        }
    }

    pub fn is_exhausted(&self) -> bool {
        let now = Instant::now();
        let cooldowns = self.cooldowns.lock().unwrap();
        self.nodes
            .iter()
            .all(|n| cooldowns.get(&n.proxy_url).is_some_and(|exp| *exp > now))
    }

    pub fn status_report(&self) -> Vec<(String, String, bool)> {
        let now = Instant::now();
        let cooldowns = self.cooldowns.lock().unwrap();
        self.nodes
            .iter()
            .map(|n| {
                let is_cool = cooldowns.get(&n.proxy_url).is_some_and(|exp| *exp > now);
                (n.proxy_url.clone(), n.container_name.clone(), !is_cool)
            })
            .collect()
    }

    fn trigger_auto_heal(&self) {
        if let Some(target) = self.nodes.first().map(|n| n.container_name.clone()) {
            info!(target = %target, "All WARP proxies rate-limited! Spawning container restart for fresh Cloudflare IP...");
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = tokio::process::Command::new("docker")
                        .args(["exec", &target, "rm", "-f", "/etc/sing-box/config.json", "/etc/sing-box/cache.db"])
                        .output()
                        .await;
                    let _ = tokio::process::Command::new("docker")
                        .args(["restart", &target])
                        .output()
                        .await;
                    info!(target = %target, "WARP container restarted with fresh IP!");
                });
            } else {
                std::thread::spawn(move || {
                    let _ = std::process::Command::new("docker")
                        .args(["exec", &target, "rm", "-f", "/etc/sing-box/config.json", "/etc/sing-box/cache.db"])
                        .output();
                    let _ = std::process::Command::new("docker")
                        .args(["restart", &target])
                        .output();
                });
            }
        }
    }
}

fn parse_port_from_url(url: &str) -> Option<u16> {
    if let Some(pos) = url.rfind(':') {
        let sub = &url[pos + 1..];
        let digits: String = sub.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().ok()
    } else {
        None
    }
}
