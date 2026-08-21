//! Layered configuration per `docs/operations.md`: TOML file → `NOCTURNAL_*`
//! env overrides → secrets from env only. Every key has a default; an empty
//! file (or none) is a valid config. Fail-fast with the offending key named.

use std::path::PathBuf;

use anyhow::{bail, Context as _};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub data: DataConfig,
    #[serde(default)]
    pub log: LogConfig,
    #[serde(default)]
    pub discord: DiscordConfig,
    #[serde(default)]
    pub health: HealthConfig,
    #[serde(default)]
    pub otlp: OtlpConfig,
    #[serde(default)]
    pub provision: ProvisionConfig,
}

/// Telemetry export (wired in the OTLP milestone step; parsed and validated now).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OtlpConfig {
    pub endpoint: Option<String>,
    #[serde(default = "default_otlp_protocol")]
    pub protocol: String,
}

impl Default for OtlpConfig {
    fn default() -> Self {
        OtlpConfig {
            endpoint: None,
            protocol: default_otlp_protocol(),
        }
    }
}

/// dpsbot successor (M8); absent = /dpstoken and /dpsrevoke disabled.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ProvisionConfig {
    pub tokens_path: Option<PathBuf>,
    pub perses_provisioning_dir: Option<PathBuf>,
    pub roles_map_path: Option<PathBuf>,
    pub dashboard_url: Option<String>,
}

fn default_otlp_protocol() -> String {
    "grpc".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataConfig {
    #[serde(default = "default_data_dir")]
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    /// "text" or "json"
    #[serde(default = "default_log_format")]
    pub format: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct DiscordConfig {
    /// Guild for scoped command registration (a test server during M3–M7).
    /// Commands are NEVER registered globally while the legacy bot lives.
    pub guild_id: Option<u64>,
    /// Prepended to every slash-command name (e.g. "controels-" makes
    /// /controels-playerdkp). Lets a test server share a bot application with
    /// other deployments without colliding names. Empty for production.
    #[serde(default)]
    pub command_prefix: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct HealthConfig {
    /// e.g. "127.0.0.1:8080" → /healthz /readyz. Unset = no HTTP server.
    pub bind: Option<String>,
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("data")
}
fn default_log_level() -> String {
    "info".into()
}
fn default_log_format() -> String {
    "text".into()
}

impl Default for DataConfig {
    fn default() -> Self {
        DataConfig {
            dir: default_data_dir(),
        }
    }
}
impl Default for LogConfig {
    fn default() -> Self {
        LogConfig {
            level: default_log_level(),
            format: default_log_format(),
        }
    }
}

impl Config {
    pub fn load(path: Option<&str>) -> anyhow::Result<Config> {
        let path = path
            .map(String::from)
            .or_else(|| std::env::var("NOCTURNAL_CONFIG").ok());
        let mut cfg: Config = match &path {
            Some(p) => {
                let text = std::fs::read_to_string(p).with_context(|| format!("reading {p}"))?;
                toml::from_str(&text).with_context(|| format!("parsing {p}"))?
            }
            None => Config::default(),
        };
        // Env overrides (the small, documented set).
        if let Ok(v) = std::env::var("NOCTURNAL_DATA__DIR") {
            cfg.data.dir = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("NOCTURNAL_LOG__LEVEL") {
            cfg.log.level = v;
        }
        if let Ok(v) = std::env::var("NOCTURNAL_DISCORD__GUILD_ID") {
            cfg.discord.guild_id = Some(
                v.parse()
                    .context("NOCTURNAL_DISCORD__GUILD_ID must be a snowflake")?,
            );
        }
        if let Ok(v) = std::env::var("NOCTURNAL_HEALTH__BIND") {
            cfg.health.bind = Some(v);
        }
        if let Ok(v) = std::env::var("NOCTURNAL_DISCORD__COMMAND_PREFIX") {
            cfg.discord.command_prefix = v;
        }
        let prefix = &cfg.discord.command_prefix;
        if !prefix
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
            || prefix.len() > 20
        {
            bail!(
                "discord.command_prefix must be lowercase [a-z0-9-_], at most 20 chars, got {prefix:?}"
            );
        }
        if cfg.otlp.protocol != "grpc" && cfg.otlp.protocol != "http/protobuf" {
            bail!(
                "otlp.protocol must be \"grpc\" or \"http/protobuf\", got {:?}",
                cfg.otlp.protocol
            );
        }
        if cfg.log.format != "text" && cfg.log.format != "json" {
            bail!(
                "log.format must be \"text\" or \"json\", got {:?}",
                cfg.log.format
            );
        }
        Ok(cfg)
    }

    /// The Discord bot token — env/secret-file only, never in the config file,
    /// never logged.
    pub fn discord_token() -> anyhow::Result<String> {
        if let Ok(path) = std::env::var("DISCORD_TOKEN_FILE") {
            return Ok(std::fs::read_to_string(&path)
                .with_context(|| format!("reading DISCORD_TOKEN_FILE {path}"))?
                .trim()
                .to_owned());
        }
        std::env::var("DISCORD_TOKEN")
            .context("set DISCORD_TOKEN or DISCORD_TOKEN_FILE (a separate bot application — never the legacy bot's token, its slash commands would collide)")
    }

    /// Redacted, resolved view for `--print-config`.
    pub fn printable(&self) -> String {
        format!(
            "data.dir = {:?}\nlog.level = {:?}\nlog.format = {:?}\ndiscord.guild_id = {:?}\ndiscord.command_prefix = {:?}\nhealth.bind = {:?}\notlp.endpoint = {:?}\notlp.protocol = {:?}\nprovision.tokens_path = {:?}\nprovision.perses_provisioning_dir = {:?}\nprovision.roles_map_path = {:?}\nprovision.dashboard_url = {:?}\n(discord token: from env, {})",
            self.data.dir,
            self.log.level,
            self.log.format,
            self.discord.guild_id,
            self.discord.command_prefix,
            self.health.bind,
            self.otlp.endpoint,
            self.otlp.protocol,
            self.provision.tokens_path,
            self.provision.perses_provisioning_dir,
            self.provision.roles_map_path,
            self.provision.dashboard_url,
            if std::env::var("DISCORD_TOKEN").is_ok() || std::env::var("DISCORD_TOKEN_FILE").is_ok()
            {
                "present"
            } else {
                "MISSING"
            }
        )
    }
}
