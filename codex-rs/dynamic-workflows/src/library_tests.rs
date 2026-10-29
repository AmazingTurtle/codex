use super::*;
use pretty_assertions::assert_eq;

fn source(name: &str) -> String {
    format!(
        "workflow.meta({{name: {name:?}, description: 'Inspect source', phases: [{{title: 'Review'}}]}});\nreturn null;"
    )
}

#[test]
fn saved_metadata_is_discovered_without_executing_and_scope_ids_stay_distinct() {
    let project = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    let library = WorkflowLibrary::new(
        AbsolutePathBuf::from_absolute_path(project.path()).unwrap(),
        AbsolutePathBuf::from_absolute_path(user.path()).unwrap(),
    );
    let user_saved = library
        .save("review", &source("review"), WorkflowScope::User)
        .unwrap();
    let project_saved = library
        .save("review", &source("review"), WorkflowScope::Project)
        .unwrap();
    assert_eq!(library.resolve("review").unwrap(), project_saved);
    assert_eq!(library.resolve("user:review").unwrap(), user_saved);
    assert_eq!(
        library.list(0, MAX_PAGE_SIZE).unwrap(),
        vec![project_saved, user_saved.clone()]
    );
    library.delete("project:review").unwrap();
    assert_eq!(library.resolve("review").unwrap(), user_saved);
}

#[test]
fn external_metadata_names_are_portable_and_ambiguous_aliases_need_scoped_ids() {
    let project = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    let library = WorkflowLibrary::new(
        AbsolutePathBuf::from_absolute_path(project.path()).unwrap(),
        AbsolutePathBuf::from_absolute_path(user.path()).unwrap(),
    );
    let directory = project.path().join(".codex/workflows");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("first.js"), source("review")).unwrap();
    let first = library.resolve("review").unwrap();
    assert_eq!(
        (
            first.id.as_str(),
            first.name.as_str(),
            first.metadata.as_ref().unwrap().name.as_str(),
            first.read_only
        ),
        ("project:first", "first", "review", false)
    );
    fs::write(directory.join("second.js"), source("review")).unwrap();
    assert!(library.resolve("review").unwrap_err().contains("ambiguous"));
    assert_eq!(library.resolve("project:first").unwrap(), first);
    assert_eq!(library.list(1, 1).unwrap()[0].id, "project:second");
}

#[test]
fn legacy_and_invalid_scripts_remain_inspectable_but_cannot_resolve_for_execution() {
    let project = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    let library = WorkflowLibrary::new(
        AbsolutePathBuf::from_absolute_path(project.path()).unwrap(),
        AbsolutePathBuf::from_absolute_path(user.path()).unwrap(),
    );
    let directory = project.path().join(".codex/workflows");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("legacy.js"),
        "await workflow.agent('old', {message: 'Inspect source'});",
    )
    .unwrap();
    fs::write(
        directory.join("broken.js"),
        "workflow.meta({name: compute()});",
    )
    .unwrap();
    let entries = library.list(0, 50).unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|entry| (entry.id.as_str(), entry.read_only, entry.format_version))
            .collect::<Vec<_>>(),
        vec![("project:broken", true, 0), ("project:legacy", true, 1)]
    );
    assert!(entries.iter().all(|entry| entry.read_only_reason.is_some()));
    assert!(library.resolve("project:legacy").is_err());
    assert!(library.resolve("project:broken").is_err());
    assert!(
        library
            .save("legacy", "return null;", WorkflowScope::Project)
            .is_err()
    );
    let renamed = library
        .save("renamed", &source("review"), WorkflowScope::Project)
        .unwrap();
    assert_eq!(library.resolve("review").unwrap(), renamed);
    assert!(library.resolve("project:../escape").is_err());
}

#[test]
fn host_read_denial_precedes_source_inspection() {
    let project = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    let library = WorkflowLibrary::new(
        AbsolutePathBuf::from_absolute_path(project.path()).unwrap(),
        AbsolutePathBuf::from_absolute_path(user.path()).unwrap(),
    );
    let target = project.path().join(".codex/workflows/private.js");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, source("private")).unwrap();
    let mut checked = Vec::new();
    let error = library
        .resolve_with_read_guard("private", |path| {
            checked.push(path.clone());
            Err("caller may not read this source".into())
        })
        .unwrap_err();
    assert_eq!(
        (error.as_str(), checked),
        (
            "caller may not read this source",
            vec![AbsolutePathBuf::from_absolute_path(target).unwrap()]
        )
    );
}

#[test]
fn empty_and_oversized_files_do_not_hide_healthy_workflows_or_leak_source_into_listing() {
    let project = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    let library = WorkflowLibrary::new(
        AbsolutePathBuf::from_absolute_path(project.path()).unwrap(),
        AbsolutePathBuf::from_absolute_path(user.path()).unwrap(),
    );
    let healthy = library
        .save("healthy", &source("healthy"), WorkflowScope::Project)
        .unwrap();
    let directory = project.path().join(".codex/workflows");
    fs::write(directory.join("empty.js"), "").unwrap();
    fs::write(directory.join("large.js"), "x".repeat(MAX_SOURCE_BYTES + 1)).unwrap();
    let listed = library.list(0, 50).unwrap();
    assert_eq!(listed[1], healthy);
    for entry in [&listed[0], &listed[2]] {
        assert_eq!(
            (
                entry.read_only,
                entry.format_version,
                entry.metadata.as_ref(),
                entry.source.as_str()
            ),
            (true, 0, None, "")
        );
        assert!(entry.read_only_reason.is_some());
        assert!(library.read(&entry.id).is_err());
        assert!(library.resolve(&entry.id).is_err());
    }
    assert_eq!(library.resolve("healthy").unwrap(), healthy);
}
