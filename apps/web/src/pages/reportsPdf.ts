import fontkit from "@pdf-lib/fontkit";
import { PDFDocument, rgb, type PDFFont, type PDFPage } from "pdf-lib";
import type { ReportCoverage, ReportSnapshot } from "../api/reports";
import { reportAnalysisText as analysisText } from "../i18n/reportAnalysis";

const PAGE_WIDTH = 595.28; // A4 in PDF points
const PAGE_HEIGHT = 841.89;
const MARGIN = 46;
const CONTENT_WIDTH = PAGE_WIDTH - MARGIN * 2;
const FOOTER_Y = 27;
const CONTENT_BOTTOM = 53;
const INK = rgb(0.13, 0.17, 0.23);
const MUTED = rgb(0.35, 0.4, 0.46);

/** Measure and split by Unicode code point, so even an unbroken opaque key fits. */
export function wrapPdfText(
  value: string,
  maxWidth: number,
  widthOf: (value: string) => number,
): string[] {
  if (maxWidth <= 0) throw new Error("PDF text width must be positive");
  const lines: string[] = [];
  for (const paragraph of value.split(/\r\n|\r|\n/u)) {
    let line = "";
    for (const character of paragraph) {
      if (line && widthOf(line + character) > maxWidth) {
        lines.push(line);
        line = "";
      }
      // A glyph wider than the usable page width cannot be clipped silently.
      if (widthOf(character) > maxWidth) {
        throw new Error("PDF 字体中有字符无法适配页面，请更换字体后重试。");
      }
      line += character;
    }
    lines.push(line);
  }
  return lines;
}

function printable(value: string, supported: Set<number>): string {
  return [...value]
    .map((character) => {
      const code = character.codePointAt(0)!;
      if (character === "\n" || character === "\r") return character;
      if (code < 32 || code === 127 || !supported.has(code)) {
        return `[U+${code.toString(16).toUpperCase().padStart(4, "0")}]`;
      }
      return character;
    })
    .join("");
}

function display(value: string | number | null | undefined): string {
  return value === null || value === undefined ? "未记录" : String(value);
}

function localTime(value: string | null, timezone: string): string {
  if (!value) return "未记录";
  const timestamp = Date.parse(value);
  if (Number.isNaN(timestamp)) return value;
  try {
    return (
      new Intl.DateTimeFormat("zh-CN", {
        timeZone: timezone,
        year: "numeric",
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
        hourCycle: "h23",
      }).format(timestamp) + ` (${timezone})`
    );
  } catch {
    return `${value} (报告时区无效：${timezone})`;
  }
}

class Layout {
  private page!: PDFPage;
  private y = 0;
  private readonly pages: PDFPage[] = [];
  private readonly supported: Set<number>;

  constructor(
    private readonly document: PDFDocument,
    private readonly font: PDFFont,
    private readonly snapshot: ReportSnapshot,
    private readonly signal?: AbortSignal,
  ) {
    this.supported = new Set(font.getCharacterSet());
    this.newPage();
  }

  private newPage(): void {
    this.page = this.document.addPage([PAGE_WIDTH, PAGE_HEIGHT]);
    this.pages.push(this.page);
    this.y = PAGE_HEIGHT - MARGIN;
  }

  private ensureSpace(height: number): void {
    if (this.y - height < CONTENT_BOTTOM) this.newPage();
  }

  private textWidth(value: string, size: number): number {
    return this.font.widthOfTextAtSize(value, size);
  }

  private drawLine(
    line: string,
    x: number,
    y: number,
    size: number,
    muted: boolean,
    page = this.page,
  ): void {
    page.drawText(line, {
      x,
      y,
      size,
      font: this.font,
      color: muted ? MUTED : INK,
    });
  }

  text(
    value: string,
    options: {
      size?: number;
      indent?: number;
      gap?: number;
      muted?: boolean;
    } = {},
  ): void {
    this.signal?.throwIfAborted();
    const size = options.size ?? 9;
    const indent = options.indent ?? 0;
    const lineHeight = size * 1.55;
    const width = CONTENT_WIDTH - indent;
    const clean = printable(value, this.supported);
    const lines = wrapPdfText(clean, width, (candidate) =>
      this.textWidth(candidate, size),
    );
    for (const line of lines) {
      this.ensureSpace(lineHeight);
      this.drawLine(
        line,
        MARGIN + indent,
        this.y - size,
        size,
        !!options.muted,
      );
      this.y -= lineHeight;
    }
    this.y -= options.gap ?? 3;
  }

