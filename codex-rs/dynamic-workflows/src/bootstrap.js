const __workflowStart = (() => {
  const bridge = tools.workflow_host;
  const read = globalThis.__workflowContextGet;
  const enter = globalThis.__workflowContextSet;
  const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
  const create = Object.create;
  const ownKeys = Object.keys;
  const encodeKey = encodeURIComponent;
  if (typeof read !== "function" || typeof enter !== "function") {
    throw new Error("workflow execution profile is unavailable");
  }
  for (const name of ["tools", "ALL_TOOLS", "text", "image", "audio", "generatedImage", "store", "load", "notify", "yield_control", "exit", "__workflowContextGet", "__workflowContextSet"]) {
    delete globalThis[name];
  }
  const scoped = (scope, task) => {
    const previous = enter(scope);
    try { return task(); } finally { enter(previous); }
  };
  const nativeTimeout = globalThis.setTimeout;
  globalThis.setTimeout = (callback, delay, ...args) => {
    const scope = read();
    return nativeTimeout(() => scoped(scope, () => callback(...args)), delay);
  };
  const nondeterministic = () => { throw new Error("workflow time and randomness are unavailable; use workflow.args or an agent result"); };
  Object.defineProperty(Math, "random", {value: nondeterministic, writable: false, configurable: false});
  const NativeDate = Date;
  function WorkflowDate(...args) {
    if (!new.target || args.length === 0) return nondeterministic();
    return Reflect.construct(NativeDate, args, new.target);
  }
  WorkflowDate.prototype = NativeDate.prototype;
  Object.defineProperty(WorkflowDate.prototype, "constructor", {value: WorkflowDate});
  Object.defineProperties(WorkflowDate, {
    now: {value: nondeterministic}, parse: {value: NativeDate.parse}, UTC: {value: NativeDate.UTC},
  });
  Object.defineProperty(globalThis, "Date", {value: WorkflowDate, writable: false, configurable: false});
  const freeze = value => {
    if (value && typeof value === "object") {
      Object.values(value).forEach(freeze);
      Object.freeze(value);
    }
    return value;
  };
  class AgentFailure extends Error {}
  return async (metadata, args, initialBudget, body, revision) => {
    let spent = initialBudget.spent || 0;
    const total = initialBudget.total;
    const work = new Set();
    const observations = new Set();
    let observationError;
    const budgetReads = new Map(Object.entries(initialBudget.observations || {}));
    const pendingBudget = new Map();
    const budgetAttribution = new Map();
    let budgetWrites = Promise.resolve();
    const attribution = scope => revision ? {scope: scope.key, statementIndex: scope.statementIndex} : null;
    const packet = (input, owner) => {
      const output = create(null);
      for (const key of ownKeys(input)) output[key] = input[key];
      if (revision && owner) {
        output.scope = owner.scope;
        output.statementIndex = owner.statementIndex;
      }
      return output;
    };
    const updateBudget = response => {
      if (response && response.budget && Number.isFinite(response.budget.spent)) {
        spent = Math.max(spent, response.budget.spent);
      }
      return response;
    };
    // Getters remain synchronous. Commit their branch inputs before any effect
    // can accept a child, so a crash cannot leave paid work without its branch.
    const flushBudget = () => {
      let batch = create(null);
      let batchAttribution = create(null);
      const payloadFor = (values, owners) => packet(revision
        ? {operation: "budgetObservations", observations: values, attribution: owners}
        : {operation: "budgetObservations", observations: values});
      const enqueue = () => {
        const payload = payloadFor(batch, batchAttribution);
        budgetWrites = budgetWrites.then(() => bridge(payload)).then(updateBudget);
        budgetWrites.catch(() => {});
        batch = create(null);
        batchAttribution = create(null);
      };
      for (const [key, value] of pendingBudget) {
        const candidate = {...batch, [key]: value};
        const candidateAttribution = {...batchAttribution, [key]: budgetAttribution.get(key)};
        if (ownKeys(batch).length >= 64 || JSON.stringify(payloadFor(candidate, candidateAttribution)).length > 16384) {
          enqueue();
        }
        batch[key] = value;
        if (revision) batchAttribution[key] = budgetAttribution.get(key);
      }
      pendingBudget.clear();
      if (ownKeys(batch).length !== 0) enqueue();
      return budgetWrites;
    };
    const budgetRead = kind => {
      const scope = read();
      const key = `${scope.key}/budget${scope.budgetNext++}`;
      let record = budgetReads.get(key);
      if (record) {
        if (record.kind !== kind) throw new Error("inconsistent replay of a workflow budget observation");
      } else {
        if (budgetReads.size >= 4096) throw new Error("workflow exceeds 4096 budget observations");
        record = {kind, spent, total};
        budgetReads.set(key, record);
        if (JSON.stringify(Object.fromEntries(budgetReads)).length > 256 * 1024) {
          throw new Error("workflow budget observations exceed 256 KiB");
        }
      }
      if (revision || !initialBudget.observations?.[key]) pendingBudget.set(key, record);
      if (revision) budgetAttribution.set(key, attribution(scope));
      return kind === "spent" ? record.spent : record.total == null ? Infinity : Math.max(0, record.total - record.spent);
    };
    const budget = Object.freeze({
      total: total == null ? Infinity : total,
      spent: () => budgetRead("spent"),
      remaining: () => budgetRead("remaining"),
    });
    const call = async input => {
      const output = packet(input, attribution(read()));
      await flushBudget();
      return updateBudget(await bridge(output));
    };
    const track = promise => {
      work.add(promise);
      promise.then(() => work.delete(promise), () => work.delete(promise));
      return promise;
    };
    const observe = promise => {
      observations.add(promise);
      promise.then(() => observations.delete(promise), error => {
        observations.delete(promise);
        observationError ??= error;
      });
      return promise;
    };
    const identity = kind => {
      const scope = read();
      if (!scope) throw new Error("workflow operation has no execution scope");
      return `${scope.key}/${kind}${scope.next++}`;
    };
    const agentIdentity = key => {
      if (key === undefined) return identity("a");
      if (typeof key !== "string" || key.length === 0) throw new Error("workflow agent key must be a nonempty string of at most 128 UTF-8 bytes");
      let encoded;
      try { encoded = encodeKey(key); } catch { throw new Error("workflow agent key must be valid Unicode"); }
      let bytes = 0;
      for (let index = 0; index < encoded.length; index++, bytes++) {
        if (encoded[index] === "%") index += 2;
      }
      if (bytes > 128) throw new Error("workflow agent key exceeds 128 UTF-8 bytes");
      const scope = read();
      if (scope.agentKeys.has(key)) throw new Error("duplicate workflow agent key in the same scope");
      scope.agentKeys.add(key);
      return `${scope.key}/key:${encoded}`;
    };
    const effectIdentity = () => {
      const scope = read();
      return `${scope.key}/effect${scope.effectNext++}`;
    };
    const scopeFor = (key, phase, depth, statementIndex = read()?.statementIndex) => ({key, phase, depth, statementIndex, next: 0, budgetNext: 0, effectNext: 0, agentKeys: new Set()});
    const array = (items, name) => {
      if (!Array.isArray(items) || items.length > 4096) {
        throw new Error(`${name} requires an array of at most 4096 entries`);
      }
    };
    const pool = async (items, task) => {
      const results = new Array(items.length);
      let next = 0;
      await Promise.all(Array.from({length: Math.min(16, items.length)}, async () => {
        for (;;) {
          const index = next++;
          if (index >= items.length) return;
          results[index] = await task(items[index], index);
        }
      }));
      return results;
    };
    const execute = async (meta, input, source, scope, prefix, revisionProgram) => {
      const titles = new Set(meta.phases.map(phase => phase.title));
      const phaseName = title => {
        if (typeof title !== "string" || !titles.has(title)) {
          throw new Error("workflow phase must match a declared metadata title");
        }
        return prefix ? `${prefix} / ${title}` : title;
      };
      const nested = (name, input) => track((async () => {
        const parent = read();
        const statementIndex = parent.statementIndex;
        if (parent.depth >= 1) throw new Error("nested workflows support one level only");
        const logicalId = identity("w");
        const loaded = await call({operation: "loadWorkflow", logicalId, name, args: input, depth: parent.depth + 1});
        return execute(loaded.metadata, input, loaded.body,
          scopeFor(logicalId, null, parent.depth + 1, statementIndex), loaded.metadata.name);
      })());
      const workflow = Object.assign(nested, {
        args: freeze(input),
        budget,
        agent: (prompt, options = {}) => {
          const current = read();
          const phase = options.phase === undefined ? current.phase : phaseName(options.phase);
          const logicalId = agentIdentity(options.key);
          return track((async () => {
            const response = await call({operation: "agent", logicalId, prompt, options, phase});
            if (response && response.ok === false && response.kind === "agentFailure") {
              throw new AgentFailure(response.error || "workflow agent failed");
            }
            if (!response || response.ok !== true) throw new Error("invalid workflow agent response");
            return response.result;
          })());
        },
        phase: title => {
          const name = phaseName(title);
          read().phase = name;
          return observe(call({operation: "phase", name, ...(revision ? {effectId: effectIdentity()} : {})}));
        },
        log: message => observe(call({operation: "log", message, phase: read().phase, ...(revision ? {effectId: effectIdentity()} : {})})),
        parallel: thunks => {
          array(thunks, "parallel");
          if (!thunks.every(thunk => typeof thunk === "function")) {
            throw new Error("parallel entries must be functions");
          }
          const parent = read();
          const statementIndex = parent.statementIndex;
          const key = identity("p");
          const scopes = thunks.map((_, index) => scopeFor(`${key}/item${index}`, parent.phase, parent.depth, statementIndex));
          return track(pool(thunks, (thunk, index) => scoped(scopes[index], async () => {
            try { return await thunk(); } catch (error) {
              if (error instanceof AgentFailure) return null;
              throw error;
            }
          })));
        },
        pipeline: (items, ...stages) => {
          array(items, "pipeline");
          if (stages.length === 0 || !stages.every(stage => typeof stage === "function")) {
            throw new Error("pipeline requires stage functions");
          }
          const parent = read();
          const statementIndex = parent.statementIndex;
          const key = identity("q");
          const initialPhase = parent.phase;
          return track(pool(items, async (item, index) => {
            let value = item;
            let phase = initialPhase;
            for (let stage = 0; stage < stages.length; stage++) {
              const context = scopeFor(`${key}/item${index}/stage${stage}`, phase, parent.depth, statementIndex);
              try {
                value = await scoped(context, () => stages[stage](value, item, index));
              } catch (error) {
                if (error instanceof AgentFailure) return null;
                throw error;
              }
              phase = context.phase;
            }
            return value;
          }));
        },
      });
      Object.freeze(workflow);
      if (revisionProgram) {
        const flushObservations = async () => {
          while (observations.size !== 0) await Promise.all([...observations]);
          if (observationError) throw observationError;
          await flushBudget();
        };
        return __workflowRevisionRuntime(workflow, source, scope, revisionProgram, scoped,
          async input => updateBudget(await bridge(input)), flushObservations, () => work.size);
      }
      const program = new AsyncFunction("workflow", `"use strict";\n${source}`);
      return scoped(scope, () => program(workflow));
    };
    const rootScope = scopeFor("root", null, 0, revision ? 0 : undefined);
    const result = await execute(metadata, args, body, rootScope, "", revision);
    if (work.size !== 0) throw new Error("workflow returned with unfinished agent or combinator operations");
    while (observations.size !== 0) await Promise.all([...observations]);
    if (observationError) throw observationError;
    await scoped(rootScope, () => call({operation: "publish", value: result === undefined ? null : result}));
  };
})();
