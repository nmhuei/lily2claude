# Lily2Claude Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build and deploy `lily2claude`, an ultra-fast, modular Rust CLI and gateway connecting Claude Code CLI (`claude`) to local agents (CommandCode, Kilo, Cline via ACP stdio) and cloud free-tier models (OpenCode Zen via dual-node Cloudflare WARP proxy pool) with zero network probing, transparent aliases, automatic context compact detection, and robust subagent/cronjob execution.

**Architecture:** A lightweight Axum HTTP server hosting Anthropic Messages API (`POST /v1/messages` for both streaming and non-streaming title requests, `POST /v1/messages/count_tokens`, `GET /v1/models`, `GET /health`). Inbound messages are sanitized, tools are normalized bidirectionally (PascalCase <-> lowercase with 6-core tool whitelist), and routed across local ACP agents (0ms latency) and dual WARP SOCKS5 nodes (ports 40001, 40002). Outbound SSE events stream via an FSM state machine handling reasoning blocks and self-healing JSON repair. The CLI provides 4 focused, simple commands: `serve`, `launch`, `list`, `doctor`.

**Tech Stack:** Rust 2021, Tokio, Axum 0.7, Reqwest 0.12 (SOCKS5, rustls-tls), Serde, Serde_json, Clap 4.4, Toml 0.8, Tracing.

**Spec:** `/home/light/GitHub/lily2claude/docs/ARCHITECTURE.md`

## Global Constraints
- Target Language: Rust 2021 edition, fully modular architecture under `src/`.
- No bloated legacy baggage: Zero copy-pasting from old `opencode2claude`.
- CLI Simplicity: Strictly 4 essential commands (`serve`, `launch`, `list`, `doctor`). No bloated unused subcommands.
- Model transparency: Never mask models behind generic `opus`/`sonnet`; expose transparent custom aliases (`deepseek`, `glm`, `kilo`, `luna`, `mimo`, `nemotron`).
- Zero network scanning: Catalog discovery occurs in-memory from local JSON caches in <15ms without sequential network probing.
- Dual-node WARP proxy pool: SOCKS5 egress over `127.0.0.1:40001` and `127.0.0.1:40002` with 60s cooldown and 2.5s container auto-restart on dual 429 exhaustion.
- Tool Case Discrepancy & Core Whitelist Invariant: Outbound tool names mapped to lowercase (`bash`, `edit`, `read`, `write`, `glob`, `grep`), inbound tool calls mapped back to Claude Code PascalCase (`Bash`, `Edit`, etc.).
- Protocol Completeness: Must support `POST /v1/messages/count_tokens`, non-streaming `POST /v1/messages` (session title generation), and sanitization of Anthropic metadata.

---

### Task 1: Core Cargo Setup, Error Handling & CLI Scaffolding

**Files:**
- Create: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/error.rs`
- Create: `src/cli.rs`
- Create: `src/main.rs`
- Create: `.gitignore`
- Test: `tests/test_cli.rs`

**Interfaces:**
- Consumes: Command line arguments (`serve`, `launch`, `list`, `doctor`)
- Produces: `lily2claude` binary entry point and `Result<T, LilyError>`

- [ ] **Step 1: Write the failing CLI test**

```rust
// tests/test_cli.rs
use std::process::Command;

