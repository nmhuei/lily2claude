use super::usage::TokenUsage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

/// Per-session token usage record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTokenRecord {
    pub session_id: String,
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_read_tokens: u64,
    pub last_activity_epoch_secs: u64,
}

/// Per-model token usage record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTokenRecord {
    pub model_name: String,
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// Full token usage snapshot returned by GET /v1/stats
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenStatsSummary {
    pub service: String,
    pub version: String,
    pub uptime_seconds: u64,
    pub total_requests: u64,
    pub cumulative: CumulativeUsage,
    pub active_sessions_count: usize,
    pub sessions: Vec<SessionTokenRecord>,
    pub models: HashMap<String, ModelTokenRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CumulativeUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_read_tokens: u64,
    pub cached_creation_tokens: u64,
}

/// Thread-safe centralized token tracker stored in AppState
pub struct TokenUsageTracker {
    start_time: Instant,
    total_requests: AtomicU64,
    total_input_tokens: AtomicU64,
    total_output_tokens: AtomicU64,
    total_cached_read_tokens: AtomicU64,
    total_cached_creation_tokens: AtomicU64,
    sessions: Arc<RwLock<HashMap<String, SessionTokenRecord>>>,
    models: Arc<RwLock<HashMap<String, ModelTokenRecord>>>,
}

impl Default for TokenUsageTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenUsageTracker {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            total_requests: AtomicU64::new(0),
            total_input_tokens: AtomicU64::new(0),
            total_output_tokens: AtomicU64::new(0),
            total_cached_read_tokens: AtomicU64::new(0),
            total_cached_creation_tokens: AtomicU64::new(0),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            models: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn total_input_tokens(&self) -> u64 {
        self.total_input_tokens.load(Ordering::Relaxed)
    }

    pub fn total_output_tokens(&self) -> u64 {
        self.total_output_tokens.load(Ordering::Relaxed)
    }

    pub fn total_requests(&self) -> u64 {
        self.total_requests.load(Ordering::Relaxed)
    }

    /// Record completed turn usage for a session and model
    pub async fn record_turn(
        &self,
        session_id: &str,
        model: &str,
        usage: TokenUsage,
    ) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        self.total_input_tokens.fetch_add(usage.input_tokens, Ordering::Relaxed);
        self.total_output_tokens.fetch_add(usage.output_tokens, Ordering::Relaxed);
        self.total_cached_read_tokens.fetch_add(usage.cache_read_input_tokens, Ordering::Relaxed);
        self.total_cached_creation_tokens.fetch_add(usage.cache_creation_input_tokens, Ordering::Relaxed);

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // 1. Update session record
        let mut sessions_lock = self.sessions.write().await;
        let session_rec = sessions_lock
            .entry(session_id.to_string())
            .or_insert_with(|| SessionTokenRecord {
                session_id: session_id.to_string(),
                request_count: 0,
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                cached_read_tokens: 0,
                last_activity_epoch_secs: now_secs,
            });
        session_rec.request_count = session_rec.request_count.saturating_add(1);
        session_rec.input_tokens = session_rec.input_tokens.saturating_add(usage.input_tokens);
        session_rec.output_tokens = session_rec.output_tokens.saturating_add(usage.output_tokens);
        session_rec.total_tokens = session_rec.input_tokens.saturating_add(session_rec.output_tokens);
        session_rec.cached_read_tokens = session_rec.cached_read_tokens.saturating_add(usage.cache_read_input_tokens);
        session_rec.last_activity_epoch_secs = now_secs;

        // 2. Update model record
        let mut models_lock = self.models.write().await;
        let model_rec = models_lock
            .entry(model.to_string())
            .or_insert_with(|| ModelTokenRecord {
                model_name: model.to_string(),
                request_count: 0,
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            });
        model_rec.request_count = model_rec.request_count.saturating_add(1);
        model_rec.input_tokens = model_rec.input_tokens.saturating_add(usage.input_tokens);
        model_rec.output_tokens = model_rec.output_tokens.saturating_add(usage.output_tokens);
        model_rec.total_tokens = model_rec.input_tokens.saturating_add(model_rec.output_tokens);
    }

    /// Reset all token counters, sessions, and model records to zero
    pub async fn reset(&self) {
        self.total_requests.store(0, Ordering::Relaxed);
        self.total_input_tokens.store(0, Ordering::Relaxed);
        self.total_output_tokens.store(0, Ordering::Relaxed);
        self.total_cached_read_tokens.store(0, Ordering::Relaxed);
        self.total_cached_creation_tokens.store(0, Ordering::Relaxed);
        let mut sessions_lock = self.sessions.write().await;
        sessions_lock.clear();
        let mut models_lock = self.models.write().await;
        models_lock.clear();
    }

    /// Generate full summary snapshot
    pub async fn snapshot(&self) -> TokenStatsSummary {
        let uptime_seconds = self.start_time.elapsed().as_secs();
        let total_requests = self.total_requests.load(Ordering::Relaxed);
        let in_tok = self.total_input_tokens.load(Ordering::Relaxed);
        let out_tok = self.total_output_tokens.load(Ordering::Relaxed);
        let cache_read = self.total_cached_read_tokens.load(Ordering::Relaxed);
        let cache_create = self.total_cached_creation_tokens.load(Ordering::Relaxed);

        let sessions_lock = self.sessions.read().await;
        let mut sessions_vec: Vec<SessionTokenRecord> = sessions_lock.values().cloned().collect();
        sessions_vec.sort_by_key(|b| std::cmp::Reverse(b.last_activity_epoch_secs));

        let models_lock = self.models.read().await;
        let models_map = models_lock.clone();

        TokenStatsSummary {
            service: "lily2claude".to_string(),
            version: "0.1.0".to_string(),
            uptime_seconds,
            total_requests,
            cumulative: CumulativeUsage {
                input_tokens: in_tok,
                output_tokens: out_tok,
                total_tokens: in_tok.saturating_add(out_tok),
                cached_read_tokens: cache_read,
                cached_creation_tokens: cache_create,
            },
            active_sessions_count: sessions_vec.len(),
            sessions: sessions_vec,
            models: models_map,
        }
    }
}
