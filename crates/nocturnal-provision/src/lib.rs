//! dpsbot successor (M8): telemetry token + Perses dashboard provisioning.
//!
//! Projections come from nocturnal-core `telemetry.*` events; this crate
//! materializes them to `tokens.txt` and Perses provisioning YAMLs —
//! atomically, idempotently, and again on every boot. The files are derived
//! state: a half-written file, a restored VM, or a manual edit gone wrong
//! heals itself at the next startup.
//!
//! # Why the ledger tracks which names it manages
//!
//! `tokens.txt` is *not* ours alone. It also carries service tokens — the
//! bot's own OTLP credential (`# nocturnal-bot`) sits in it, next to the
//! members'. Rewriting the file purely from the projection would delete that
//! line on the first boot and the bot would revoke its own telemetry.
//!
//! So a line is only ever removed when the ledger *knows the name is one of
//! its own* — the `managed` set, which every username the ledger has issued a
//! token to joins and never leaves. Anything else is copied through untouched.
//!
//! The same rule governs the Perses directory, which holds the guild's shared
//! dashboards alongside the per-user files.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

/// One member's grant, exactly as the ledger projects it.
///
/// Carries the token's *fingerprint*, never the token. The secret exists in
/// exactly one place — `tokens.txt` — so a ledger backup, a Parquet
/// partition, an off-site archive copy or a dump pasted into a dispute
/// thread contains nothing worth stealing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub token_fp: String,
    pub role: String,
}

/// sha256 of a token, hex-encoded — what the ledger records instead of the
/// secret.
///
/// A plain hash rather than a password KDF on purpose: the input is 96 bits
/// of `getrandom` output, not something a human chose, so there is no
/// dictionary to slow down.
pub fn fingerprint(token: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Where the derived files live. Both come from config; neither is guessed.
#[derive(Debug, Clone)]
pub struct Paths {
    /// Gateway bearer-token file, e.g. `/etc/eq-otel/tokens.txt`.
    pub tokens: PathBuf,
    /// Perses provisioning directory, e.g. `/etc/perses/provisioning`.
    pub perses_dir: PathBuf,
}

/// What one materialization actually changed, for the log line.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub tokens_rewritten: bool,
    pub files_written: usize,
    pub files_removed: usize,
    /// Members holding a grant whose secret is not in `tokens.txt`.
    ///
    /// The ledger cannot conjure one back — that is the point of not storing
    /// it — so these need an officer to `/dpsrevoke` and the member to run
    /// `/dpstoken` again. Surfaced rather than silently ignored.
    pub grants_without_secret: Vec<String>,
}

/// Discord's post-2023 username grammar, as the legacy bot enforced it.
/// Anything else is refused before it can reach a filename.
pub fn valid_username(name: &str) -> bool {
    let len = name.chars().count();
    (2..=32).contains(&len)
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_')
}

/// The four per-user provisioning files, plus the legacy name that revocation
/// still has to clean up.
fn user_file_names(user: &str) -> [String; 5] {
    [
        format!("rb-{user}.yaml"),
        format!("50-project-{user}.yaml"),
        format!("51-ds-{user}.yaml"),
        format!("52-rb-own-{user}.yaml"),
        // dpsbot wrote this before the personal-project layout; deprovision
        // removed it, so we must too or a revoked member keeps stale access.
        format!("user-{user}.yaml"),
    ]
}

/// The datasource body, byte-identical to dpsbot's `DS_SPEC`.
const DS_SPEC: &str = "  display:
    name: Prometheus (EverQuest metrics)
  default: true
  plugin:
    kind: PrometheusDatasource
    spec:
      proxy:
        kind: HTTPProxy
        spec:
          url: http://127.0.0.1:9090
          allowedEndpoints:
            - endpointPattern: /api/v1/labels
              method: POST
            - endpointPattern: /api/v1/series
              method: POST
            - endpointPattern: /api/v1/metadata
              method: GET
            - endpointPattern: /api/v1/query
              method: POST
            - endpointPattern: /api/v1/query_range
              method: POST
            - endpointPattern: /api/v1/label/([a-zA-Z0-9_-]+)/values
              method: GET
";

