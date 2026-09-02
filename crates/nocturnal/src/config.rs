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
    pub archive: ArchiveConfig,
    #[serde(default)]
    pub bell: BellConfig,
    #[serde(default)]
    pub provision: ProvisionConfig,
    #[serde(default)]
    pub compaction: CompactionConfig,
    #[serde(default)]
    pub roster: RosterConfig,
}

/// The guild roster, absorbed from the roster bot.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RosterConfig {
    /// Raid-access flags members may attach to a character (the roster
    /// bot's `access.txt`). Order is display order.
    #[serde(default = "default_access_labels")]
    pub access_labels: Vec<String>,
    /// Where the rendered roster payload is written for the web page to read.
    /// Unset = the page is not materialized.
    pub output_path: Option<PathBuf>,
    /// Ourios' query endpoint, for character profile events sent by members'
    /// Zeal clients. Unset = no profiles on the site.
    pub ourios_query_url: Option<String>,
    #[serde(default = "default_ourios_tenant")]
    pub ourios_tenant: String,
    /// Where the puller unpacks the Perses island (`island.js`, `island.css`).
    /// The page server serves it under /assets. Unset = charts render as a
    /// note saying the island is not installed.
    pub assets_dir: Option<PathBuf>,
}

fn default_ourios_tenant() -> String {
    "nocturnal".to_owned()
}

fn default_access_labels() -> Vec<String> {
    ["VP", "ST", "Emp", "VT"].map(String::from).to_vec()
}

impl Default for RosterConfig {
    fn default() -> Self {
        RosterConfig {
            access_labels: default_access_labels(),
            output_path: None,
            ourios_query_url: None,
            ourios_tenant: default_ourios_tenant(),
            assets_dir: None,
        }
    }
}

/// Rolling sealed WAL segments into month-partitioned Parquet.
///
/// Off unless an interval is set. Nothing has ever compacted automatically,
/// so the WAL only grows — `nocturnal.wal.size` is the gauge that says how
/// much runway is left, and turning this on is what reclaims it. Compaction
/// is crash-safe and idempotent by construction (temp file, fsync, rename,
/// read back and count, and only then delete a WAL segment), so a run
/// interrupted anywhere is simply re-done by the next one.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CompactionConfig {
    /// Seconds between runs, e.g. 86400 for nightly. Unset = never.
    pub interval_secs: Option<u64>,
}

/// The auction bell. On by default, because that is how officers know it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BellConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional sound file; the binary embeds the legacy bell otherwise.
    pub path: Option<PathBuf>,
}

