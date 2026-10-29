// The compiler yields these boundaries; authored source receives neither the
// iterator nor a checkpoint callback. Only the host can open suffix admission.
const __workflowRevisionRuntime = (() => {
  const FunctionConstructor = Function;
  const iteratorNext = Object.getPrototypeOf(Object.getPrototypeOf((async function* () {})())).next;
  const apply = Reflect.apply;
  const create = Object.create;
  return async (workflow, source, scope, configuration, scoped, checkpoint, flushObservations, pendingWork) => {
    const {statementCount, frontier, marker} = configuration;
    let suffixOpen = false;
    let nextSuffixStatement = frontier;
    const mark = index => {
      if (!suffixOpen || index !== nextSuffixStatement || index >= statementCount) {
        throw new Error("workflow suffix provenance is invalid");
      }
      scope.statementIndex = index;
      nextSuffixStatement++;
    };
    // Keep the authored function's own arguments unchanged. The private marker
    // is a closure binding and carries no permission to open suffix admission.
    const factory = new FunctionConstructor(marker, `return async function* (workflow) {"use strict";\n${source}\n};`);
    const iterator = factory(mark)(workflow);
    return scoped(scope, async () => {
      let expected = 0;
      const boundary = async (statementIndex, complete) => {
        const packet = create(null);
        packet.operation = "checkpoint";
        packet.scope = scope.key;
        packet.statementIndex = statementIndex;
        packet.complete = complete;
        packet.pendingWork = pendingWork();
        await checkpoint(packet);
      };
      for (;;) {
        const item = await apply(iteratorNext, iterator, []);
        if (item.done) {
          await flushObservations();
          const statementIndex = scope.statementIndex < statementCount ? scope.statementIndex + 1 : statementCount;
          await boundary(statementIndex, true);
          return item.value;
        }
        if (item.value !== expected || expected > frontier) {
          throw new Error("workflow compiler checkpoint sequence is invalid");
        }
        // Any effects created in the preceding statement retain that statement's
        // attribution until its observation writes have reached durable storage.
        await flushObservations();
        scope.statementIndex = expected;
        await boundary(expected, false);
        if (expected === frontier) suffixOpen = true;
        expected++;
      }
    });
  };
})();
