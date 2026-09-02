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
use nocturnal_provision::{append_token, fingerprint, materialize, valid_username, Grant, Paths};
use nocturnal_telemetry::attr;
use poise::serenity_prelude as serenity;

use crate::config::ProvisionConfig;
use crate::discord::{ack_ephemeral, officer_check, Context, Error};
use crate::driver::DriverHandle;

/// Resolved provisioning configuration. `None` anywhere = commands disabled.
#[derive(Debug, Clone)]
pub struct Provisioning {
    pub paths: Paths,
    pub roles_map: std::path::PathBuf,
    pub dashboard_url: String,
    /// See `ProvisionConfig::zeal_build`.
    pub zeal_build: Option<String>,
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
            zeal_build: cfg.zeal_build.clone(),
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
                            token_fp: t.token_fp.clone(),
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
            for user in &report.grants_without_secret {
                // Not recoverable: the ledger never held the secret. Say so
                // loudly rather than leaving a member silently unable to send.
                tracing::warn!(
                    { attr::NOCTURNAL_DISCORD_USER_NAME } = %user,
                    "grant has no token line; the member must be revoked and re-issued"
                );
            }
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
/// The modal that hands the token over: three pre-filled inputs the member
/// copies straight into the game. Same trick as the bid modal, inverted —
/// it carries text out instead of in. Nothing lands in any message history;
/// closing the modal is the end of the secret's visible life.
/// Where members get the guild's Zeal build.
pub const ZEAL_RELEASE_URL: &str =
    "https://github.com/jensholdgaard/NewZeal/releases/tag/otlp-sdk-preview";
/// The button under the Zeal gate; pressing it is the "I have it" answer.
pub const ZEAL_GATE_ID: &str = "dpstoken:zeal_ok";

/// What `/dpstoken` says *before* any token exists: the token is useless on
/// an old `Zeal.asi`, and members kept finding that out from a usage line in
/// game. With `zeal_build` configured the check is exact; without it, the
/// tell is whether `/otlp setup` exists.
pub fn zeal_gate_text(zeal_build: Option<&str>) -> String {
    let check = match zeal_build {
        Some(b) => format!(
            "In game, `/zeal version` must say `1.4.5+{b}`. Anything else — `1.4.5+UNOFFICIAL` \
             included — is an older file: swap it first."
        ),
        None => {
            "In game, `/otlp setup` must be a known command. If it prints a usage line instead, \
                 you're on an older file: swap it first."
                .to_owned()
        }
    };
    format!(
        "**Before your token: the DPS meter needs the Nocturnal Zeal build.**\n\
         **1.** Get the latest `Zeal.asi`: {ZEAL_RELEASE_URL} — drop it into your EverQuest folder, \
         replacing the one there (keep the old one aside; you still need your normal Zeal \
         install).\n\
         **2.** {check}\n\n\
         Then press the button. It hands you one line to paste in game."
    )
}

/// The handover: an ephemeral message only the member sees — the one line in
/// a code block (one click to copy on desktop), the rest of the setup under
/// it, and "dismiss" when done. Not a modal: a modal always ends in a
/// Submit/Cancel choice that added nothing here. Not a DM: nothing should
/// outlive the moment it was needed.
pub fn token_handover(token: &str, dashboard: &str) -> String {
    format!(
        "**Your line — paste it in game:**\n```\n/otlp setup {token}\n```\n{}\n\n\
         *Done? Dismiss this message (⋯ → Dismiss message). Only you can see it, and it is never \
         shown again.*",
        setup_steps(dashboard)
    )
}

/// The rest of the setup, shown after the modal closes — everything **but**
/// the secret, so this reply is safe to sit in the (ephemeral) history.
pub fn setup_steps(dashboard: &str) -> String {
    format!(
        "**Then:**\n\
         **1.** Latest Zeal: \
         https://github.com/jensholdgaard/NewZeal/releases/tag/otlp-sdk-preview — drop `Zeal.asi` \
         into your EverQuest folder, replacing the one there (keep a copy of the old one; you \
         still need your normal Zeal install). The one-line setup needs this build.\n\
         **2.** Start EverQuest and paste the line from the popup. That sets the endpoint, \
         stores the token and turns reporting on.\n\
         **3.** `/otlp status` should show `token: set (ends ...)` and `last HTTP status: 200` \
         with the payload count going up. A `401` means the server refused the token — tell an \
         officer, that is not you doing it wrong.\n\n\
         The token is stored encrypted and tied to your Windows account; it is never shown \
         again. **Don't paste it in a public channel**, and note chat is written to your \
         `eqlog` file when logging is on.\n\
         Dashboard: {dashboard} (log in with Discord — access is already set up).\n\
         Lost the token? Ask an officer to `/dpsrevoke` you, then run `/dpstoken` again.\n\
         Bonus: `/magelo` in game puts your gear on the guild site."
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
                    // The secret stays exactly where it already is — in
                    // tokens.txt. Only its fingerprint enters the log.
                    token_fp: nocturnal_provision::fingerprint(token),
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
/// How an issue attempt ended, for the reply that follows it.
pub enum Issued {
    /// A brand-new token: show it once, in the modal.
    Fresh(String),
    /// The member already had one; access was refreshed to this role.
    Existing(String),
}

/// The one path that creates a grant, shared by the slash command's refresh
/// branch and the gate button: existing grant → refresh access; otherwise
/// mint, commit the fingerprint, append the token line. `Err` carries the
/// member-facing text.
pub async fn issue(
    data: &crate::discord::Data,
    p: &Provisioning,
    ledger_guild: u64,
    user_id: u64,
    user: &str,
    role: &str,
) -> Result<Issued, String> {
    let existing = {
        let u = user.to_owned();
        data.driver
            .query(move |l| {
                l.state()
                    .guild(ledger_guild)
                    .and_then(|g| g.telemetry.get(&u).map(|t| t.role.clone()))
            })
            .await
    };
    if existing.is_some() {
        let _ = data
            .driver
            .execute(
                ledger_guild,
                Actor::User(user_id),
                Command::RefreshAccess {
                    username: user.to_owned(),
                    role: role.to_owned(),
                },
            )
            .await;
        record("refresh", "accepted");
        return Ok(Issued::Existing(role.to_owned()));
    }
    let token =
        mint_token().map_err(|_| "Couldn't mint a token right now — try again.".to_owned())?;
    if let Err(e) = data
        .driver
        .execute(
            ledger_guild,
            Actor::User(user_id),
            Command::IssueToken {
                username: user.to_owned(),
                token_fp: fingerprint(&token),
                role: role.to_owned(),
            },
        )
        .await
    {
        record("issue", "error");
        tracing::error!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "issuing a telemetry token failed");
        return Err("Couldn't record that right now — try again in a moment.".to_owned());
    }
    let paths = p.paths.clone();
    let (u, t) = (user.to_owned(), token.clone());
    match tokio::task::spawn_blocking(move || append_token(&paths, &u, &t)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            record("issue", "error");
            tracing::error!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "writing the token line failed");
            return Err(
                "Issued, but the gateway file couldn't be written — tell an officer.".to_owned(),
            );
        }
        Err(e) => {
            record("issue", "error");
            tracing::error!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "token writer task failed");
            return Err(
                "Issued, but the gateway file couldn't be written — tell an officer.".to_owned(),
            );
        }
    }
    record("issue", "accepted");
    Ok(Issued::Fresh(token))
}