/// The files one provisioned member should have, as `(name, contents)`.
///
/// Byte-compatible with dpsbot's output: the systemd `.path` units watching
/// this directory, and Perses itself, must not be able to tell which bot
/// wrote a file.
pub fn user_files(user: &str, role: &str) -> Vec<(String, String)> {
    let project = format!("u-{user}");
    vec![
        (
            format!("rb-{user}.yaml"),
            format!(
                "apiVersion: perses.dev/v1alpha1\n\
                 kind: RoleBinding\n\
                 metadata:\n  \
                 name: {role}-{user}\n  \
                 project: everquest\n\
                 spec:\n  \
                 role: {role}\n  \
                 subjects:\n    \
                 - kind: User\n      \
                 name: {user}\n"
            ),
        ),
        (
            format!("50-project-{user}.yaml"),
            format!(
                "apiVersion: perses.dev/v1alpha1\n\
                 kind: Project\n\
                 metadata:\n  \
                 name: {project}\n\
                 spec:\n  \
                 display:\n    \
                 name: \"{user} (personal)\"\n"
            ),
        ),
        (
            format!("51-ds-{user}.yaml"),
            format!(
                "apiVersion: perses.dev/v1alpha1\n\
                 kind: Datasource\n\
                 metadata:\n  \
                 name: prometheus\n  \
                 project: {project}\n\
                 spec:\n{DS_SPEC}"
            ),
        ),
        (
            format!("52-rb-own-{user}.yaml"),
            format!(
                "apiVersion: perses.dev/v1alpha1\n\
                 kind: RoleBinding\n\
                 metadata:\n  \
                 name: owner-{user}\n  \
                 project: {project}\n\
                 spec:\n  \
                 role: owner\n  \
                 subjects:\n    \
                 - kind: User\n      \
                 name: {user}\n"
            ),
        ),
    ]
}

/// Rebuild `tokens.txt` from the projection while preserving every line the
/// ledger does not manage.
///
/// Pure so the rule can be tested without a filesystem — it is the one place
/// where getting it wrong logs the bot out of its own telemetry pipeline.
pub fn tokens_file(
    existing: &str,
    managed: &BTreeSet<String>,
    grants: &BTreeMap<String, Grant>,
) -> (String, Vec<String>) {
    let mut kept: BTreeMap<String, String> = BTreeMap::new();
    let mut out = String::new();
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // "One token per line; anything after whitespace is a comment" — so a
        // comment-only line would register `#` as a usable bearer token. Never
        // emit one, and drop any that a human left behind.
        if trimmed.starts_with('#') {
            continue;
        }
        let Some(user) = line_username(trimmed) else {
            out.push_str(trimmed);
            out.push('\n');
            continue;
        };
        if !managed.contains(user) {
            // Not ours — a service credential, or something added by hand.
            out.push_str(trimmed);
            out.push('\n');
            continue;
        }
        // Ours. Keep it only while a live grant vouches for this exact secret;
        // a line whose fingerprint does not match is an orphan from a crash or
        // a hand edit, and leaving it would keep a token valid that nothing
        // records.
        let secret = trimmed.split_whitespace().next().unwrap_or_default();
        if grants
            .get(user)
            .is_some_and(|g| g.token_fp == fingerprint(secret))
        {
            kept.insert(user.to_owned(), trimmed.to_owned());
        }
    }
    // Sorted, so the same inputs always produce the same bytes.
    for line in kept.values() {
        out.push_str(line);
        out.push('\n');
    }
    // A grant whose secret we no longer hold cannot be rebuilt — by design.
    let missing = grants
        .keys()
        .filter(|u| !kept.contains_key(*u))
        .cloned()
        .collect();
    (out, missing)
}

/// Add a freshly issued token. The only moment the secret is in hand, and the
/// only writer that ever introduces a line.
pub fn append_token(paths: &Paths, user: &str, secret: &str) -> io::Result<()> {
    if let Some(dir) = paths.tokens.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = std::fs::read_to_string(&paths.tokens).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("{secret} # {user}\n"));
    write_atomic(&paths.tokens, &text)
}

/// The name in a `<token> # <name>` line, if it has one.
fn line_username(line: &str) -> Option<&str> {
    let (_, comment) = line.split_once(char::is_whitespace)?;
    let name = comment.trim().strip_prefix('#')?.trim();
    (!name.is_empty()).then_some(name)
}