  heading(title: string): void {
    const headingLines = wrapPdfText(
      printable(title, this.supported),
      CONTENT_WIDTH,
      (line) => this.textWidth(line, 13),
    ).length;
    this.ensureSpace(12 + headingLines * (13 * 1.55) + 8 + 9 * 1.55);
    this.y -= 12;
    this.text(title, { size: 13, gap: 8 });
  }

  field(label: string, value: string | number | null | undefined): void {
    this.text(`${label}：${display(value)}`, { indent: 10 });
  }

  coverage(title: string, coverage: ReportCoverage): void {
    this.heading(title);
    this.field("清单状态", coverage.availability);
    this.field("冻结计划分母", coverage.expected_count ?? "未知");
    this.field("已观察", coverage.observed_count);
    if (coverage.reason) this.field("覆盖缺口原因", coverage.reason);
    if (coverage.availability !== "available") {
      this.text("缺失或未封存的输入不等于失败、未提及或零分。", {
        indent: 10,
        muted: true,
      });
    }
    const counts = Object.entries(coverage.counts);
    if (!counts.length) this.field("分类计数", "无");
    for (const [key, count] of counts) this.field(`冻结状态 ${key}`, count);
  }

  finish(): void {
    for (const [index, page] of this.pages.entries()) {
      const footer = `不可变周报 · 修订 ${this.snapshot.revision} · ${index + 1} / ${this.pages.length}`;
      const lines = wrapPdfText(
        printable(footer, this.supported),
        CONTENT_WIDTH,
        (line) => this.textWidth(line, 8),
      );
      for (const [lineIndex, line] of lines.entries()) {
        this.drawLine(
          line,
          MARGIN,
          FOOTER_Y + (lines.length - lineIndex - 1) * 9,
          8,
          true,
          page,
        );
      }
    }
  }
}

