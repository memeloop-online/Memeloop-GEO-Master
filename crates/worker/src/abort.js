// The bundle uses abort signals to propagate cancellation through one turn.
(() => {
  const states = new WeakMap();
  const token = Symbol("AbortSignal constructor");

  function abortSignal(signal, reason) {
    const state = states.get(signal);
    if (state.aborted) return;
    state.aborted = true;
    state.reason = reason;
    const event = { type: "abort", target: signal, currentTarget: signal };
    for (const [listener, once] of state.listeners) {
      if (!state.listeners.has(listener)) continue;
      if (once) state.listeners.delete(listener);
      if (typeof listener === "function") listener.call(signal, event);
      else listener.handleEvent?.(event);
    }
    if (typeof signal.onabort === "function") signal.onabort.call(signal, event);
  }

  class AbortSignal {
    onabort = null;

    constructor(key) {
      if (key !== token) throw new TypeError("Illegal constructor");
      states.set(this, { aborted: false, reason: undefined, listeners: new Map() });
    }

    get aborted() { return states.get(this).aborted; }
    get reason() { return states.get(this).reason; }

    addEventListener(type, listener, options = {}) {
      if (type !== "abort" || listener == null) return;
      const listeners = states.get(this).listeners;
      if (!listeners.has(listener)) listeners.set(listener, Boolean(options === true || options?.once));
    }

    removeEventListener(type, listener) {
      if (type === "abort") states.get(this).listeners.delete(listener);
    }

    throwIfAborted() {
      const state = states.get(this);
      if (state.aborted) throw state.reason;
    }
  }

  class AbortController {
    #signal = new AbortSignal(token);
    get signal() { return this.#signal; }
    abort(reason) {
      if (arguments.length === 0) {
        reason = new Error("The operation was aborted");
        reason.name = "AbortError";
      }
      abortSignal(this.#signal, reason);
    }
  }

  Object.defineProperties(globalThis, {
    AbortController: { value: AbortController, writable: true, configurable: true },
    AbortSignal: { value: AbortSignal, writable: true, configurable: true },
  });
})();
