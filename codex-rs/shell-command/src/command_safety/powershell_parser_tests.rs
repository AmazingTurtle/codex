use std::collections::HashMap;

use pretty_assertions::assert_eq;

use super::PowershellParseOutcome;
use super::parse_with_cached_process;
use super::script_contains_parse_time_construct;

#[test]
fn parse_time_constructs_are_rejected_before_parser_spawn() {
    let mut parser_processes = HashMap::new();

    assert_eq!(
        parse_with_cached_process(
            &mut parser_processes,
            "codex-missing-powershell-parser",
            "configuration HostBaseline { Import-DscResource -ModuleName .\\HostBaseline.psd1 }",
        ),
        PowershellParseOutcome::Unsupported
    );
    assert!(parser_processes.is_empty());
}

#[test]
fn parse_time_construct_detection_covers_alternate_spellings() {
    assert!(script_contains_parse_time_construct(
        "#requires -Modules HostBaseline\nGet-ChildItem"
    ));
    assert!(script_contains_parse_time_construct(
        "using module .\\HostBaseline.psm1\nGet-ChildItem"
    ));
    assert!(script_contains_parse_time_construct(
        "configura`tion HostBaseline {}"
    ));
}

#[test]
fn parse_time_construct_detection_ignores_inert_text() {
    assert!(!script_contains_parse_time_construct(
        "Write-Output 'configuration'; Write-Output \"using module\"; Get-ChildItem # configuration"
    ));
}
