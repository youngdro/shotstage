// Connect this editor to Claude (or any MCP client) through the relay, with nothing to install.
//
// The page opens a WebSocket to the relay (relay/, a Cloudflare Worker) under a private link
// code; the relay serves an MCP endpoint at https://<relay>/mcp/<code>. The person adds that
// address to Claude as a connector, and every tool call Claude makes runs here, in this
// editor, through the same API as the page (`editor.request` / `editor.call`): changes show up
// live and land in the undo history, snapshots come from this browser's renderer.
//
//   import { connectRelay, callTool } from "./agent.js";
//   const link = connectRelay(editor, "https://relay.example.workers.dev", code, { onStatus });
//
// Wire format: the relay sends {id, call: {name, arguments}}; the page answers {id, result}
// with an MCP CallToolResult.

import { TOOLS } from "./agent-tools.js";

/** How many pictures a storyboard / sequence sends back as images. */
const MAX_IMAGES = 12;
const DATA_URL = /^data:(image\/[a-z]+);base64,/;

const text = (v, isError = false) => ({
  content: [{ type: "text", text: typeof v === "string" ? v : JSON.stringify(v, null, 2) }],
  ...(v && typeof v === "object" && !Array.isArray(v) ? { structuredContent: v } : {}),
  isError,
});

const failure = (e) => text({ error: { code: e?.code ?? "failed", message: String(e?.message ?? e) } }, true);

/** Replace every data-URL picture in `value` by a short label; return them as MCP images. */
function pullImages(value) {
  const found = [];
  const walk = (v, path) => {
    if (typeof v === "string") {
      const m = DATA_URL.exec(v);
      if (!m) return v;
      found.push({ path, mimeType: m[1], data: v.slice(m[0].length) });
      return `(picture ${found.length})`;
    }
    if (Array.isArray(v)) return v.map((x, i) => walk(x, `${path}[${i}]`));
    if (v && typeof v === "object") {
      const out = {};
      for (const [k, x] of Object.entries(v)) {
        if (k === "html" || k === "dataUrl") continue; // the same pictures again
        out[k] = walk(x, path ? `${path}.${k}` : k);
      }
      return out;
    }
    return v;
  };
  const doc = walk(value, "");
  // Clay pictures first: they read best. Then spread the rest evenly over the board.
  const clay = found.filter((f) => !/depth|normal/.test(f.path));
  const pool = clay.length ? clay : found;
  const step = Math.max(1, Math.ceil(pool.length / MAX_IMAGES));
  const content = [{ type: "text", text: JSON.stringify(doc) }];
  pool.forEach((f, i) => {
    if (i % step) return;
    content.push({ type: "text", text: `picture ${found.indexOf(f) + 1}: ${f.path}` });
    content.push({ type: "image", data: f.data, mimeType: f.mimeType });
  });
  return { content, isError: false };
}

async function snapshot(editor, params) {
  const r = await editor.call("render.snapshot", params ?? {});
  const { data, mimeType, dataUrl, ...meta } = r;
  return {
    content: [
      { type: "image", data, mimeType: mimeType ?? "image/png" },
      { type: "text", text: JSON.stringify(meta) },
    ],
    isError: false,
  };
}

async function runCommand(editor, command, params) {
  if (command === "render.snapshot") return snapshot(editor, params);
  if (command === "render.storyboard" || command === "render.sequence") {
    return pullImages(await editor.call(command, params ?? {}));
  }
  if (command === "command.list") return text({ commands: editor.commands() });
  const before = editor.revision();
  const o = editor.request({ command, params: params ?? {} });
  return text({ result: o.result, revision: o.revision, changed: o.changed ?? o.revision !== before });
}