/// Write `contents` to `path` so a reader never sees a partial file: a temp
/// file in the same directory, fsynced, then renamed over the target.
fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    use std::io::Write as _;
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("out")
    ));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    // The rename itself is only durable once the directory entry is.
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Bring both derived surfaces in line with the projection.
///
/// Idempotent: running it twice writes nothing the second time, which is what
/// makes calling it on every boot free.
pub fn materialize(
    paths: &Paths,
    managed: &BTreeSet<String>,
    grants: &BTreeMap<String, Grant>,
) -> io::Result<Report> {
    let mut report = Report::default();

    // --- tokens.txt -------------------------------------------------------
    let existing = std::fs::read_to_string(&paths.tokens).unwrap_or_default();
    let (desired, missing) = tokens_file(&existing, managed, grants);
    report.grants_without_secret = missing;
    if desired != existing {
        if let Some(dir) = paths.tokens.parent() {
            std::fs::create_dir_all(dir)?;
        }
        write_atomic(&paths.tokens, &desired)?;
        report.tokens_rewritten = true;
    }

    // --- Perses provisioning ---------------------------------------------
    std::fs::create_dir_all(&paths.perses_dir)?;
    for (user, grant) in grants {
        for (name, contents) in user_files(user, &grant.role) {
            let path = paths.perses_dir.join(&name);
            if std::fs::read_to_string(&path).ok().as_deref() != Some(contents.as_str()) {
                write_atomic(&path, &contents)?;
                report.files_written += 1;
            }
        }
    }
    // Managed names with no current grant were revoked: remove their files.
    for user in managed.iter().filter(|u| !grants.contains_key(*u)) {
        for name in user_file_names(user) {
            let path = paths.perses_dir.join(&name);
            if path.exists() {
                std::fs::remove_file(&path)?;
                report.files_removed += 1;
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grant as the ledger holds it: the fingerprint of a secret, never the
    /// secret.
    fn grant_for(secret: &str, role: &str) -> Grant {
        Grant {
            token_fp: fingerprint(secret),
            role: role.into(),
        }
    }

    /// The whole point of the redesign: nothing that reaches the event log,
    /// a backup tarball, a Parquet partition or an off-site archive can be
    /// turned back into a working token.
    #[test]
    fn a_fingerprint_does_not_reveal_the_token() {
        let secret = "0123456789abcdef0123456789abcdef0123456789abcdef";
        let fp = fingerprint(secret);
        assert_eq!(fp.len(), 64, "sha256 hex");
        assert!(
            !fp.contains(&secret[..8]),
            "the secret leaked into its own fingerprint"
        );
        assert_ne!(fp, secret);
        assert_eq!(fp, fingerprint(secret), "must be deterministic");
        assert_ne!(
            fp,
            fingerprint("0123456789abcdef0123456789abcdef0123456789abcde0")
        );
    }

    /// The failure this design exists to prevent: `tokens.txt` carries the
    /// bot's own OTLP credential beside the members', so a rewrite driven only
    /// by the projection would log the bot out of its own pipeline.
    #[test]
    fn a_service_token_survives_a_rewrite_that_revokes_everyone() {
        let existing = "aaa # ziglax\nbbb # nocturnal-bot\nccc # magis\n";
        let managed = BTreeSet::from(["ziglax".to_owned(), "magis".to_owned()]);
        let (out, missing) = tokens_file(existing, &managed, &BTreeMap::new());
        assert_eq!(out, "bbb # nocturnal-bot\n");
        assert!(missing.is_empty());
    }

    /// A name the ledger has never issued to is not ours to delete.
    #[test]
    fn unmanaged_member_lines_are_left_alone() {
        let (out, _) = tokens_file("aaa # someone_else\n", &BTreeSet::new(), &BTreeMap::new());
        assert_eq!(out, "aaa # someone_else\n");
    }

    /// "Anything after whitespace is a comment" means a comment-only line
    /// parses as the bearer token `#`. Never write one; clear any found.
    #[test]
    fn comment_only_lines_are_dropped_because_they_would_be_valid_tokens() {
        let existing = "# a header someone added\naaa # ziglax\n";
        let managed = BTreeSet::from(["ziglax".to_owned()]);
        let grants = BTreeMap::from([("ziglax".to_owned(), grant_for("aaa", "viewer"))]);
        let (out, _) = tokens_file(existing, &managed, &grants);
        assert_eq!(out, "aaa # ziglax\n");
        assert!(!out.lines().any(|l| l.trim_start().starts_with('#')));
    }

    /// The secret is preserved verbatim — the projection cannot rebuild it,
    /// so a rewrite must carry the existing line through untouched.
    #[test]
    fn an_existing_secret_is_carried_through_not_regenerated() {
        let managed = BTreeSet::from(["ziglax".to_owned()]);
        let grants = BTreeMap::from([("ziglax".to_owned(), grant_for("zzz", "editor"))]);
        let (once, missing) = tokens_file("bbb # nocturnal-bot\nzzz # ziglax\n", &managed, &grants);
        assert_eq!(once, "bbb # nocturnal-bot\nzzz # ziglax\n");
        assert!(missing.is_empty());
        let (twice, _) = tokens_file(&once, &managed, &grants);
        assert_eq!(once, twice, "a second pass changed the file");
    }

    /// A line whose secret does not match the recorded fingerprint is an
    /// orphan — a crash between the event and the append, or a hand edit.
    /// Leaving it would keep a token valid that nothing records.
    #[test]
    fn an_orphaned_token_line_is_removed_and_reported() {
        let managed = BTreeSet::from(["ziglax".to_owned()]);
        let grants = BTreeMap::from([("ziglax".to_owned(), grant_for("the-real-one", "viewer"))]);
        let (out, missing) = tokens_file("something-else # ziglax\n", &managed, &grants);
        assert_eq!(out, "", "the unrecorded token must not stay valid");
        assert_eq!(
            missing,
            vec!["ziglax".to_owned()],
            "and it must be reported"
        );
    }

    /// The legacy grammar, refused before a name can become a path segment.
    #[test]
    fn usernames_are_validated_before_they_reach_a_filename() {
        assert!(valid_username("scryll."));
        assert!(valid_username("elscorcho008"));
        assert!(!valid_username("a"), "too short");
        assert!(!valid_username(&"x".repeat(33)), "too long");
        assert!(
            !valid_username("Ziglax"),
            "uppercase is not a 2023 username"
        );
        assert!(!valid_username("../etc/passwd"), "path traversal");
        assert!(!valid_username("has space"));
        assert!(
            !valid_username("nocturnal-bot"),
            "a service name, not a member"
        );
    }

    #[test]
    fn materialize_is_idempotent_and_cleans_up_revoked_members() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            tokens: dir.path().join("tokens.txt"),
            perses_dir: dir.path().join("provisioning"),
        };
        std::fs::create_dir_all(&paths.perses_dir).expect("mkdir");
        std::fs::write(&paths.tokens, "bbb # nocturnal-bot\n").expect("seed");

        // Issue: the one moment the secret is in hand.
        append_token(&paths, "ziglax", "zzz").expect("append");
        let managed = BTreeSet::from(["ziglax".to_owned()]);
        let grants = BTreeMap::from([("ziglax".to_owned(), grant_for("zzz", "editor"))]);

        let first = materialize(&paths, &managed, &grants).expect("materialize");
        assert_eq!(first.files_written, 4);
        assert!(first.grants_without_secret.is_empty());

        let second = materialize(&paths, &managed, &grants).expect("again");
        assert_eq!(second, Report::default(), "a second run must write nothing");

        let after = materialize(&paths, &managed, &BTreeMap::new()).expect("revoke");
        assert!(after.tokens_rewritten);
        assert_eq!(after.files_removed, 4);
        assert_eq!(
            std::fs::read_to_string(&paths.tokens).expect("read"),
            "bbb # nocturnal-bot\n",
            "the service token outlived the member"
        );
    }

    /// A partially written file must never be visible to the systemd `.path`
    /// units watching this directory.
    #[test]
    fn writes_leave_no_temp_files_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            tokens: dir.path().join("tokens.txt"),
            perses_dir: dir.path().join("provisioning"),
        };
        let grants = BTreeMap::from([("magis".to_owned(), grant_for("mmm", "viewer"))]);
        materialize(&paths, &BTreeSet::new(), &grants).expect("materialize");
        let leftovers: Vec<_> = std::fs::read_dir(&paths.perses_dir)
            .expect("read dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp") || n.starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }
}
