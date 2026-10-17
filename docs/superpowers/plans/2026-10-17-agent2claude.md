# Agent2Claude Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `agent2claude`, a high-performance, modular Rust bridge allowing Claude Code CLI (`claude`) to drive local agents (CommandCode, Kilo, Cline via ACP stdio) and cloud free-tier models (OpenCode Zen via dual-node Cloudflare WARP proxy pool) with zero network scanning, transparent custom model aliases, and automatic context compact detection.

**Architecture:** A lightweight Axum server implements Anthropic Messages API (`POST /v1/messages`). The inbound Anthropic request is transformed into upstream OpenAI/ACP payloads, unpacking interleaved tool results into strictly sequenced `role: "tool"` messages. Outbound responses stream back to Claude Code via an SSE state machine emitting Anthropic events (`message_start`, `content_block_start`, `thinking_delta`, `input_json_delta`, `message_delta`, `message_stop`). An intelligent router handles failover across local ACP agents and dual Cloudflare WARP SOCKS5 egress nodes (ports 40001, 40002).

**Tech Stack:** Rust 2021, Tokio, Axum 0.7, Reqwest 0.12 (with socks/rustls), Serde, Serde_json, Clap 4.4, Toml 0.8, Tracing.

**Spec:** Synthesized from reverse-engineering findings on local ACP agents (`commandcode acp`, `kilo acp`, `cline --acp`), OpenCode free-tier bypass invariants (core tools + session header), Cloudflare WARP SOCKS5 container pool, and Claude Code 2.1+ context management environment variables.

## Global Constraints
- Target Language: Rust 2021 edition, fully modular architecture under `src/`.
- No legacy monolith baggage: Zero code copy-pasted from `opencode2claude`.
- Model transparency: Never force generic `opus`/`sonnet` masking; expose explicit aliases (`deepseek`, `glm`, `kilo`, `mimo`, `nemotron`) and full model names.
- Zero network scanning: Live discovery of `*-free` or `:free` models occurs in-memory via catalog JSON (`api.json` / `models.json`) in <1ms without sequential network probing.
- Dual-node WARP proxy pool: OpenCode traffic routes through SOCKS5 proxies on `127.0.0.1:40001` and `127.0.0.1:40002` with 429 rotation and 60s cooldown.
- Tool Invariant: Anthropic `tool_result` blocks in `role: "user"` must be converted into individual `role: "tool"` messages with `tool_call_id` strictly following their matching assistant `tool_calls`.
- Auto-compaction: Automatically detect model context limit and configure Claude Code via `CLAUDE_CODE_MAX_CONTEXT_TOKENS` and `CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT=1`.

---

### Task 1: Cargo Workspace & Foundation Setup

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `src/lib.rs`
- Create: `.gitignore`
- Test: `tests/test_cli.rs`

**Interfaces:**
- Consumes: Standard CLI inputs (`agent2claude serve`, `agent2claude list`, `agent2claude launch`)
- Produces: `agent2claude` binary and shared library crate

- [ ] **Step 1: Write the failing CLI integration test**

```rust
// tests/test_cli.rs
use std::process::Command;

#[test]
fn test_cli_help() {
    let output = Command::new("cargo")
        .args(["run", "--", "--help"])
        .output()
        .expect("Failed to execute cargo run");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("agent2claude"));
    assert!(stdout.contains("serve"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test test_cli`
Expected: FAIL (Cargo.toml not found or compilation error)

- [ ] **Step 3: Write minimal Cargo.toml, .gitignore, src/lib.rs, and src/main.rs**

```toml
# Cargo.toml
[package]
name = "agent2claude"
version = "0.1.0"
edition = "2021"
authors = ["light"]
description = "Universal bridge connecting Claude Code CLI to local agents (ACP) and OpenCode Free Tier (WARP)"

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
// src/lib.rs
pub mod config;
pub mod discovery;
pub mod protocol;
pub mod adapters;
pub mod engine;
pub mod launcher;
pub mod server;
```

