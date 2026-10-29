use super::*;
use pretty_assertions::assert_eq;

fn source(body: &str) -> String {
    format!(
        "workflow.meta({{name:'revision',description:'Static revision analysis',phases:[]}});\n{body}"
    )
}

#[test]
fn safe_append_keeps_exact_unreferenced_prefix_and_metadata_edits() {
    let original = source("const first = await workflow.agent('first');\nreturn first;");
    let revised = source(
        "const first = await workflow.agent('first');\nconst next = await workflow.agent('second');\nreturn [first,next];",
    );
    assert_eq!(
        analyze_revision(&original, &revised, /*args_changed*/ false).unwrap(),
        RevisionAnalysis {
            reusable_statements: 1,
            original_statements: 2,
            revised_statements: 3,
            invalidation_reason: "Executable statement 2 changed".into(),
        }
    );
    let metadata_only = original.replace("Static revision analysis", "An updated description");
    assert_eq!(
        analyze_revision(&original, &metadata_only, /*args_changed*/ false)
            .unwrap()
            .reusable_statements,
        2
    );
    assert_eq!(
        analyze_revision(&original, &revised, /*args_changed*/ true)
            .unwrap()
            .reusable_statements,
        0
    );
}

#[test]
fn changed_hoisted_bindings_include_typeof_captures_and_aliases() {
    for prefix in [
        "const value = typeof helper; await workflow.agent(value);",
        "const get = () => helper; await workflow.agent('first');",
        "const alias = helper; await workflow.agent('first');",
    ] {
        let original = source(&format!("{prefix}\nfunction helper() {{ return 'old'; }}"));
        let revised = source(&format!("{prefix}\nfunction helper() {{ return 'new'; }}"));
        assert_eq!(
            analyze_revision(&original, &revised, /*args_changed*/ false)
                .unwrap()
                .reusable_statements,
            0
        );
    }
    let old = source("await workflow.agent(typeof later);\nif (true) { var later = 'old'; }");
    let new = old.replace("var later", "var other");
    assert_eq!(
        analyze_revision(&old, &new, /*args_changed*/ false)
            .unwrap()
            .reusable_statements,
        0
    );
    let old = source(r"await workflow.agent(`${helper}`); function \u0068elper() {return 'old';}");
    let new = old.replace("'old'", "'new'");
    assert_eq!(
        analyze_revision(&old, &new, /*args_changed*/ false)
            .unwrap()
            .reusable_statements,
        0
    );
    let old = source("await workflow.agent(typeof later); const {later} = workflow.args;");
    let new = old.replace("{later}", "{other}");
    assert_eq!(
        analyze_revision(&old, &new, /*args_changed*/ false)
            .unwrap()
            .reusable_statements,
        0
    );
}

#[test]
fn combinator_statement_is_atomic_and_escaping_work_stops_reuse() {
    let prefix = "const first = await workflow.agent('first');";
    let old = source(&format!(
        "{prefix}\nconst batch = await workflow.parallel([async () => workflow.agent('old')]);\nreturn batch;"
    ));
    let new = old.replace("agent('old')", "agent('new')");
    assert_eq!(
        analyze_revision(&old, &new, /*args_changed*/ false)
            .unwrap()
            .reusable_statements,
        1
    );
    for escape in [
        "void workflow.agent('detached');",
        "setTimeout(() => workflow.agent('timer'),0);",
        "Promise.resolve().then(() => workflow.agent('later'));",
        "const launch = workflow.agent;",
        "dynamicCall();",
    ] {
        let old = source(&format!("{prefix}\n{escape}\nreturn 1;"));
        let new = old.replace("return 1", "return 2");
        assert_eq!(
            analyze_revision(&old, &new, /*args_changed*/ false)
                .unwrap()
                .reusable_statements,
            1,
            "{escape}"
        );
    }
    let old = source(
        "const box={get promise(){return workflow.agent('detached')}}; box.promise; return 1;",
    );
    let new = old.replace("return 1", "return 2");
    assert_eq!(
        analyze_revision(&old, &new, /*args_changed*/ false)
            .unwrap()
            .reusable_statements,
        0
    );
}

#[test]
fn instrumentation_preserves_lexical_statements_without_exposing_a_checkpoint_function() {
    let compiled = compile_script(&source(
        "const result = await workflow.agent('first');\nreturn result;",
    ))
    .unwrap();
    let program = instrument_revision_script(&compiled, /*frontier*/ 1).unwrap();
    assert_eq!(program.statement_count, 2);
    assert!(program.body.contains("yield 0;\nconst result"));
    assert!(
        program
            .body
            .contains(&format!("yield 1;\n{}(1);\nreturn result;", program.marker))
    );
    assert!(!program.body.contains("yield 2;"));
    assert!(!program.body.contains("checkpoint("));
}
