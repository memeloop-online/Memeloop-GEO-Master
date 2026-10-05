import { createAgentAgentLoopDefinition } from "memeloop/loop-api";

// This is an approved, build-time script passed as `context.script` to the
// installed MemeLoop agent-agent-loop. Its only input is a durable execution
// reference; every branch resumes from the Rust-owned item state.
const PAGE_SIZE = 32;
const PAGE_CONCURRENCY = 4;
const TARGET_PAGE_SIZE = 64;
const TERMINAL = new Set([
  "ready",
  "blocked",
  "deferred",
  "not_applicable",
  "cancelled",
]);

function host() {
  const test = globalThis.__GEO_CONTENT_TEST_HOST__;
  if (test) return test;
  const ops = globalThis.Deno?.core?.ops;
  for (const name of [
    "op_host_content_items_read_v1",
    "op_host_content_prepare_v1",
    "op_host_content_generate_v1",
    "op_host_content_check_v1",
    "op_host_content_repair_v1",
    "op_host_content_close_v1",
    "op_host_content_execution_read_v1",
    "op_host_distribution_start_v1",
    "op_host_distribution_resume_v1",
    "op_host_distribution_targets_read_v1",
    "op_host_emit",
  ]) {
    if (typeof ops?.[name] !== "function") {
      throw new Error("The approved content host-op surface is unavailable.");
    }
  }
  const invoke = async (name, input) =>
    JSON.parse(await ops[name](JSON.stringify(input)));
  return {
    itemsRead: (input) => invoke("op_host_content_items_read_v1", input),
    prepare: (input) => invoke("op_host_content_prepare_v1", input),
    generate: (input) => invoke("op_host_content_generate_v1", input),
    check: (input) => invoke("op_host_content_check_v1", input),
    repair: (input) => invoke("op_host_content_repair_v1", input),
    close: (input) => invoke("op_host_content_close_v1", input),
    executionRead: (input) =>
      invoke("op_host_content_execution_read_v1", input),
    distributionStart: (input) =>
      invoke("op_host_distribution_start_v1", input),
    distributionResume: (input) =>
      invoke("op_host_distribution_resume_v1", input),
    distributionTargetsRead: (input) =>
      invoke("op_host_distribution_targets_read_v1", input),
    emit: (topic, value) => ops.op_host_emit(topic, JSON.stringify(value)),
  };
}

function validateDistribution(manifest, execution, handoff, previous) {
  if (
    typeof manifest?.manifest_id !== "string" ||
    manifest.cycle_id !== execution.cycle_id ||
    manifest.content_execution_id !== execution.execution_id ||
    manifest.content_handoff_id !== handoff.handoff_id ||
    !Number.isSafeInteger(manifest.expected_count) ||
    manifest.expected_count < 0 ||
    !Number.isSafeInteger(manifest.expansion_cursor) ||
    manifest.expansion_cursor < 0 ||
    manifest.expansion_cursor > manifest.expected_count ||
    (previous &&
      (manifest.manifest_id !== previous.manifest_id ||
        manifest.expected_count !== previous.expected_count ||
        manifest.expansion_cursor < previous.expansion_cursor))
  ) {
    throw new Error(
      "Distribution manifest changed or is outside the content handoff.",
    );
  }
  return manifest;
}

