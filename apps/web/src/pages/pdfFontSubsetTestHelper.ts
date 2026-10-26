export { Blob as NodeBlob } from "node:buffer";
import { pathToFileURL } from "node:url";
import { resolve } from "node:path";
import { readFileSync } from "node:fs";

export const fullFontBytes = new Uint8Array(
  readFileSync(
    "../../packages/browser-runner/assets/fonts/NotoSansCJKsc-Regular.otf",
  ),
);

type Subsetter = (input: {
  codepoints: number[];
  signal?: AbortSignal;
}) => Promise<Uint8Array>;

// Use the production HarfBuzz utility without adding its Node-only runtime to
// the browser's module graph. Each test response contains the actual subset.
const utilityUrl = pathToFileURL(
  resolve("../../packages/browser-runner/src/pdf-font-subset.mjs"),
).href;
const subsetter: Promise<Subsetter> = import(
  /* @vite-ignore */ utilityUrl
).then((utility) =>
  utility.createPdfFontSubsetter({ fontBytes: fullFontBytes }),
);

export async function subsetFontResponse(
  _url: unknown,
  init?: RequestInit,
): Promise<Response> {
  const { codepoints } = JSON.parse(String(init?.body)) as {
    codepoints: number[];
  };
  const bytes = await (
    await subsetter
  )({
    codepoints,
    signal: init?.signal ?? undefined,
  });
  return new Response(new Uint8Array(bytes), {
    status: 200,
    headers: { "Content-Type": "font/otf" },
  });
}