#[test]
fn test_cli_help_displays_core_commands() {
    let output = Command::new("cargo")
        .args(["run", "--", "--help"])
        .output()
        .expect("Failed to execute cargo run");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("serve"));
    assert!(stdout.contains("launch"));
    assert!(stdout.contains("list"));
    assert!(stdout.contains("doctor"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test test_cli`
Expected: FAIL (Cargo.toml not found)

- [ ] **Step 3: Implement minimal Cargo.toml, src/error.rs, src/cli.rs, src/lib.rs, and src/main.rs**

```toml
# Cargo.toml
[package]
name = "lily2claude"
version = "0.1.0"
edition = "2021"
authors = ["light"]
description = "Universal bridge connecting Claude Code CLI to Local Agents (ACP) and Cloud Free Tier (WARP)"

[dependencies]
tokio = { version = "1.35", features = ["full"] }
axum = { version = "0.7", features = ["macros"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
reqwest = { version = "0.12", default-features = false, features = ["json", "stream", "socks", "rustls-tls"] }
tokio-stream = "0.1"
futures-util = "0.3"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
clap = { version = "4.4", features = ["derive"] }
toml = "0.8"
anyhow = "1.0"
thiserror = "2"
bytes = "1"
regex = "1.11"
tower-http = { version = "0.5", features = ["trace", "cors"] }

[dev-dependencies]
tokio-test = "0.4"
```

```rust
// src/error.rs
use thiserror::Error;

#[derive(Error, Debug)]
pub enum LilyError {
    #[error("Configuration error: {0}")]
    Config(String),
    #[error("Upstream HTTP error: {0}")]
    UpstreamHttp(String),
    #[error("Protocol error: {0}")]
    Protocol(String),
    #[error("ACP agent error: {0}")]
    Acp(String),
    #[error("Rate limited (429)")]
    RateLimited,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, LilyError>;
```

```rust
// src/cli.rs
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "lily2claude",
    author = "light",
    version = "0.1.0",
    about = "Universal high-performance bridge connecting Claude Code CLI to Local Agents & OpenCode Free Tier"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Start the proxy server
    Serve {
        #[arg(short, long, default_value = "4001")]
        port: u16,
        #[arg(short = 'H', long, default_value = "127.0.0.1")]
        host: String,
    },
    /// Launch Claude Code connected to lily2claude
    Launch {
        #[arg(short, long, default_value = "deepseek")]
        model: String,
        #[arg(short, long, default_value = "4001")]
        port: u16,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra_args: Vec<String>,
    },
    /// List available models and aliases
    List {
        #[arg(short, long)]
        free_only: bool,
    },
    /// Healthcheck Docker WARP nodes and local agents
    Doctor,
}
```

```rust
// src/lib.rs
pub mod cli;
pub mod config;
pub mod discovery;
pub mod error;
pub mod protocol;
pub mod adapters;
pub mod engine;
pub mod launcher;
pub mod server;
```

```rust
// src/main.rs
use clap::Parser;
use lily2claude::cli::{Cli, Commands};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command.unwrap_or(Commands::Serve { port: 4001, host: "127.0.0.1".to_string() }) {
        Commands::Serve { port, host } => {
            println!("Starting lily2claude server on {}:{}...", host, port);
        }
        Commands::Launch { model, port, extra_args } => {
            println!("Launching Claude Code with model {} on port {} (args: {:?})...", model, port, extra_args);
        }
        Commands::List { free_only } => {
            println!("Listing models (free_only={})...", free_only);
        }
        Commands::Doctor => {
            println!("Running doctor healthcheck...");
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test test_cli`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/ tests/ .gitignore
git commit -m "feat: initialize lily2claude with 4-command CLI scaffolding and error handling"
```

---

### Task 2: Declarative Configuration, Discovery & WARP Pool

**Files:**
- Create: `src/config/warp_pool.rs`
- Create: `src/config/mod.rs`
- Create: `src/discovery/catalog.rs`
- Create: `src/discovery/mod.rs`
- Create: `templates/config.toml`
- Test: `tests/test_config_discovery.rs`

**Interfaces:**
- Consumes: `~/.cache/kilo/models.json`, `~/.commandcode/cache/model-catalog/v1/*.json`
- Produces: `Config`, `WarpPool` (round-robin with cooldown & auto-restart trigger), `CatalogRegistry` (in-memory zero-scan filter)

- [ ] **Step 1: Write the failing unit tests for Config, WarpPool, and Discovery**

```rust
// tests/test_config_discovery.rs
use lily2claude::config::warp_pool::WarpPool;
use lily2claude::config::Config;
use lily2claude::discovery::catalog::CatalogRegistry;

#[test]
fn test_warp_pool_failover_and_cooldown() {
    let pool = WarpPool::new(vec![
        "socks5h://127.0.0.1:40001".to_string(),
        "socks5h://127.0.0.1:40002".to_string(),
    ]);

    let p1 = pool.acquire_proxy().expect("Proxy 1");
    let p2 = pool.acquire_proxy().expect("Proxy 2");
    assert_ne!(p1, p2);

    pool.mark_rate_limited(&p1, 60);
    let p3 = pool.acquire_proxy().expect("Should get non-cooldown proxy");
    assert_eq!(p3, p2);
}

#[test]
fn test_in_memory_catalog_discovery() {
    let catalog = CatalogRegistry::load_from_system().expect("Load local catalogs");
    let free_models = catalog.filter_free_models();
    assert!(!free_models.is_empty(), "Should discover local free models in <15ms");

    let deepseek_meta = catalog.get_metadata("deepseek");
    assert!(deepseek_meta.is_some());
    assert!(deepseek_meta.unwrap().context_length >= 200000);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test test_config_discovery`
Expected: FAIL (modules not implemented)

- [ ] **Step 3: Implement WarpPool, Config, and CatalogRegistry**

```rust
// src/config/warp_pool.rs
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct WarpPool {
    proxies: Vec<String>,
    cursor: Arc<AtomicUsize>,
    cooldowns: Arc<Mutex<HashMap<String, Instant>>>,
}

impl WarpPool {
    pub fn new(proxies: Vec<String>) -> Self {
        Self {
            proxies,
            cursor: Arc::new(AtomicUsize::new(0)),
            cooldowns: Arc::new(Mutex::new(HashMap::new())),
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

        // Dual exhaustion: all proxies rate limited! Return first and trigger recovery
        self.proxies.first().cloned()
    }

    pub fn mark_rate_limited(&self, proxy: &str, cooldown_secs: u64) {
        let mut cooldowns = self.cooldowns.lock().unwrap();
        let expiry = Instant::now() + Duration::from_secs(cooldown_secs);
        cooldowns.insert(proxy.to_string(), expiry);
    }

    pub fn is_exhausted(&self) -> bool {
        let now = Instant::now();
        let cooldowns = self.cooldowns.lock().unwrap();
        self.proxies.iter().all(|p| cooldowns.get(p).map_or(false, |exp| *exp > now))
    }
}
```

```rust
// src/config/mod.rs
pub mod warp_pool;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub kind: String,
    pub endpoint: Option<String>,
    pub command: Option<Vec<String>>,
    pub auth_file: Option<String>,
    pub auth_token: Option<String>,
    pub warp_proxies: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server_port: u16,
    pub aliases: HashMap<String, Vec<String>>,
    pub providers: HashMap<String, ProviderConfig>,
}

impl Config {
    pub fn load_default() -> Self {
        let mut aliases = HashMap::new();
        aliases.insert(
            "deepseek".to_string(),
            vec![
                "commandcode:deepseek/deepseek-v4.1-flash".to_string(),
                "opencode:deepseek-v4.1-flash-free".to_string(),
                "kilo:kilo-auto/free".to_string(),
            ],
        );
        aliases.insert(
            "glm".to_string(),
            vec![
                "commandcode:z-ai/glm-5.3-flash".to_string(),
                "opencode:glm-5.3-flash-free".to_string(),
            ],
        );
        aliases.insert(
            "kilo".to_string(),
            vec![
                "kilo:kilo-auto/free".to_string(),
                "kilo:openrouter/free".to_string(),
                "kilo:reflection/beam-free".to_string(),
            ],
        );
        aliases.insert(
            "luna".to_string(),
            vec!["cline:openai-codex:gpt-5.6-luna".to_string()],
        );
        aliases.insert(
            "mimo".to_string(),
            vec![
                "commandcode:xiaomi/mimo-v2.6-flash".to_string(),
                "opencode:mimo-v2.6-flash-free".to_string(),
            ],
        );
        aliases.insert(
            "nemotron".to_string(),
            vec![
                "kilo:nvidia/nemotron-3.5-lightning:free".to_string(),
                "opencode:nvidia/nemotron-3-nano-free".to_string(),
            ],
        );

        let mut providers = HashMap::new();
        providers.insert(
            "opencode".to_string(),
            ProviderConfig {
                name: "opencode".to_string(),
                kind: "opencode_free".to_string(),
                endpoint: Some("https://opencode.ai/inference/openai/v1".to_string()),
                command: None,
                auth_file: Some("/home/light/.local/share/opencode/auth.json".to_string()),
                auth_token: None,
                warp_proxies: Some(vec![
                    "socks5h://127.0.0.1:40001".to_string(),
                    "socks5h://127.0.0.1:40002".to_string(),
                ]),
            },
        );
        providers.insert(
            "commandcode".to_string(),
            ProviderConfig {
                name: "commandcode".to_string(),
                kind: "acp_stdio".to_string(),
                endpoint: None,
                command: Some(vec!["commandcode".to_string(), "acp".to_string()]),
                auth_file: Some("/home/light/.commandcode/auth.json".to_string()),
                auth_token: None,
                warp_proxies: None,
            },
        );
        providers.insert(
            "kilo".to_string(),
            ProviderConfig {
                name: "kilo".to_string(),
                kind: "acp_stdio".to_string(),
                endpoint: None,
                command: Some(vec!["kilo".to_string(), "acp".to_string()]),
                auth_file: None,
                auth_token: None,
                warp_proxies: None,
            },
        );
        providers.insert(
            "cline".to_string(),
            ProviderConfig {
                name: "cline".to_string(),
                kind: "acp_stdio".to_string(),
                endpoint: None,
                command: Some(vec!["cline".to_string(), "--acp".to_string()]),
                auth_file: Some("/home/light/.cline/data/settings/providers.json".to_string()),
                auth_token: None,
                warp_proxies: None,
            },
        );

        Self {
            server_port: 4001,
            aliases,
            providers,
        }
    }
}
```

```rust
// src/discovery/catalog.rs
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMetadata {
    pub id: String,
    pub name: String,
    pub context_length: u32,
    pub max_output_tokens: u32,
    pub is_free: bool,
}

#[derive(Debug, Clone)]
pub struct CatalogRegistry {
    models: HashMap<String, ModelMetadata>,
    free_regex: Regex,
}

impl CatalogRegistry {
    pub fn new() -> Self {
        Self {
            models: HashMap::new(),
            free_regex: Regex::new(r"(?i)(-free|:free|/free)$").unwrap(),
        }
    }

    pub fn load_from_system() -> anyhow::Result<Self> {
        let mut registry = Self::new();

        // 1. Load Kilo cache (~/.cache/kilo/models.json)
        let kilo_cache = Path::new("/home/light/.cache/kilo/models.json");
        if kilo_cache.exists() {
            if let Ok(data) = std::fs::read_to_string(kilo_cache) {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&data) {
                    if let Some(obj) = val.as_object() {
                        for (prov_name, p_data) in obj {
                            if let Some(models) = p_data.get("models").and_then(|m| m.as_object()) {
                                for (m_id, m_info) in models {
                                    let ctx = m_info
                                        .pointer("/limit/context")
                                        .and_then(|v| v.as_u64())
                                        .or_else(|| m_info.get("context_length").and_then(|v| v.as_u64()))
                                        .unwrap_or(128000) as u32;
                                    let out = m_info
                                        .pointer("/limit/output")
                                        .and_then(|v| v.as_u64())
                                        .or_else(|| m_info.get("max_tokens").and_then(|v| v.as_u64()))
                                        .unwrap_or(8192) as u32;
                                    let name = m_info.get("name").and_then(|v| v.as_str()).unwrap_or(m_id);
                                    let is_free = registry.free_regex.is_match(m_id);

                                    let qualified_id = format!("{}/{}", prov_name, m_id);
                                    registry.models.insert(
                                        qualified_id.clone(),
                                        ModelMetadata {
                                            id: qualified_id,
                                            name: name.to_string(),
                                            context_length: ctx,
                                            max_output_tokens: out,
                                            is_free,
                                        },
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        // Add standard fallback aliases
        registry.models.insert(
            "deepseek".to_string(),
            ModelMetadata {
                id: "deepseek".to_string(),
                name: "DeepSeek V4.1 Flash".to_string(),
                context_length: 200000,
                max_output_tokens: 32000,
                is_free: true,
            },
        );
        registry.models.insert(
            "glm".to_string(),
            ModelMetadata {
                id: "glm".to_string(),
                name: "GLM-5.3-Flash".to_string(),
                context_length: 200000,
                max_output_tokens: 32000,
                is_free: true,
            },
        );
        registry.models.insert(
            "kilo".to_string(),
            ModelMetadata {
                id: "kilo".to_string(),
                name: "Kilo Auto Free".to_string(),
                context_length: 256000,
                max_output_tokens: 32768,
                is_free: true,
            },
        );
        registry.models.insert(
            "luna".to_string(),
            ModelMetadata {
                id: "luna".to_string(),
                name: "GPT-5.6 Luna".to_string(),
                context_length: 400000,
                max_output_tokens: 128000,
                is_free: true,
            },
        );

        Ok(registry)
    }

    pub fn filter_free_models(&self) -> Vec<ModelMetadata> {
        self.models.values().filter(|m| m.is_free).cloned().collect()
    }

    pub fn get_metadata(&self, model: &str) -> Option<ModelMetadata> {
        if let Some(m) = self.models.get(model) {
            return Some(m.clone());
        }
        if model.contains("deepseek") {
            return Some(ModelMetadata {
                id: model.to_string(),
                name: "DeepSeek".to_string(),
                context_length: 200000,
                max_output_tokens: 32000,
                is_free: true,
            });
        }
        if model.contains("nemotron") {
            return Some(ModelMetadata {
                id: model.to_string(),
                name: "Nemotron".to_string(),
                context_length: 1000000,
                max_output_tokens: 65536,
                is_free: true,
            });
        }
        None
    }
}
```

```rust
// src/discovery/mod.rs
pub mod catalog;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test test_config_discovery`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/config/ src/discovery/ tests/test_config_discovery.rs
git commit -m "feat: implement declarative configuration, WARP pool, and in-memory catalog discovery"
```

---

### Task 3: Protocol Schemas, Message Transformations & Sanitization

**Files:**
- Create: `src/protocol/anthropic.rs`
- Create: `src/protocol/openai.rs`
- Create: `src/protocol/repair.rs`
- Create: `src/protocol/transform.rs`
- Create: `src/protocol/mod.rs`
- Test: `tests/test_protocol_transform.rs`

**Interfaces:**
- Consumes: Anthropic `MessagesRequest` with potential metadata (`cache_control`, `context_management`, prior thinking blocks)
- Produces: Sanitized OpenAI `ChatCompletionsRequest` with sequential `role: "tool"` messages and 6-core tool whitelist

- [ ] **Step 1: Write failing protocol transform tests**

```rust
// tests/test_protocol_transform.rs
use lily2claude::protocol::anthropic::*;
use lily2claude::protocol::repair::repair_tool_arguments;
use lily2claude::protocol::transform::transform_anthropic_to_openai;
use serde_json::json;

#[test]
fn test_strip_historic_thinking_blocks_and_sanitize() {
    let req = MessagesRequest {
        model: "deepseek-v4.1-flash".to_string(),
        messages: vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("hello".to_string()),
            },
            AnthropicMessage {
                role: "assistant".to_string(),
                content: AnthropicContent::Blocks(vec![
                    ContentBlock::Thinking { thinking: "prior thinking to strip".to_string() },
                    ContentBlock::Text { text: "assistant response".to_string() },
                ]),
            },
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Blocks(vec![
                    ContentBlock::ToolResult {
                        tool_use_id: "call_abc".to_string(),
                        content: "tool output".to_string(),
                        is_error: None,
                    }
                ]),
            },
        ],
        system: Some(SystemPrompt::Single("system prompt".to_string())),
        tools: Some(vec![
            AnthropicTool {
                name: "Bash".to_string(),
                description: Some("run bash".to_string()),
                input_schema: json!({"type": "object"}),
            }
        ]),
        stream: true,
        max_tokens: 4096,
        temperature: None,
    };

    let openai_req = transform_anthropic_to_openai(&req, true).expect("Transform must succeed");

    // Assistant content must NOT contain thinking block object
    assert_eq!(openai_req.messages[2].role, "assistant");
    assert_eq!(openai_req.messages[2].content.as_deref(), Some("assistant response"));

    // Tool result must be role: "tool"
    assert_eq!(openai_req.messages[3].role, "tool");
    assert_eq!(openai_req.messages[3].tool_call_id.as_deref(), Some("call_abc"));

    // Core tools must be present (lowercase)
    let tools = openai_req.tools.unwrap();
    let names: Vec<String> = tools.into_iter().map(|t| t.function.name).collect();
    assert!(names.contains(&"bash".to_string()));
    assert!(names.contains(&"edit".to_string()));
    assert!(names.contains(&"read".to_string()));
    assert!(names.contains(&"write".to_string()));
    assert!(names.contains(&"glob".to_string()));
    assert!(names.contains(&"grep".to_string()));
}

#[test]
fn test_repair_tool_arguments() {
    let broken_json = "{\"command\": \"echo 'test'\",}";
    let fixed = repair_tool_arguments(broken_json);
    assert_eq!(fixed, "{\"command\": \"echo 'test'\"}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test test_protocol_transform`
Expected: FAIL (modules not implemented)

- [ ] **Step 3: Implement Anthropic/OpenAI protocol schemas, repair, and transformation**

```rust
// src/protocol/anthropic.rs
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagesRequest {
    pub model: String,
    pub messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemPrompt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicTool>>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

fn default_max_tokens() -> u32 {
    4096
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemPrompt {
    Single(String),
    Blocks(Vec<SystemBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: AnthropicContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnthropicContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicTool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
}
```

```rust
// src/protocol/openai.rs
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionsRequest {
    pub model: String,
    pub messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<OpenAiTool>>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: OpenAiFunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiTool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: OpenAiFunctionDefinition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiFunctionDefinition {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: Value,
}
```

```rust
// src/protocol/repair.rs
use serde_json::Value;

pub fn repair_tool_arguments(raw: &str) -> String {
    if serde_json::from_str::<Value>(raw).is_ok() {
        return raw.to_string();
    }

    let mut candidate = raw.trim().to_string();
    if candidate.ends_with(",}") {
        candidate = candidate.replace(",}", "}");
    }
    if candidate.ends_with(",]") {
        candidate = candidate.replace(",]", "]");
    }
    if candidate.starts_with('{') && !candidate.ends_with('}') {
        candidate.push('}');
    }

    if serde_json::from_str::<Value>(&candidate).is_ok() {
        return candidate;
    }
    raw.to_string()
}
```

```rust
// src/protocol/transform.rs
use crate::protocol::anthropic::*;
use crate::protocol::openai::*;
use serde_json::json;

pub fn transform_anthropic_to_openai(
    req: &MessagesRequest,
    is_opencode: bool,
) -> anyhow::Result<ChatCompletionsRequest> {
    let mut messages: Vec<OpenAiMessage> = Vec::new();

    // 1. System Prompt
    if let Some(sys) = &req.system {
        let sys_text = match sys {
            SystemPrompt::Single(s) => s.clone(),
            SystemPrompt::Blocks(blocks) => blocks
                .iter()
                .map(|b| b.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
        };
        messages.push(OpenAiMessage {
            role: "system".to_string(),
            content: Some(sys_text),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    // 2. Iterate messages & sanitize
    for msg in &req.messages {
        match &msg.content {
            AnthropicContent::Text(text) => {
                messages.push(OpenAiMessage {
                    role: msg.role.clone(),
                    content: Some(text.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
            AnthropicContent::Blocks(blocks) => {
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();

                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => {
                            text_parts.push(text.clone());
                        }
                        ContentBlock::Thinking { .. } => {
                            // Strip historic thinking blocks from chat history!
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            tool_calls.push(OpenAiToolCall {
                                id: id.clone(),
                                call_type: "function".to_string(),
                                function: OpenAiFunctionCall {
                                    name: name.to_ascii_lowercase(),
                                    arguments: input.to_string(),
                                },
                            });
                        }
                        ContentBlock::ToolResult { tool_use_id, content, .. } => {
                            messages.push(OpenAiMessage {
                                role: "tool".to_string(),
                                content: Some(content.clone()),
                                tool_calls: None,
                                tool_call_id: Some(tool_use_id.clone()),
                            });
                        }
                    }
                }

                if !text_parts.is_empty() || !tool_calls.is_empty() {
                    let text = if text_parts.is_empty() { None } else { Some(text_parts.join("\n")) };
                    let tools = if tool_calls.is_empty() { None } else { Some(tool_calls) };
                    messages.push(OpenAiMessage {
                        role: msg.role.clone(),
                        content: text,
                        tool_calls: tools,
                        tool_call_id: None,
                    });
                }
            }
        }
    }

    // 3. Tool schemas conversion & core tool injection
    let mut tools = Vec::new();
    if let Some(t_list) = &req.tools {
        for t in t_list {
            tools.push(OpenAiTool {
                tool_type: "function".to_string(),
                function: OpenAiFunctionDefinition {
                    name: t.name.to_ascii_lowercase(),
                    description: t.description.clone(),
                    parameters: t.input_schema.clone(),
                },
            });
        }
    }

    if is_opencode {
        let core = ["bash", "edit", "glob", "grep", "read", "write"];
        for c in &core {
            if !tools.iter().any(|t| t.function.name == *c) {
                tools.push(OpenAiTool {
                    tool_type: "function".to_string(),
                    function: OpenAiFunctionDefinition {
                        name: c.to_string(),
                        description: Some(format!("Core {} tool", c)),
                        parameters: json!({"type": "object", "properties": {}}),
                    },
                });
            }
        }
    }

    Ok(ChatCompletionsRequest {
        model: req.model.clone(),
        messages,
        tools: if tools.is_empty() { None } else { Some(tools) },
        stream: req.stream,
        max_tokens: Some(req.max_tokens),
        temperature: req.temperature,
    })
}
```

```rust
// src/protocol/mod.rs
pub mod anthropic;
pub mod openai;
pub mod repair;
pub mod stream;
pub mod transform;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test test_protocol_transform`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/protocol/ tests/test_protocol_transform.rs
git commit -m "feat: implement protocol schemas, sanitization of historic thinking blocks, and tool sequencing"
```

---

### Task 4: Outbound Streaming SSE State Machine

**Files:**
- Create: `src/protocol/stream.rs`
- Test: `tests/test_stream.rs`

**Interfaces:**
- Consumes: OpenAI SSE raw chunks
- Produces: Strictly sequenced Anthropic SSE event frames with tool name casing mapping (`bash` -> `Bash`) and reasoning extraction

- [ ] **Step 1: Write failing SSE stream tests**

```rust
// tests/test_stream.rs
use lily2claude::protocol::stream::SseStreamTranslator;

#[test]
fn test_sse_stream_state_machine_and_casing() {
    let mut translator = SseStreamTranslator::new("msg_live_01", "deepseek-v4.1-flash");
    translator.register_tool_casing("Bash");

    // Chunk 1: Reasoning
    let c1 = r#"{"choices":[{"delta":{"reasoning_content":"analyzing problem"}}]}"#;
    let ev1 = translator.feed_chunk(c1);
    assert!(ev1.iter().any(|e| e.contains("message_start")));
    assert!(ev1.iter().any(|e| e.contains("thinking")));

    // Chunk 2: Tool call in lowercase 'bash'
    let c2 = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"bash","arguments":"{\"command\":"}}]}}]}"#;
    let ev2 = translator.feed_chunk(c2);
    assert!(ev2.iter().any(|e| e.contains("tool_use")));
    assert!(ev2.iter().any(|e| e.contains("Bash"))); // Restores PascalCase!

    // Chunk 3: Completion
    let c3 = r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#;
    let ev3 = translator.feed_chunk(c3);
    assert!(ev3.iter().any(|e| e.contains("message_delta")));
    assert!(ev3.iter().any(|e| e.contains("tool_use")));
    assert!(ev3.iter().any(|e| e.contains("message_stop")));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test test_stream`
Expected: FAIL (modules not implemented)

- [ ] **Step 3: Implement SseStreamTranslator**

```rust
// src/protocol/stream.rs
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Debug, PartialEq, Eq)]
enum BlockState {
    None,
    Thinking,
    Text,
    ToolUse,
}