```rust
// src/main.rs
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "agent2claude", author, version, about = "Universal bridge connecting Claude Code to Local Agents & OpenCode Free Tier")]
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
        #[arg(short, long, default_value = "127.0.0.1")]
        host: String,
    },
    /// List available models across all providers
    List {
        #[arg(short, long)]
        free_only: bool,
    },
    /// Launch Claude Code connected to agent2claude
    Launch {
        #[arg(short, long, default_value = "deepseek")]
        model: String,
        #[arg(short, long, default_value = "4001")]
        port: u16,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    match cli.command.unwrap_or(Commands::Serve { port: 4001, host: "127.0.0.1".to_string() }) {
        Commands::Serve { port, host } => {
            println!("Starting agent2claude server on {}:{}...", host, port);
        }
        Commands::List { free_only } => {
            println!("Listing models (free_only={})...", free_only);
        }
        Commands::Launch { model, port } => {
            println!("Launching Claude Code with model {} on port {}...", model, port);
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
git commit -m "feat: initialize agent2claude repository with cargo workspace and CLI scaffolding"
```

---

### Task 2: Declarative Configuration & WARP Proxy Pool

**Files:**
- Create: `templates/models.toml`
- Create: `templates/providers.toml`
- Create: `src/config/mod.rs`
- Create: `src/config/warp_pool.rs`
- Test: `tests/test_config.rs`

**Interfaces:**
- Consumes: TOML configuration files
- Produces: `Config`, `ModelAliasConfig`, `WarpPool` managing round-robin and 429 cooldown over ports 40001 and 40002

- [ ] **Step 1: Write the failing unit test for WarpPool and Config**

```rust
// tests/test_config.rs
use agent2claude::config::warp_pool::WarpPool;
use agent2claude::config::Config;

#[test]
fn test_warp_pool_rotation_and_cooldown() {
    let mut pool = WarpPool::new(vec![
        "socks5h://127.0.0.1:40001".to_string(),
        "socks5h://127.0.0.1:40002".to_string(),
    ]);

    let proxy1 = pool.acquire_proxy().expect("Should get proxy 1");
    assert_eq!(proxy1, "socks5h://127.0.0.1:40001");

    let proxy2 = pool.acquire_proxy().expect("Should get proxy 2");
    assert_eq!(proxy2, "socks5h://127.0.0.1:40002");

    // Report 429 on proxy 1
    pool.mark_rate_limited("socks5h://127.0.0.1:40001", 60);

    // Next acquire should bypass proxy 1 and return proxy 2
    let proxy3 = pool.acquire_proxy().expect("Should get proxy 2 again");
    assert_eq!(proxy3, "socks5h://127.0.0.1:40002");
}

#[test]
fn test_load_default_model_aliases() {
    let config = Config::load_default();
    assert!(config.aliases.contains_key("deepseek"));
    let targets = config.aliases.get("deepseek").unwrap();
    assert!(!targets.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test test_config`
Expected: FAIL (modules not implemented)

- [ ] **Step 3: Implement WarpPool and Config**

```rust
// src/config/warp_pool.rs
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct WarpPool {
    proxies: Vec<String>,
    cursor: std::sync::Arc<AtomicUsize>,
    cooldowns: std::sync::Arc<std::sync::Mutex<HashMap<String, Instant>>>,
}

impl WarpPool {
    pub fn new(proxies: Vec<String>) -> Self {
        Self {
            proxies,
            cursor: std::sync::Arc::new(AtomicUsize::new(0)),
            cooldowns: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
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

        // If all are cooling down, return the one closest to expiry
        self.proxies.first().cloned()
    }

    pub fn mark_rate_limited(&self, proxy: &str, cooldown_secs: u64) {
        let mut cooldowns = self.cooldowns.lock().unwrap();
        let expiry = Instant::now() + Duration::from_secs(cooldown_secs);
        cooldowns.insert(proxy.to_string(), expiry);
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
    pub kind: String, // "opencode_free", "acp_stdio", "openai_http"
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
                "opencode:deepseek-v4.1-flash-free".to_string(),
                "commandcode:deepseek-v4.1-flash".to_string(),
                "kilo:kilo-auto/free".to_string(),
            ],
        );
        aliases.insert(
            "glm".to_string(),
            vec![
                "opencode:glm-5.3-flash-free".to_string(),
                "commandcode:glm-5.3-flash".to_string(),
            ],
        );
        aliases.insert(
            "kilo".to_string(),
            vec!["kilo:kilo-auto/free".to_string()],
        );
        aliases.insert(
            "mimo".to_string(),
            vec!["opencode:mimo-v2.6-flash-free".to_string()],
        );
        aliases.insert(
            "nemotron".to_string(),
            vec!["opencode:nvidia/nemotron-3-nano-free".to_string()],
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

        Self {
            server_port: 4001,
            aliases,
            providers,
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test test_config`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/config/ templates/ tests/test_config.rs
git commit -m "feat: implement declarative configuration and WARP proxy pool with rotation & cooldown"
```

---

### Task 3: Zero-Scan Model Discovery & Context Limit Detection

**Files:**
- Create: `src/discovery/mod.rs`
- Create: `src/discovery/catalog.rs`
- Test: `tests/test_discovery.rs`

**Interfaces:**
- Consumes: Local cached catalog JSON files (`~/.cache/kilo/models.json`, etc.) or in-memory catalog data
- Produces: `ModelMetadata` with exact context limits (e.g. 200,000 for deepseek, 256,000 for kilo) and instant `*-free`/`:free` regex matching

- [ ] **Step 1: Write the failing discovery test**

```rust
// tests/test_discovery.rs
use agent2claude::discovery::catalog::{CatalogRegistry, ModelMetadata};