/** Build solely from the selected immutable snapshot; no API calls or writes. */
export async function buildReportPdf(
  snapshot: ReportSnapshot,
  fontBytes: Uint8Array,
  signal?: AbortSignal,
): Promise<Uint8Array> {
  signal?.throwIfAborted();
  if (!fontBytes.length) {
    throw new Error("报告中文字库不可用，请刷新页面后重试下载。");
  }
  const document = await PDFDocument.create();
  document.registerFontkit(fontkit);
  let font: PDFFont;
  try {
    // CFF subsetting loses Chinese outlines in PDFBox. This font's localized
    // alternate digits map to private-use code points in pdf-lib's ToUnicode
    // table, so disable that substitution to preserve searchable ASCII IDs.
    font = await document.embedFont(fontBytes, {
      subset: false,
      features: { locl: false },
    });
    if (!font.getCharacterSet().includes("中".codePointAt(0)!)) {
      throw new Error("Chinese glyph missing");
    }
  } catch {
    throw new Error("报告中文字库无效，请刷新页面后重试下载。");
  }
  document.setTitle(`周报 ${snapshot.report_id} 修订 ${snapshot.revision}`);
  document.setSubject("不可变报告快照；无商业费用和下一轮动作数据");
  document.setCreator("GEO");
  document.setProducer("GEO PDF export");
  const generated = new Date(snapshot.generated_at);
  if (!Number.isNaN(generated.getTime())) {
    document.setCreationDate(generated);
    document.setModificationDate(generated);
  }

  const layout = new Layout(document, font, snapshot, signal);
  const at = (value: string | null) =>
    localTime(value, snapshot.report_timezone);
  layout.text("每周报告 · 不可变快照", { size: 19, gap: 12 });
  layout.text(
    snapshot.status === "partial"
      ? "部分覆盖：只陈述冻结快照内的证据，未完成或缺失输入不推断为失败。"
      : "完整覆盖：仅对应本次冻结快照，不代表未提供的商业或后续动作数据。",
    { size: 10, gap: 10 },
  );
  layout.text("字体不支持的原字符显示为 U+ 码位，避免无声丢失证据字符。", {
    size: 8,
    muted: true,
  });
  layout.heading("快照、周期与更正关系");
  layout.field("报告 ID", snapshot.report_id);
  layout.field("项目 ID", snapshot.project_id);
  layout.field("周期 ID", snapshot.cycle_id);
  layout.field("修订", snapshot.revision);
  layout.field(
    "更正自",
    snapshot.correction_of ?? "无；此版本未声明更正前快照",
  );
  layout.field("状态", snapshot.status);
  layout.field("报告时区", snapshot.report_timezone);
  layout.field("窗口开始", at(snapshot.report_window_start_at));
  layout.field("窗口结束", at(snapshot.report_window_end_at));
  layout.field("冻结截止", at(snapshot.cutoff_at));
  layout.field("证据水位", at(snapshot.evidence_as_of));
  layout.field("生成时间", at(snapshot.generated_at));
  layout.field("汇聚版本", snapshot.reducer_version);
  layout.field("输入摘要", snapshot.input_hash);

  layout.heading("冻结输入清单");
  if (!snapshot.input_manifest_versions.length) {
    layout.text("没有输入清单版本；不能将覆盖范围推断为完整。");
  }
  for (const [index, entry] of snapshot.input_manifest_versions.entries()) {
    await yieldToBrowser(index, signal);
    layout.text(
      `${entry.kind} · ${entry.manifest_id} · 修订 ${entry.revision} · ${entry.sealed ? "已封存" : "未封存"} · 计划 ${entry.expected_count ?? "未知"}`,
      { indent: 10 },
    );
  }

  layout.coverage("文档覆盖与资料缺口", snapshot.documents);
  layout.coverage("发布目标覆盖", snapshot.publications);
  layout.coverage("独立 AI 渠道测量覆盖", snapshot.measurements);
  layout.heading("发布平台覆盖组");
  if (!snapshot.publication_groups.length) {
    layout.text("未提供逐平台组；不能推断各平台状态。");
  }
  for (const [index, group] of snapshot.publication_groups.entries()) {
    await yieldToBrowser(index, signal);
    layout.coverage(`平台 ${group.platform_id}`, group.coverage);
  }
  layout.heading("AI 测量比较口径");
  layout.text("各冻结口径独立呈现；缺测不等于未提及，不跨协议合并为趋势。", {
    muted: true,
  });
  if (!snapshot.measurement_groups.length) {
    layout.text("未提供独立测量组；不能推断引用、提及或趋势。");
  }
  for (const [index, group] of snapshot.measurement_groups.entries()) {
    await yieldToBrowser(index, signal);
    layout.coverage(`比较键 ${group.comparison_key}`, group.coverage);
    layout.text(
      group.purpose === "optimization"
        ? "问题用途：优化"
        : group.purpose === "frozen_evaluation"
          ? "问题用途：冻结评估（不进入优化）"
          : "问题用途：旧未分类（不进入优化）",
      { muted: true },
    );
  }

  if (snapshot.supplementary_measurements?.length) {
    layout.heading(analysisText("title"));
    layout.text(
      analysisText("count", {
        count: snapshot.supplementary_measurements.length,
      }),
    );
    layout.text(analysisText("note"));
    for (const [index, item] of snapshot.supplementary_measurements.entries()) {
      await yieldToBrowser(index, signal);
      const analysis = item.observation.provenance;
      layout.field(analysisText("target"), item.target_id);
      layout.field(analysisText("attempt"), item.attempt_id);
      layout.field(analysisText("plan"), item.plan_id);
      layout.field(analysisText("comparison"), item.comparison_key);
      layout.text(
        analysisText(
          item.question_binding?.purpose === "frozen_evaluation"
            ? "evaluation"
            : item.question_binding?.purpose === "optimization"
              ? "optimization"
              : "unclassified",
        ),
      );
      layout.field(analysisText("observed"), at(item.observation.observed_at));
      layout.field(analysisText("received"), at(item.observation.received_at));
      if (analysis) {
        layout.field(analysisText("analyzed"), at(analysis.analyzed_at));
        layout.field(analysisText("model"), analysis.actual_model);
        layout.field(analysisText("config"), analysis.config_revision);
        layout.field(analysisText("revision"), analysis.revision_id);
        layout.field(analysisText("source"), JSON.stringify(analysis.source));
        layout.field(analysisText("digest"), analysis.source_sha256);
        layout.field(analysisText("parser"), analysis.parser_version);
        layout.field(analysisText("prompt"), analysis.prompt_version);
      }
      layout.field(analysisText("answer"), item.observation.raw_answer);
      for (const url of item.observation.citations)
        layout.field(analysisText("citations"), url);
      if (!item.observation.citations.length)
        layout.text(analysisText("noCitations"));
    }
  }

  layout.heading("结论与证据引用");
  if (!snapshot.findings.length) {
    layout.text("此快照没有可追溯结论；不推断效果变化。");
  }
  const evidenceIds = new Set(
    snapshot.evidence.map((item) => item.evidence_id),
  );
  for (const [index, finding] of snapshot.findings.entries()) {
    await yieldToBrowser(index, signal);
    layout.field("结论 ID", finding.finding_id);
    layout.field("类型", finding.kind);
    layout.field("摘要", finding.summary);
    layout.field("数据不足原因", finding.insufficient_reason ?? "未提供");
    if (!finding.evidence_ids.length) {
      layout.text("此结论未引用证据；不视作已验证效果。", {
        indent: 10,
        muted: true,
      });
    }
    for (const id of finding.evidence_ids) {
      layout.field(
        "引用证据 ID",
        `${id}${evidenceIds.has(id) ? "" : "（不在快照证据中）"}`,
      );
    }
  }

  layout.heading("商业与后续动作");
  layout.text(
    "费用、预留、结算、资产明细和下一轮动作未由此快照提供；此 PDF 不生成替代数据。",
  );
  layout.heading("证据附录 · 快照内全部证据");
  if (!snapshot.evidence.length) layout.text("此快照未包含证据引用。");
  for (const [index, item] of snapshot.evidence.entries()) {
    await yieldToBrowser(index, signal);
    layout.field("证据 ID", item.evidence_id);
    layout.field("证据类型", item.kind);
    layout.field("资源 ID", item.resource_id);
    layout.field("资源版本", item.resource_version);
    layout.field("发生时间", at(item.occurred_at));
    layout.field("接收时间", at(item.received_at));
    layout.field("摘要", item.summary);
    layout.text("", { gap: 5 });
  }
  layout.finish();
  try {
    signal?.throwIfAborted();
    const bytes = await document.save();
    signal?.throwIfAborted();
    return bytes;
  } catch {
    signal?.throwIfAborted();
    throw new Error("报告 PDF 生成失败，请刷新页面后重试下载。");
  }
}

