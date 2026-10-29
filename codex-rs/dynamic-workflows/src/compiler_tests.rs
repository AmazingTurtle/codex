use super::*;
use pretty_assertions::assert_eq;

#[test]
fn canonical_metadata_is_extracted_without_executing_the_body() {
    let source = r#"// author
workflow.meta({name:'audit',description:'Check \'source\'',phases:[{title:'Review'}]});
throw new Error('must not execute');
return await workflow.agent('Review it');"#;
    let compiled = compile_script(source).unwrap();
    assert_eq!(
        compiled.metadata,
        ScriptMetadata {
            name: "audit".into(),
            description: "Check 'source'".into(),
            phases: vec![ScriptPhase {
                title: "Review".into()
            }],
        }
    );
    assert_eq!(compiled.body.lines().count(), source.lines().count());
    assert_eq!(compiled.body.len(), source.len());
    assert!(
        compiled
            .body
            .contains("throw new Error('must not execute')")
    );
    assert!(!compiled.body.contains("workflow.meta"));
}

#[test]
fn metadata_rejects_executable_or_ambiguous_forms() {
    for source in [
        "workflow.meta({name: sideEffect(),description:'x'}); return 1;",
        "workflow.meta({name:'x',description:`template`}); return 1;",
        "workflow.meta({name:'x',description:'x',...other}); return 1;",
        "workflow.meta({['name']:'x',description:'x'}); return 1;",
        "workflow.meta({name:'x',description:'x',name:'y'}); return 1;",
        "workflow.meta({name:'x',description:'x',phases:[,]}); return 1;",
        "workflow.meta({name:'x',description:'x',phases:[{title:'a'},{title:'a'}]}); return 1;",
        "workflow['meta']({name:'x',description:'x'}); return 1;",
        "workflow?.meta({name:'x',description:'x'}); return 1;",
        "workflow.meta?.({name:'x',description:'x'}); return 1;",
        "const x=1; workflow.meta({name:'x',description:'x'}); return 1;",
        "workflow.meta({name:'x',description:'x'}); workflow.meta({name:'x',description:'x'});",
        "workflow.meta({name:'x',description:'x'}); return import('file:///tmp/x');",
        "export const meta={name:'x',description:'x'}; return 1;",
        "export default async function(){return 1;}",
    ] {
        assert!(
            parse_script(source).is_err(),
            "accepted invalid workflow: {source}"
        );
    }
}

#[test]
fn unicode_and_javascript_string_escapes_are_decoded_statically() {
    let parsed = compile_script(r#"workflow.meta({name:"audit\u002d\x31",description:'\u{1f980}\uD83D\uDE80',phases:[{title:'研究'}]}); return 1;"#).unwrap();
    assert_eq!(
        parsed.metadata,
        ScriptMetadata {
            name: "audit-1".into(),
            description: "🦀🚀".into(),
            phases: vec![ScriptPhase {
                title: "研究".into()
            }]
        }
    );
}

#[test]
fn old_namespace_scripts_are_visible_but_never_compile_for_execution() {
    let source = "await workflow.publish(await workflow.agent('step',{message:'inspect'}));";
    assert_eq!(parse_script(source), Ok(ParsedScript::LegacyReadOnly));
    assert!(compile_script(source).unwrap_err().contains("read-only"));
}

#[test]
fn metadata_names_follow_the_saved_workflow_identifier_contract() {
    for name in ["", "two words", "研究", &"a".repeat(81)] {
        let source = format!(
            "workflow.meta({{name:{},description:'check'}}); return 1;",
            serde_json::to_string(name).unwrap()
        );
        assert!(
            compile_script(&source).is_err(),
            "accepted invalid workflow name: {name}"
        );
    }
    let source = format!(
        "workflow.meta({{name:{},description:'check'}}); return 1;",
        serde_json::to_string(&"a".repeat(80)).unwrap()
    );
    assert!(compile_script(&source).is_ok());
}