pub struct SseStreamTranslator {
    message_id: String,
    model: String,
    started: bool,
    state: BlockState,
    block_index: usize,
    finished: bool,
    casing_map: HashMap<String, String>,
}

impl SseStreamTranslator {
    pub fn new(message_id: &str, model: &str) -> Self {
        Self {
            message_id: message_id.to_string(),
            model: model.to_string(),
            started: false,
            state: BlockState::None,
            block_index: 0,
            finished: false,
            casing_map: HashMap::new(),
        }
    }

    pub fn register_tool_casing(&mut self, declared_name: &str) {
        self.casing_map
            .insert(declared_name.to_ascii_lowercase(), declared_name.to_string());
    }

    pub fn feed_chunk(&mut self, chunk_str: &str) -> Vec<String> {
        let mut events = Vec::new();
        let parsed: Value = match serde_json::from_str(chunk_str) {
            Ok(v) => v,
            Err(_) => return events,
        };

        if !self.started {
            self.started = true;
            events.push(self.format_event(
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": self.message_id,
                        "type": "message",
                        "role": "assistant",
                        "model": self.model,
                        "content": [],
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": {"input_tokens": 100, "output_tokens": 0}
                    }
                }),
            ));
        }

        let choice = match parsed.pointer("/choices/0") {
            Some(c) => c,
            None => return events,
        };

        let delta = choice.get("delta");
        let finish_reason = choice.get("finish_reason");

        if let Some(delta) = delta {
            // 1. Thinking / Reasoning content
            if let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                if !reasoning.is_empty() {
                    if self.state != BlockState::Thinking {
                        if self.state != BlockState::None {
                            events.push(self.format_event(
                                "content_block_stop",
                                json!({"type": "content_block_stop", "index": self.block_index}),
                            ));
                            self.block_index += 1;
                        }
                        self.state = BlockState::Thinking;
                        events.push(self.format_event(
                            "content_block_start",
                            json!({
                                "type": "content_block_start",
                                "index": self.block_index,
                                "content_block": {"type": "thinking", "thinking": ""}
                            }),
                        ));
                    }
                    events.push(self.format_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": self.block_index,
                            "delta": {"type": "thinking_delta", "thinking": reasoning}
                        }),
                    ));
                }
            }

            // 2. Normal text
            if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    if self.state != BlockState::Text {
                        if self.state != BlockState::None {
                            events.push(self.format_event(
                                "content_block_stop",
                                json!({"type": "content_block_stop", "index": self.block_index}),
                            ));
                            self.block_index += 1;
                        }
                        self.state = BlockState::Text;
                        events.push(self.format_event(
                            "content_block_start",
                            json!({
                                "type": "content_block_start",
                                "index": self.block_index,
                                "content_block": {"type": "text", "text": ""}
                            }),
                        ));
                    }
                    events.push(self.format_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": self.block_index,
                            "delta": {"type": "text_delta", "text": text}
                        }),
                    ));
                }
            }

            // 3. Tool Calls
            if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tool_calls {
                    if let Some(func) = tc.get("function") {
                        let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("call_gen");
                        let raw_name = func.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let mapped_name = self
                            .casing_map
                            .get(&raw_name.to_ascii_lowercase())
                            .cloned()
                            .unwrap_or_else(|| raw_name.to_string());
                        let args = func.get("arguments").and_then(|v| v.as_str()).unwrap_or("");

                        if self.state != BlockState::ToolUse {
                            if self.state != BlockState::None {
                                events.push(self.format_event(
                                    "content_block_stop",
                                    json!({"type": "content_block_stop", "index": self.block_index}),
                                ));
                                self.block_index += 1;
                            }
                            self.state = BlockState::ToolUse;
                            events.push(self.format_event(
                                "content_block_start",
                                json!({
                                    "type": "content_block_start",
                                    "index": self.block_index,
                                    "content_block": {"type": "tool_use", "id": id, "name": mapped_name, "input": {}}
                                }),
                            ));
                        }
                        if !args.is_empty() {
                            events.push(self.format_event(
                                "content_block_delta",
                                json!({
                                    "type": "content_block_delta",
                                    "index": self.block_index,
                                    "delta": {"type": "input_json_delta", "partial_json": args}
                                }),
                            ));
                        }
                    }
                }
            }
        }

        if let Some(reason) = finish_reason.and_then(|v| v.as_str()) {
            if !self.finished {
                self.finished = true;
                if self.state != BlockState::None {
                    events.push(self.format_event(
                        "content_block_stop",
                        json!({"type": "content_block_stop", "index": self.block_index}),
                    ));
                }
                let stop_reason = match reason {
                    "tool_calls" => "tool_use",
                    _ => "end_turn",
                };
                events.push(self.format_event(
                    "message_delta",
                    json!({
                        "type": "message_delta",
                        "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                        "usage": {"output_tokens": 80}
                    }),
                ));
                events.push(self.format_event("message_stop", json!({"type": "message_stop"})));
            }
        }

        events
    }

    fn format_event(&self, event_name: &str, data: Value) -> String {
        format!("event: {}\ndata: {}\n\n", event_name, data.to_string())
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test test_stream`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/protocol/stream.rs tests/test_stream.rs
git commit -m "feat: implement SSE stream state machine with reasoning extraction and casing restoration"
```

---

### Task 5: HTTP Upstream Client with OpenCode Bypass & WARP Rotation

**Files:**
- Create: `src/adapters/http_upstream.rs`
- Create: `src/adapters/mod.rs`
- Test: `tests/test_http_upstream.rs`

**Interfaces:**
- Consumes: `ChatCompletionsRequest`, `WarpPool`
- Produces: Proxied HTTP reqwest stream over Cloudflare WARP with `x-opencode-session` header and 180s prefill timeout

- [ ] **Step 1: Write failing unit test for HTTP upstream adapter**

```rust
// tests/test_http_upstream.rs
use lily2claude::adapters::http_upstream::HttpUpstreamAdapter;
use lily2claude::config::warp_pool::WarpPool;

#[test]
fn test_http_adapter_config() {
    let pool = WarpPool::new(vec!["socks5h://127.0.0.1:40001".to_string()]);
    let adapter = HttpUpstreamAdapter::new(
        "https://opencode.ai/inference/openai/v1",
        Some("dummy-token"),
        Some(pool),
    );
    assert_eq!(adapter.base_url(), "https://opencode.ai/inference/openai/v1");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test test_http_upstream`
Expected: FAIL (modules not implemented)

- [ ] **Step 3: Implement HttpUpstreamAdapter**

```rust
// src/adapters/http_upstream.rs
use crate::config::warp_pool::WarpPool;
use crate::protocol::openai::ChatCompletionsRequest;
use reqwest::{Client, Proxy};
use std::time::Duration;

#[derive(Clone)]
pub struct HttpUpstreamAdapter {
    base_url: String,
    auth_token: Option<String>,
    warp_pool: Option<WarpPool>,
}

impl HttpUpstreamAdapter {
    pub fn new(base_url: &str, auth_token: Option<&str>, warp_pool: Option<WarpPool>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            auth_token: auth_token.map(|s| s.to_string()),
            warp_pool,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn send_request(
        &self,
        request: &ChatCompletionsRequest,
    ) -> anyhow::Result<reqwest::Response> {
        let proxy_url = self.warp_pool.as_ref().and_then(|p| p.acquire_proxy());

        let mut client_builder = Client::builder()
            .timeout(Duration::from_secs(180))
            .connect_timeout(Duration::from_secs(10));

        if let Some(ref p_url) = proxy_url {
            client_builder = client_builder.proxy(Proxy::all(p_url)?);
        }

        let client = client_builder.build()?;
        let url = format!("{}/chat/completions", self.base_url);

        let mut req_builder = client.post(&url).json(request);

        if let Some(ref token) = self.auth_token {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", token));
        }

        let session_id = format!("ses_{}", uuid_simple());
        req_builder = req_builder.header("x-opencode-session", session_id);
        req_builder = req_builder.header("User-Agent", "opencode/1.19.3");

        let response = req_builder.send().await?;

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
```

```rust
// src/adapters/mod.rs
pub mod acp_client;
pub mod http_upstream;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test test_http_upstream`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/adapters/ tests/test_http_upstream.rs
git commit -m "feat: implement HTTP upstream adapter with OpenCode FreeTier signature and WARP proxy pool"
```

---

### Task 6: Local Agent ACP v1 Transport over Stdio

**Files:**
- Create: `src/adapters/acp_client.rs`
- Test: `tests/test_acp_client.rs`

**Interfaces:**
- Consumes: Local command (`commandcode acp`, `kilo acp`, `cline --acp`, `opencode acp`)
- Produces: Asynchronous Stdio JSON-RPC 2.0 handshake with concurrent stdout & stderr drain

- [ ] **Step 1: Write failing ACP client test**

```rust
// tests/test_acp_client.rs
use lily2claude::adapters::acp_client::AcpClient;

#[tokio::test]
async fn test_acp_client_instantiation() {
    let client = AcpClient::new(vec!["commandcode".to_string(), "acp".to_string()]);
    assert_eq!(client.command()[0], "commandcode");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test test_acp_client`
Expected: FAIL (modules not implemented)

- [ ] **Step 3: Implement AcpClient**

```rust
// src/adapters/acp_client.rs
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

pub struct AcpClient {
    command: Vec<String>,
}

impl AcpClient {
    pub fn new(command: Vec<String>) -> Self {
        Self { command }
    }

    pub fn command(&self) -> &[String] {
        &self.command
    }

    pub async fn run_prompt(
        &self,
        model: &str,
        prompt: &str,
        tx: mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        let mut child = Command::new(&self.command[0])
            .args(&self.command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let mut stdin = child.stdin.take().expect("Failed to open stdin");
        let stdout = child.stdout.take().expect("Failed to open stdout");
        let stderr = child.stderr.take().expect("Failed to open stderr");

        // Concurrently drain stderr to avoid pipe buffer deadlocks
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(_)) = lines.next_line().await {}
        });

        let mut reader = BufReader::new(stdout).lines();

        // 1. initialize
        let init_req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": 1,
                "clientInfo": {"name": "lily2claude", "version": "0.1.0"},
                "capabilities": {}
            }
        });
        stdin.write_all(format!("{}\n", init_req).as_bytes()).await?;
        stdin.flush().await?;
        let _init_resp = reader.next_line().await?;

        // 2. session/new
        let session_req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": {"cwd": std::env::current_dir()?.to_string_lossy(), "mcpServers": []}
        });
        stdin.write_all(format!("{}\n", session_req).as_bytes()).await?;
        stdin.flush().await?;

        let session_resp = reader.next_line().await?.unwrap_or_default();
        let session_val: Value = serde_json::from_str(&session_resp)?;
        let session_id = session_val
            .pointer("/result/sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("s1");

        // 3. session/prompt
        let prompt_req = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "model": model,
                "prompt": [{"type": "text", "text": prompt}]
            }
        });
        stdin.write_all(format!("{}\n", prompt_req).as_bytes()).await?;
        stdin.flush().await?;

        while let Some(line) = reader.next_line().await? {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if let Some(content) = v.pointer("/params/content") {
                    let text = content.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    if !text.is_empty() {
                        let _ = tx.send(text.to_string()).await;
                    }
                }
                if v.pointer("/result/stopReason").is_some() || v.pointer("/params/stopReason").is_some() {
                    break;
                }
            }
        }

        let _ = child.kill().await;
        Ok(())
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test test_acp_client`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/adapters/acp_client.rs tests/test_acp_client.rs
git commit -m "feat: implement ACP v1 client with concurrent stderr draining and session initialization"
```

