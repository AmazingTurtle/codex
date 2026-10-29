//! Private compiler boundaries execute in real V8, outside authored lexical scope.

use codex_code_mode_protocol::CodeModeCellExecutionProfile;
use codex_code_mode_protocol::CodeModeSessionCellExecutionLimits;
use codex_code_mode_runtime::ExecuteRequest;
use codex_code_mode_runtime::FunctionCallOutputContentItem;
use codex_code_mode_runtime::InProcessCodeModeSession;
use codex_code_mode_runtime::NoopCodeModeSessionDelegate;
use codex_code_mode_runtime::RuntimeResponse;
use codex_dynamic_workflows::compile_script;
use codex_dynamic_workflows::instrument_revision_script;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;

async fn run(body: &str, frontier: usize, reject_at: Option<usize>) -> Result<Value, String> {
    let compiled = compile_script(&format!(
        "workflow.meta({{name:'private-boundary',description:'Exercise the real compiler iterator'}});\n{body}"
    ))?;
    let program = instrument_revision_script(&compiled, frontier)?;
    let source = format!(
        r#"
{}
const enter = __workflowContextSet;
const encode = JSON.stringify;
const decode = JSON.parse;
const calls = [];
const checkpoints = [];
const scope = {{key:'root'}};
const scoped = (scope, task) => {{ const previous = enter(scope); try {{ return task(); }} finally {{ enter(previous); }} }};
const workflow = Object.freeze({{agent: async prompt => {{ calls.push(prompt); return prompt; }} }});
let result, error;
try {{
  result = await __workflowRevisionRuntime(workflow, {}, scope, {}, scoped,
    async checkpoint => {{
      const wire = decode(encode(checkpoint));
      checkpoints.push(wire.statementIndex);
      if (wire.statementIndex === {}) throw new Error('cached prefix receipt is missing');
    }}, async () => {{}}, () => 0);
}} catch (caught) {{ error = String(caught); }}
delete Object.prototype.toJSON;
text({{result,error,calls,checkpoints}});
"#,
        include_str!("../../src/bootstrap_revision.js"),
        serde_json::to_string(&program.body).map_err(|error| error.to_string())?,
        json!({"statementCount":program.statement_count,"frontier":program.frontier,"marker":program.marker}),
        json!(reject_at)
    );
    execute(CodeModeCellExecutionProfile::Workflow, source).await
}

async fn execute(profile: CodeModeCellExecutionProfile, source: String) -> Result<Value, String> {
    let session = InProcessCodeModeSession::with_limits(CodeModeSessionCellExecutionLimits {
        profile,
        ..Default::default()
    });
    let started = session
        .execute(
            ExecuteRequest {
                tool_call_id: "private-revision-boundary".into(),
                enabled_tools: Vec::new(),
                source,
                yield_time_ms: Some(/*value*/ 5_000),
                max_output_tokens: None,
            },
            Arc::new(NoopCodeModeSessionDelegate),
            /*preempt*/ None,
        )
        .await?;
    let response = started.initial_response().await?;
    let RuntimeResponse::Result {
        content_items,
        error_text: None,
        ..
    } = response
    else {
        return Err(format!("unexpected V8 response: {response:?}"));
    };
    let [FunctionCallOutputContentItem::InputText { text }] = content_items.as_slice() else {
        return Err(format!(
            "expected one structured V8 output: {content_items:?}"
        ));
    };
    serde_json::from_str(text).map_err(|error| error.to_string())
}

