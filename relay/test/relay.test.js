// End-to-end: the relay under `wrangler dev` (local workerd), a fake editor page on a
// WebSocket, and MCP requests over HTTP, the way Claude sends them.
import { spawn } from "node:child_process";
import { once } from "node:events";
import { after, before, test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

import { callTool, newCode } from "../../web/agent.js";

const PORT = 8790 + Math.floor(Math.random() * 100);
const BASE = `http://127.0.0.1:${PORT}`;
const dir = fileURLToPath(new URL("..", import.meta.url));
let dev;

before(async () => {
  dev = spawn("npx", ["wrangler", "dev", "--port", String(PORT), "--ip", "127.0.0.1"], { cwd: dir, stdio: ["ignore", "pipe", "pipe"], detached: true });
  let log = "";
  dev.stdout.on("data", (d) => (log += d));
  dev.stderr.on("data", (d) => (log += d));
  for (let i = 0; i < 120; i++) {
    try {
      if ((await fetch(BASE)).ok) return;
    } catch {}
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`wrangler dev did not start:\n${log}`);
});

after(() => {
  // wrangler starts workerd under it: stop the whole group
  try { process.kill(-dev.pid, "SIGTERM"); } catch {}
});

let rpcId = 0;
async function rpc(code, method, params = {}) {
  const r = await fetch(`${BASE}/mcp/${code}`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json, text/event-stream" },
    body: JSON.stringify({ jsonrpc: "2.0", id: ++rpcId, method, params }),
  });
  assert.equal(r.status, 200);
  const body = await r.json();
  assert.equal(body.id, rpcId);
  return body.result ?? body;
}

/** A fake browser editor: answers each call with `answer(call)`. */
async function page(code, answer, origin = "http://localhost:8080") {
  const ws = new WebSocket(`ws://127.0.0.1:${PORT}/page/${code}`, { headers: { Origin: origin } });
  const seen = [];
  ws.addEventListener("message", async (ev) => {
    if (ev.data === "pong") return;
    const msg = JSON.parse(ev.data);
    seen.push(msg);
    if (msg.call) ws.send(JSON.stringify({ id: msg.id, result: await answer(msg.call) }));
  });
  await once(ws, "open");
  return { ws, seen };
}

test("initialize and tools/list work before any editor connects", async () => {
  const code = newCode();
  const init = await rpc(code, "initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "t", version: "1" } });
  assert.equal(init.serverInfo.name, "shotstage");
  assert.equal(init.protocolVersion, "2025-06-18");
  assert.match(init.instructions, /command_run/);
  const n = await fetch(`${BASE}/mcp/${code}`, { method: "POST", body: JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) });
  assert.equal(n.status, 202);
  const { tools } = await rpc(code, "tools/list");
  for (const name of ["editor_status", "command_list", "command_run", "command_batch", "render_snapshot", "render_storyboard"]) {
    assert.ok(tools.some((t) => t.name === name), name);
  }
  const status = await rpc(code, "tools/call", { name: "editor_status", arguments: {} });
  assert.equal(status.isError, false);
  assert.equal(status.structuredContent.connected, false);
  const run = await rpc(code, "tools/call", { name: "command_run", arguments: { command: "scene.summary" } });
  assert.equal(run.isError, true);
  assert.match(run.content[0].text, /No shotstage editor is connected/);
});

test("tool calls run in the connected page", async () => {
  const code = newCode();
  const p = await page(code, (call) => ({ content: [{ type: "text", text: `ran ${call.name} ${JSON.stringify(call.arguments)}` }], isError: false }));
  const r = await rpc(code, "tools/call", { name: "command_run", arguments: { command: "scene.summary", params: {} } });
  assert.equal(r.isError, false);
  assert.equal(r.content[0].text, 'ran command_run {"command":"scene.summary","params":{}}');
  // the keep-alive is answered by the relay
  p.ws.send("ping");
  await new Promise((res) => p.ws.addEventListener("message", function f(ev) { if (ev.data === "pong") { p.ws.removeEventListener("message", f); res(); } }));
  p.ws.close();
});

test("a second tab takes over the link", async () => {
  const code = newCode();
  const first = await page(code, () => ({ content: [{ type: "text", text: "first" }], isError: false }));
  const told = new Promise((res) => first.ws.addEventListener("message", (ev) => ev.data.includes("replaced") && res()));
  const second = await page(code, () => ({ content: [{ type: "text", text: "second" }], isError: false }));
  await told;
  const r = await rpc(code, "tools/call", { name: "editor_status", arguments: {} });
  assert.equal(r.content[0].text, "second");
  second.ws.close();
  first.ws.close();
});

test("a page that disconnects mid-call gives an error, not a hang", async () => {
  const code = newCode();
  const p = await page(code, () => new Promise(() => {}));
  const pending = rpc(code, "tools/call", { name: "render_snapshot", arguments: {} });
  await new Promise((r) => setTimeout(r, 300));
  p.ws.close();
  const r = await pending;
  assert.equal(r.isError, true);
  assert.match(r.content[0].text, /lost its connection/);
});

