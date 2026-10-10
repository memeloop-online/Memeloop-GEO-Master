import assert from "node:assert/strict";
import { test } from "node:test";
import {
  observationRejectionReason,
  validateAiObservation,
} from "../src/ai-observation-grounding.mjs";

const modern = {
  messages: [
    { chat: { id: "chat-1" } },
    {
      message: {
        id: "answer-1",
        role: "assistant",
        status: "MESSAGE_STATUS_COMPLETED",
        refs: {
          searchChunks: [
            {
              id: "source-1",
              base: {
                url: "https://example.org/source",
                title: "Public source",
              },
            },
          ],
        },
      },
    },
    {
      block: {
        id: "tool-1",
        messageId: "answer-1",
        tool: {
          name: "web_search",
          status: "STATUS_DONE",
          contents: [{ searchResult: {} }],
        },
      },
    },
    {
      block: {
        id: "text-1",
        messageId: "answer-1",
        text: { content: "Answer from the source." },
      },
    },
  ],
  rendered_text: "Prompt in page. Answer from the source. Footer.",
};

const modernExtracted = {
  decision: "searched_answer",
  chat_id: { path: "/messages/0/chat/id" },
  message_id: { path: "/messages/1/message/id" },
  answer_owner: { path: "/messages/3/block/messageId" },
  search_owner: { path: "/messages/2/block/messageId" },
  search_block_id: { path: "/messages/2/block/id" },
  completion: { path: "/messages/1/message/status" },
  search_activity: { path: "/messages/2/block/tool/contents" },
  answer_segments: [{ path: "/messages/3/block/text/content" }],
  citations: [
    {
      url: { path: "/messages/1/message/refs/searchChunks/0/base/url" },
      usage: { path: "/messages/1/message/refs/searchChunks/0/id" },
    },
  ],
};

const validate = (overrides = {}, document = modern) =>
  validateAiObservation(document, { ...modernExtracted, ...overrides });

const semanticSource = {
  messages: [
    {
      finished: false,
      searched: 0,
      answer: "Exact observed answer.",
      reference: "Used https://example.org/source in this answer.",
    },
  ],
};
const semanticCandidate = {
  completion: "complete",
  completion_evidence: [{ path: "/messages/0/finished" }],
  search_used: "yes",
  search_evidence: [{ path: "/messages/0/searched" }],
  answer_segments: [{ path: "/messages/0/answer" }],
  citations: [
    {
      url: {
        path: "/messages/0/reference",
        quote: "https://example.org/source",
      },
      usage: { path: "/messages/0/reference", quote: "in this answer" },
    },
  ],
};

test("semantic v4 grounds selections without provider identities or status interpretation", () => {
  const result = validateAiObservation(semanticSource, semanticCandidate, {
    semanticOnly: true,
  });
  assert.equal(result.raw_answer, "Exact observed answer.");
  assert.deepEqual(result.citations, ["https://example.org/source"]);
  assert.equal(result.search_used, "yes");
  assert.equal(result.completion, "complete");
  for (const field of ["chat_id", "message_id", "block_id"])
    assert.equal(Object.hasOwn(result, field), false);
  assert.equal(
    result.audit.refs.find((ref) => ref.role === "completion").value,
    false,
  );
  assert.equal(
    result.audit.refs.find((ref) => ref.role === "search_activity").value,
    0,
  );
  const source = structuredClone(semanticSource);
  source.messages[0].finished = { any_provider_shape: ["final evidence"] };
  source.messages[0].searched = "Observed search in arbitrary schema";
  assert.ok(validateAiObservation(source, semanticCandidate));
  const quoted = structuredClone(semanticCandidate);
  quoted.search_evidence[0].quote = "Observed search";
  assert.ok(validateAiObservation(source, quoted));
  quoted.search_evidence[0].quote = "invented";
  assert.equal(validateAiObservation(source, quoted), null);
});

test("semantic v4 rejects ungrounded fields and does not silently accept the legacy contract", () => {
  for (const change of [
    (candidate) => (candidate.search_used = "no"),
    (candidate) => (candidate.search_used = "unknown"),
    (candidate) => (candidate.completion = "incomplete"),
    (candidate) => (candidate.completion = "unknown"),
    (candidate) => (candidate.completion_evidence = []),
    (candidate) => (candidate.search_evidence = []),
    (candidate) =>
      (candidate.search_evidence = Array(257).fill({
        path: "/messages/0/searched",
      })),
    (candidate) => (candidate.search_evidence[0].path = "/messages/99/missing"),
    (candidate) => (candidate.answer_segments[0].start = 0),
    (candidate) => (candidate.chat_id = { path: "/messages/0/answer" }),
    (candidate) => (candidate.citations[0].usage.quote = "invented"),
    (candidate) =>
      (candidate.citations[0].url.quote = "https://localhost/private"),
  ]) {
    const candidate = structuredClone(semanticCandidate);
    change(candidate);
    assert.equal(validateAiObservation(semanticSource, candidate), null);
  }
  assert.equal(
    validateAiObservation(modern, modernExtracted, { semanticOnly: true }),
    null,
  );
  assert.ok(
    validateAiObservation(modern, modernExtracted),
    "historical replay remains valid",
  );
});