/// Role names a member holds, from the cache.
fn role_names(member: Option<&serenity::Member>, cache: &serenity::Context) -> Vec<String> {
    member
        .and_then(|m| m.roles(cache))
        .map(|roles| roles.iter().map(|r| r.name.to_string()).collect())
        .unwrap_or_default()
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
    let user = ctx.author().name.clone();
    if !valid_username(&user) {
        record("issue", "rejected");
        ctx.say("Sorry, can't handle that username.").await?;
        return Ok(());
    }
    let member = ctx.author_member().await;
    let held = role_names(member.as_deref(), ctx.serenity_context());
    let Some(role) = role_for(&p.roles_map, &held) else {
        record("issue", "rejected");
        ctx.say(
            "You need a guild rank (Trial/Recruit/Member/Raider or officer) for dashboard access. \
             Ask an officer if you think this is wrong.",
        )
        .await?;
        return Ok(());
    };

    // Already provisioned: refresh and say so; the token is never re-shown.
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
        if let Ok(Issued::Existing(role)) = issue(
            ctx.data(),
            &p,
            ledger_guild,
            ctx.author().id.get(),
            &user,
            &role,
        )
        .await
        {
            ctx.say(format!(
                "You already have a token — refreshed your dashboard access to `{role}`. \
                 Lost the token? Ask an officer to `/dpsrevoke` you first.\n\
                 Make sure you're on the current Zeal build: {ZEAL_RELEASE_URL}"
            ))
            .await?;
            rematerialize(&ctx.data().driver, &p, ledger_guild).await;
        }
        return Ok(());
    }

    // No grant yet: the Zeal gate first. The button mints and opens the modal.
    let button = serenity::CreateButton::new(ZEAL_GATE_ID)
        .label("I have the latest Zeal — give me my token")
        .style(serenity::ButtonStyle::Primary);
    ctx.send(
        poise::CreateReply::default()
            .content(zeal_gate_text(p.zeal_build.as_deref()))
            .ephemeral(true)
            .components(vec![serenity::CreateActionRow::Buttons(vec![button])]),
    )
    .await?;
    Ok(())
}