/** Run one MCP tool call against `editor`; resolves to an MCP CallToolResult. */
export async function callTool(editor, name, args = {}) {
  args = args ?? {};
  try {
    switch (name) {
      case "editor_status": {
        let summary = null;
        try { summary = editor.dispatch("scene.summary", {}); } catch { /* older catalogs */ }
        return text({ connected: true, revision: editor.revision(), summary });
      }
      case "command_list": {
        const f = typeof args.filter === "string" ? args.filter : "";
        return text({ commands: editor.commands().filter((c) => c.id.startsWith(f)) });
      }
      case "command_run":
        if (typeof args.command !== "string") return text({ error: "missing `command`" }, true);
        return await runCommand(editor, args.command, args.params);
      case "command_batch": {
        if (!Array.isArray(args.steps)) return text({ error: "missing `steps`" }, true);
        const o = editor.request({ batch: args.steps });
        return text({ result: o.result, revision: o.revision, changed: o.changed });
      }
      case "render_snapshot":
        return await snapshot(editor, args);
      case "render_storyboard":
        return pullImages(await editor.call("render.storyboard", args));
      default: {
        // A catalog command published under its tool name (`camera_frame`), as `shotstage mcp` does.
        const cmd = editor.commands().find((c) => c.id.replace(/\./g, "_") === name);
        if (cmd) return await runCommand(editor, cmd.id, args);
        return text({ error: `unknown tool \`${name}\`; tools: ${TOOLS.map((t) => t.name).join(", ")}` }, true);
      }
    }
  } catch (e) {
    return failure(e);
  }
}

// ---- The link code: a private, unguessable name for this browser's connection. ----

const CODE_KEY = "shotstage.agent.code";

/** 26 random base32 characters (130 bits). Whoever has the link can drive this editor. */
export function newCode() {
  const alphabet = "abcdefghijklmnopqrstuvwxyz234567";
  const bytes = crypto.getRandomValues(new Uint8Array(26));
  return Array.from(bytes, (b) => alphabet[b & 31]).join("");
}

/** This browser's link code (made once, then remembered); `fresh` replaces it. */
export function linkCode(store, fresh = false) {
  let code = fresh ? null : store.get(CODE_KEY);
  if (!code || !/^[a-z2-7]{20,64}$/.test(code)) {
    code = newCode();
    store.set(CODE_KEY, code);
  }
  return code;
}

/** The MCP address to give Claude for this code. */
export const mcpUrl = (relay, code) => `${relay.replace(/\/+$/, "")}/mcp/${code}`;

/**
 * Keep `editor` connected to the relay under `code` until `close()`, reconnecting as needed.
 * @param {{onStatus?: (status: "connecting"|"connected"|"disconnected"|"replaced", detail?: string) => void,
 *          onCall?: (name: string) => void}} [options]
 */
export function connectRelay(editor, relay, code, options = {}) {
  const onStatus = options.onStatus ?? (() => {});
  const url = `${relay.replace(/\/+$/, "").replace(/^http/, "ws")}/page/${code}`;
  let ws = null;
  let closed = false;
  let delay = 1000;
  let pinger = 0;
  let queue = Promise.resolve(); // one call at a time, in order

  const open = () => {
    if (closed) return;
    onStatus("connecting");
    ws = new WebSocket(url);
    const socket = ws;
    socket.onopen = () => {
      delay = 1000;
      onStatus("connected");
      clearInterval(pinger);
      // keeps proxies from closing an idle socket; the relay answers without waking up
      pinger = setInterval(() => socket.readyState === WebSocket.OPEN && socket.send("ping"), 30000);
    };
    socket.onmessage = (ev) => {
      if (ev.data === "pong") return;
      let msg;
      try { msg = JSON.parse(ev.data); } catch { return; }
      if (msg?.replaced) {
        // the same link was opened in another tab: that tab drives now
        closed = true;
        onStatus("replaced");
        return;
      }
      if (msg?.id == null || !msg.call) return;
      options.onCall?.(msg.call.name, msg.call.arguments);
      queue = queue.then(async () => {
        const result = await callTool(editor, msg.call.name, msg.call.arguments);
        if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ id: msg.id, result }));
      });
    };
    socket.onclose = () => {
      clearInterval(pinger);
      if (closed) return;
      onStatus("disconnected");
      setTimeout(open, delay);
      delay = Math.min(delay * 2, 15000);
    };
    socket.onerror = () => {}; // onclose follows
  };
  open();
  return {
    close() {
      closed = true;
      clearInterval(pinger);
      ws?.close();
    },
    get connected() {
      return ws?.readyState === WebSocket.OPEN;
    },
  };
}