test("every residual grounding failure has a fixed diagnostic without relaxing validation", () => {
  const scenarios = [
    ["identifier_rejected", (document) => (document.messages[0].chat.id = 123)],
    [
      "identifier_rejected",
      (document) => (document.messages[2].block.id = "private:id"),
    ],
    [
      "evidence_empty",
      (document) => (document.messages[1].message.status = ""),
    ],
    [
      "evidence_empty",
      (document) => (document.messages[2].block.tool.contents = []),
    ],
    [
      "evidence_empty",
      (document) =>
        (document.messages[1].message.refs.searchChunks[0].id = false),
    ],
    [
      "answer_type_rejected",
      (document) =>
        (document.messages[3].block.text.content = { private_value: "hidden" }),
    ],
    [
      "answer_type_rejected",
      (document) => (document.messages[3].block.text.content = ""),
    ],
    [
      "answer_empty",
      (document) => (document.messages[3].block.text.content = " \n"),
    ],
    [
      "answer_too_large",
      (document) =>
        (document.messages[3].block.text.content = "界".repeat(33_334)),
    ],
    [
      "unicode_rejected",
      (document) => (document.messages[3].block.text.content = "\uD800"),
    ],
    [
      "answer_bounds_rejected",
      (_document, candidate) =>
        Object.assign(candidate.answer_segments[0], { start: -1, end: 2 }),
    ],
    [
      "answer_bounds_rejected",
      (_document, candidate) =>
        Object.assign(candidate.answer_segments[0], {
          quote: "Answer",
          start: 0,
          end: 6,
        }),
    ],
    [
      "quote_rejected",
      (_document, candidate) =>
        (candidate.answer_segments[0].quote = undefined),
    ],
    [
      "citation_rejected",
      (_document, candidate) => (candidate.citations[0].url.quote = undefined),
    ],
    [
      "shape_rejected",
      (_document, candidate) => (candidate.citations = [null]),
    ],
  ];
  for (const [reason, change] of scenarios) {
    const document = structuredClone(modern);
    const candidate = structuredClone(modernExtracted);
    change(document, candidate);
    assert.equal(validateAiObservation(document, candidate), null, reason);
    assert.equal(observationRejectionReason(document, candidate), reason);
  }
  assert.ok(validateAiObservation(modern, modernExtracted));
});

test("rejection reasons are fixed vocabulary without accepting any candidate", () => {
  const explain = (overrides) =>
    observationRejectionReason(modern, {
      ...modernExtracted,
      ...overrides,
    });
  assert.equal(explain({ decision: "unverified" }), "model_unverified");
  assert.equal(explain({ answer_segments: [] }), "shape_rejected");
  assert.equal(
    explain({ completion: { path: "/messages/42/unknown" } }),
    "path_rejected",
  );
  assert.equal(
    explain({ answer_owner: { path: "/messages/0/chat/id" } }),
    "owner_rejected",
  );
  assert.equal(
    explain({
      answer_segments: [
        { path: "/messages/3/block/text/content", quote: "not present" },
      ],
    }),
    "quote_rejected",
  );
  assert.equal(
    explain({
      citations: [
        {
          url: { path: "/messages/1/message/role" },
          usage: modernExtracted.citations[0].usage,
        },
      ],
    }),
    "citation_rejected",
  );
});

test("preserves all 65 streamed answer deltas while enforcing the total answer byte limit", () => {
  const document = structuredClone(modern);
  const deltas = Array.from({ length: 65 }, (_, index) => `段${index}。`);
  document.messages[3].block.text.deltas = deltas;
  const answer_segments = deltas.map((_, index) => ({
    path: `/messages/3/block/text/deltas/${index}`,
  }));
  const result = validate({ answer_segments }, document);
  assert.equal(result.raw_answer, deltas.join(""));
  assert.equal(
    result.audit.refs.filter((ref) => ref.role === "answer_segment").length,
    65,
  );
  assert.deepEqual(result.citations, ["https://example.org/source"]);
  document.messages[3].block.text.deltas = deltas.map(() => "界".repeat(513));
  assert.equal(validate({ answer_segments }, document), null);
});