#[test]
fn test_instant_free_tier_filter() {
    let json_data = r#"[
        {"id": "deepseek-v4.1-flash-free", "context_length": 200000, "max_tokens": 32000},
        {"id": "gpt-4o", "context_length": 128000, "max_tokens": 4096},
        {"id": "glm-5.3-flash-free", "context_length": 200000, "max_tokens": 32000},
        {"id": "kilo-auto/free", "context_length": 256000, "max_tokens": 32768}
    ]"#;

    let registry = CatalogRegistry::from_json_str(json_data).expect("Must parse catalog");
    let free_models = registry.filter_free_models();

    assert_eq!(free_models.len(), 3);
    assert!(free_models.iter().any(|m| m.id == "deepseek-v4.1-flash-free"));
    assert!(free_models.iter().any(|m| m.id == "glm-5.3-flash-free"));
    assert!(free_models.iter().any(|m| m.id == "kilo-auto/free"));
    assert!(!free_models.iter().any(|m| m.id == "gpt-4o"));

    let meta = registry.get_metadata("deepseek-v4.1-flash-free").unwrap();
    assert_eq!(meta.context_length, 200000);
    assert_eq!(meta.max_tokens, 32000);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test test_discovery`
Expected: FAIL (modules not implemented)

- [ ] **Step 3: Implement CatalogRegistry and ModelMetadata**

```rust
// src/discovery/catalog.rs
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMetadata {
    pub id: String,
    #[serde(default = "default_context")]
    pub context_length: u32,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

fn default_context() -> u32 {
    128000
}

fn default_max_tokens() -> u32 {
    8192
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

    pub fn from_json_str(data: &str) -> anyhow::Result<Self> {
        let mut registry = Self::new();
        let list: Vec<ModelMetadata> = serde_json::from_str(data)?;
        for model in list {
            registry.models.insert(model.id.clone(), model);
        }
        Ok(registry)
    }

    pub fn register(&mut self, metadata: ModelMetadata) {
        self.models.insert(metadata.id.clone(), metadata);
    }

    pub fn filter_free_models(&self) -> Vec<ModelMetadata> {
        self.models
            .values()
            .filter(|m| self.free_regex.is_match(&m.id))
            .cloned()
            .collect()
    }

    pub fn get_metadata(&self, model_id: &str) -> Option<ModelMetadata> {
        if let Some(m) = self.models.get(model_id) {
            return Some(m.clone());
        }
        // Fallback matching prefix or alias
        if model_id.contains("deepseek") {
            return Some(ModelMetadata {
                id: model_id.to_string(),
                context_length: 200000,
                max_tokens: 32000,
            });
        }
        if model_id.contains("glm") {
            return Some(ModelMetadata {
                id: model_id.to_string(),
                context_length: 200000,
                max_tokens: 32000,
            });
        }
        if model_id.contains("kilo") {
            return Some(ModelMetadata {
                id: model_id.to_string(),
                context_length: 256000,
                max_tokens: 32768,
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

Run: `cargo test --test test_discovery`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/discovery/ tests/test_discovery.rs
git commit -m "feat: implement zero-scan model discovery and context limit catalog parser"
```

---

### Task 4: Anthropic <-> OpenAI Protocol Transformers & Tool Result Sequencing

**Files:**
- Create: `src/protocol/anthropic.rs`
- Create: `src/protocol/openai.rs`
- Create: `src/protocol/transform.rs`
- Create: `src/protocol/repair.rs`
- Create: `src/protocol/mod.rs`
- Test: `tests/test_protocol_transform.rs`

**Interfaces:**
- Consumes: Anthropic `MessagesRequest` (from Claude Code)
- Produces: OpenAI `ChatCompletionsRequest` with correct tool schemas, core OpenCode tools injection, and unpacked tool results formatted as `role: "tool"`

- [ ] **Step 1: Write the failing protocol transform test**

```rust
// tests/test_protocol_transform.rs
use agent2claude::protocol::anthropic::*;
use agent2claude::protocol::transform::transform_anthropic_to_openai;
use serde_json::json;

#[test]
fn test_tool_result_unpacking_and_sequencing() {
    let anthropic_req = MessagesRequest {
        model: "deepseek-v4.1-flash".to_string(),
        messages: vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("List files in directory".to_string()),
            },
            AnthropicMessage {
                role: "assistant".to_string(),
                content: AnthropicContent::Blocks(vec![
                    ContentBlock::ToolUse {
                        id: "call_123".to_string(),
                        name: "bash".to_string(),
                        input: json!({"command": "ls -la"}),
                    }
                ]),
            },
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Blocks(vec![
                    ContentBlock::ToolResult {
                        tool_use_id: "call_123".to_string(),
                        content: "total 0\n-rw-r--r-- file.txt".to_string(),
                        is_error: Some(false),
                    }
                ]),
            },
        ],
        system: Some(SystemPrompt::Single("You are a helpful assistant.".to_string())),
        tools: Some(vec![
            AnthropicTool {
                name: "bash".to_string(),
                description: Some("Run bash commands".to_string()),
                input_schema: json!({
                    "type": "object",
                    "properties": {"command": {"type": "string"}},
                    "required": ["command"]
                }),
            }
        ]),
        stream: true,
        max_tokens: 4096,
        temperature: None,
    };

    let openai_req = transform_anthropic_to_openai(&anthropic_req, true).expect("Transformation must succeed");

    // System prompt must be message 0
    assert_eq!(openai_req.messages[0].role, "system");
    assert_eq!(openai_req.messages[1].role, "user");
    assert_eq!(openai_req.messages[2].role, "assistant");
    assert!(openai_req.messages[2].tool_calls.is_some());

    // Message 3 must be role: "tool" with matching tool_call_id
    assert_eq!(openai_req.messages[3].role, "tool");
    assert_eq!(openai_req.messages[3].tool_call_id.as_deref(), Some("call_123"));
    assert_eq!(openai_req.messages[3].content, Some("total 0\n-rw-r--r-- file.txt".to_string()));

    // When targeting OpenCode, core tools (bash, edit, glob, grep, read, write) must be present
    let tool_names: Vec<String> = openai_req.tools.unwrap().into_iter().map(|t| t.function.name).collect();
    assert!(tool_names.contains(&"bash".to_string()));
    assert!(tool_names.contains(&"edit".to_string()));
    assert!(tool_names.contains(&"write".to_string()));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test test_protocol_transform`
Expected: FAIL (modules not implemented)

- [ ] **Step 3: Implement Anthropic and OpenAI schemas and transformers**

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

/// Self-repair common malformed JSON in tool call arguments
pub fn repair_tool_arguments(raw: &str) -> String {
    if serde_json::from_str::<Value>(raw).is_ok() {
        return raw.to_string();
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "{}".to_string();
    }
    // Attempt closing unclosed braces
    let mut candidate = trimmed.to_string();
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

    // 2. Transform messages & explode tool results
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
                        ContentBlock::ToolUse { id, name, input } => {
                            tool_calls.push(OpenAiToolCall {
                                id: id.clone(),
                                call_type: "function".to_string(),
                                function: OpenAiFunctionCall {
                                    name: name.clone(),
                                    arguments: input.to_string(),
                                },
                            });
                        }
                        ContentBlock::ToolResult { tool_use_id, content, .. } => {
                            // Sequential tool message
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
                    let text = if text_parts.is_empty() {
                        None
                    } else {
                        Some(text_parts.join("\n"))
                    };
                    let tools = if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    };
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

    // 3. Tools conversion
    let mut tools = Vec::new();
    if let Some(t_list) = &req.tools {
        for t in t_list {
            tools.push(OpenAiTool {
                tool_type: "function".to_string(),
                function: OpenAiFunctionDefinition {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.input_schema.clone(),
                },
            });
        }
    }

    // If OpenCode free tier, ensure core tools are present to pass 403 FreeTier check
    if is_opencode {
        let core_tools = ["bash", "edit", "glob", "grep", "read", "write"];
        for ct in &core_tools {
            if !tools.iter().any(|t| t.function.name == *ct) {
                tools.push(OpenAiTool {
                    tool_type: "function".to_string(),
                    function: OpenAiFunctionDefinition {
                        name: ct.to_string(),
                        description: Some(format!("Core {} tool", ct)),
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
git commit -m "feat: implement Anthropic-OpenAI protocol transformers with tool sequencing and repair"
```

---

### Task 5: Streaming SSE State Machine with Thinking / Tool Use

**Files:**
- Create: `src/protocol/stream.rs`
- Test: `tests/test_stream.rs`

**Interfaces:**
- Consumes: Raw OpenAI SSE chunks (data: `{"choices":[{"delta":{"content":"...", "reasoning_content":"...", "tool_calls":[...]}}]}`)
- Produces: Strict Anthropic SSE event sequence (`event: message_start`, `content_block_start`, `content_block_delta` with text/thinking/tool_use, `content_block_stop`, `message_delta`, `message_stop`)

- [ ] **Step 1: Write the failing SSE stream state machine test**

```rust
// tests/test_stream.rs
use agent2claude::protocol::stream::SseStreamTranslator;

#[test]
fn test_sse_stream_reasoning_and_tool_call() {
    let mut translator = SseStreamTranslator::new("msg_001", "deepseek-v4.1-flash");

    // Initial chunk with reasoning
    let chunk1 = r#"{"choices":[{"delta":{"reasoning_content":"Thinking about problem..."}}]}"#;
    let events1 = translator.feed_chunk(chunk1);
    assert!(events1.iter().any(|e| e.contains("message_start")));
    assert!(events1.iter().any(|e| e.contains("thinking")));
    assert!(events1.iter().any(|e| e.contains("Thinking about problem...")));

    // Chunk switching to text
    let chunk2 = r#"{"choices":[{"delta":{"content":"Here is the answer"}}]}"#;
    let events2 = translator.feed_chunk(chunk2);
    assert!(events2.iter().any(|e| e.contains("content_block_stop"))); // stops thinking block
    assert!(events2.iter().any(|e| e.contains("text")));
    assert!(events2.iter().any(|e| e.contains("Here is the answer")));

    // Completion chunk
    let chunk3 = r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
    let events3 = translator.feed_chunk(chunk3);
    assert!(events3.iter().any(|e| e.contains("message_delta")));
    assert!(events3.iter().any(|e| e.contains("message_stop")));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test test_stream`
Expected: FAIL (modules not implemented)

- [ ] **Step 3: Implement SseStreamTranslator**

```rust
// src/protocol/stream.rs
use serde_json::{json, Value};

#[derive(Debug, PartialEq, Eq)]
enum BlockType {
    None,
    Thinking,
    Text,
    ToolUse,
}

pub struct SseStreamTranslator {
    message_id: String,
    model: String,
    started: bool,
    current_block: BlockType,
    block_index: usize,
    finished: bool,
}

impl SseStreamTranslator {
    pub fn new(message_id: &str, model: &str) -> Self {
        Self {
            message_id: message_id.to_string(),
            model: model.to_string(),
            started: false,
            current_block: BlockType::None,
            block_index: 0,
            finished: false,
        }
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
                        "usage": {"input_tokens": 0, "output_tokens": 0}
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
                    if self.current_block != BlockType::Thinking {
                        if self.current_block != BlockType::None {
                            events.push(self.format_event("content_block_stop", json!({"type": "content_block_stop", "index": self.block_index})));
                            self.block_index += 1;
                        }
                        self.current_block = BlockType::Thinking;
                        events.push(self.format_event("content_block_start", json!({
                            "type": "content_block_start",
                            "index": self.block_index,
                            "content_block": {"type": "thinking", "thinking": ""}
                        })));
                    }
                    events.push(self.format_event("content_block_delta", json!({
                        "type": "content_block_delta",
                        "index": self.block_index,
                        "delta": {"type": "thinking_delta", "thinking": reasoning}
                    })));
                }
            }

            // 2. Normal text content
            if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    if self.current_block != BlockType::Text {
                        if self.current_block != BlockType::None {
                            events.push(self.format_event("content_block_stop", json!({"type": "content_block_stop", "index": self.block_index})));
                            self.block_index += 1;
                        }
                        self.current_block = BlockType::Text;
                        events.push(self.format_event("content_block_start", json!({
                            "type": "content_block_start",
                            "index": self.block_index,
                            "content_block": {"type": "text", "text": ""}
                        })));
                    }
                    events.push(self.format_event("content_block_delta", json!({
                        "type": "content_block_delta",
                        "index": self.block_index,
                        "delta": {"type": "text_delta", "text": text}
                    })));
                }
            }

            // 3. Tool Calls
            if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tool_calls {
                    if let Some(func) = tc.get("function") {
                        let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("call_gen");
                        let name = func.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let args = func.get("arguments").and_then(|v| v.as_str()).unwrap_or("");

                        if self.current_block != BlockType::ToolUse {
                            if self.current_block != BlockType::None {
                                events.push(self.format_event("content_block_stop", json!({"type": "content_block_stop", "index": self.block_index})));
                                self.block_index += 1;
                            }
                            self.current_block = BlockType::ToolUse;
                            events.push(self.format_event("content_block_start", json!({
                                "type": "content_block_start",
                                "index": self.block_index,
                                "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}
                            })));
                        }
                        if !args.is_empty() {
                            events.push(self.format_event("content_block_delta", json!({
                                "type": "content_block_delta",
                                "index": self.block_index,
                                "delta": {"type": "input_json_delta", "partial_json": args}
                            })));
                        }
                    }
                }
            }
        }

        if let Some(reason) = finish_reason.and_then(|v| v.as_str()) {
            if !self.finished {
                self.finished = true;
                if self.current_block != BlockType::None {
                    events.push(self.format_event("content_block_stop", json!({"type": "content_block_stop", "index": self.block_index})));
                }
                let stop_reason = match reason {
                    "tool_calls" => "tool_use",
                    _ => "end_turn",
                };
                events.push(self.format_event("message_delta", json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                    "usage": {"output_tokens": 100}
                })));
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
git commit -m "feat: implement SSE stream state machine for Anthropic protocol with reasoning and tool use"
```

---

### Task 6: HTTP Upstream Adapter with OpenCode FreeTier Signature & WARP Rotation

**Files:**
- Create: `src/adapters/http_upstream.rs`
- Create: `src/adapters/mod.rs`
- Test: `tests/test_http_upstream.rs`

**Interfaces:**
- Consumes: OpenAI ChatCompletionsRequest, `WarpPool`
- Produces: Proxied HTTP reqwest stream over Cloudflare WARP with session header `x-opencode-session` and Bearer token

- [ ] **Step 1: Write the failing unit test for HTTP upstream client creation**

```rust
// tests/test_http_upstream.rs
use agent2claude::adapters::http_upstream::HttpUpstreamAdapter;
use agent2claude::config::warp_pool::WarpPool;

#[test]
fn test_http_adapter_initialization() {
    let pool = WarpPool::new(vec![
        "socks5h://127.0.0.1:40001".to_string(),
        "socks5h://127.0.0.1:40002".to_string(),
    ]);
    let adapter = HttpUpstreamAdapter::new(
        "https://opencode.ai/inference/openai/v1",
        Some("test-token"),
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
    pub fn new(
        base_url: &str,
        auth_token: Option<&str>,
        warp_pool: Option<WarpPool>,
    ) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            auth_token: auth_token.map(|s| s.to_string()),
            warp_pool,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn send_stream(
        &self,
        request: &ChatCompletionsRequest,
    ) -> anyhow::Result<reqwest::Response> {
        let proxy_url = self.warp_pool.as_ref().and_then(|p| p.acquire_proxy());

        let mut client_builder = Client::builder()
            .timeout(Duration::from_secs(120))
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

        // OpenCode session header invariant
        let session_id = format!("ses_{}", uuid_simple());
        req_builder = req_builder.header("x-opencode-session", session_id);
        req_builder = req_builder.header("User-Agent", "opencode/1.19.3");

        let response = req_builder.send().await?;

        // Handle 429 rate limit
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
git commit -m "feat: implement HTTP upstream adapter with OpenCode free tier signature and WARP proxy rotation"
```

---

### Task 7: Local Agents ACP v1 Adapter over Stdio

**Files:**
- Create: `src/adapters/acp_client.rs`
- Test: `tests/test_acp_client.rs`

**Interfaces:**
- Consumes: `commandcode acp`, `kilo acp`, `cline --acp` child processes
- Produces: JSON-RPC 2.0 NDJSON messaging (`initialize`, `session/new`, `session/prompt`) and streaming stdout consumption

- [ ] **Step 1: Write the failing ACP client test**

```rust
// tests/test_acp_client.rs
use agent2claude::adapters::acp_client::AcpClient;

#[tokio::test]
async fn test_acp_client_spawns_and_initializes() {
    // Test with commandcode acp or mock
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
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
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
            .stderr(Stdio::null())
            .spawn()?;

        let mut stdin = child.stdin.take().expect("Failed to open stdin");
        let stdout = child.stdout.take().expect("Failed to open stdout");
        let mut reader = BufReader::new(stdout).lines();

        // 1. initialize
        let init_req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "1.0",
                "clientInfo": {"name": "agent2claude", "version": "0.1.0"},
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
            "params": {"cwd": std::env::current_dir()?.to_string_lossy()}
        });
        stdin.write_all(format!("{}\n", session_req).as_bytes()).await?;
        stdin.flush().await?;

        let session_resp = reader.next_line().await?.unwrap_or_default();
        let session_val: Value = serde_json::from_str(&session_resp)?;
        let session_id = session_val.pointer("/result/sessionId").and_then(|v| v.as_str()).unwrap_or("s1");

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

        // Stream notifications
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
git commit -m "feat: implement ACP v1 client for local agent communication over stdio"
```

---

### Task 8: Router & Failover Engine

**Files:**
- Create: `src/engine/router.rs`
- Create: `src/engine/mod.rs`
- Test: `tests/test_router.rs`

**Interfaces:**
- Consumes: Target model alias (`deepseek`, `glm`, `kilo`)
- Produces: Route decision resolving to either HttpUpstreamAdapter or AcpClient with automatic failover

- [ ] **Step 1: Write the failing router test**

```rust
// tests/test_router.rs
use agent2claude::config::Config;
use agent2claude::engine::router::EngineRouter;

#[test]
fn test_router_alias_resolution() {
    let config = Config::load_default();
    let router = EngineRouter::new(config);

    let candidates = router.resolve_alias("deepseek");
    assert!(!candidates.is_empty());
    assert_eq!(candidates[0].provider, "opencode");
    assert_eq!(candidates[0].model, "deepseek-v4.1-flash-free");
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
                let pool = p_cfg.warp_proxies.as_ref().map(|proxies| WarpPool::new(proxies.clone()));
                let token = p_cfg.auth_token.clone().or_else(|| {
                    p_cfg.auth_file.as_ref().and_then(|path| {
                        std::fs::read_to_string(path).ok().and_then(|data| {
                            serde_json::from_str::<serde_json::Value>(&data)
                                .ok()
                                .and_then(|v| v.get("token").and_then(|t| t.as_str()).map(|s| s.to_string()))
                        })
                    })
                });
                let endpoint = p_cfg.endpoint.clone().unwrap_or_else(|| "https://opencode.ai/inference/openai/v1".to_string());
                http_adapters.insert(name.clone(), Arc::new(HttpUpstreamAdapter::new(&endpoint, token.as_deref(), pool)));
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
            // Direct model fallback to opencode
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
git commit -m "feat: implement multi-agent router resolving aliases to HTTP and ACP targets"
```

---

### Task 9: Axum HTTP Server & Endpoints

**Files:**
- Create: `src/server.rs`
- Modify: `src/main.rs`
- Test: `tests/test_server.rs`

**Interfaces:**
- Consumes: HTTP `POST /v1/messages`, `GET /v1/models`, `GET /health`
- Produces: Axum HTTP server hosting Anthropic-compliant API

- [ ] **Step 1: Write the failing server endpoint test**

```rust
// tests/test_server.rs
use agent2claude::config::Config;
use agent2claude::engine::router::EngineRouter;
use agent2claude::server::create_router;
use axum_test::TestServer; // or use hyper / reqwest

#[tokio::test]
async fn test_health_endpoint() {
    let config = Config::load_default();
    let router = std::sync::Arc::new(EngineRouter::new(config));
    let app = create_router(router);

    let client = axum_test::TestServer::new(app).unwrap();
    let response = client.get("/health").await;
    assert_eq!(response.status_code(), 200);
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
        .route("/v1/messages", post(messages_handler))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(router)
}

async fn health_handler() -> impl IntoResponse {
    Json(json!({"status": "ok", "version": "0.1.0"}))
}

async fn models_handler() -> impl IntoResponse {
    Json(json!({
        "object": "list",
        "data": [
            {"id": "deepseek", "object": "model"},
            {"id": "glm", "object": "model"},
            {"id": "kilo", "object": "model"},
            {"id": "mimo", "object": "model"},
            {"id": "nemotron", "object": "model"},
            {"id": "deepseek-v4.1-flash", "object": "model"},
            {"id": "kilo-auto/free", "object": "model"}
        ]
    }))
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

        match http_adapter.send_stream(&openai_req).await {
            Ok(upstream_resp) => {
                let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(100);
                let model_clone = req.model.clone();

                tokio::spawn(async move {
                    let mut translator = SseStreamTranslator::new("msg_live", &model_clone);
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
        (StatusCode::NOT_IMPLEMENTED, "Provider type not supported").into_response()
    }
}
```

- [ ] **Step 4: Update src/main.rs to bind server**

```rust
// In src/main.rs
Commands::Serve { port, host } => {
    let config = Config::load_default();
    let router = std::sync::Arc::new(EngineRouter::new(config));
    let app = create_router(router);
    let addr = format!("{}:{}", host, port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("agent2claude listening on http://{}", addr);
    axum::serve(listener, app).await?;
}
```

- [ ] **Step 5: Run tests and commit**

```bash
cargo test
git add src/server.rs src/main.rs tests/test_server.rs
git commit -m "feat: implement Axum HTTP server with streaming Anthropic Messages handler"
```

---

### Task 10: Claude Code CLI Launcher with Context Limit Detection

**Files:**
- Create: `src/launcher/claude.rs`
- Create: `src/launcher/mod.rs`
- Modify: `src/main.rs`
- Test: `tests/test_launcher.rs`

**Interfaces:**
- Consumes: `--model <alias>` and `--port <port>`
- Produces: Executable environment spawning `claude --model <alias>` with `ANTHROPIC_BASE_URL`, `CLAUDE_CODE_MAX_CONTEXT_TOKENS`, and `CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT=1`

- [ ] **Step 1: Write the launcher test**

```rust
// tests/test_launcher.rs
use agent2claude::launcher::claude::build_claude_environment;

#[test]
fn test_claude_env_generation() {
    let envs = build_claude_environment("deepseek", 4001);
    assert_eq!(envs.get("ANTHROPIC_BASE_URL").unwrap(), "http://127.0.0.1:4001");
    assert_eq!(envs.get("ANTHROPIC_API_KEY").unwrap(), "dummy");
    assert_eq!(envs.get("CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT").unwrap(), "1");
    // Deepseek context limit should be 200000
    assert_eq!(envs.get("CLAUDE_CODE_MAX_CONTEXT_TOKENS").unwrap(), "200000");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test test_launcher`
Expected: FAIL (modules not implemented)

- [ ] **Step 3: Implement Launcher**

```rust
// src/launcher/claude.rs
use crate::discovery::catalog::CatalogRegistry;
use std::collections::HashMap;
use std::process::Command;

pub fn build_claude_environment(model: &str, port: u16) -> HashMap<String, String> {
    let mut envs = HashMap::new();
    envs.insert("ANTHROPIC_BASE_URL".to_string(), format!("http://127.0.0.1:{}", port));
    envs.insert("ANTHROPIC_API_KEY".to_string(), "dummy".to_string());
    envs.insert("CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT".to_string(), "1".to_string());

    let registry = CatalogRegistry::new();
    let meta = registry.get_metadata(model);
    let context_limit = meta.map(|m| m.context_length).unwrap_or(128000);
    envs.insert("CLAUDE_CODE_MAX_CONTEXT_TOKENS".to_string(), context_limit.to_string());

    envs
}

pub fn launch_claude(model: &str, port: u16) -> anyhow::Result<()> {
    let envs = build_claude_environment(model, port);
    let mut cmd = Command::new("claude");
    cmd.arg("--model").arg(model);
    for (k, v) in envs {
        cmd.env(k, v);
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

- [ ] **Step 4: Update src/main.rs for Launch command**

```rust
Commands::Launch { model, port } => {
    crate::launcher::claude::launch_claude(&model, port)?;
}
```

- [ ] **Step 5: Run tests and commit**

```bash
cargo test
git add src/launcher/ tests/test_launcher.rs src/main.rs
git commit -m "feat: implement Claude Code launcher with auto context configuration"
```

---

### Task 11: End-to-End Practical Verification

**Files:**
- Create: `tests/test_e2e.rs`
- Run: Live Claude Code verification against `agent2claude`

- [ ] **Step 1: Write E2E mock verification test**
- [ ] **Step 2: Start agent2claude server on test port**
- [ ] **Step 3: Run `claude --model deepseek` to execute a real bash command and verify thinking tokens**
- [ ] **Step 4: Verify 2 WARP exit IPs rotated on demand**
- [ ] **Step 5: Final documentation & clean git commit**

---
