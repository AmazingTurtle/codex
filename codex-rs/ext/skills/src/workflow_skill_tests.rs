//! Upgrading embedded skills replaces retired system entries without touching user skills.
use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn upgrading_workflow_skill_cache_preserves_user_skills_and_retires_the_old_entry() {
    let codex_home = tempfile::tempdir().expect("temporary home");
    let cwd = tempfile::tempdir().expect("temporary project");
    write_user_skill(
        &codex_home,
        ".system/workflow-authoring",
        "workflow-authoring",
        "Old bundled authoring reference",
    );
    write_user_skill(
        &codex_home,
        "notes",
        "project-notes",
        "Keep this user-owned skill",
    );
    let system = codex_home.path().join("skills/.system");
    fs::write(
        system.join(".codex-system-skills.marker"),
        "old-embedded-fingerprint",
    )
    .expect("seed the installed old bundle");
    let user_path = codex_home.path().join("skills/notes/SKILL.md");
    let user_source = fs::read(&user_path).expect("read the user-owned skill");

    let service = HostSkillsService::new(
        codex_home.path().abs(),
        /*bundled_skills_enabled*/ true,
    );
    let outcome =
        skills_for_config_with_stack(&service, &cwd, &config_stack(&codex_home, ""), &[]).await;
    let workflow = outcome
        .skills
        .iter()
        .find(|skill| skill.name == "dynamic-workflow")
        .expect("discover the installed public workflow skill");
    assert_eq!(
        workflow.path_to_skills_md,
        PathUri::from_abs_path(
            &dunce::canonicalize(system.join("dynamic-workflow/SKILL.md"))
                .expect("canonical installed skill path")
                .abs()
        )
    );
    assert_eq!(
        (
            outcome
                .skills
                .iter()
                .any(|skill| skill.name == "workflow-authoring"),
            system.join("workflow-authoring").exists(),
            outcome
                .skills
                .iter()
                .any(|skill| skill.name == "project-notes"),
            fs::read(user_path).expect("read the retained user-owned skill"),
        ),
        (false, false, true, user_source)
    );
    assert!(
        outcome.errors.is_empty(),
        "normal discovery must succeed: {:?}",
        outcome.errors
    );
    let reference = fs::read_to_string(system.join("dynamic-workflow/references/authoring.md"))
        .expect("the installed entry's authoring link resolves");
    assert!(reference.contains("workflow.meta({"));
    assert!(
        system
            .join("dynamic-workflow/references/repair-loop.js")
            .is_file()
    );
}
