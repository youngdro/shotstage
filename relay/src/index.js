// shotstage relay: lets Claude (or any MCP client) drive the online editor a person has open,
// with nothing installed on their computer.
//
//   browser editor  ──wss──▶  /page/<code>  ┐
//                                            ├─ one Durable Object per link code
//   Claude (MCP)    ──https─▶ /mcp/<code>   ┘
//
// The page makes a private link code (130 random bits) and connects to /page/<code>. The
// person gives Claude https://<relay>/mcp/<code> as a connector (Streamable HTTP, no sign-in:
// the code is the secret). The relay answers `initialize` and `tools/list` itself and hands
// each `tools/call` to the page, which runs it in the editor and sends back the result.
// Nothing is stored: the relay only passes messages while both sides are connected.

import { INSTRUCTIONS, SERVER_NAME, TOOLS } from "../../web/agent-tools.js";

const CODE = /^[a-z2-7]{20,64}$/;
const PROTOCOLS = ["2025-06-18", "2025-03-26", "2024-11-05"];
const CALL_TIMEOUT_MS = 180_000;
const MAX_BODY = 1 << 20;
const CORS = {
  "Access-Control-Allow-Origin": "*",
  "Access-Control-Allow-Methods": "GET, POST, DELETE, OPTIONS",
  "Access-Control-Allow-Headers": "Content-Type, Accept, Authorization, Mcp-Session-Id, Mcp-Protocol-Version, Last-Event-ID",
  "Access-Control-Expose-Headers": "Mcp-Session-Id",
};

const json = (body, status = 200, headers = {}) =>
  new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json", ...CORS, ...headers } });

/** Origins allowed to connect a page: the configured list, plus any localhost for development. */
function originAllowed(origin, env) {
  if (!origin) return false;
  const allowed = (env.ALLOWED_ORIGINS ?? "").split(",").map((s) => s.trim()).filter(Boolean);
  if (allowed.includes("*") || allowed.includes(origin)) return true;
  try {
    const u = new URL(origin);
    return ["localhost", "127.0.0.1", "[::1]"].includes(u.hostname);
  } catch {
    return false;
  }
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const [, kind, code, extra] = url.pathname.split("/");
    if (request.method === "OPTIONS") return new Response(null, { status: 204, headers: CORS });
    if (!kind) {
      return json({
        name: "shotstage relay",
        about: "Connects Claude to the shotstage editor open in a browser. Open the editor, click the Claude button and copy the address it shows.",
        editor: env.PAGE_URL,
      });
    }
    if ((kind !== "mcp" && kind !== "page") || !CODE.test(code ?? "") || extra) {
      return json({ error: "not found" }, 404);
    }
    if (kind === "page") {
      if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") return json({ error: "expected a WebSocket" }, 426);
      if (!originAllowed(request.headers.get("Origin"), env)) return json({ error: "origin not allowed" }, 403);
    }
    const stub = env.LINKS.get(env.LINKS.idFromName(code));
    return stub.fetch(request);
  },
};

/** One link: at most one connected page, any number of MCP requests. */
export class Link {
  constructor(ctx, env) {
    this.ctx = ctx;
    this.env = env;
    this.pending = new Map(); // call id -> {resolve, timer}
    this.next = 1;
    // the page's keep-alive, answered without waking the object
    ctx.setWebSocketAutoResponse(new WebSocketRequestResponsePair("ping", "pong"));
  }

  page() {
    return this.ctx.getWebSockets().find((ws) => ws.readyState === WebSocket.OPEN) ?? null;
  }

  async fetch(request) {
    const kind = new URL(request.url).pathname.split("/")[1];
    if (kind === "page") return this.connectPage();
    return this.mcp(request);
  }

  connectPage() {
    // a link drives one editor: the newest tab takes over
    for (const old of this.ctx.getWebSockets()) {
      try {
        old.send(JSON.stringify({ replaced: true }));
        old.close(4000, "opened in another tab");
      } catch {
        // already gone
      }
    }
    const { 0: client, 1: server } = new WebSocketPair();
    this.ctx.acceptWebSocket(server);
    return new Response(null, { status: 101, webSocket: client });
  }

