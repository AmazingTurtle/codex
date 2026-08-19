use super::*;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use tempfile::tempdir;
use tokio::fs as tokio_fs;

#[tokio::test]
async fn build_memory_tool_developer_instructions_renders_embedded_template() {
    let temp = tempdir().unwrap();
    let codex_home = AbsolutePathBuf::from_absolute_path(temp.path()).unwrap();
    let memories_dir = codex_home.join("memories");
    tokio_fs::create_dir_all(&memories_dir).await.unwrap();
    tokio_fs::write(
        memories_dir.join("memory_summary.md"),
        "Short memory summary for tests.",
    )
    .await
    .unwrap();

    let instructions = build_memory_tool_developer_instructions(&codex_home)
        .await
        .unwrap();

    assert!(instructions.contains(&format!(
        "- {}/memory_summary.md (already provided below; do NOT open again)",
        memories_dir.display()
    )));
    assert!(instructions.contains("Short memory summary for tests."));
    assert_eq!(
        instructions
            .matches("========= MEMORY_SUMMARY BEGINS =========")
            .count(),
        1
    );
}

#[cfg(unix)]
#[tokio::test]
async fn build_memory_tool_developer_instructions_rejects_memory_summary_symlink() {
    let temp = tempdir().unwrap();
    let codex_home = AbsolutePathBuf::from_absolute_path(temp.path()).unwrap();
    let memories_dir = codex_home.join("memories");
    tokio_fs::create_dir_all(&memories_dir).await.unwrap();

    let outside_secret = temp.path().join("outside_secret.txt");
    tokio_fs::write(&outside_secret, "SYMLINKED OUTSIDE MEMORY CONTENT")
        .await
        .unwrap();
    std::os::unix::fs::symlink(&outside_secret, memories_dir.join("memory_summary.md")).unwrap();

    let instructions = build_memory_tool_developer_instructions(&codex_home).await;

    assert_eq!(instructions, None);
}

#[cfg(unix)]
#[tokio::test]
async fn build_memory_tool_developer_instructions_rejects_memories_root_symlink() {
    let temp = tempdir().unwrap();
    let codex_home_path = temp.path().join("home");
    let codex_home = AbsolutePathBuf::from_absolute_path(&codex_home_path).unwrap();
    tokio_fs::create_dir_all(&codex_home_path).await.unwrap();

    let outside_memories = temp.path().join("outside_memories");
    tokio_fs::create_dir_all(&outside_memories).await.unwrap();
    tokio_fs::write(
        outside_memories.join("memory_summary.md"),
        "SYMLINKED OUTSIDE MEMORY CONTENT",
    )
    .await
    .unwrap();
    std::os::unix::fs::symlink(&outside_memories, codex_home.join("memories")).unwrap();

    let instructions = build_memory_tool_developer_instructions(&codex_home).await;

    assert_eq!(instructions, None);
}
