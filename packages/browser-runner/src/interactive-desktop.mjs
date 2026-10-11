import { randomBytes, randomInt } from "node:crypto";
import { spawn } from "node:child_process";
import { createServer, connect } from "node:net";
import { mkdtemp, mkdir, chmod, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const READY_TIMEOUT_MS = 12_000;

async function freePort() {
  const server = createServer();
  try {
    await new Promise((resolve, reject) => {
      server.once("error", reject);
      server.listen(0, "127.0.0.1", resolve);
    });
    return server.address().port;
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
}

function runWithInput(command, args, input) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: ["pipe", "pipe", "ignore"] });
    const chunks = [];
    child.stdout.on("data", (chunk) => chunks.push(chunk));
    child.stdin.on("error", () => {});
    child.on("error", reject);
    child.on("close", (code) =>
      code === 0
        ? resolve(Buffer.concat(chunks))
        : reject(new Error("desktop_setup_failed")),
    );
    child.stdin.end(input);
  });
}

function start(command, args, env) {
  const child = spawn(command, args, {
    env,
    stdio: "ignore",
    // Process children are reaped on normal runner shutdown.
    detached: false,
  });
  child.on("error", () => {});
  return child;
}

async function ready(child, port) {
  const deadline = Date.now() + READY_TIMEOUT_MS;
  while (Date.now() < deadline) {
    if (!child.pid || child.exitCode !== null || child.signalCode !== null)
      throw new Error("desktop_process_exited");
    const connected = await new Promise((resolve) => {
      const socket = connect({ host: "127.0.0.1", port });
      socket.once("connect", () => {
        socket.destroy();
        resolve(true);
      });
      socket.once("error", () => resolve(false));
    });
    if (connected) return;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error("desktop_start_timeout");
}

async function stop(child) {
  if (!child?.pid || child.exitCode !== null || child.signalCode !== null)
    return;
  const exited = new Promise((resolve) => child.once("exit", resolve));
  child.kill("SIGTERM");
  let timer;
  await Promise.race([
    exited,
    new Promise((resolve) => {
      timer = setTimeout(resolve, 2_000);
    }),
  ]);
  clearTimeout(timer);
  if (child.exitCode === null && child.signalCode === null) {
    child.kill("SIGKILL");
    await exited;
  }
}

/**
 * Linux-only, private desktop transport. The endpoint is for a trusted
 * server-side gateway, NEVER an API response or an Internet-facing port.
 * Process/display separation is not a container security boundary: deploy
 * each untrusted session in its own restricted container/pod.
 */
export function createLinuxDesktopRuntime({
  spawnProcess = start,
  reservePort = freePort,
  waitReady = ready,
  stopProcess = stop,
  runCommand = runWithInput,
} = {}) {
  if (process.platform !== "linux") throw new Error("linux_desktop_required");
  return {
    async open({
      browserType,
      browserChannel,
      proxy,
      storageState,
      viewport,
      onSession,
      onAcquired,
    }) {
      const directory = await mkdtemp(join(tmpdir(), "geo-desktop-"));
      await chmod(directory, 0o700);
      const authority = join(directory, "authority");
      const home = join(directory, "home");
      const configHome = join(home, ".config");
      const cacheHome = join(home, ".cache");
      let display;
      let displayLock;
      let xvnc;
      let relay;
      let windowManager;
      let browser;
      let closed = false;
      let inputClosed = false;
      let closePromise;
      const stopOnExit = () => {
        relay?.kill();
        windowManager?.kill();
        xvnc?.kill();
      };
      process.once("exit", stopOnExit);
      const endpoint = () => {
        if (closed || inputClosed) throw new Error("desktop_input_closed");
        return {
          host: "127.0.0.1",
          port: websocketPort,
        };
      };
      let inputClosePromise;
      const closeInput = () => {
        if (inputClosePromise) return inputClosePromise;
        if (inputClosed) return;
        inputClosed = true;
        inputClosePromise = Promise.resolve()
          .then(() => stopProcess(relay))
          .catch((error) => {
            inputClosed = false;
            throw error;
          })
          .finally(() => {
            inputClosePromise = undefined;
          });
        return inputClosePromise;
      };
      const close = () => {
        if (!closePromise) {
          closed = true;
          inputClosed = true;
          closePromise = (async () => {
            // Attempt every resource, but do not treat a rejected stop as
            // proof of absence or remove the retry/exit-cleanup handle.
            const results = await Promise.allSettled([
              stopProcess(relay),
              browser?.close(),
              stopProcess(windowManager),
              stopProcess(xvnc),
            ]);
            if (results.some((result) => result.status === "rejected"))
              throw new Error("desktop_close_failed");
            await rm(directory, { recursive: true, force: true });
            if (displayLock)
              await rm(displayLock, { recursive: true, force: true });
            process.removeListener("exit", stopOnExit);
          })().catch((error) => {
            closePromise = undefined;
            throw error;
          });
        }
        return closePromise;
      };
      let websocketPort;
      const resource = {
        endpoint,
        closeInput,
        close,
        get closed() {
          return closed;
        },
      };
      onSession?.(resource);
      try {
        // Chromium's crashpad database and Openbox config must be writable
        // even when the container root filesystem (including /home) is RO.
        // Never put browser profile/cache state in another session's HOME.
        await mkdir(home, { mode: 0o700 });
        await mkdir(configHome, { mode: 0o700 });
        await mkdir(cacheHome, { mode: 0o700 });
        for (const location of [home, configHome, cacheHome]) {
          const check = join(location, ".writable");
          await writeFile(check, "", { mode: 0o600, flag: "wx" });
          await rm(check);
        }
        for (let attempt = 0; attempt < 64; attempt++) {
          const candidate = randomInt(100, 4000);
          const lock = join(tmpdir(), `geo-display-${candidate}.lock`);
          try {
            await mkdir(lock, { mode: 0o700 });
            display = candidate;
            displayLock = lock;
            break;
          } catch (error) {
            if (error.code !== "EEXIST") throw error;
          }
        }
        if (!displayLock) throw new Error("desktop_display_exhausted");
        const cookie = randomBytes(16).toString("hex");
        // RFB remains private to this display's loopback and the authenticated
        // runner relay; a VNC credential is never needed in browser code.
        await runCommand(
          "xauth",
          ["-f", authority],
          `add :${display} MIT-MAGIC-COOKIE-1 ${cookie}\n`,
        );
        await chmod(authority, 0o600);
        const vncPort = await reservePort();
        websocketPort = await reservePort();
        if (vncPort === websocketPort) throw new Error("desktop_port_conflict");
        const env = {
          ...process.env,
          HOME: home,
          XDG_CONFIG_HOME: configHome,
          XDG_CACHE_HOME: cacheHome,
          DISPLAY: `:${display}`,
          XAUTHORITY: authority,
          XDG_RUNTIME_DIR: directory,
        };
        xvnc = spawnProcess(
          "Xtigervnc",
          [
            `:${display}`,
            "-geometry",
            `${viewport.width}x${viewport.height}`,
            "-depth",
            "24",
            "-localhost",
            "yes",
            "-nolisten",
            "tcp",
            "-SecurityTypes",
            "None",
            "-rfbport",
            String(vncPort),
            "-auth",
            authority,
          ],
          env,
        );
        await waitReady(xvnc, vncPort);
        windowManager = spawnProcess("openbox", ["--sm-disable"], env);
        relay = spawnProcess(
          "websockify",
          [`127.0.0.1:${websocketPort}`, `127.0.0.1:${vncPort}`],
          env,
        );
        await waitReady(relay, websocketPort);
        if (
          !windowManager.pid ||
          windowManager.exitCode !== null ||
          windowManager.signalCode !== null
        )
          throw new Error("desktop_window_manager_exited");
        browser = await browserType.launch({
          headless: false,
          ...(browserChannel ? { channel: browserChannel } : {}),
          env,
        });
        // Once the browser is owned, closing it also interrupts context/page
        // creation. Before this point launch can still materialize a resource.
        onAcquired?.();
        if (closed) throw new Error("desktop_input_closed");
        const context = await browser.newContext({
          viewport,
          ...(proxy ? { proxy } : {}),
          ...(storageState ? { storageState } : {}),
        });
        if (closed) throw new Error("desktop_input_closed");
        const page = await context.newPage();
        if (closed) throw new Error("desktop_input_closed");
        if (
          xvnc.exitCode !== null ||
          relay.exitCode !== null ||
          windowManager.exitCode !== null
        )
          throw new Error("desktop_process_exited");
        // An unexpected process exit invalidates this transport and browser.
        xvnc.once("exit", () => void close().catch(() => {}));
        windowManager.once("exit", () => {
          if (!closed) void close().catch(() => {});
        });
        relay.once("exit", () => {
          if (!inputClosed) void close().catch(() => {});
        });
        return Object.assign(resource, {
          context,
          page,
        });
      } catch (error) {
        await close();
        throw error;
      }
    },
  };
}
