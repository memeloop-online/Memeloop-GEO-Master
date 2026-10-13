// Privacy projection only: this is not an answer, citation, search or completion
// parser. Kept values retain their source positions and text; omitted values
// are declared separately. The resulting source is not a byte-identical capture.
const privateKey =
  /^(?:authorization|cookies?|password|access[-_]?token|refresh[-_]?token|session[-_]?(?:token|key)|api[-_]?key|credentials?|proxy|encrypted[-_]?(?:proxy|session)|email|phone|user[-_]?id|account[-_]?id)$/iu;
const reasoningKey = /^(?:think|thinking|reasoning|reasoning[-_]?content)$/iu;
const fragmentTypes = new Set([
  "THINK",
  "RESPONSE",
  "TEMPLATE_RESPONSE",
  "SEARCH",
  "TOOL_SEARCH",
  "TOOL_OPEN",
  "TOOL_FIND",
  "REQUEST",
  "FILE",
  "TIP",
  "READ_LINK",
]);
const responseFields = new Set([
  "message_id",
  "parent_id",
  "role",
  "status",
  "quasi_status",
  "accumulated_token_usage",
  "token_usage",
  "feedback",
  "ban_regenerate",
  "search_enabled",
  "search_triggered",
  "thinking_enabled",
  "inserted_at",
  "has_pending_fragment",
  "conversation_mode",
  "extra_search_providers",
  "incomplete_message",
  "fragments",
]);
const eventFields = {
  ready: ["response_message_id", "request_message_id", "model_type"],
  close: ["click_behavior"],
  finish: [],
  title: ["content"],
  toast: ["content", "type", "finish_reason"],
  hint: ["content", "type", "clear_response", "finish_reason"],
  update_session: ["updated_at"],
};
// These first-party events are outside the delta parser. Their opaque debug,
// prior-message and file metadata is not answer evidence or safe to retain.
const omittedAuxiliaryEvents = new Set([
  "debug",
  "update_parent_message",
  "update_file",
]);
const object = (value) =>
  value !== null && typeof value === "object" && !Array.isArray(value);
const fragmentFieldShape = {
  content: (value) => typeof value === "string",
  status: (value) => typeof value === "string",
  pattern: (value) => typeof value === "string",
  queries: Array.isArray,
  results: Array.isArray,
  references: (value) => value === null || Array.isArray(value),
  result: (value) => value === null || object(value),
};
const unknownFragment = Symbol("unknown-fragment");
const pointer = (path, key) =>
  `${path}/${String(key).replace(/~/gu, "~0").replace(/\//gu, "~1")}`;

/**
 * Project one complete captured SSE exchange before persistence or AI input.
 * Null means its privacy shape is unsupported, never that it is clean.
 * Optional onDiagnostic receives only a fixed rejection code, without context.
 *
 * Delta p/o inherit independently, initially ""/SET. Each BATCH has a fresh
 * child inheritance scope, while its siblings share that scope. Fragment array
 * SET/replacement/APPEND insertion carries type and omitted-field provenance.
 * Only direct, shape-checked fragment-field deltas are retained. Updating an
 * arbitrary nested subtree would require privacy provenance beyond this index;
 * such paths fail closed, including nested discriminator changes.
 * No accumulated response text or semantic measurement result is constructed.
 */
