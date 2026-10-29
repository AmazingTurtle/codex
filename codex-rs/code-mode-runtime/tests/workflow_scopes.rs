//! Workflow bindings propagate through V8 continuations without changing general execution.

use codex_code_mode_protocol::CodeModeCellExecutionProfile;
use codex_code_mode_protocol::CodeModeSessionCellExecutionLimits;
use codex_code_mode_runtime::ExecuteRequest;
use codex_code_mode_runtime::FunctionCallOutputContentItem;
use codex_code_mode_runtime::InProcessCodeModeSession;
use codex_code_mode_runtime::NoopCodeModeSessionDelegate;
use codex_code_mode_runtime::RuntimeResponse;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;

async fn output(profile: CodeModeCellExecutionProfile, source: &str) -> Result<Value, String> {
    let session = InProcessCodeModeSession::with_limits(CodeModeSessionCellExecutionLimits {
        profile,
        ..Default::default()
    });
    let started = session
        .execute(
            ExecuteRequest {
                tool_call_id: "workflow-context".into(),
                enabled_tools: Vec::new(),
                source: source.into(),
                yield_time_ms: Some(/*value*/ 5_000),
                max_output_tokens: None,
            },
            Arc::new(NoopCodeModeSessionDelegate),
            /*preempt*/ None,
        )
        .await
        .map_err(|error| format!("start real V8 cell: {error}"))?;
    let response = started
        .initial_response()
        .await
        .map_err(|error| format!("finish real V8 cell: {error}"))?;
    let RuntimeResponse::Result {
        content_items,
        error_text: None,
        ..
    } = response
    else {
        return Err(format!("unexpected V8 response: {response:?}"));
    };
    let [FunctionCallOutputContentItem::InputText { text }] = content_items.as_slice() else {
        return Err(format!("expected one JSON output: {content_items:?}"));
    };
    serde_json::from_str(text).map_err(|error| format!("decode V8 JSON output: {error}"))
}

#[tokio::test]
async fn general_execution_has_no_workflow_bindings_and_keeps_time_and_random() -> Result<(), String>
{
    assert_eq!(
        output(
            CodeModeCellExecutionProfile::General,
            r#"
text({get:typeof __workflowContextGet,set:typeof __workflowContextSet,
 date:typeof Date.now(),random:typeof Math.random()});
"#
        )
        .await?,
        json!({"get":"undefined","set":"undefined","date":"number","random":"number"})
    );
    Ok(())
}

#[tokio::test]
async fn workflow_context_survives_native_await_fulfillment_and_timer_continuations()
-> Result<(), String> {
    assert_eq!(
        output(
            CodeModeCellExecutionProfile::Workflow,
            r#"
const read = __workflowContextGet;
const enter = __workflowContextSet;
enter({name:'root'});
const releases = {};
function start(name) {
  const previous = enter({name});
  try {
    return (async () => {
      const before = read().name;
      await new Promise(resolve => { releases[name] = resolve; });
      await Promise.resolve('already fulfilled');
      await {then(resolve) { resolve('thenable'); }};
      await new Promise(resolve => setTimeout(resolve, 0));
      return {before,after:read().name};
    })();
  } finally { enter(previous); }
}
const first = start('first');
const second = start('second');
releases.second();
await Promise.resolve();
releases.first();
const values = await Promise.all([first,second]);
text({values,root:read().name});
"#
        )
        .await?,
        json!({"values":[{"before":"first","after":"first"},{"before":"second","after":"second"}],"root":"root"})
    );
    Ok(())
}

#[tokio::test]
async fn workflow_profile_rejects_dynamic_import_even_through_generated_code() -> Result<(), String>
{
    let result = output(
        CodeModeCellExecutionProfile::Workflow,
        r#"
try {
  await (new Function("return import('data:text/javascript,export default 1')"))();
} catch(error) { text({error:String(error)}); }
"#,
    )
    .await?;
    assert!(
        result["error"]
            .as_str()
            .is_some_and(|error| error.contains("workflow scripts cannot load modules"))
    );
    Ok(())
}