  webSocketMessage(ws, message) {
    if (typeof message !== "string") return;
    let msg;
    try {
      msg = JSON.parse(message);
    } catch {
      return;
    }
    const p = this.pending.get(msg?.id);
    if (!p) return;
    this.pending.delete(msg.id);
    clearTimeout(p.timer);
    p.resolve(msg.result ?? notice("The editor sent back an empty answer.", true));
  }

  webSocketClose() {
    if (this.page()) return;
    for (const [id, p] of this.pending) {
      clearTimeout(p.timer);
      p.resolve(notice("The editor was closed or lost its connection while running this call; the change may or may not have been made. Check with editor_status.", true));
      this.pending.delete(id);
    }
  }

  webSocketError(ws) {
    this.webSocketClose(ws);
  }

  /** Hand one tool call to the page; resolves to its CallToolResult. */
  callPage(page, name, args) {
    const id = this.next++;
    return new Promise((resolve) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        resolve(notice(`The editor did not answer within ${CALL_TIMEOUT_MS / 1000} s.`, true));
      }, CALL_TIMEOUT_MS);
      this.pending.set(id, { resolve, timer });
      try {
        page.send(JSON.stringify({ id, call: { name, arguments: args ?? {} } }));
      } catch (e) {
        clearTimeout(timer);
        this.pending.delete(id);
        resolve(notice(`Could not reach the editor: ${e?.message ?? e}`, true));
      }
    });
  }

  offline() {
    return {
      connected: false,
      open: this.env.PAGE_URL,
      howToConnect: `No shotstage editor is connected to this link. Ask the person to open ${this.env.PAGE_URL} in their browser and click "Claude" in the top bar (connect it there); the address it shows must be this connector's address. Then try again.`,
    };
  }

  async tool(name, args) {
    if (typeof name !== "string" || !name) return notice("missing tool name", true);
    const page = this.page();
    if (!page) {
      const off = this.offline();
      return name === "editor_status" ? notice(off) : notice(off.howToConnect, true);
    }
    return this.callPage(page, name, args);
  }

  async rpc(msg) {
    const id = msg?.id;
    const method = typeof msg?.method === "string" ? msg.method : "";
    const params = msg?.params ?? {};
    const reply = (result) => ({ jsonrpc: "2.0", id, result });
    if (id === undefined || id === null) return null; // notification
    switch (method) {
      case "initialize": {
        const asked = params.protocolVersion;
        return reply({
          protocolVersion: PROTOCOLS.includes(asked) ? asked : PROTOCOLS[0],
          capabilities: { tools: { listChanged: false } },
          serverInfo: { name: SERVER_NAME, title: "shotstage (online editor)", version: "1.0.0" },
          instructions: INSTRUCTIONS,
        });
      }
      case "ping":
        return reply({});
      case "tools/list":
        return reply({ tools: TOOLS });
      case "tools/call":
        return reply(await this.tool(params.name, params.arguments));
      default:
        return { jsonrpc: "2.0", id, error: { code: -32601, message: `method not found: ${method}` } };
    }
  }

  async mcp(request) {
    if (request.method === "GET") {
      // no server-initiated messages: tell the client there is no stream to open
      return new Response(null, { status: 405, headers: { Allow: "POST", ...CORS } });
    }
    if (request.method === "DELETE") return new Response(null, { status: 204, headers: CORS });
    if (request.method !== "POST") return json({ error: "use POST" }, 405);
    const body = await request.text();
    if (body.length > MAX_BODY) return json({ jsonrpc: "2.0", id: null, error: { code: -32600, message: "request too large" } }, 413);
    let msg;
    try {
      msg = JSON.parse(body);
    } catch {
      return json({ jsonrpc: "2.0", id: null, error: { code: -32700, message: "parse error" } }, 400);
    }
    if (Array.isArray(msg)) {
      const replies = (await Promise.all(msg.map((m) => this.rpc(m)))).filter(Boolean);
      return replies.length ? json(replies) : new Response(null, { status: 202, headers: CORS });
    }
    const r = await this.rpc(msg);
    return r ? json(r) : new Response(null, { status: 202, headers: CORS });
  }
}

function notice(v, isError = false) {
  return {
    content: [{ type: "text", text: typeof v === "string" ? v : JSON.stringify(v, null, 2) }],
    ...(typeof v === "object" ? { structuredContent: v } : {}),
    isError,
  };
}
