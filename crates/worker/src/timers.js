// deno_core owns timer scheduling, cancellation and event-loop liveness.
// This callback-only adapter exposes the globals required by approved bundles;
// string callbacks are deliberately unsupported (there is no eval capability).
(() => {
  const { createTimer, cancelTimer } = Deno.core;
  const timers = new Map();
  let nextId = 1;

  function setTimeout(callback, delay = 0, ...args) {
    if (typeof callback !== "function") {
      throw new TypeError("setTimeout requires a function callback");
    }
    const id = nextId++;
    const timer = createTimer(
      () => {
        timers.delete(id);
        callback(...args);
      },
      delay,
      undefined,
      false,
      true,
      false,
    );
    timers.set(id, timer);
    return id;
  }

  function clearTimeout(id) {
    const timer = timers.get(id);
    if (timer === undefined) return;
    timers.delete(id);
    cancelTimer(timer);
  }

  Object.defineProperties(globalThis, {
    setTimeout: { value: setTimeout, writable: true, configurable: true },
    clearTimeout: { value: clearTimeout, writable: true, configurable: true },
  });
})();