async function yieldToBrowser(
  index: number,
  signal?: AbortSignal,
): Promise<void> {
  signal?.throwIfAborted();
  if (index > 0 && index % 20 === 0) {
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    signal?.throwIfAborted();
  }
}

/** Invoked after the user selects an already-loaded, saved report. */
export async function downloadReportPdf(
  snapshot: ReportSnapshot,
  options: { signal?: AbortSignal } = {},
): Promise<void> {
  options.signal?.throwIfAborted();
  let response: Response;
  try {
    response = await fetch("/fonts/NotoSansCJKsc-Regular.otf", {
      credentials: "same-origin",
      signal: options.signal,
    });
    if (!response.ok) throw new Error("Font unavailable");
  } catch (error) {
    options.signal?.throwIfAborted();
    if (error instanceof DOMException && error.name === "AbortError")
      throw error;
    throw new Error("报告中文字库下载失败，请检查网络后重试。");
  }
  let fontBytes: Uint8Array;
  try {
    fontBytes = new Uint8Array(await response.arrayBuffer());
  } catch {
    options.signal?.throwIfAborted();
    throw new Error("报告中文字库下载不完整，请检查网络后重试。");
  }
  const bytes = await buildReportPdf(snapshot, fontBytes, options.signal);
  options.signal?.throwIfAborted();
  const blob = new Blob([new Uint8Array(bytes)], { type: "application/pdf" });
  options.signal?.throwIfAborted();
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = `report-${snapshot.report_id}-r${snapshot.revision}.pdf`;
  link.hidden = true;
  document.body.append(link);
  try {
    options.signal?.throwIfAborted();
    link.click();
  } finally {
    link.remove();
    URL.revokeObjectURL(url);
  }
}
