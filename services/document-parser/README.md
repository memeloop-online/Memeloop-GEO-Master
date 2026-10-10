# Isolated PDF and Office document adapter

This service processes caller-supplied PDF, DOCX and XLSX bytes only. It cannot fetch URLs or
read host documents. Keep it on an internal network; the Rust application owns
authentication, authorization, upload verification, durable jobs and evidence.
The Java adapter never performs OCR. Deploy with a read-only root filesystem,
`/tmp` as a writable tmpfs, a memory limit, and no outbound network access.
It creates no persistent files and does not log document contents.

Each request is parsed in a disposable child JVM, never in the HTTP JVM.
The supervisor streams PDF bytes over the child's stdin and concurrently
consumes at most 32 MiB of its JSON stdout. It starts the child with fixed
operation/page arguments, a fixed Java executable and its own artifact
classpath, a 256 MiB heap and a scrubbed environment; parser diagnostics on
stderr are discarded. Its 20-second wall-clock deadline forcibly kills and
reaps the entire child before releasing its local capacity permit, returning
HTTP 504 `parser_timeout` with `retryable:true`. A child crash or protocol
error returns HTTP 502 `parser_failed` with `retryable:true`; an oversized
child response returns `output_too_large`. A client abandoning an already
uploaded request does not cancel parsing immediately: the independent child
still terminates by the same 20-second deadline. This deadline is not a
substitute for a container CPU quota and memory limit.

The 1 GiB Docker profile defaults to one parser slot with a 320 MiB HTTP
heap plus one 256 MiB worker heap and native JVM overhead. Raising
`GEO_PDF_MAX_CONCURRENT` adds one 256 MiB child heap and native overhead per
slot; increase the container memory limit accordingly.

Pinned parser profile: `tika-3.2.3_pdfbox-3.0.5_text-v1`. The Apache Tika
3.2.3 parent POM pins Apache PDFBox 3.0.5.

Pinned Office profile: `poi-5.4.1_ooxml-struct-v1` with Apache POI OOXML 5.4.1.
The original PDF routes and profile remain unchanged. Both formats share the
same bounded request semaphore and disposable worker process, 20-second
deadline, 100 MiB upload limit, and 32 MiB child stdout limit. The Office
worker validates OPC package structure and declared format, rejects external
package relationships, suspicious archive entries and expanded ZIP data over
256 MiB. It does not run formulas, macros, URL fetching, external relationships
or file-path resolution. An unreadable document returns a fixed error code,
never an exception, document text, filename or path.

`POST /v1/office/inspect` accepts raw bytes with the exact DOCX or XLSX OOXML
Content-Type (not a multipart body). It returns a `geo.office.parse.v1`
manifest with `input_sha256` of the original bytes, `parser_version`,
`media_type`, and a format-tagged `document` with deterministic `units`.
DOCX units cover at most 64 top-level body elements each; body indices are
zero-based. XLSX units cover at most 128 original worksheet row numbers each;
row coordinates and worksheet identity are one-based and zero-based
respectively. Sparse row ranges do not synthesize absent rows. The manifest
permits at most 20,000 units.

`POST /v1/office/units/{unit_id}/parse` with exactly the same original bytes
returns the same identity fields and `result`, tagged `docx_success`,
`xlsx_success`, or `failure` with a static unit-level code. DOCX paragraphs
carry their heading path and original body/paragraph indices; DOCX tables
preserve document order, row/column indices, cell text and verified merge
spans. XLSX cells carry worksheet row and A1 reference, raw stored value,
declared number-format display, typed kind, explicit merged range if present,
and formula source plus cached type/value only when a stored cache exists.
No numeric zero is inferred for formulas with absent caches. `header_range`
comes only from actual workbook table metadata, never first-row guessing.
Unsupported or oversized units fail independently; one failed unit does not
discard successful units. This path never fabricates PDF page or OCR locators.

`GET /health` retains its original PDF fields and adds
`office_schema_version`, `office_parser_version`, `office_capacity`. Capacity
reports configured concurrent parser slots, not currently free slots.
Malformed OOXML, encrypted packages and package/extraction limits return
static non-retryable error codes, while exhausted local capacity and child
deadline/crash retain retryable `parser_busy`, `parser_timeout`,
`parser_failed`. Shaded distribution retains Apache POI and dependency
LICENSE/NOTICE texts, checked by package integration tests.

`POST /v1/pdf/inspect` with raw `application/pdf` bytes (at most 100 MiB):

```json
{"schema_version":"geo.pdf.parse.v1","input_sha256":"<lowercase hex>","parser_version":"tika-3.2.3_pdfbox-3.0.5_text-v1","page_count":2}
```

`POST /v1/pdf/pages/1/parse` with the *same raw bytes*:

```json
{"schema_version":"geo.pdf.parse.v1","input_sha256":"<lowercase hex>","parser_version":"tika-3.2.3_pdfbox-3.0.5_text-v1","page":1,"text":"Page one"}
```

An empty text result includes `"reason":"ocr_required"` when its PDF page
contains raster images or `"reason":"empty_text"` otherwise. These are
evidence limitations, not successful OCR. Each response must be bound by its
SHA-256, parser profile and original one-based page number. Invalid PDF,
encrypted input, out-of-range pages and limits return only static error codes
in `{"error":{"code":"...","retryable":false}}`.
Raster detection includes inline `BI` images in page content and nested Form
XObjects; it does not turn those pixels into extracted text.
The local concurrency-limit response is HTTP 503
`{"error":{"code":"parser_busy","retryable":true}}`.

Limits: 100 MiB input, 10,000 pages per PDF, 8 MiB re-serialized single page,
4,000,000 text characters and 4 MiB UTF-8 page text. A page over the limits
fails independently; the document need not be declared wholly parsed.

`GET /health` returns the fixed `schema_version`, `parser_version` and numeric
`capacity` (the configured local parser concurrency, 1–64; server default 2,
Docker's 1 GiB profile default 1) without
accessing or disclosing any submitted document. It does not claim free slots.
The standalone server disables PDFBox/Commons Logging diagnostics, since
library diagnostic messages can include untrusted document text. Its shaded
runtime has no SLF4J logging provider.

Build/test with Java 21+ and Maven:

```text
mvn -B -ntp -f services/document-parser/pom.xml clean verify
```

Use a build cache and temporary directory outside a space-constrained
workspace if needed (`-Dmaven.repo.local=<cache-directory>`). Production
container: `docker build -t geo-document-parser services/document-parser`.
