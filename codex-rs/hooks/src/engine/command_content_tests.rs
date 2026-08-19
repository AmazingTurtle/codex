use std::fs;

use pretty_assertions::assert_eq;
use tempfile::tempdir;

use super::command_content_target;
use super::hook_command_content_digest;

#[test]
fn hashes_interpreter_script_contents() {
    let temp = tempdir().expect("create temp dir");
    let script = temp.path().join("check.sh");
    fs::write(&script, "#!/bin/sh\nexit 0\n").expect("write script");

    let before = hook_command_content_digest("sh check.sh", temp.path())
        .expect("script content should be hashed");
    fs::write(&script, "#!/bin/sh\nprintf changed\n").expect("rewrite script");
    let after = hook_command_content_digest("sh check.sh", temp.path())
        .expect("changed script content should be hashed");

    assert_ne!(before, after);
}

#[test]
fn extracts_literal_script_targets() {
    assert_eq!(command_content_target("sh scripts/check.sh"), Some("scripts/check.sh"));
    assert_eq!(
        command_content_target("bash -e 'scripts/check with space.sh'"),
        Some("scripts/check with space.sh")
    );
    assert_eq!(
        command_content_target("./scripts/check.sh --flag"),
        Some("./scripts/check.sh")
    );
}

#[test]
fn ignores_dynamic_or_inline_script_targets() {
    let temp = tempdir().expect("create temp dir");
    fs::write(temp.path().join("check.sh"), "#!/bin/sh\nexit 0\n").expect("write script");

    assert_eq!(command_content_target("sh -c 'printf ok'"), None);
    assert_eq!(
        hook_command_content_digest("sh $HOOK_SCRIPT", temp.path()),
        None
    );
}
