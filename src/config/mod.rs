pub mod warp_pool;

use crate::config::warp_pool::WarpConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Best-effort home directory of the user running the daemon.
///
/// Never hardcode a developer's `$HOME`: the same binary has to resolve its
/// own agent paths on any machine. Falls back to the process temp directory
/// when `$HOME` is unset (containers, stripped-down shells).
pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Per-user directory for runtime state (pid files, daemon logs, traffic logs).
///
/// Deliberately outside `/tmp`: on most distros `/tmp` is a tmpfs, so anything
/// written there costs RAM and vanishes on reboot (or gets wiped mid-session
/// while the daemon still holds the file open).
pub fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LILY2CLAUDE_STATE_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir).join("lily2claude");
    }
    home_dir().join(".local/state/lily2claude")
}

/// [`state_dir`], created on demand.
pub fn ensure_state_dir() -> PathBuf {
    let dir = state_dir();
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Is the opt-in debug log enabled?
///
/// Gates used to test `L2C_DEBUG_LOG` with `is_some()`, so `L2C_DEBUG_LOG=0`
/// turned verbose stderr output on while [`debug_log_path`] still resolved the
/// default file. One definition keeps the flag and the destination in sync.
pub fn debug_enabled() -> bool {
    match std::env::var_os("L2C_DEBUG_LOG") {
        Some(value) if !value.is_empty() => {
            let flag = value.to_string_lossy();
            flag != "0" && !flag.eq_ignore_ascii_case("false")
        }
        _ => false,
    }
}

/// Path of the opt-in request/response traffic log.
///
/// `L2C_DEBUG_LOG` may name an explicit file; a bare flag (`1`, `true`, or an
/// empty value) falls back to the state directory instead of a fixed path in
/// a world-writable temp directory.
pub fn debug_log_path() -> PathBuf {
    match std::env::var_os("L2C_DEBUG_LOG") {
        Some(value) if !value.is_empty() && value != "1" && value != "true" => PathBuf::from(value),
        _ => state_dir().join("traffic.log"),
    }
}

/// Open a file for appending, creating it owner-only (0600 on Unix).
///
/// Debug logs carry full prompt and response text, so they must not be
/// world-readable in a shared temp directory.
pub fn open_private_append(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

/// Create or truncate a file owner-only (0600 on Unix).
pub fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)?.write_all(contents)
}

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
pub struct LauncherConfig {
    pub default_model: String,
    pub anthropic_model: String,
    pub subagent_model: String,
    pub haiku_model: String,
    pub sonnet_model: String,
    pub opus_model: String,
    #[serde(default = "default_true")]
    pub dangerously_skip_permissions: bool,
}

fn default_true() -> bool {
    true
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            default_model: "opencode:deepseek-v4.1-flash-free".to_string(),
            anthropic_model: "claude-sonnet-5-5".to_string(),
            subagent_model: "claude-haiku-4-5-20251001".to_string(),
            haiku_model: "claude-haiku-4-5-20251001".to_string(),
            sonnet_model: "claude-sonnet-5-5".to_string(),
            opus_model: "claude-opus-5-5".to_string(),
            dangerously_skip_permissions: true,
        }
    }
}

fn default_target_fallback() -> String {
    "deepseek".to_string()
}

fn default_disable_commandcode() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server_port: u16,
    pub server_host: String,
    #[serde(default = "default_target_fallback")]
    pub default_target: String,
    #[serde(default = "default_disable_commandcode")]
    pub disable_commandcode: bool,
    #[serde(default)]
    pub launcher: LauncherConfig,
    #[serde(default)]
    pub warp: WarpConfig,
    pub aliases: HashMap<String, Vec<String>>,
    pub providers: HashMap<String, ProviderConfig>,
}

