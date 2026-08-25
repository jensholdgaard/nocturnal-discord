//! `/dpstoken` and `/dpsrevoke` — the dpsbot successor (M8).
//!
//! Same UX officers and members already know; ledger-backed internals. Every
//! grant is a `telemetry.*` event, and `tokens.txt` plus the Perses
//! provisioning YAMLs are rewritten from the projection after each change and
//! again on boot, so the files can never drift from the log for long.
//!
//! Both commands disable themselves cleanly when unconfigured — the DKP side
//! of the bot does not care whether it is running on the observability VM.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context as _;
use nocturnal_core::{Actor, Command};
use nocturnal_provision::{materialize, valid_username, Grant, Paths};
use nocturnal_telemetry::attr;
use poise::serenity_prelude as serenity;

use crate::config::ProvisionConfig;
use crate::discord::{ack_ephemeral, Context, Error};
use crate::driver::DriverHandle;

/// Resolved provisioning configuration. `None` anywhere = commands disabled.
#[derive(Debug, Clone)]
pub struct Provisioning {
    pub paths: Paths,
    pub roles_map: std::path::PathBuf,
    pub dashboard_url: String,
}

impl Provisioning {
    /// Every path must be present; a half-configured provisioner would issue
    /// tokens the gateway never learns about.
    pub fn from_config(cfg: &ProvisionConfig) -> Option<Provisioning> {
        Some(Provisioning {
            paths: Paths {
                tokens: cfg.tokens_path.clone()?,
                perses_dir: cfg.perses_provisioning_dir.clone()?,
            },
            roles_map: cfg.roles_map_path.clone()?,
            dashboard_url: cfg.dashboard_url.clone()?,
        })
    }
}

/// A 48-hex-character token, as the legacy bot minted (`secrets.token_hex(24)`).
fn mint_token() -> anyhow::Result<String> {
    let mut bytes = [0u8; 24];
    let filled = rustix::rand::getrandom(&mut bytes[..], rustix::rand::GetRandomFlags::empty())?;
    // A short read would silently shorten the token's entropy.
    anyhow::ensure!(filled == bytes.len(), "short read from getrandom");
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Highest Perses role the member's Discord roles grant, or `None` for a
/// member holding no mapped rank.
///
/// The map is re-read on every command so officers can edit `roles.yaml` live,
/// exactly as the legacy bot did. Parsed by hand rather than with a YAML
/// crate: the file is a fixed two-level shape, and an officer's stray
/// indentation should degrade to "no match", never to a boot failure.
pub fn role_for(roles_map: &std::path::Path, held: &[String]) -> Option<String> {
    let text = std::fs::read_to_string(roles_map).ok()?;
    let mut mapping: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim_end();
        if line.trim().is_empty() {
            continue;
        }
        if !line.starts_with(char::is_whitespace) && line.ends_with(':') {
            let key = line.trim_end_matches(':').trim().to_owned();
            mapping.entry(key.clone()).or_default();
            current = Some(key);
        } else if let (Some(key), Some(value)) = (current.as_ref(), line.trim().strip_prefix("- "))
        {
            mapping
                .entry(key.clone())
                .or_default()
                .push(value.trim().to_owned());
        }
    }
    let held: BTreeSet<&str> = held.iter().map(String::as_str).collect();
    // Highest first, so an officer who is also a Raider gets editor.
    for perses_role in ["editor", "viewer"] {
        if mapping
            .get(perses_role)
            .is_some_and(|names| names.iter().any(|n| held.contains(n.as_str())))
        {
            return Some(perses_role.to_owned());
        }
    }
    None
}

