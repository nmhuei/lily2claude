use super::acp::AcpSession;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

pub type SessionEntry = (Option<u32>, Arc<Mutex<AcpSession>>);

#[derive(Clone)]
pub struct SessionManager {
    sessions: Arc<Mutex<HashMap<String, SessionEntry>>>,
    warp_pool: Option<Arc<crate::config::warp_pool::WarpPool>>,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    pub fn new() -> Self {
        let warp_pool = if std::env::var_os("L2C_DISABLE_WARP").is_none() {
            let config = crate::config::Config::load();
            // `require_warp` is the switch. The node list alone must not force
            // proxying: nodes default to a non-empty list, so `||` here made
            // `require_warp = false` impossible to honour.
            if config.warp.require_warp && !config.warp.nodes.is_empty() {
                Some(Arc::new(crate::config::warp_pool::WarpPool::from_config(
                    &config.warp,
                )))
            } else {
                None
            }
        } else {
            None
        };
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            warp_pool,
        }
    }

    pub fn with_pool(warp_pool: Option<Arc<crate::config::warp_pool::WarpPool>>) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            warp_pool,
        }
    }

    pub fn warp_pool(&self) -> Option<Arc<crate::config::warp_pool::WarpPool>> {
        self.warp_pool.clone()
    }

    pub async fn active_count(&self) -> usize {
        let lock = self.sessions.lock().await;
        lock.len()
    }

    pub async fn get_or_create(
        &self,
        session_key: &str,
        provider: &str,
        command: &[String],
    ) -> anyhow::Result<Arc<Mutex<AcpSession>>> {
        let base_key = format!("{}:{}", provider, session_key);

        // 1. Try to find an existing idle worker without spawning
        {
            let mut lock = self.sessions.lock().await;
            let mut dead_keys = Vec::new();
            for worker_idx in 0..4 {
                let full_key = if worker_idx == 0 {
                    base_key.clone()
                } else {
                    format!("{}:worker_{}", base_key, worker_idx)
                };

                if let Some((_, existing)) = lock.get(&full_key) {
                    let (is_alive, is_dead) = match existing.try_lock() {
                        Ok(mut session_lock) => {
                            let proxy_cooling = if let (Some(ref pool), Some(ref proxy)) = (&self.warp_pool, &session_lock.proxy_used) {
                                pool.is_proxy_cooling(proxy)
                            } else {
                                false
                            };

                            if proxy_cooling {
                                tracing::warn!(
                                    session = %full_key,
                                    proxy = ?session_lock.proxy_used,
                                    "Session proxy is cooling down; terminating worker to force failover"
                                );
                                let _ = session_lock.child.start_kill();
                                (None, true)
                            } else {
                                match session_lock.child.try_wait() {
                                    Ok(None) => (Some(existing.clone()), false),
                                    _ => (None, true),
                                }
                            }
                        }
                        Err(_) => (None, false),
                    };

                    if let Some(found) = is_alive {
                        for dk in dead_keys {
                            lock.remove(&dk);
                        }
                        return Ok(found);
                    }
                    if is_dead {
                        dead_keys.push(full_key);
                    }
                }
            }
            for dk in dead_keys {
                lock.remove(&dk);
            }
        }

        // 2. Spawn new session OUTSIDE the lock to eliminate any possibility of deadlock
        let new_session = AcpSession::spawn(command, self.warp_pool.clone()).await?;
        let child_pid = new_session.child_pid;
        let handle = Arc::new(Mutex::new(new_session));

        // 3. Insert into sessions map with lock
        let mut lock = self.sessions.lock().await;
        for worker_idx in 0..4 {
            let full_key = if worker_idx == 0 {
                base_key.clone()
            } else {
                format!("{}:worker_{}", base_key, worker_idx)
            };
            if let std::collections::hash_map::Entry::Vacant(e) = lock.entry(full_key) {
                e.insert((child_pid, handle.clone()));
                return Ok(handle);
            }
        }
        lock.insert(base_key, (child_pid, handle.clone()));
        Ok(handle)
    }

    pub async fn sweep_idle(&self, max_idle: Duration) {
        let to_kill = {
            let mut lock = self.sessions.lock().await;
            let mut to_remove_keys = Vec::new();
            let mut sessions_to_kill = Vec::new();

            for (k, (pid, sess)) in lock.iter() {
                if let Ok(s) = sess.try_lock() {
                    if s.last_used.elapsed() > max_idle {
                        to_remove_keys.push(k.clone());
                        sessions_to_kill.push((*pid, sess.clone()));
                    }
                }
            }

            for k in to_remove_keys {
                lock.remove(&k);
            }
            sessions_to_kill
        };

        for (pid, sess) in to_kill {
            if let Ok(mut s) = sess.try_lock() {
                let _ = s.child.kill().await;
                let _ = s.child.wait().await;
            }
            if let Some(p) = pid {
                let _ = tokio::process::Command::new("kill").args(["-9", &p.to_string()]).output().await;
            }
        }
    }

    pub async fn evict_session(&self, session_key: &str, provider: &str) {
        let base_prefix = format!("{}:{}", provider, session_key);
        let to_kill = {
            let mut lock = self.sessions.lock().await;
            let mut to_remove_keys = Vec::new();
            let mut sessions_to_kill = Vec::new();

            for (k, (pid, sess)) in lock.iter() {
                if k == &base_prefix || k.starts_with(&format!("{}:", base_prefix)) {
                    to_remove_keys.push(k.clone());
                    sessions_to_kill.push((*pid, sess.clone()));
                }
            }

            for k in to_remove_keys {
                lock.remove(&k);
            }
            sessions_to_kill
        };

        for (pid, sess) in to_kill {
            if let Ok(mut s) = sess.try_lock() {
                let _ = s.child.kill().await;
                let _ = s.child.wait().await;
            }
            if let Some(p) = pid {
                let _ = tokio::process::Command::new("kill").args(["-9", &p.to_string()]).output().await;
            }
        }
    }
}
