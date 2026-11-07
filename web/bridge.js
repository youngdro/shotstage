// Live bridge client: lets `shotstage mcp --live` (or `shotstage serve --live`) drive this
// editor. Every request the server sends runs here through the same API the page has
// (`editor.request` / `editor.call`), so an agent's edits appear on screen as they happen,
// land in the same undo history, and snapshots come from this browser's renderer.
//
//   import { connectBridge } from "./bridge.js";
//   const link = connectBridge(editor, "ws://127.0.0.1:7777/bridge", { onStatus: s => console.log(s) });
//   link.close();
//
// Wire format: the server sends {id, request}; this answers {id, ok: true, outcome: {result,
// revision, changed}} or {id, ok: false, error: {code, message}}.

/** Commands that resolve asynchronously in the browser (they render on the GPU). */
const ASYNC = new Set(["render.snapshot", "render.storyboard", "render.sequence"]);

async function run(editor, request) {
  if (request.batch || request.merge || !ASYNC.has(request.command)) {
    return editor.request(request);
  }
  const revision = editor.revision();
  const result = await editor.call(request.command, request.params ?? {});
  if (request.command === "render.snapshot" && result) delete result.dataUrl; // `data` is enough
  return { result, revision: editor.revision(), changed: editor.revision() !== revision };
}

/**
 * Connect `editor` to a bridge and keep reconnecting until `close()`.
 * @param {object} editor  a mounted editor (`mount(...)`)
 * @param {string} url     e.g. "ws://127.0.0.1:7777/bridge"
 * @param {{onStatus?: (status: "connecting"|"connected"|"disconnected", detail?: string) => void}} [options]
 */
export function connectBridge(editor, url, options = {}) {
  const onStatus = options.onStatus ?? (() => {});
  let ws = null;
  let closed = false;
  let delay = 500;
  let queue = Promise.resolve(); // run requests one at a time, in order

  const open = () => {
    if (closed) return;
    onStatus("connecting", url);
    ws = new WebSocket(url);
    ws.onopen = () => {
      delay = 500;
      onStatus("connected", url);
    };
    ws.onmessage = (ev) => {
      let msg;
      try {
        msg = JSON.parse(ev.data);
      } catch {
        return;
      }
      if (msg == null || msg.request == null) return;
      const socket = ws;
      queue = queue.then(async () => {
        let reply;
        try {
          reply = { id: msg.id, ok: true, outcome: await run(editor, msg.request) };
        } catch (e) {
          reply = { id: msg.id, ok: false, error: { code: e?.code ?? "failed", message: String(e?.message ?? e) } };
        }
        if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify(reply));
      });
    };
    ws.onclose = () => {
      onStatus("disconnected", url);
      if (!closed) {
        setTimeout(open, delay);
        delay = Math.min(delay * 2, 10000);
      }
    };
    ws.onerror = () => {}; // onclose follows
  };
  open();
  return {
    close() {
      closed = true;
      ws?.close();
    },
    get connected() {
      return ws?.readyState === WebSocket.OPEN;
    },
  };
}

/**
 * Where this page should connect, if anywhere: `?bridge=ws://...` in the address, or the
 * bridge that served the page (`/bridge.json`, answered by `shotstage mcp --live --web dist`).
 */
export async function findBridge() {
  const param = new URLSearchParams(location.search).get("bridge");
  if (param) return param;
  try {
    const r = await fetch("bridge.json", { cache: "no-store" });
    if (r.ok) {
      const info = await r.json();
      if (typeof info.ws === "string") return info.ws;
    }
  } catch {
    // not served by a bridge
  }
  return null;
}