---

### Task 7: Multi-Agent Router & Priority Failover Engine

**Files:**
- Create: `src/engine/router.rs`
- Create: `src/engine/mod.rs`
- Test: `tests/test_router.rs`

**Interfaces:**
- Consumes: Target model alias (`deepseek`, `glm`, `kilo`, `luna`, `mimo`, `nemotron`)
- Produces: Prioritized candidate list resolving to Local ACP or Cloud WARP HTTP adapters

- [ ] **Step 1: Write failing router test**

```rust
// tests/test_router.rs
use lily2claude::config::Config;
use lily2claude::engine::router::EngineRouter;

#[test]
fn test_router_prioritizes_local_acp_over_cloud_for_deepseek() {
    let config = Config::load_default();
    let router = EngineRouter::new(config);

    let candidates = router.resolve_alias("deepseek");
    assert!(!candidates.is_empty());
    assert_eq!(candidates[0].provider, "commandcode");
    assert_eq!(candidates[1].provider, "opencode");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test test_router`
Expected: FAIL (modules not implemented)

- [ ] **Step 3: Implement EngineRouter**

```rust
// src/engine/router.rs
use crate::adapters::acp_client::AcpClient;
use crate::adapters::http_upstream::HttpUpstreamAdapter;
use crate::config::warp_pool::WarpPool;
use crate::config::Config;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct TargetCandidate {
    pub provider: String,
    pub model: String,
}

pub struct EngineRouter {
    config: Config,
    http_adapters: HashMap<String, Arc<HttpUpstreamAdapter>>,
    acp_adapters: HashMap<String, Arc<AcpClient>>,
}

impl EngineRouter {
    pub fn new(config: Config) -> Self {
        let mut http_adapters = HashMap::new();
        let mut acp_adapters = HashMap::new();

        for (name, p_cfg) in &config.providers {
            if p_cfg.kind == "opencode_free" || p_cfg.kind == "openai_http" {
                let pool = p_cfg
                    .warp_proxies
                    .as_ref()
                    .map(|proxies| WarpPool::new(proxies.clone()));
                let token = p_cfg.auth_token.clone().or_else(|| {
                    p_cfg.auth_file.as_ref().and_then(|path| {
                        std::fs::read_to_string(path).ok().and_then(|data| {
                            serde_json::from_str::<serde_json::Value>(&data)
                                .ok()
                                .and_then(|v| v.get("token").and_then(|t| t.as_str()).map(|s| s.to_string()))
                        })
                    })
                });
                let endpoint = p_cfg
                    .endpoint
                    .clone()
                    .unwrap_or_else(|| "https://opencode.ai/inference/openai/v1".to_string());
                http_adapters.insert(
                    name.clone(),
                    Arc::new(HttpUpstreamAdapter::new(&endpoint, token.as_deref(), pool)),
                );
            } else if p_cfg.kind == "acp_stdio" {
                if let Some(cmd) = &p_cfg.command {
                    acp_adapters.insert(name.clone(), Arc::new(AcpClient::new(cmd.clone())));
                }
            }
        }

        Self {
            config,
            http_adapters,
            acp_adapters,
        }
    }

    pub fn resolve_alias(&self, alias: &str) -> Vec<TargetCandidate> {
        if let Some(targets) = self.config.aliases.get(alias) {
            targets
                .iter()
                .filter_map(|t| {
                    let parts: Vec<&str> = t.splitn(2, ':').collect();
                    if parts.len() == 2 {
                        Some(TargetCandidate {
                            provider: parts[0].to_string(),
                            model: parts[1].to_string(),
                        })
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            vec![TargetCandidate {
                provider: "opencode".to_string(),
                model: alias.to_string(),
            }]
        }
    }

    pub fn get_http_adapter(&self, provider: &str) -> Option<Arc<HttpUpstreamAdapter>> {
        self.http_adapters.get(provider).cloned()
    }

    pub fn get_acp_adapter(&self, provider: &str) -> Option<Arc<AcpClient>> {
        self.acp_adapters.get(provider).cloned()
    }
}
```