async function prepareDistribution(execution, handoff, capability, ctx) {
  let manifest = validateDistribution(
    await capability.distributionStart({ cycle_id: execution.cycle_id }),
    execution,
    handoff,
  );
  while (!manifest.complete) {
    if (ctx.isCancelled())
      throw new Error("Distribution preparation cancelled.");
    const advanced = validateDistribution(
      await capability.distributionResume({
        manifest_id: manifest.manifest_id,
      }),
      execution,
      handoff,
      manifest,
    );
    if (advanced.expansion_cursor <= manifest.expansion_cursor) {
      throw new Error("Distribution expansion cursor made no progress.");
    }
    manifest = advanced;
  }
  if (manifest.expansion_cursor !== manifest.expected_count) {
    throw new Error(
      "Distribution expansion did not cover its frozen denominator.",
    );
  }

  // Expansion and eligibility recheck are distinct: an already complete
  // manifest still needs every materialized target revisited after recovery.
  let afterOrdinal;
  let observed = 0;
  let finished = false;
  while (!finished) {
    if (ctx.isCancelled())
      throw new Error("Distribution preparation cancelled.");
    manifest = validateDistribution(
      await capability.distributionResume({
        manifest_id: manifest.manifest_id,
        ...(afterOrdinal === undefined ? {} : { after_ordinal: afterOrdinal }),
      }),
      execution,
      handoff,
      manifest,
    );
    // One trusted resume rechecks at most four 64-target pages. Inspect all
    // four pages before requesting the next bounded recheck window.
    for (let index = 0; index < 4; index++) {
      const page = await capability.distributionTargetsRead({
        manifest_id: manifest.manifest_id,
        ...(afterOrdinal === undefined ? {} : { after_ordinal: afterOrdinal }),
        limit: TARGET_PAGE_SIZE,
      });
      if (
        page.manifest_id !== manifest.manifest_id ||
        page.expected_count !== manifest.expected_count ||
        !Array.isArray(page.items) ||
        page.items.length > TARGET_PAGE_SIZE
      ) {
        throw new Error("Distribution target page changed during preparation.");
      }
      let ordinal = afterOrdinal ?? -1;
      for (const item of page.items) {
        if (
          typeof item.target_id !== "string" ||
          !Number.isSafeInteger(item.ordinal) ||
          item.ordinal !== ordinal + 1 ||
          item.status === "pending"
        ) {
          throw new Error(
            "Distribution target is missing or not materialized.",
          );
        }
        ordinal = item.ordinal;
      }
      observed += page.items.length;
      if (page.next_ordinal != null) {
        if (
          !page.items.length ||
          page.next_ordinal !== ordinal ||
          page.next_ordinal <= (afterOrdinal ?? -1)
        ) {
          throw new Error("Distribution target cursor made no progress.");
        }
        afterOrdinal = page.next_ordinal;
      } else {
        if (observed !== manifest.expected_count) {
          throw new Error("Distribution target denominator is incomplete.");
        }
        finished = true;
        break;
      }
    }
  }
  await capability.emit("distribution.prepared", {
    execution_id: execution.execution_id,
    cycle_id: execution.cycle_id,
    handoff_id: handoff.handoff_id,
    manifest_id: manifest.manifest_id,
    targets: observed,
  });
}

async function processItem(item, executionId, capability, ctx) {
  const request = { execution_id: executionId, item_id: item.item_id };
  let current = item;
  let repairsThisInvocation = 0;
  let checksThisInvocation = 0;
  // Completed steps are never inferred from a JS checkpoint. Reentry reads
  // durable item state and a step result is persisted before Rust replies.
  try {
    if (current.status === "pending") {
      current = await capability.prepare(request);
    }
    if (current.status === "prepared") {
      current = await capability.generate(request);
    }
    while (current.status === "drafted" || current.status === "needs_repair") {
      if (
        !Number.isSafeInteger(current.automatic_repair_count) ||
        current.automatic_repair_count < 0 ||
        current.automatic_repair_count > 2
      ) {
        throw new Error("The durable repair count is invalid.");
      }
      if (current.status === "drafted") {
        if (checksThisInvocation >= 3) {
          throw new Error("The content branch exhausted its check budget.");
        }
        const previousCount = current.automatic_repair_count;
        current = await capability.check(request);
        checksThisInvocation++;
        if (
          current.status === "drafted" ||
          current.automatic_repair_count !== previousCount
        ) {
          throw new Error("The content check made no durable progress.");
        }
      } else {
        if (repairsThisInvocation >= 2 || current.automatic_repair_count >= 2) {
          throw new Error("The content branch exhausted its repair budget.");
        }
        const previousCount = current.automatic_repair_count;
        current = await capability.repair(request);
        repairsThisInvocation++;
        if (
          current.automatic_repair_count !== previousCount + 1 ||
          current.status !== "drafted"
        ) {
          throw new Error("The repair did not persist a new draft.");
        }
      }
    }
    if (!TERMINAL.has(current.status)) {
      throw new Error("The content branch returned an unsupported state.");
    }
    ctx.emit({
      type: "thinking",
      data: {
        status: "content-branch-completed",
        itemId: item.item_id,
        branchKey: item.branch_key,
        result: current.status,
      },
    });
    return current.status;
  } catch (error) {
    // The backend may have classified this item as blocked before an op
    // rejected. Other branches still proceed; the durable close decides if
    // any item remains incomplete and refuses to forge a handoff.
    ctx.emit({
      type: "thinking",
      data: {
        status: "content-branch-failed",
        itemId: item.item_id,
        branchKey: item.branch_key,
      },
    });
    return "failed";
  }
}