/// Rewrite the derived files from the current projection.
///
/// Called after every change and on boot. Never fatal: the ledger is
/// authoritative, and a filesystem that is briefly unwritable must not take
/// the bot down or roll back a grant that is already durable.
pub async fn rematerialize(driver: &DriverHandle, p: &Provisioning, guild: u64) {
    let (managed, grants) = driver
        .query(move |l| {
            let Some(g) = l.state().guild(guild) else {
                return (BTreeSet::new(), BTreeMap::new());
            };
            let grants = g
                .telemetry
                .iter()
                .map(|(user, t)| {
                    (
                        user.clone(),
                        Grant {
                            token: t.token.clone(),
                            role: t.role.clone(),
                        },
                    )
                })
                .collect();
            (g.telemetry_managed.clone(), grants)
        })
        .await;

    let paths = p.paths.clone();
    let done = tokio::task::spawn_blocking(move || materialize(&paths, &managed, &grants)).await;
    match done {
        Ok(Ok(report)) if report != nocturnal_provision::Report::default() => {
            tracing::info!(
                { attr::NOCTURNAL_PROVISION_FILES_WRITTEN } = report.files_written,
                { attr::NOCTURNAL_PROVISION_FILES_REMOVED } = report.files_removed,
                { attr::NOCTURNAL_PROVISION_TOKENS_REWRITTEN } = report.tokens_rewritten,
                "provisioning files rewritten from the ledger"
            );
        }
        Ok(Ok(_)) => {} // already in step; the common case on boot
        Ok(Err(e)) => {
            tracing::error!(
                { attr::NOCTURNAL_ERROR_MESSAGE } = %e,
                "could not write the provisioning files; the ledger is still correct"
            );
        }
        Err(e) => {
            tracing::error!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "materialize task failed")
        }
    }
}

fn record(operation: &'static str, outcome: &'static str) {
    nocturnal_telemetry::metrics().provision_operations.add(
        1,
        &[
            opentelemetry::KeyValue::new(attr::NOCTURNAL_PROVISION_OPERATION, operation),
            opentelemetry::KeyValue::new(attr::NOCTURNAL_DECISION_OUTCOME, outcome),
        ],
    );
}

/// The DM the member receives, byte-for-byte the legacy template.
fn dm_body(token: &str, dashboard: &str) -> String {
    format!(
        "Your personal DPS meter token (keep it private):\n\
         ```\n{token}\n```\n\
         **Windows setup — open PowerShell (no admin) and paste this one line:**\n\
         ```powershell\n\
         & ([scriptblock]::Create((irm https://raw.githubusercontent.com/jensholdgaard/everquest-observability/main/client/windows/install.ps1))) -Token {token}\n\
         ```\n\
         It finds your EverQuest folder by itself. Then in game: `/otlp on`\n\
         (That line contains your token — don't paste it in a public channel. It also stays in your\n\
         PowerShell history; `-Uninstall` on the same script removes everything later.)\n\
         Dashboard: {dashboard} (log in with Discord — your access is already set up)\n\
         You also get a personal project to save your own dashboards in; the guild ones stay read-only.\n\
         Lost the token? Ask an officer to `/dpsrevoke` you, then run `/dpstoken` again."
    )
}