test("other sites cannot connect a page; bad codes are not found", async () => {
  const code = newCode();
  const evil = new WebSocket(`ws://127.0.0.1:${PORT}/page/${code}`, { headers: { Origin: "https://evil.example" } });
  const [ev] = await Promise.race([once(evil, "error"), once(evil, "open")]);
  assert.equal(ev.type, "error");
  assert.equal((await fetch(`${BASE}/mcp/short`, { method: "POST", body: "{}" })).status, 404);
  assert.equal((await fetch(`${BASE}/mcp/${code}`)).status, 405);
  const bad = await fetch(`${BASE}/mcp/${code}`, { method: "POST", body: "not json" });
  assert.equal((await bad.json()).error.code, -32700);
});

// ---- The page side (web/agent.js) against a fake editor. ----

function fakeEditor() {
  let revision = 0;
  const log = [];
  return {
    log,
    revision: () => revision,
    commands: () => [{ id: "scene.summary" }, { id: "camera.frame" }, { id: "render.snapshot" }],
    dispatch: (c) => (c === "scene.summary" ? { entities: 3 } : null),
    request(req) {
      log.push(req);
      if (req.command === "bad.command") throw Object.assign(new Error("no such command"), { code: "unknown_command" });
      revision++;
      return { result: { ok: true }, revision, changed: true };
    },
    async call(command, params) {
      if (command === "render.snapshot") return { mimeType: "image/png", data: "AAAA", dataUrl: "data:image/png;base64,AAAA", width: 4, height: 2 };
      if (command === "render.storyboard") {
        const img = (p) => ({ images: { clay: "data:image/png;base64,QkJC", [p]: "data:image/png;base64,Q0ND" } });
        return { shots: [{ frames: [img("depth"), img("depth")] }], html: "<html>…</html>" };
      }
      if (command === "render.shotSheet") {
        log.push({ command, params });
        return { fileName: "shot-sheet.pdf", mimeType: "application/pdf", pages: 1, data: "JVBERg==", preview: "UFJF", shots: [{ number: 1, movement: "static", text: { movement: "固定镜头" } }] };
      }
      throw new Error("unexpected");
    },
  };
}

test("page: commands, batches, snapshots and storyboards become MCP results", async () => {
  const ed = fakeEditor();
  const run = await callTool(ed, "command_run", { command: "camera.frame", params: { size: "medium" } });
  assert.equal(run.isError, false);
  assert.deepEqual(ed.log[0], { command: "camera.frame", params: { size: "medium" } });
  assert.equal(run.structuredContent.revision, 1);

  const err = await callTool(ed, "command_run", { command: "bad.command" });
  assert.equal(err.isError, true);
  assert.equal(err.structuredContent.error.code, "unknown_command");

  const list = await callTool(ed, "command_list", { filter: "camera." });
  assert.deepEqual(list.structuredContent.commands, [{ id: "camera.frame" }]);

  const batch = await callTool(ed, "command_batch", { steps: [{ command: "scene.summary" }] });
  assert.equal(batch.isError, false);
  assert.deepEqual(ed.log[2], { batch: [{ command: "scene.summary" }] });

  // catalog tool names work too, as with `shotstage mcp`
  await callTool(ed, "camera_frame", { size: "wide" });
  assert.deepEqual(ed.log[3], { command: "camera.frame", params: { size: "wide" } });

  const snap = await callTool(ed, "render_snapshot", {});
  assert.deepEqual(snap.content[0], { type: "image", data: "AAAA", mimeType: "image/png" });
  assert.ok(!snap.content[1].text.includes("dataUrl"));

  const board = await callTool(ed, "render_storyboard", {});
  const images = board.content.filter((c) => c.type === "image");
  assert.equal(images.length, 2); // the clay pictures only
  assert.ok(images.every((i) => i.data === "QkJC"));
  assert.ok(!board.content[0].text.includes("html"));
  assert.ok(!board.content[0].text.includes("base64"));

  // the shot sheet: page 1 comes back as a picture, the file itself stays out of the reply
  const sheet = await callTool(ed, "render_shotSheet", { lang: "zh", out: "/tmp/x.pdf" });
  assert.equal(sheet.isError, false);
  assert.deepEqual(ed.log.at(-1), { command: "render.shotSheet", params: { lang: "zh", preview: true } });
  assert.deepEqual(sheet.content[1], { type: "image", data: "UFJF", mimeType: "image/png" });
  assert.ok(!sheet.content[0].text.includes("JVBERg") && !sheet.content[0].text.includes("固定镜头"));
  assert.ok(JSON.parse(sheet.content[0].text).shots[0].movement === "static");

  const status = await callTool(ed, "editor_status", {});
  assert.deepEqual(status.structuredContent.summary, { entities: 3 });
});