impl Default for BellConfig {
    fn default() -> Self {
        BellConfig {
            enabled: true,
            path: None,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Off-site archive for compacted Parquet history (Hetzner Object Storage or
/// any S3-compatible endpoint). Credentials and endpoint come from the
/// standard AWS environment — `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
/// `AWS_ENDPOINT_URL_S3`, `AWS_REGION` — because those are the conventional
/// names; only the bucket and prefix, which have no standard variable, live
/// here. No bucket = no archive.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ArchiveConfig {
    pub bucket: Option<String>,
    #[serde(default)]
    pub prefix: String,
}

/// dpsbot successor (M8); absent = /dpstoken and /dpsrevoke disabled.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ProvisionConfig {
    pub tokens_path: Option<PathBuf>,
    pub perses_provisioning_dir: Option<PathBuf>,
    pub roles_map_path: Option<PathBuf>,
    pub dashboard_url: Option<String>,
    /// Guild the provisioning commands register in, under their *own* bot
    /// identity (`PROVISION_DISCORD_TOKEN_FILE`). Unset = single identity, and
    /// `/dpstoken` registers alongside the DKP commands like everything else.
    ///
    /// This exists so the members' guild keeps the bot it already has. The DKP
    /// side is still guild-scoped to the test server behind a command prefix
    /// until cutover; the token commands are ready now, and are the whole
    /// reason a second Python process was still running.
    pub guild_id: Option<u64>,
    /// Override for the commit `/dpstoken` asks members to be on (what
    /// `/zeal version` prints after `1.4.5+`). Normally unset: the bot reads
    /// `build.txt` from the nocturnal-zeal release, which the NewZeal
    /// workflow publishes with every build.
    #[serde(default)]
    pub zeal_build: Option<String>,
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
    /// Ledger guild to serve when commands arrive from `guild_id` — lets a
    /// test server browse (and later mutate) data imported from the real
    /// guild. Unset in production, where the two coincide.
    pub data_guild_id: Option<u64>,
    /// Prepended to every slash-command name (e.g. "controels-" makes
    /// /controels-playerdkp). Lets a test server share a bot application with
    /// other deployments without colliding names. Empty for production.
    #[serde(default)]
    pub command_prefix: String,
    /// The guild's feedback channel, mirrored into Ourios as `nocturnal.feedback.message`
    /// records (see docs/feedback.md). Setting it makes the bot request the
    /// privileged MESSAGE_CONTENT intent, which must be enabled for the app
    /// in the Discord developer portal first — otherwise the gateway refuses
    /// the connection and the bot is down. Unset = no intent, no mirror.
    #[serde(default)]
    pub feedback_channel_id: Option<u64>,
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

/// Config files are YAML by default — the format the rest of this guild's
/// stack already speaks (Perses, the collector, Jaeger, roles.yaml) — with
/// TOML still accepted so existing deployments keep working. The format is
/// chosen by extension.
fn parse_config(path: &str, text: &str) -> anyhow::Result<Config> {
    let is_toml = std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("toml"));
    if is_toml {
        toml::from_str(text).with_context(|| format!("parsing {path} as TOML"))
    } else {
        serde_yaml_ng::from_str(text).with_context(|| format!("parsing {path} as YAML"))
    }
}

/// Looked for, in order, when neither `--config` nor `NOCTURNAL_CONFIG` says
/// otherwise. Absent entirely is fine: every key has a default.
const DEFAULT_CONFIG_FILES: [&str; 3] = ["nocturnal.yaml", "nocturnal.yml", "nocturnal.toml"];

impl Config {
    pub fn load(path: Option<&str>) -> anyhow::Result<Config> {
        let path = path
            .map(String::from)
            .or_else(|| std::env::var("NOCTURNAL_CONFIG").ok())
            .or_else(|| {
                DEFAULT_CONFIG_FILES
                    .iter()
                    .find(|f| std::path::Path::new(f).is_file())
                    .map(|f| (*f).to_owned())
            });
        let mut cfg: Config = match &path {
            Some(p) => {
                let text = std::fs::read_to_string(p).with_context(|| format!("reading {p}"))?;
                parse_config(p, &text)?
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
        if let Ok(v) = std::env::var("NOCTURNAL_ARCHIVE__BUCKET") {
            cfg.archive.bucket = Some(v);
        }
        if let Ok(v) = std::env::var("NOCTURNAL_ARCHIVE__PREFIX") {
            cfg.archive.prefix = v;
        }
        if let Ok(v) = std::env::var("NOCTURNAL_DISCORD__COMMAND_PREFIX") {
            cfg.discord.command_prefix = v;
        }
        if let Ok(v) = std::env::var("NOCTURNAL_COMPACTION__INTERVAL_SECS") {
            cfg.compaction.interval_secs = Some(v.parse().context(
                "NOCTURNAL_COMPACTION__INTERVAL_SECS must be a whole number of seconds",
            )?);
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

    /// The provisioning identity's bot token, when the commands run under
    /// their own application. Same precedence as `discord_token`: a file
    /// first, so systemd can hand it over as a credential rather than an
    /// environment variable.
    pub fn provision_token() -> anyhow::Result<Option<String>> {
        if let Ok(path) = std::env::var("PROVISION_DISCORD_TOKEN_FILE") {
            return Ok(Some(
                std::fs::read_to_string(&path)
                    .with_context(|| format!("reading PROVISION_DISCORD_TOKEN_FILE {path}"))?
                    .trim()
                    .to_owned(),
            ));
        }
        Ok(std::env::var("PROVISION_DISCORD_TOKEN").ok())
    }

    /// Redacted, resolved view for `--print-config`.
    pub fn printable(&self) -> String {
        format!(
            "data.dir = {:?}\nlog.level = {:?}\nlog.format = {:?}\ndiscord.guild_id = {:?}\ndiscord.data_guild_id = {:?}\ndiscord.command_prefix = {:?}\nhealth.bind = {:?}\narchive.bucket = {:?} prefix = {:?}\ncompaction.interval_secs = {:?}\notlp = standard OTEL_* environment (endpoint: {:?}, protocol: {:?})\nprovision.tokens_path = {:?}\nprovision.perses_provisioning_dir = {:?}\nprovision.roles_map_path = {:?}\nprovision.dashboard_url = {:?}\n(discord token: from env, {})",
            self.data.dir,
            self.log.level,
            self.log.format,
            self.discord.guild_id,
            self.discord.data_guild_id,
            self.discord.command_prefix,
            self.health.bind,
            self.archive.bucket,
            self.archive.prefix,
            self.compaction.interval_secs,
            std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok(),
            std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL").ok(),
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

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion
mod tests {
    use super::{parse_config, Config};

    const YAML: &str = r#"
data:
  dir: /var/lib/nocturnal
log:
  level: debug
  format: json
discord:
  guild_id: 1540111927995539506
  command_prefix: "controels-"
archive:
  bucket: nocturnal-ledger
  prefix: prod
health:
  bind: 127.0.0.1:8090
"#;

    const TOML: &str = r#"
[data]
dir = "/var/lib/nocturnal"
[log]
level = "debug"
format = "json"
[discord]
guild_id = 1540111927995539506
command_prefix = "controels-"
[archive]
bucket = "nocturnal-ledger"
prefix = "prod"
[health]
bind = "127.0.0.1:8090"
"#;

    /// The same deployment, expressed either way, must resolve identically.
    #[test]
    fn yaml_and_toml_agree() {
        let from_yaml = parse_config("nocturnal.yaml", YAML).expect("yaml parses");
        let from_toml = parse_config("nocturnal.toml", TOML).expect("toml parses");
        assert_eq!(format!("{from_yaml:?}"), format!("{from_toml:?}"));
        assert_eq!(from_yaml.data.dir.to_str(), Some("/var/lib/nocturnal"));
        assert_eq!(from_yaml.discord.command_prefix, "controels-");
        assert_eq!(
            from_yaml.archive.bucket.as_deref(),
            Some("nocturnal-ledger")
        );
    }

    /// An empty file is a valid config: every key has a default.
    #[test]
    fn empty_yaml_is_all_defaults() {
        let cfg = parse_config("nocturnal.yaml", "{}").expect("empty yaml parses");
        assert_eq!(format!("{cfg:?}"), format!("{:?}", Config::default()));
    }

    /// A typo must fail loudly rather than be silently ignored.
    #[test]
    fn unknown_keys_are_rejected() {
        let err = parse_config("nocturnal.yaml", "log:\n  levle: info\n").unwrap_err();
        assert!(format!("{err:#}").contains("levle") || format!("{err:#}").contains("unknown"));
    }
}