/// One-time import of dpsbot's files into the ledger (M8 migration).
///
/// Every `<token> # <name>` line whose name is a real Discord username becomes
/// a `telemetry.token.issued` event, with the role read back from that
/// member's existing `rb-<name>.yaml`.
///
/// This has to run **before** the commands go live. Until a member's grant is
/// in the ledger their `tokens.txt` line is unmanaged, so `/dpstoken` would
/// see no grant, mint a *second* token, and leave the first one valid beside
/// it — two working credentials for one person, and no way to revoke the one
/// nobody recorded.
///
/// Idempotent: a member the ledger already knows is skipped, so re-running is
/// free and a half-finished import simply resumes.
pub async fn import_legacy(
    driver: &DriverHandle,
    p: &Provisioning,
    guild: u64,
) -> anyhow::Result<(usize, usize)> {
    let tokens = std::fs::read_to_string(&p.paths.tokens)
        .with_context(|| format!("reading {}", p.paths.tokens.display()))?;

    let known: BTreeSet<String> = driver
        .query(move |l| {
            l.state()
                .guild(guild)
                .map(|g| g.telemetry_managed.clone())
                .unwrap_or_default()
        })
        .await;

    let mut imported = 0usize;
    let mut skipped = 0usize;
    for line in tokens.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((token, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let Some(user) = rest.trim().strip_prefix('#').map(str::trim) else {
            continue;
        };
        // A service credential is not a member. Post-2023 Discord usernames
        // cannot contain `-`, which is what `nocturnal-bot` is named with, and
        // a member always has a role binding on disk.
        if !valid_username(user) || !p.paths.perses_dir.join(format!("rb-{user}.yaml")).exists() {
            skipped += 1;
            continue;
        }
        if known.contains(user) {
            skipped += 1;
            continue;
        }
        // Their current dashboard role, so the import changes nobody's access.
        let role = std::fs::read_to_string(p.paths.perses_dir.join(format!("rb-{user}.yaml")))
            .ok()
            .and_then(|rb| {
                rb.lines()
                    .find_map(|l| l.trim().strip_prefix("role: ").map(|r| r.trim().to_owned()))
            })
            .unwrap_or_else(|| "viewer".to_owned());

        driver
            .execute(
                guild,
                Actor::System,
                Command::IssueToken {
                    username: user.to_owned(),
                    token: token.to_owned(),
                    role,
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!("importing {user}: {e}"))?;
        imported += 1;
    }
    Ok((imported, skipped))
}

/// Get your personal token for the guild DPS meter.
#[poise::command(slash_command, ephemeral)]
#[tracing::instrument(name = "command.dpstoken", skip_all, fields(otel.kind = "server"))]
pub async fn dpstoken(ctx: Context<'_>) -> Result<(), Error> {
    let Some(p) = ctx.data().provisioning.clone() else {
        ctx.say("Telemetry provisioning isn't configured on this deployment.")
            .await?;
        return Ok(());
    };
    let ledger_guild = crate::discord::require_guild(&ctx)?;
    ack_ephemeral(&ctx).await?;

    let user = ctx.author().name.clone();
    if !valid_username(&user) {
        record("issue", "rejected");
        ctx.say("Sorry, can't handle that username.").await?;
        return Ok(());
    }

    // Guild roles by name, re-read from roles.yaml on every invocation.
    let held: Vec<String> = match ctx.author_member().await {
        Some(m) => m
            .roles(ctx.serenity_context())
            .map(|roles| roles.iter().map(|r| r.name.to_string()).collect())
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let Some(role) = role_for(&p.roles_map, &held) else {
        record("issue", "rejected");
        ctx.say(
            "You need a guild rank (Trial/Recruit/Member/Raider or officer) for dashboard access. \
             Ask an officer if you think this is wrong.",
        )
        .await?;
        return Ok(());
    };

    let existing = {
        let u = user.clone();
        ctx.data()
            .driver
            .query(move |l| {
                l.state()
                    .guild(ledger_guild)
                    .and_then(|g| g.telemetry.get(&u).map(|t| t.role.clone()))
            })
            .await
    };

    if existing.is_some() {
        // Legacy behaviour: refresh access, never re-issue. The member's
        // client is already using the old token.
        let _ = ctx
            .data()
            .driver
            .execute(
                ledger_guild,
                Actor::User(ctx.author().id.get()),
                Command::RefreshAccess {
                    username: user.clone(),
                    role: role.clone(),
                },
            )
            .await;
        rematerialize(&ctx.data().driver, &p, ledger_guild).await;
        record("refresh", "accepted");
        ctx.say(format!(
            "You already have a token — refreshed your dashboard access to `{role}`. \
             Lost the token? Ask an officer to `/dpsrevoke` you first."
        ))
        .await?;
        return Ok(());
    }

    let token = mint_token()?;
    if let Err(e) = ctx
        .data()
        .driver
        .execute(
            ledger_guild,
            Actor::User(ctx.author().id.get()),
            Command::IssueToken {
                username: user.clone(),
                token: token.clone(),
                role: role.clone(),
            },
        )
        .await
    {
        record("issue", "error");
        tracing::error!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "issuing a telemetry token failed");
        ctx.say("Couldn't record that right now — try again in a moment.")
            .await?;
        return Ok(());
    }
    // Files only after the event is durable: a crash here heals on boot.
    rematerialize(&ctx.data().driver, &p, ledger_guild).await;
    record("issue", "accepted");

    let dm = ctx
        .author()
        .direct_message(
            ctx.serenity_context(),
            serenity::CreateMessage::new().content(dm_body(&token, &p.dashboard_url)),
        )
        .await;
    match dm {
        Ok(_) => {
            ctx.say("Sent — check your DMs. 📨").await?;
        }
        // DMs closed: the legacy fallback puts the token in a spoiler, in a
        // reply only the caller can see.
        Err(_) => {
            ctx.say(format!(
                "Couldn't DM you (privacy settings). Only you can see this:\n||{token}||\n\
                 Setup guide: {} → see #announcements or the repo README.",
                p.dashboard_url
            ))
            .await?;
        }
    }
    Ok(())
}

/// (officers) Revoke a member's DPS meter token.
#[poise::command(slash_command, ephemeral)]
#[tracing::instrument(name = "command.dpsrevoke", skip_all, fields(otel.kind = "server"))]
pub async fn dpsrevoke(
    ctx: Context<'_>,
    #[description = "The member to revoke"] member: serenity::Member,
) -> Result<(), Error> {
    let Some(p) = ctx.data().provisioning.clone() else {
        ctx.say("Telemetry provisioning isn't configured on this deployment.")
            .await?;
        return Ok(());
    };
    let ledger_guild = crate::discord::require_guild(&ctx)?;
    ack_ephemeral(&ctx).await?;

    // Administrator or Manage Guild, exactly as the legacy gate. The
    // interaction carries resolved permissions, which is the same field the
    // officer gate elsewhere in this bot reads.
    let permitted = ctx
        .author_member()
        .await
        .and_then(|m| m.permissions)
        .is_some_and(|perms| perms.administrator() || perms.manage_guild());
    if !permitted {
        record("revoke", "rejected");
        ctx.say("Officers only.").await?;
        return Ok(());
    }

    let user = member.user.name.clone();
    if !valid_username(&user) {
        record("revoke", "rejected");
        ctx.say("Sorry, can't handle that username.").await?;
        return Ok(());
    }

    match ctx
        .data()
        .driver
        .execute(
            ledger_guild,
            Actor::User(ctx.author().id.get()),
            Command::RevokeToken {
                username: user.clone(),
            },
        )
        .await
    {
        Ok(_) => {
            rematerialize(&ctx.data().driver, &p, ledger_guild).await;
            record("revoke", "accepted");
            ctx.say(format!("Revoked `{user}` (token + dashboard access)."))
                .await?;
        }
        Err(crate::driver::ExecError::Rejected(_)) => {
            record("revoke", "rejected");
            ctx.say(format!("`{user}` doesn't have a token.")).await?;
        }
        Err(e) => {
            record("revoke", "error");
            tracing::error!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "revoking a telemetry token failed");
            ctx.say("Couldn't record that right now — try again in a moment.")
                .await?;
        }
    }
    Ok(())
}

pub fn commands() -> Vec<poise::Command<crate::discord::Data, Error>> {
    vec![dpstoken(), dpsrevoke()]
}

#[cfg(test)]
mod tests {
    use super::{mint_token, role_for};

    const ROLES_YAML: &str = "\
# Maps Discord guild roles to Perses roles.
editor:
  - admin
  - Guild Leader
  - Officer
viewer:
  - Raider
";

    fn write(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("roles.yaml");
        std::fs::write(&path, text).expect("write");
        (dir, path)
    }

    /// Highest match wins: an officer who also holds Raider is an editor, not
    /// a viewer — the legacy order, and demoting them would be a silent
    /// permission change.
    #[test]
    fn the_highest_matching_role_wins() {
        let (_d, path) = write(ROLES_YAML);
        assert_eq!(
            role_for(&path, &["Raider".into(), "Officer".into()]),
            Some("editor".to_owned())
        );
        assert_eq!(
            role_for(&path, &["Raider".into()]),
            Some("viewer".to_owned())
        );
    }

    /// No mapped rank is a refusal, not a default grant.
    #[test]
    fn an_unmapped_member_gets_nothing() {
        let (_d, path) = write(ROLES_YAML);
        assert_eq!(role_for(&path, &["Guest".into()]), None);
        assert_eq!(role_for(&path, &[]), None);
    }

    /// Officers edit this file live. A missing or malformed map must degrade
    /// to "no access" rather than panicking inside a command.
    #[test]
    fn a_broken_or_missing_map_denies_instead_of_failing() {
        let (_d, path) = write("editor:\n\tnot: valid: yaml\n  - admin\n");
        assert_eq!(
            role_for(&path, &["admin".into()]),
            Some("editor".to_owned())
        );
        assert_eq!(
            role_for(
                std::path::Path::new("/nonexistent/roles.yaml"),
                &["admin".into()]
            ),
            None
        );
    }

    /// Comments are stripped, as the legacy parser did — a role listed only
    /// inside a comment must not grant anything.
    #[test]
    fn commented_out_roles_do_not_grant() {
        let (_d, path) = write("editor:\n  - admin\n#  - Sneaky\nviewer:\n  - Raider\n");
        assert_eq!(role_for(&path, &["Sneaky".into()]), None);
    }

    /// 48 hex characters, matching `secrets.token_hex(24)`, and never twice
    /// the same.
    #[test]
    fn tokens_are_48_hex_chars_and_unique() {
        let a = mint_token().expect("token");
        let b = mint_token().expect("token");
        assert_eq!(a.len(), 48, "{a}");
        assert!(a
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
        assert_ne!(a, b);
    }
}