impl Config {
    pub fn config_path() -> std::path::PathBuf {
        if let Ok(p) = std::env::var("LILY2CLAUDE_CONFIG") {
            return std::path::PathBuf::from(p);
        }
        let local = std::path::PathBuf::from("lily2claude.toml");
        if local.exists() {
            return local;
        }
        home_dir().join(".config/lily2claude/config.toml")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(mut cfg) = toml::from_str::<Config>(&content) {
                    let default_cfg = Self::load_default();
                    for (k, v) in default_cfg.aliases {
                        cfg.aliases.entry(k).or_insert(v);
                    }
                    return cfg;
                }
            }
        }
        let default_cfg = Self::load_default();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(serialized) = toml::to_string_pretty(&default_cfg) {
            let _ = std::fs::write(&path, serialized);
        }
        default_cfg
    }

    pub fn load_default() -> Self {
        let agent_reg = crate::agents::AgentRegistry::default();
        let mut aliases = agent_reg.default_aliases();

        let deepseek_targets = vec![
            "opencode:deepseek-v4.1-flash-free".to_string(),
        ];

        // Ensure transparent aliases have full fallbacks
        aliases.insert("deepseek".to_string(), deepseek_targets.clone());
        aliases.insert("deepseek-v4.1-flash".to_string(), deepseek_targets.clone());
        aliases.insert("deepseek/deepseek-v4.1-flash".to_string(), deepseek_targets.clone());

        // Map Anthropic subagent models transparently to DeepSeek V4.1 Flash
        for anthropic_model in &[
            "claude-haiku-4-5",
            "claude-sonnet-4-5",
            "claude-sonnet-4-6",
            "claude-opus-4-5",
            "claude-opus-4-6",
            "claude-opus-4-7",
            "claude-3-7-sonnet-20250219",
            "claude-3-7-sonnet",
            "claude-3-5-sonnet-20241022",
            "claude-3-5-haiku-20241022",
            "claude-3-haiku-20240307",
            "claude-3-opus-20240229",
            "haiku-4-5",
            "sonnet-4-5",
            "sonnet-4-6",
            "opus-4-5",
            "haiku",
            "sonnet",
            "opus",
            "default",
        ] {
            aliases.entry(anthropic_model.to_string()).or_insert_with(|| deepseek_targets.clone());
        }

        aliases.entry("glm".to_string()).or_insert_with(|| vec![
            "opencode:glm-5.3-flash-free".to_string(),
        ]);
        aliases.entry("kilo".to_string()).or_insert_with(|| vec![
            "kilo:kilo-auto/free".to_string(),
        ]);
        aliases.entry("luna".to_string()).or_insert_with(|| vec![
            "cline:openai-codex:gpt-5.6-luna".to_string(),
        ]);
        aliases.entry("mimo".to_string()).or_insert_with(|| vec![
            "opencode:mimo-v2.6-flash-free".to_string(),
        ]);
        aliases.entry("nemotron".to_string()).or_insert_with(|| vec![
            "opencode:nemotron-3.5-lightning-free".to_string(),
            "opencode:nemotron-3-ultra-free".to_string(),
        ]);
        aliases.entry("qwen".to_string()).or_insert_with(|| vec![
            "opencode:qwen3.6-plus-free".to_string(),
        ]);
        aliases.entry("minimax".to_string()).or_insert_with(|| vec![
            "opencode:minimax-m3.1-flash-free".to_string(),
            "opencode:minimax-m3-free".to_string(),
        ]);
        aliases.entry("kimi".to_string()).or_insert_with(|| vec![
            "opencode:kimi-k2.5-free".to_string(),
        ]);

        let mut providers = HashMap::new();
        providers.insert(
            "opencode".to_string(),
            ProviderConfig {
                name: "opencode".to_string(),
                kind: "acp_stdio".to_string(),
                endpoint: None,
                command: Some(vec!["opencode".to_string(), "acp".to_string()]),
                auth_file: Some(
                    home_dir()
                        .join(".local/share/opencode/auth.json")
                        .to_string_lossy()
                        .to_string(),
                ),
                auth_token: None,
                warp_proxies: None,
            },
        );
        providers.insert(
            "commandcode".to_string(),
            ProviderConfig {
                name: "commandcode".to_string(),
                kind: "acp_stdio".to_string(),
                endpoint: None,
                command: Some(vec!["commandcode".to_string(), "acp".to_string()]),
                auth_file: Some(
                    home_dir()
                        .join(".commandcode/auth.json")
                        .to_string_lossy()
                        .to_string(),
                ),
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
                auth_file: Some(
                    home_dir()
                        .join(".cline/data/settings/providers.json")
                        .to_string_lossy()
                        .to_string(),
                ),
                auth_token: None,
                warp_proxies: None,
            },
        );

        Self {
            server_port: 4001,
            server_host: "127.0.0.1".to_string(),
            default_target: "deepseek".to_string(),
            disable_commandcode: true,
            launcher: LauncherConfig::default(),
            warp: WarpConfig::default(),
            aliases,
            providers,
        }
    }
}
