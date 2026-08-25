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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub token: String,
    pub role: String,
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
) -> String {
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
        match line_username(trimmed) {
            // Ours: the projection below decides whether it lives.
            Some(user) if managed.contains(user) => {}
            // Not ours — a service token, or something added by hand.
            _ => {
                out.push_str(trimmed);
                out.push('\n');
            }
        }
    }
    // Sorted, so the same projection always produces the same bytes.
    for (user, grant) in grants {
        out.push_str(&format!("{} # {}\n", grant.token, user));
    }
    out
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
    let desired = tokens_file(&existing, managed, grants);
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

    fn grant(token: &str, role: &str) -> Grant {
        Grant {
            token: token.into(),
            role: role.into(),
        }
    }

    /// The failure this whole design exists to prevent: `tokens.txt` carries
    /// the bot's own OTLP credential beside the members', so a rewrite driven
    /// only by the projection would log the bot out of its own pipeline.
    #[test]
    fn a_service_token_survives_a_rewrite_that_revokes_everyone() {
        let existing = "aaa # ziglax\nbbb # nocturnal-bot\nccc # magis\n";
        let managed = BTreeSet::from(["ziglax".to_owned(), "magis".to_owned()]);
        let out = tokens_file(existing, &managed, &BTreeMap::new());
        assert_eq!(
            out, "bbb # nocturnal-bot\n",
            "the unmanaged service token must be the only line left"
        );
    }

    /// A name the ledger has never issued to is not ours to delete, even if it
    /// looks exactly like a member line.
    #[test]
    fn unmanaged_member_lines_are_left_alone() {
        let existing = "aaa # someone_else\n";
        let out = tokens_file(existing, &BTreeSet::new(), &BTreeMap::new());
        assert_eq!(out, "aaa # someone_else\n");
    }

    /// "Anything after whitespace is a comment" means a comment-only line
    /// parses as the bearer token `#`. We must never write one, and should
    /// clear any a human left behind.
    #[test]
    fn comment_only_lines_are_dropped_because_they_would_be_valid_tokens() {
        let existing = "# a header someone added\naaa # ziglax\n";
        let managed = BTreeSet::from(["ziglax".to_owned()]);
        let grants = BTreeMap::from([("ziglax".to_owned(), grant("aaa", "viewer"))]);
        let out = tokens_file(existing, &managed, &grants);
        assert_eq!(out, "aaa # ziglax\n");
        assert!(!out.lines().any(|l| l.trim_start().starts_with('#')));
    }

    /// Same projection, same bytes — otherwise "rewrite on every boot" would
    /// churn the file and restart the gateway for nothing.
    #[test]
    fn rewriting_is_a_fixed_point() {
        let managed = BTreeSet::from(["ziglax".to_owned(), "magis".to_owned()]);
        let grants = BTreeMap::from([
            ("magis".to_owned(), grant("mmm", "viewer")),
            ("ziglax".to_owned(), grant("zzz", "editor")),
        ]);
        let once = tokens_file("bbb # nocturnal-bot\n", &managed, &grants);
        let twice = tokens_file(&once, &managed, &grants);
        assert_eq!(once, twice, "a second pass changed the file");
        assert_eq!(once, "bbb # nocturnal-bot\nmmm # magis\nzzz # ziglax\n");
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

        let managed = BTreeSet::from(["ziglax".to_owned()]);
        let grants = BTreeMap::from([("ziglax".to_owned(), grant("zzz", "editor"))]);

        let first = materialize(&paths, &managed, &grants).expect("materialize");
        assert!(first.tokens_rewritten);
        assert_eq!(first.files_written, 4);

        let second = materialize(&paths, &managed, &grants).expect("materialize again");
        assert_eq!(
            second,
            Report::default(),
            "a second run must write nothing at all"
        );

        // Revoke: the grant goes, the managed name stays.
        let after = materialize(&paths, &managed, &BTreeMap::new()).expect("revoke");
        assert!(after.tokens_rewritten);
        assert_eq!(after.files_removed, 4);
        assert_eq!(
            std::fs::read_to_string(&paths.tokens).expect("read"),
            "bbb # nocturnal-bot\n",
            "the service token outlived the member"
        );
        for name in user_file_names("ziglax") {
            assert!(!paths.perses_dir.join(name).exists());
        }
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
        let grants = BTreeMap::from([("magis".to_owned(), grant("mmm", "viewer"))]);
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