/// The gate button: re-run the cheap checks (roles can change between the
/// command and the click), mint, and answer with the token modal — a modal is
/// a valid first response to a button. `Ok(false)` = not ours.
pub async fn handle_component(
    ctx: &serenity::Context,
    component: &serenity::ComponentInteraction,
    data: &crate::discord::Data,
) -> anyhow::Result<bool> {
    if component.data.custom_id != ZEAL_GATE_ID {
        return Ok(false);
    }
    let say = |text: String| {
        serenity::CreateInteractionResponse::Message(
            serenity::CreateInteractionResponseMessage::new()
                .content(text)
                .ephemeral(true),
        )
    };
    let Some(p) = data.provisioning.clone() else {
        component
            .create_response(
                ctx,
                say("Telemetry provisioning isn't configured on this deployment.".into()),
            )
            .await?;
        return Ok(true);
    };
    let Some(guild) = component.guild_id.map(|g| g.get()) else {
        component
            .create_response(
                ctx,
                say("This only works inside the guild's Discord server.".into()),
            )
            .await?;
        return Ok(true);
    };
    let ledger_guild = match data.data_guild {
        Some((from, to)) if from == guild => to,
        _ => guild,
    };
    let user = component.user.name.clone();
    if !valid_username(&user) {
        record("issue", "rejected");
        component
            .create_response(ctx, say("Sorry, can't handle that username.".into()))
            .await?;
        return Ok(true);
    }
    let held = role_names(component.member.as_ref(), ctx);
    let Some(role) = role_for(&p.roles_map, &held) else {
        record("issue", "rejected");
        component
            .create_response(
                ctx,
                say(
                    "You need a guild rank (Trial/Recruit/Member/Raider or officer) for dashboard \
                     access. Ask an officer if you think this is wrong."
                        .into(),
                ),
            )
            .await?;
        return Ok(true);
    };
    match issue(
        data,
        &p,
        ledger_guild,
        component.user.id.get(),
        &user,
        &role,
    )
    .await
    {
        Ok(Issued::Fresh(token)) => {
            component
                .create_response(ctx, say(token_handover(&token, &p.dashboard_url)))
                .await?;
            let (driver, p2) = (data.driver.clone(), p.clone());
            tokio::spawn(async move { rematerialize(&driver, &p2, ledger_guild).await });
        }
        Ok(Issued::Existing(role)) => {
            component
                .create_response(
                    ctx,
                    say(format!(
                        "You already have a token — refreshed your dashboard access to `{role}`. \
                         Lost the token? Ask an officer to `/dpsrevoke` you first."
                    )),
                )
                .await?;
            let (driver, p2) = (data.driver.clone(), p.clone());
            tokio::spawn(async move { rematerialize(&driver, &p2, ledger_guild).await });
        }
        Err(text) => {
            component.create_response(ctx, say(text)).await?;
        }
    }
    Ok(true)
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
    vec![dpstoken(), dpsrevoke(), dpsstatus()]
}

/// (officers) Who is reporting telemetry, on what Zeal build, last seen.
#[poise::command(
    slash_command,
    ephemeral,
    rename = "dpsstatus",
    check = "officer_check"
)]
#[tracing::instrument(name = "command.dpsstatus", skip_all, fields(otel.kind = "server"))]
pub async fn dpsstatus(ctx: Context<'_>) -> Result<(), Error> {
    let Some((url, tenant)) = ctx.data().ourios.clone() else {
        ctx.say("Telemetry storage (Ourios) isn't configured on this deployment.")
            .await?;
        return Ok(());
    };
    ack_ephemeral(&ctx).await?;
    let rows = crate::profiles::reporter_status(&url, &tenant).await;
    if rows.is_empty() {
        ctx.say(
            "No character profiles in the last 14 days — nobody is reporting, or Ourios did not \
             answer in time. Members start with `/dpstoken`, then `/magelo` in game.",
        )
        .await?;
        return Ok(());
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    fn ago(now_ms: i64, then_ms: i64) -> String {
        let m = ((now_ms - then_ms) / 60_000).max(0);
        if m < 60 {
            format!("{m}m")
        } else if m < 1440 {
            format!("{}h", m / 60)
        } else {
            format!("{}d", m / 1440)
        }
    }
    let mut body = format!(
        "**Telemetry reporters — {} in the last 14 days**\n```\n",
        rows.len()
    );
    body.push_str(&format!(
        "{:<16} {:<18} {:>6} {:>5}\n",
        "reporter", "zeal build", "last", "n"
    ));
    for r in rows.iter().take(40) {
        // A build tail (after the '+') is enough to spot who is behind.
        let build = r.version.rsplit('+').next().unwrap_or(&r.version);
        body.push_str(&format!(
            "{:<16.16} {:<18.18} {:>6} {:>5}\n",
            r.reporter,
            build,
            ago(now_ms, r.last_seen_ms),
            r.count
        ));
    }
    body.push_str("```");
    if rows.len() > 40 {
        body.push_str(&format!("\n…and {} more.", rows.len() - 40));
    }
    ctx.say(body).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{mint_token, role_for, zeal_gate_text, ZEAL_RELEASE_URL};

    #[test]
    fn the_gate_names_the_build_when_known_and_the_usage_tell_otherwise() {
        let exact = zeal_gate_text(Some("2b3cf2b"));
        assert!(exact.contains("`1.4.5+2b3cf2b`"), "{exact}");
        assert!(exact.contains(ZEAL_RELEASE_URL));
        let generic = zeal_gate_text(None);
        assert!(
            generic.contains("`/otlp setup` must be a known command"),
            "{generic}"
        );
        assert!(
            !generic.contains("UNOFFICIAL"),
            "without a known build, UNOFFICIAL proves nothing"
        );
    }

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