export function projectDeepseekSource(exchange, onDiagnostic) {
  // Only locally selected codes escape this boundary, never exception details.
  // Structural categories identify the rejected layer, not its keys or values.
  let rejectionCode = "shape_rejected";
  const reject = (code = "shape_rejected") => {
    rejectionCode = code;
    throw new Error("source_privacy_unverified");
  };
  try {
    if (
      !object(exchange) ||
      !Array.isArray(exchange.messages) ||
      exchange.messages.length > 10_000 ||
      Buffer.byteLength(JSON.stringify(exchange.messages)) > 750_000
    )
      reject();
    const omitted = [];
    let visited = 0;
    let types = null;
    const bound = (depth) => {
      if (depth > 32 || ++visited > 100_000) reject();
    };
    const omit = (path) => {
      omitted.push(path);
    };
    const clean = (value, path, depth = 0) => {
      bound(depth);
      if (Array.isArray(value))
        return value.map((item, index) =>
          clean(item, pointer(path, index), depth + 1),
        );
      if (!object(value)) return value;
      // A typed reasoning object may carry text under arbitrary future keys.
      // Keep only its original discriminator, not an invented empty content.
      if (value.type === "THINK") {
        for (const key of Object.keys(value))
          if (key !== "type") omit(pointer(path, key));
        return { type: value.type };
      }
      return Object.fromEntries(
        Object.entries(value).flatMap(([key, child]) => {
          if (privateKey.test(key) || reasoningKey.test(key)) {
            omit(pointer(path, key));
            return [];
          }
          if (["__proto__", "constructor", "prototype"].includes(key)) reject();
          return [[key, clean(child, pointer(path, key), depth + 1)]];
        }),
      );
    };
    const fragment = (value, path, depth) => {
      bound(depth);
      if (!object(value)) reject("source_fragment_rejected");
      const taintedFields = new Set();
      if (!fragmentTypes.has(value.type)) {
        for (const key of Object.keys(value)) omit(pointer(path, key));
        return {
          value: {},
          state: { type: unknownFragment, taintedFields },
        };
      }
      const retained = Object.fromEntries(
        Object.entries(value).filter(([key, child]) => {
          if (
            value.type !== "THINK" &&
            Object.hasOwn(fragmentFieldShape, key) &&
            !fragmentFieldShape[key](child)
          ) {
            omit(pointer(path, key));
            taintedFields.add(key);
            return false;
          }
          return true;
        }),
      );
      return {
        value: clean(retained, path, depth),
        state: { type: value.type, taintedFields },
      };
    };
    const fragments = (value, path, depth) => {
      if (!Array.isArray(value) || value.length > 10_000)
        reject("source_fragment_rejected");
      const projected = value.map((item, index) =>
        fragment(item, pointer(path, index), depth + 1),
      );
      return {
        value: projected.map((item) => item.value),
        types: projected.map((item) => item.state),
      };
    };
    const response = (value, path, depth) => {
      if (!object(value)) reject("source_response_rejected");
      // Unknown snapshot metadata is conservatively private. Omit the whole
      // value without interpreting it; field-path deltas remain fail-closed.
      const retained = [];
      for (const key of Object.keys(value))
        if (
          !responseFields.has(key) &&
          !privateKey.test(key) &&
          !reasoningKey.test(key)
        )
          omit(pointer(path, key));
        else retained.push([key, value[key]]);
      value = Object.fromEntries(retained);
      // Snapshots reset the type index; absence cannot inherit old fragments.
      types = null;
      if (Object.hasOwn(value, "fragments")) {
        const result = fragments(
          value.fragments,
          pointer(path, "fragments"),
          depth + 1,
        );
        types = result.types;
        const { fragments: ignored, ...rest } = value;
        return { ...clean(rest, path, depth), fragments: result.value };
      }
      return clean(value, path, depth);
    };
    const leaf = (value, path, operation, at, depth) => {
      const parts = path.split("/").filter(Boolean);
      // Even private-key omissions cannot turn an unverified fragment slot or
      // nested mutation into an accepted delta.
      if (
        parts[0] === "response" &&
        parts[1] === "fragments" &&
        parts.length > 3
      ) {
        const number = Number(parts[2]);
        const index = number < 0 ? (types?.length ?? 0) + number : number;
        if (parts.length !== 4 || types?.[index]?.type === unknownFragment)
          reject("path_rejected");
      }
      if (
        parts.some((part) => privateKey.test(part) || reasoningKey.test(part))
      ) {
        omit(at);
        return { keep: false };
      }
      if (!parts.length) {
        if (
          operation !== "SET" ||
          !object(value) ||
          Object.keys(value).some((key) => key !== "response")
        )
          reject("source_root_rejected");
        return {
          keep: true,
          value: {
            response: response(value.response, pointer(at, "response"), depth),
          },
        };
      }
      if (parts[0] !== "response") reject("path_rejected");
      if (parts.length === 1) {
        if (operation !== "SET") reject();
        return { keep: true, value: response(value, at, depth) };
      }
      if (!responseFields.has(parts[1])) reject("path_rejected");
      if (parts[1] !== "fragments") {
        // Complete metadata field replacement is independent of prior nested
        // privacy state. Incremental or nested metadata updates are unverified.
        if (parts.length !== 2) reject("path_rejected");
        if (operation !== "SET") reject();
        return { keep: true, value: clean(value, at, depth) };
      }
      if (parts.length === 2) {
        const result = fragments(value, at, depth);
        if (operation === "SET") types = result.types;
        else {
          if (types === null) reject();
          types.push(...result.types);
        }
        if (types.length > 10_000) reject();
        return { keep: true, value: result.value };
      }
      if (types === null || !/^-?\d+$/u.test(parts[2])) reject("path_rejected");
      const number = Number(parts[2]);
      if (!Number.isSafeInteger(number)) reject("path_rejected");
      const index = number < 0 ? types.length + number : number;
      if (parts.length === 3) {
        if (operation === "APPEND") {
          const result = fragments(value, at, depth);
          // Array APPEND at a numeric path uses splice insertion (including
          // negative and out-of-range clamping), not element concatenation.
          types.splice(number, 0, ...result.types);
          if (types.length > 10_000) reject();
          return { keep: true, value: result.value };
        }
        if (index < 0 || index > types.length) reject("path_rejected");
        const result = fragment(value, at, depth);
        types[index] = result.state;
        return { keep: true, value: result.value };
      }
      if (index < 0 || index >= types.length) reject("path_rejected");
      // Changing a discriminator independently could reinterpret earlier
      // retained text. Such shapes need a new reviewed privacy contract.
      if (
        parts.length !== 4 ||
        parts[3] === "type" ||
        types[index].type === unknownFragment
      )
        reject("path_rejected");
      if (types[index].type === "THINK") {
        omit(at);
        return { keep: false };
      }
      if (!Object.hasOwn(fragmentFieldShape, parts[3])) reject("path_rejected");
      if (
        !fragmentFieldShape[parts[3]](value) ||
        (operation === "APPEND" && types[index].taintedFields.has(parts[3])) ||
        (operation === "APPEND" &&
          (parts[3] === "status" ||
            !(typeof value === "string" || Array.isArray(value))))
      )
        reject("source_fragment_rejected");
      const cleaned = clean(value, at, depth);
      if (operation === "SET") types[index].taintedFields.delete(parts[3]);
      return { keep: true, value: cleaned };
    };
    const delta = (data, scope, prefix, at, depth) => {
      bound(depth);
      if (
        !object(data) ||
        Object.keys(data).some((key) => !["p", "o", "v"].includes(key)) ||
        !Object.hasOwn(data, "v")
      )
        reject("source_delta_rejected");
      const path = data.p ?? scope.path;
      const operation = data.o ?? scope.operation;
      if (typeof path !== "string" || path.length > 2_048)
        reject("path_rejected");
      if (!["SET", "APPEND", "BATCH"].includes(operation))
        reject("source_delta_rejected");
      scope.path = path;
      scope.operation = operation;
      const absolute = prefix ? `${prefix}/${path}` : path;
      const result = { ...data };
      if (operation === "BATCH") {
        if (!Array.isArray(data.v)) reject("source_delta_rejected");
        const child = { path: "", operation: "SET" };
        result.v = data.v.map((item, index) =>
          delta(
            item,
            child,
            absolute,
            pointer(pointer(at, "v"), index),
            depth + 1,
          ),
        );
      } else {
        const projected = leaf(
          data.v,
          absolute,
          operation,
          pointer(at, "v"),
          depth + 1,
        );
        if (projected.keep) result.v = projected.value;
        else delete result.v;
      }
      return result;
    };
    const scope = { path: "", operation: "SET" };
    const messages = exchange.messages.map((message, index) => {
      const at = `/messages/${index}`;
      if (
        !object(message) ||
        Object.keys(message).some(
          (key) => !["event", "data", "raw_data", "id"].includes(key),
        )
      )
        reject("source_event_rejected");
      const event = message.event ?? "";
      if (typeof event !== "string") reject("source_event_rejected");
      const result = { ...message };
      if (Object.hasOwn(message, "id")) {
        if (typeof message.id !== "string") reject("identifier_rejected");
        // SSE transport IDs are not provider message IDs. Omit them entirely;
        // they cannot establish ownership or contribute to semantic evidence.
        delete result.id;
        omit(pointer(at, "id"));
      }
      if (omittedAuxiliaryEvents.has(event)) {
        if (
          !Object.hasOwn(message, "data") ||
          Object.hasOwn(message, "raw_data")
        )
          reject("source_event_rejected");
        delete result.data;
        omit(pointer(at, "data"));
        return result;
      }
      if (event === "" || event === "message" || event === "delta") {
        if (Object.hasOwn(message, "raw_data")) reject("source_event_rejected");
        return {
          ...result,
          data: delta(message.data, scope, "", pointer(at, "data"), 0),
        };
      }
      if (!Object.hasOwn(eventFields, event)) reject("source_event_rejected");
      if (Object.hasOwn(message, "raw_data")) {
        // Opaque finish data is not AI evidence. Do not inspect it for a
        // completion claim or silently preserve a possible reasoning payload.
        if (event !== "finish") reject("source_event_rejected");
        delete result.raw_data;
        omit(pointer(at, "raw_data"));
      }
      if (Object.hasOwn(message, "data")) {
        if (!object(message.data)) reject("source_event_rejected");
        // Known named events do not participate in delta inheritance. Unknown
        // metadata can be omitted wholly without interpreting its contents or
        // discarding the event and its reviewed source fields.
        const retained = [];
        for (const [key, value] of Object.entries(message.data)) {
          if (
            !eventFields[event].includes(key) &&
            !privateKey.test(key) &&
            !reasoningKey.test(key)
          )
            omit(pointer(pointer(at, "data"), key));
          else retained.push([key, value]);
        }
        result.data = clean(Object.fromEntries(retained), pointer(at, "data"));
      } else if (event !== "finish") reject("source_event_rejected");
      return result;
    });
    return {
      exchange: {
        messages,
        // These are transport context, never synthesized source messages.
        ...Object.fromEntries(
          [
            "received_at",
            "started_at",
            "sse_terminal",
            "created_session",
            "capture_status",
          ]
            .filter((key) => Object.hasOwn(exchange, key))
            .map((key) => [key, exchange[key]]),
        ),
      },
      redaction: {
        schema_version: "geo.observation.source-redaction.v1",
        policy: "reasoning-and-private-fields",
        omitted_paths: omitted,
      },
    };
  } catch {
    try {
      onDiagnostic?.(rejectionCode);
    } catch {
      // Diagnostics must not change the privacy decision or null contract.
    }
    return null;
  }
}
