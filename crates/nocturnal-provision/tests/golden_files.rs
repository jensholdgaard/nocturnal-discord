//! Byte-compatibility with dpsbot.
//!
//! The fixtures in `tests/golden/` are not hand-written: they were copied off
//! the observability VM, where the Python bot produced them for a real member.
//! Perses and the root-owned systemd `.path` units watching that directory
//! must not be able to tell which bot wrote a file — a stray newline or a
//! changed key order is a silent access change for whoever it belongs to.

use nocturnal_provision::user_files;

/// The member whose real files were captured, and the role they held.
const USER: &str = "magis";
const ROLE: &str = "viewer";

fn golden(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading golden fixture {}: {e}", path.display()))
}

#[test]
fn every_provisioning_file_matches_what_dpsbot_wrote() {
    let produced = user_files(USER, ROLE);
    assert_eq!(produced.len(), 4, "the file set itself changed");

    for (name, contents) in produced {
        let expected = golden(&name);
        assert_eq!(
            contents, expected,
            "\n{name} differs from dpsbot's output.\n\
             --- ours ---\n{contents}\n--- dpsbot ---\n{expected}"
        );
    }
}

/// The role appears twice in the RoleBinding — as part of the resource name
/// and as the granted role. An editor whose binding is still named
/// `viewer-<user>` would collide with nothing and quietly keep the old name.
#[test]
fn the_role_reaches_both_places_in_the_binding() {
    let files = user_files(USER, "editor");
    let (_, rb) = files
        .iter()
        .find(|(n, _)| n == "rb-magis.yaml")
        .expect("the role binding");
    assert!(rb.contains("name: editor-magis"), "resource name: {rb}");
    assert!(rb.contains("role: editor"), "granted role: {rb}");
    assert!(!rb.contains("viewer"), "the old role lingered: {rb}");
}

/// The personal project is `u-<user>` in three places that must agree, or the
/// datasource and owner binding land in a project that does not exist.
#[test]
fn the_personal_project_name_agrees_across_its_three_files() {
    let files = user_files(USER, ROLE);
    let get = |n: &str| {
        files
            .iter()
            .find(|(name, _)| name == n)
            .map(|(_, c)| c.clone())
            .unwrap_or_else(|| panic!("missing {n}"))
    };
    assert!(get("50-project-magis.yaml").contains("name: u-magis"));
    assert!(get("51-ds-magis.yaml").contains("project: u-magis"));
    assert!(get("52-rb-own-magis.yaml").contains("project: u-magis"));
}

/// `tokens.txt` is every member's bearer token, and it is rewritten from the
/// ledger on every boot and every `/dpstoken`. Replacing a file by rename takes
/// the *new* file's permissions, not the old one's, so a rewrite under a
/// different umask silently widened it from 0640 to 0664 on the live VM. The
/// mode is therefore set on the temp file before any content reaches it.
#[test]
fn a_rewritten_tokens_file_is_never_world_readable() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tokens.txt");
    // Start from a deliberately wide file: the rewrite must narrow it.
    std::fs::write(&path, "old\n").expect("seed");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).expect("chmod");

    nocturnal_provision::write_atomic_for_test(&path, "new\n").expect("write");

    let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o640,
        "got {mode:o}: other must not be able to read tokens"
    );
}
