use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct WarpPool {
    proxies: Vec<String>,
    cursor: Arc<AtomicUsize>,
    cooldowns: Arc<Mutex<HashMap<String, Instant>>>,
    container_names: Vec<String>,
}

impl WarpPool {
    pub fn new(proxies: Vec<String>) -> Self {
        Self {
            proxies,
            cursor: Arc::new(AtomicUsize::new(0)),
            cooldowns: Arc::new(Mutex::new(HashMap::new())),
            container_names: vec!["test-warp-1".to_string(), "test-warp-2".to_string()],
        }
    }

    pub fn acquire_proxy(&self) -> Option<String> {
        if self.proxies.is_empty() {
            return None;
        }

        let now = Instant::now();
        let mut cooldowns = self.cooldowns.lock().unwrap();
        cooldowns.retain(|_, expiry| *expiry > now);

        let total = self.proxies.len();
        for _ in 0..total {
            let idx = self.cursor.fetch_add(1, Ordering::Relaxed) % total;
            let candidate = &self.proxies[idx];
            if !cooldowns.contains_key(candidate) {
                return Some(candidate.clone());
            }
        }

        // All proxies rate-limited! Trigger auto-healing restart
        drop(cooldowns);
        self.trigger_auto_heal();
        self.proxies.first().cloned()
    }

    pub fn mark_rate_limited(&self, proxy: &str, cooldown_secs: u64) {
        warn!(proxy, cooldown_secs, "Marking WARP proxy rate-limited (429)");
        let mut cooldowns = self.cooldowns.lock().unwrap();
        let expiry = Instant::now() + Duration::from_secs(cooldown_secs);
        cooldowns.insert(proxy.to_string(), expiry);
    }

    pub fn is_exhausted(&self) -> bool {
        let now = Instant::now();
        let cooldowns = self.cooldowns.lock().unwrap();
        self.proxies
            .iter()
            .all(|p| cooldowns.get(p).map_or(false, |exp| *exp > now))
    }

    fn trigger_auto_heal(&self) {
        if let Some(target) = self.container_names.first().cloned() {
            info!(target = %target, "All WARP proxies rate-limited! Spawning non-blocking container restart for fresh Cloudflare IP...");
            tokio::spawn(async move {
                let _ = tokio::process::Command::new("docker")
                    .args(["restart", &target])
                    .output()
                    .await;
                info!(target = %target, "WARP container restarted with fresh IP!");
            });
        }
    }
}