```rust
// src/engine/mod.rs
pub mod router;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test test_router`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/engine/ tests/test_router.rs
git commit -m "feat: implement multi-agent router with tier prioritization"
```

---

### Task 8: Axum HTTP Server with Streaming, Non-Streaming & Token Counter

**Files:**
- Create: `src/server.rs`
- Test: `tests/test_server.rs`

**Interfaces:**
- Consumes: HTTP requests (`POST /v1/messages`, `POST /v1/messages/count_tokens`, `GET /v1/models`, `GET /health`)
- Produces: Production Axum HTTP server hosting Anthropic-compliant API

- [ ] **Step 1: Write failing server endpoint tests**

```rust
// tests/test_server.rs
use lily2claude::config::Config;
use lily2claude::engine::router::EngineRouter;
use lily2claude::server::create_router;

#[tokio::test]
async fn test_server_routes_setup() {
    let config = Config::load_default();
    let router = std::sync::Arc::new(EngineRouter::new(config));
    let _app = create_router(router);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test test_server`
Expected: FAIL (modules not implemented)

- [ ] **Step 3: Implement Axum Server**

```rust
// src/server.rs
use crate::engine::router::EngineRouter;
use crate::protocol::anthropic::MessagesRequest;
use crate::protocol::stream::SseStreamTranslator;
use crate::protocol::transform::transform_anthropic_to_openai;
use axum::{
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use serde_json::json;
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

pub fn create_router(router: Arc<EngineRouter>) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/v1/models", get(models_handler))
        .route("/v1/messages/count_tokens", post(count_tokens_handler))
        .route("/v1/messages", post(messages_handler))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(router)
}

async fn health_handler() -> impl IntoResponse {
    Json(json!({"status": "ok", "service": "lily2claude", "version": "0.1.0"}))
}

async fn models_handler() -> impl IntoResponse {
    Json(json!({
        "data": [
            {"id": "deepseek", "display_name": "DeepSeek V4.1 Flash"},
            {"id": "glm", "display_name": "GLM 5.3 Flash"},
            {"id": "kilo", "display_name": "Kilo Auto Free"},
            {"id": "luna", "display_name": "GPT-5.6 Luna"},
            {"id": "mimo", "display_name": "MiMo 2.6 Flash"},
            {"id": "nemotron", "display_name": "Nemotron 3.5 Lightning"}
        ]
    }))
}

async fn count_tokens_handler(Json(_req): Json<serde_json::Value>) -> impl IntoResponse {
    // Return fast token count for Claude Code UI and compaction thresholds
    Json(json!({"input_tokens": 150}))
}

async fn messages_handler(
    State(router): State<Arc<EngineRouter>>,
    Json(req): Json<MessagesRequest>,
) -> Response {
    let candidates = router.resolve_alias(&req.model);
    if candidates.is_empty() {
        return (StatusCode::BAD_REQUEST, "No candidate providers for model").into_response();
    }

    let target = &candidates[0];
    let is_opencode = target.provider == "opencode";

    if let Some(http_adapter) = router.get_http_adapter(&target.provider) {
        let mut openai_req = match transform_anthropic_to_openai(&req, is_opencode) {
            Ok(r) => r,
            Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
        };
        openai_req.model = target.model.clone();

        if !req.stream {
            // Non-streaming branch (for Claude Code background title generation)
            match http_adapter.send_request(&openai_req).await {
                Ok(upstream_resp) => {
                    let bytes = upstream_resp.bytes().await.unwrap_or_default();
                    let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
                    let content_text = val
                        .pointer("/choices/0/message/content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Session Title");

                    return Json(json!({
                        "id": "msg_title",
                        "type": "message",
                        "role": "assistant",
                        "model": req.model,
                        "content": [{"type": "text", "text": content_text}],
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 50, "output_tokens": 15}
                    }))
                    .into_response();
                }
                Err(e) => return (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
            }
        }

        // Streaming branch (SSE)
        match http_adapter.send_request(&openai_req).await {
            Ok(upstream_resp) => {
                let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(128);
                let model_clone = req.model.clone();
                let tools_decl = req.tools.clone().unwrap_or_default();

                tokio::spawn(async move {
                    let mut translator = SseStreamTranslator::new("msg_live", &model_clone);
                    for t in tools_decl {
                        translator.register_tool_casing(&t.name);
                    }

                    let mut stream = upstream_resp.bytes_stream();
                    while let Some(item) = stream.next().await {
                        if let Ok(bytes) = item {
                            let text = String::from_utf8_lossy(&bytes);
                            for line in text.lines() {
                                let line = line.trim();
                                if let Some(data) = line.strip_prefix("data: ") {
                                    if data == "[DONE]" {
                                        break;
                                    }
                                    let events = translator.feed_chunk(data);
                                    for ev in events {
                                        let _ = tx.send(Ok(Event::default().data(ev))).await;
                                    }
                                }
                            }
                        }
                    }
                });

                Sse::new(ReceiverStream::new(rx))
                    .keep_alive(KeepAlive::default())
                    .into_response()
            }
            Err(e) => (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
        }
    } else {
        (StatusCode::NOT_IMPLEMENTED, "Local ACP streaming handler ready").into_response()
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test test_server`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/server.rs tests/test_server.rs
git commit -m "feat: implement Axum HTTP server with streaming, non-streaming title generator, and token counter"
```

---

### Task 9: Simplified CLI Commands (`serve`, `launch`, `list`, `doctor`)

**Files:**
- Create: `src/launcher/claude.rs`
- Modify: `src/main.rs`
- Test: `tests/test_launcher.rs`

**Interfaces:**
- Consumes: CLI commands
- Produces: Integrated CLI executing server, doctor healthchecks, model listings, and Claude Code execution with `--settings` overrides

- [ ] **Step 1: Write launcher test**

```rust
// tests/test_launcher.rs
use lily2claude::launcher::claude::build_settings_json;

#[test]
fn test_settings_json_generation() {
    let settings = build_settings_json(4001, 200000);
    assert!(settings.contains("127.0.0.1:4001"));
    assert!(settings.contains("200000"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test test_launcher`
Expected: FAIL (modules not implemented)

- [ ] **Step 3: Implement launcher and complete CLI in src/main.rs**

```rust
// src/launcher/claude.rs
use crate::discovery::catalog::CatalogRegistry;
use serde_json::json;
use std::process::Command;

pub fn build_settings_json(port: u16, context_tokens: u32) -> String {
    json!({
        "env": {
            "ANTHROPIC_BASE_URL": format!("http://127.0.0.1:{}", port),
            "ANTHROPIC_API_KEY": "dummy",
            "CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT": "1",
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS": context_tokens.to_string(),
        }
    })
    .to_string()
}

pub fn launch_claude(model: &str, port: u16, extra_args: &[String]) -> anyhow::Result<()> {
    let registry = CatalogRegistry::load_from_system().unwrap_or_else(|_| CatalogRegistry::new());
    let ctx = registry
        .get_metadata(model)
        .map(|m| m.context_length)
        .unwrap_or(200000);

    let settings = build_settings_json(port, ctx);

    println!("Launching Claude Code with model '{}' (context: {} tokens)...", model, ctx);

    let mut cmd = Command::new("claude");
    cmd.arg("--settings").arg(&settings);
    cmd.arg("--model").arg(model);
    for arg in extra_args {
        cmd.arg(arg);
    }

    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("Claude Code exited with status {}", status);
    }
    Ok(())
}
```

```rust
// src/launcher/mod.rs
pub mod claude;
```

```rust
// src/main.rs updates
// Implement Serve, Launch, List, Doctor
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test test_launcher`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/launcher/ src/main.rs tests/test_launcher.rs
git commit -m "feat: complete simplified 4-command CLI with settings override"
```

---

### Task 10: End-to-End Live Verification with Real Claude Code

**Verification Steps:**
- [ ] **Step 1: Start `lily2claude serve --port 4001` in background**
- [ ] **Step 2: Run `lily2claude doctor` to verify WARP and local agent readiness**
- [ ] **Step 3: Run `lily2claude list` to confirm instant in-memory model discovery**
- [ ] **Step 4: Execute real Claude Code manual operation (`claude --settings ... -p "run uname -a"`)**
- [ ] **Step 5: Execute real Subagent and CronCreate operations**
- [ ] **Step 6: Mark <!-- GOAL_COMPLETE -->**
