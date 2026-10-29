//! Workflow-only access to V8's continuation-preserved embedder data.
//!
//! V8 carries this value across native async/await and promise reactions. The
//! workflow bootstrap captures and removes these bindings before running user code.

use super::globals::helper_function;
use super::globals::set_global;
use super::value::throw_type_error;

pub(super) fn install(scope: &mut v8::PinScope<'_, '_>) -> Result<(), String> {
    let global = scope.get_current_context().global(scope);
    let get = helper_function(scope, "__workflowContextGet", get)?;
    let set = helper_function(scope, "__workflowContextSet", set)?;
    set_global(scope, global, "__workflowContextGet", get.into())?;
    set_global(scope, global, "__workflowContextSet", set.into())
}

fn get(
    scope: &mut v8::PinScope<'_, '_>,
    _args: v8::FunctionCallbackArguments,
    mut result: v8::ReturnValue<v8::Value>,
) {
    result.set(scope.get_continuation_preserved_embedder_data());
}

fn set(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut result: v8::ReturnValue<v8::Value>,
) {
    if args.length() != 1 {
        throw_type_error(scope, "workflow context requires one value");
        return;
    }
    let previous = scope.get_continuation_preserved_embedder_data();
    scope.set_continuation_preserved_embedder_data(args.get(0));
    result.set(previous);
}