#[tokio::test]
async fn private_checkpoint_is_inaccessible_and_a_rejected_frontier_never_dispatches_suffix()
-> Result<(), String> {
    let actual = run(r#"
const hidden = [typeof __workflowRevisionRuntime, typeof checkpoint, typeof iterator];
if (hidden.some(value => value !== 'undefined')) throw new Error('private compiler capability leaked');
await workflow.agent('suffix must remain undispatched');
return 'done';
"#, /*frontier*/ 2, Some(/*value*/ 2)).await?;
    assert_eq!(
        actual,
        json!({
            "error":"Error: cached prefix receipt is missing", "calls":[], "checkpoints":[0,1,2],
        })
    );
    Ok(())
}

#[tokio::test]
async fn generator_checkpoints_preserve_lexical_bindings_and_return_values() -> Result<(), String> {
    let actual = run("const first = await workflow.agent('first');\nconst second = first + '-second';\nreturn {first,second};", /*frontier*/ 1, /*reject_at*/ None).await?;
    assert_eq!(
        actual,
        json!({"result":{"first":"first","second":"first-second"},"calls":["first"],"checkpoints":[0,1,3]})
    );
    Ok(())
}

#[tokio::test]
async fn authored_generator_prototype_changes_cannot_forge_private_boundaries() -> Result<(), String>
{
    let actual = run(r#"
Object.getPrototypeOf(Object.getPrototypeOf((async function* () {})())).next = async () => ({value:999,done:false});
await workflow.agent('suffix must remain undispatched');
"#, /*frontier*/ 1, Some(/*value*/ 1)).await?;
    assert_eq!(
        actual,
        json!({"error":"Error: cached prefix receipt is missing","calls":[],"checkpoints":[0,1]})
    );
    Ok(())
}

#[tokio::test]
async fn authored_math_changes_cannot_forge_the_completion_checkpoint() -> Result<(), String> {
    let actual = run(
        "Math.min = () => 999; return 'done';",
        /*frontier*/ 0,
        /*reject_at*/ None,
    )
    .await?;
    assert_eq!(
        actual,
        json!({"result":"done","calls":[],"checkpoints":[0,2]})
    );
    Ok(())
}

#[tokio::test]
async fn authored_object_serialization_cannot_forge_a_private_checkpoint_packet()
-> Result<(), String> {
    let actual = run(
        "Object.prototype.toJSON = () => ({statementIndex:999}); return 'done';",
        /*frontier*/ 0,
        /*reject_at*/ None,
    )
    .await?;
    assert_eq!(
        actual,
        json!({"result":"done","calls":[],"checkpoints":[0,2]})
    );
    Ok(())
}

#[tokio::test]
async fn a_fresh_async_suffix_keeps_ordinary_microtask_order_and_its_own_arguments()
-> Result<(), String> {
    let body = "let value=0; Promise.resolve().then(()=>{value=1;}); return {value,arguments:arguments.length};";
    let ordinary = execute(CodeModeCellExecutionProfile::General, format!(
        "const AsyncFunction=Object.getPrototypeOf(async function(){{}}).constructor; text(await (new AsyncFunction('workflow', {}))({{}}));",
        serde_json::to_string(&format!("\"use strict\";\n{body}")).map_err(|error| error.to_string())?,
    )).await?;
    let actual = run(body, /*frontier*/ 0, /*reject_at*/ None).await?;
    assert_eq!(actual["result"], ordinary);
    assert_eq!(
        actual,
        json!({"result":{"value":0,"arguments":1},"calls":[],"checkpoints":[0,3]})
    );
    Ok(())
}

#[tokio::test]
async fn an_escaped_authored_binding_cannot_collide_with_the_private_marker() -> Result<(), String>
{
    let actual = run(
        r"const \u005f_workflowStatement='authored'; return \u005f_workflowStatement;",
        /*frontier*/ 0,
        /*reject_at*/ None,
    )
    .await?;
    assert_eq!(
        actual,
        json!({"result":"authored","calls":[],"checkpoints":[0,2]})
    );
    Ok(())
}

#[tokio::test]
async fn forged_synchronous_provenance_cannot_open_the_private_prefix_gate() -> Result<(), String> {
    // Build the lookup dynamically so it does not select another collision-free
    // binding. Direct eval remains an ambiguous source boundary for cache reuse.
    let actual = run(
        r#"
eval('__workflow' + 'Statement')(2);
await workflow.agent('must remain undispatched');
"#,
        /*frontier*/ 1,
        Some(/*value*/ 1),
    )
    .await?;
    assert_eq!(
        actual,
        json!({"error":"Error: workflow suffix provenance is invalid","calls":[],"checkpoints":[0]})
    );
    Ok(())
}