test("grounds a citation URL in a unique exact source quote and rejects fabricated or ambiguous quotes", () => {
  const document = structuredClone(modern);
  const url = "https://example.org/source";
  document.messages[3].block.text.content = `A supported answer cites [the public source](${url}).`;
  const citation = {
    url: { path: "/messages/3/block/text/content", quote: url },
    usage: modernExtracted.citations[0].usage,
  };
  const result = validate({ citations: [citation] }, document);
  assert.deepEqual(result.citations, [url]);
  assert.equal(result.raw_answer, document.messages[3].block.text.content);
  assert.equal(
    validate(
      {
        citations: [
          {
            ...citation,
            url: { ...citation.url, quote: "https://example.org/fabricated" },
          },
        ],
      },
      document,
    ),
    null,
  );
  assert.equal(
    validate(
      {
        citations: [
          {
            ...citation,
            url: { ...citation.url, quote: ` ${url}` },
          },
        ],
      },
      document,
    ),
    null,
  );
  document.messages[3].block.text.content += ` Again: ${url}`;
  assert.equal(validate({ citations: [citation] }, document), null);
});

test("preserves whitespace-only streaming deltas but rejects a whitespace-only final answer", () => {
  const document = structuredClone(modern);
  document.messages[3].block.text.deltas = [
    "A",
    " ",
    "source-backed",
    "\n",
    "answer.",
    "\t",
  ];
  const answer_segments = document.messages[3].block.text.deltas.map(
    (_, index) => ({
      path: `/messages/3/block/text/deltas/${index}`,
    }),
  );
  const result = validate({ answer_segments }, document);
  assert.equal(result.raw_answer, "A source-backed\nanswer.\t");
  assert.deepEqual(result.citations, ["https://example.org/source"]);
  assert.equal(
    validate(
      {
        answer_segments: [
          answer_segments[1],
          answer_segments[3],
          answer_segments[5],
        ],
      },
      document,
    ),
    null,
  );
  document.messages[3].block.text.deltas[1] = "";
  assert.equal(validate({ answer_segments }, document), null);
});

test("grounds a complete observation in exact source values and records bounded audit refs", () => {
  const result = validate();
  assert.equal(result.raw_answer, "Answer from the source.");
  assert.deepEqual(result.citations, ["https://example.org/source"]);
  assert.equal(result.chat_id, "chat-1");
  assert.equal(result.message_id, "answer-1");
  assert.equal(result.block_id, "tool-1");
  assert.equal(result.audit.method, "llm_grounded");
  assert.ok(
    result.audit.refs.some(
      (ref) =>
        ref.role === "completion" &&
        ref.path === "/messages/1/message/status" &&
        ref.value === "MESSAGE_STATUS_COMPLETED",
    ),
  );
  assert.ok(
    result.audit.refs.some(
      (ref) => ref.role === "citation_usage" && ref.value === "source-1",
    ),
  );
  assert.ok(
    result.audit.refs.some(
      (ref) => ref.role === "answer_segment" && ref.value === result.raw_answer,
    ),
  );
});

test("accepts an unrelated provider schema and exact unique Unicode quote from page text", () => {
  const document = {
    messages: [
      { conversation: { key: "c-2" } },
      {
        reply: {
          key: "m-2",
          state: { finished: true },
          excerpt: "前文🌍答案。后文",
        },
      },
      { event: { key: "b-2", owner: "m-2", search: { hits: [1] } } },
      { paragraph: { owner: "m-2" } },
      {
        references: [{ href: "https://example.net/verified", marker: "used" }],
      },
    ],
    rendered_text: "问题。前文🌍答案。后文",
  };
  const extracted = {
    decision: "searched_answer",
    chat_id: { path: "/messages/0/conversation/key" },
    message_id: { path: "/messages/1/reply/key" },
    answer_owner: { path: "/messages/3/paragraph/owner" },
    search_owner: { path: "/messages/2/event/owner" },
    search_block_id: { path: "/messages/2/event/key" },
    completion: { path: "/messages/1/reply/state" },
    search_activity: { path: "/messages/2/event/search/hits" },
    answer_segments: [
      { path: "/messages/1/reply/excerpt", quote: "🌍答案。" },
      { path: "/rendered_text", quote: "后文" },
    ],
    citations: [
      {
        url: { path: "/messages/4/references/0/href" },
        usage: { path: "/messages/4/references/0/marker" },
      },
    ],
  };
  const result = validateAiObservation(document, extracted);
  assert.equal(result.raw_answer, "🌍答案。后文");
  assert.equal(
    result.audit.refs.find(
      (ref) => ref.role === "answer_segment" && ref.path === "/rendered_text",
    ).value,
    "后文",
  );
});

