use anyhow::Result;
use pretty_assertions::assert_eq;
use std::fs;

#[test]
fn report_generates_retained_html_without_login() -> Result<()> {
    let home = tempfile::tempdir()?;
    let output = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin(
        codex_product_info::CLI_NAME,
    )?)
    .env(codex_product_info::HOME_ENV, home.path())
    .arg("report")
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let output = String::from_utf8(output)?;
    let lines: Vec<_> = output.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("[Open telemetry dashboard](file://"));
    let path = std::path::Path::new(lines[1]);
    assert_eq!(path.file_name().unwrap(), "report.html");
    let html = fs::read_to_string(path)?;
    assert!(html.contains("schemaVersion"));
    assert!(html.contains("<script"));
    fs::remove_dir_all(path.parent().unwrap())?;
    Ok(())
}
