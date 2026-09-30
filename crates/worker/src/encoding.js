// UTF-8 text conversion only. No filesystem, network, timers or browser surface.
(() => {
  const { encode, decode } = Deno.core;

  function bytesFrom(input) {
    if (input === undefined) return new Uint8Array(0);
    if (input instanceof ArrayBuffer) return new Uint8Array(input);
    if (ArrayBuffer.isView(input)) {
      return new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
    }
    throw new TypeError("TextDecoder input must be a BufferSource");
  }

  function validUtf8(bytes) {
    for (let i = 0; i < bytes.length;) {
      const first = bytes[i++];
      if (first < 0x80) continue;
      let count, minimum, maximum;
      if (first >= 0xc2 && first <= 0xdf) {
        count = 1; minimum = 0x80; maximum = 0xbf;
      } else if (first === 0xe0) {
        count = 2; minimum = 0xa0; maximum = 0xbf;
      } else if (first >= 0xe1 && first <= 0xec || first >= 0xee && first <= 0xef) {
        count = 2; minimum = 0x80; maximum = 0xbf;
      } else if (first === 0xed) {
        count = 2; minimum = 0x80; maximum = 0x9f;
      } else if (first === 0xf0) {
        count = 3; minimum = 0x90; maximum = 0xbf;
      } else if (first >= 0xf1 && first <= 0xf3) {
        count = 3; minimum = 0x80; maximum = 0xbf;
      } else if (first === 0xf4) {
        count = 3; minimum = 0x80; maximum = 0x8f;
      } else {
        return false;
      }
      if (i + count > bytes.length || bytes[i++] < minimum || bytes[i - 1] > maximum) return false;
      while (--count > 0) {
        const continuation = bytes[i++];
        if (continuation < 0x80 || continuation > 0xbf) return false;
      }
    }
    return true;
  }

  class TextEncoder {
    get encoding() { return "utf-8"; }
    encode(input = "") { return encode(String(input)); }
    encodeInto(input = "", destination) {
      if (!(destination instanceof Uint8Array)) {
        throw new TypeError("TextEncoder.encodeInto requires a Uint8Array");
      }
      const text = String(input);
      let read = 0, written = 0;
      while (read < text.length) {
        const codePoint = text.codePointAt(read);
        const codeUnits = codePoint > 0xffff ? 2 : 1;
        const scalar = codePoint >= 0xd800 && codePoint <= 0xdfff
          ? "\ufffd" : text.slice(read, read + codeUnits);
        const bytes = encode(scalar);
        if (written + bytes.length > destination.length) break;
        destination.set(bytes, written);
        written += bytes.length;
        read += codeUnits;
      }
      return { read, written };
    }
  }

  class TextDecoder {
    constructor(label = "utf-8", options = {}) {
      if (!["utf-8", "utf8", "unicode-1-1-utf-8"].includes(String(label).trim().toLowerCase())) {
        throw new RangeError("Only UTF-8 decoding is available");
      }
      this.fatal = Boolean(options.fatal);
      this.ignoreBOM = Boolean(options.ignoreBOM);
    }
    get encoding() { return "utf-8"; }
    decode(input, options = {}) {
      if (options.stream) throw new TypeError("Streaming UTF-8 decoding is unavailable");
      const bytes = bytesFrom(input);
      if (this.fatal && !validUtf8(bytes)) throw new TypeError("Invalid UTF-8 data");
      // deno_core.decode strips a leading BOM; ignoreBOM preserves it.
      if (this.ignoreBOM && bytes.length >= 3 &&
          bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf) {
        return "\ufeff" + decode(bytes.subarray(3));
      }
      return decode(bytes);
    }
  }

  Object.defineProperties(globalThis, {
    TextEncoder: { value: TextEncoder, writable: true, configurable: true },
    TextDecoder: { value: TextDecoder, writable: true, configurable: true },
  });
})();