export async function contentWorkflow(ctx) {
  const executionId = ctx.input.message;
  if (
    typeof executionId !== "string" ||
    !/^[0-9a-f]{8}-[0-9a-f-]{27,}$/iu.test(executionId)
  ) {
    throw new TypeError("Content workflow requires an execution reference.");
  }
  const capability = host();
  const execution = await capability.executionRead({
    execution_id: executionId,
  });
  if (
    execution.execution_id !== executionId ||
    typeof execution.cycle_id !== "string"
  ) {
    throw new Error("Content execution has no immutable cycle binding.");
  }
  let cursor;
  let total;
  let observed = 0;
  let failed = 0;
  do {
    if (ctx.isCancelled()) throw new Error("Content workflow cancelled.");
    const page = await capability.itemsRead({
      execution_id: executionId,
      ...(cursor ? { cursor } : {}),
      limit: PAGE_SIZE,
    });
    if (
      page.execution_id !== executionId ||
      !Array.isArray(page.items) ||
      page.items.length > PAGE_SIZE ||
      (total !== undefined && total !== page.total)
    ) {
      throw new Error("The content manifest page changed during execution.");
    }
    total = page.total;
    const pageIds = new Set();
    for (
      let offset = 0;
      offset < page.items.length;
      offset += PAGE_CONCURRENCY
    ) {
      const group = page.items.slice(offset, offset + PAGE_CONCURRENCY);
      for (const item of group) {
        if (
          typeof item.item_id !== "string" ||
          typeof item.branch_key !== "string" ||
          pageIds.has(item.item_id)
        ) {
          throw new Error(
            "The content manifest returned an invalid or repeated item.",
          );
        }
        pageIds.add(item.item_id);
      }
      const results = await Promise.all(
        group.map((item) =>
          TERMINAL.has(item.status)
            ? item.status
            : processItem(item, executionId, capability, ctx),
        ),
      );
      observed += results.length;
      failed += results.filter((result) => result === "failed").length;
    }
    if (
      page.next_cursor === cursor ||
      (page.next_cursor && page.items.length === 0)
    ) {
      throw new Error("The content manifest cursor made no progress.");
    }
    cursor = page.next_cursor ?? undefined;
  } while (cursor);
  if (observed !== total) {
    throw new Error(
      "The content manifest denominator changed during execution.",
    );
  }
  // Even when a branch errored, try Rust's eligibility check: a failed op may
  // have persisted a terminal blocked result before responding. If it did not,
  // close fails explicitly, leaving an execution reference resumable.
  const handoff = await capability.close({ execution_id: executionId });
  if (handoff.execution_id !== executionId || handoff.total !== total) {
    throw new Error("Content handoff did not match its frozen denominator.");
  }
  await capability.emit("content.completed", {
    execution_id: executionId,
    handoff_id: handoff.handoff_id,
    total,
    failed_steps: failed,
  });
  await prepareDistribution(execution, handoff, capability, ctx);
  ctx.finish(
    `Content execution ${executionId} and distribution preparation completed.`,
  );
}

export async function workflowMain(input) {
  if (typeof input?.execution_id !== "string") {
    throw new TypeError("execution_id is required.");
  }
  const runner = createAgentAgentLoopDefinition().createRunner({
    script: contentWorkflow,
    runtime: {
      // The runtime does not claim a JS snapshot is a durable business result.
      // The installed loop owns lifecycle and steps; Rust item state is read
      // again on every invocation and is the sole replay authority.
      log() {},
    },
  });
  let completed = false;
  for await (const step of runner({
    conversationId: input.execution_id,
    runId: input.execution_id,
    message: input.execution_id,
  })) {
    if (step.type === "thinking" && step.data?.status === "completed") {
      completed = true;
    }
  }
  if (!completed)
    throw new Error("The native MemeLoop workflow did not complete.");
  return { execution_id: input.execution_id, completed };
}