test("returns no verified observation without every required ownership and source proof", () => {
  assert.equal(validateAiObservation(modern, { decision: "unverified" }), null);
  assert.deepEqual(validate({ citations: [] }).citations, []);
  assert.equal(validate({ answer_segments: [] }), null);
  assert.equal(validate({ message_id: { path: "/messages/0/chat/id" } }), null);
  assert.equal(validate({ search_block_id: modernExtracted.message_id }), null);
  assert.equal(
    validate({
      completion: { path: "/messages/2/block/tool/unknown" },
    }),
    null,
  );
  assert.equal(
    validate({
      search_activity: {
        path: "/messages/2/block/tool/contents/0/searchResult",
      },
    }),
    null,
    "empty object is not search activity",
  );
  assert.equal(
    validate({
      search_activity: { path: "/messages/1/message/role" },
      answer_owner: { path: "/messages/2/block/messageId" },
      citations: [
        {
          url: { path: "/messages/1/message/role" },
          usage: modernExtracted.citations[0].usage,
        },
      ],
    }),
    null,
    "invented URL fails even if another field is nonempty",
  );
});

test("JSON Pointers require own properties, canonical indexes, and safe escapes", () => {
  for (const path of [
    "/messages/01/message/id",
    "/messages/-1/message/id",
    "/messages/1/message/id/~2",
    "/messages/1/message/__proto__",
    "/messages/1/message/constructor",
    "/messages/1/message/missing",
    "/messages/999/message/id",
    "",
  ]) {
    assert.equal(validate({ message_id: { path } }), null, path);
  }
  const inherited = Object.create({ messages: modern.messages });
  assert.equal(validate({}, inherited), null);
});

test("rejects fabricated citations and unsafe or non-public URLs", () => {
  for (const url of [
    "javascript:alert(1)",
    "https://user:password@example.org/",
    "https://localhost/private",
    "http://127.0.0.1/private",
    "http://10.1.2.3/private",
    "http://192.168.1.5/private",
    "http://[::1]/private",
    "http://example.invalid/",
    "https://example.org".padEnd(2100, "x"),
  ]) {
    const document = structuredClone(modern);
    document.messages[1].message.refs.searchChunks[0].base.url = url;
    assert.equal(validate({}, document), null, url.slice(0, 40));
  }
  assert.equal(
    validate({
      citations: [
        {
          ...modernExtracted.citations[0],
          usage: { path: "/messages/1/message/refs/searchChunks/0/missing" },
        },
      ],
    }),
    null,
  );
  const document = structuredClone(modern);
  document.messages[1].message.refs.searchChunks[0].id = "";
  assert.equal(validate({}, document), null);
});

test("quotes must be unique and exact; offsets must not split Unicode surrogates", () => {
  assert.equal(
    validate({
      answer_segments: [{ path: "/rendered_text", quote: "not present" }],
    }),
    null,
  );
  assert.equal(
    validate({
      answer_segments: [
        { path: "/rendered_text", quote: "Answer", start: 0, end: 6 },
      ],
    }),
    null,
  );
  assert.equal(
    validate({
      answer_segments: [
        { path: "/messages/3/block/text/content", start: -1, end: 3 },
      ],
    }),
    null,
  );
  const repeated = { ...modern, rendered_text: "same same" };
  assert.equal(
    validate(
      {
        answer_segments: [{ path: "/rendered_text", quote: "same" }],
      },
      repeated,
    ),
    null,
  );
  const unicode = structuredClone(modern);
  unicode.rendered_text = "🌍";
  assert.equal(
    validate(
      {
        answer_segments: [{ path: "/rendered_text", start: 0, end: 1 }],
      },
      unicode,
    ),
    null,
  );
});

test("enforces answer and citation bounds, storing a digest for long evidence", () => {
  const large = structuredClone(modern);
  large.messages[3].block.text.content = "x".repeat(100_001);
  assert.equal(validate({}, large), null);
  large.messages[3].block.text.content = "x".repeat(2_000);
  const result = validate({}, large);
  const evidence = result.audit.refs.find(
    (ref) => ref.role === "answer_segment",
  );
  assert.equal(
    evidence.byte_length,
    2_002,
    "JSON value includes string quotes",
  );
  assert.equal(evidence.preview.length, 512);
  assert.match(evidence.sha256, /^[0-9a-f]{64}$/u);
  assert.equal(
    validate({
      citations: Array.from({ length: 51 }, () => modernExtracted.citations[0]),
    }),
    null,
  );
});
