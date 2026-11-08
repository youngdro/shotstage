#!/usr/bin/env python3
"""Play a list of MCP tool calls against `shotstage mcp`, the way Claude would.

    python3 examples/run-mcp-example.py examples/fresh-produce-ad.json --scene /tmp/ad.json
    python3 examples/run-mcp-example.py examples/fresh-produce-ad.json --live   # in the browser

Each step is {"tool", "arguments", "note"?, "save"?: "file.png"}. Pictures the server returns
are written next to the scene file. Stdlib only.
"""
import argparse, base64, json, os, subprocess, sys

ap = argparse.ArgumentParser()
ap.add_argument("steps")
ap.add_argument("--scene", default="example.shotstage.json")
ap.add_argument("--bin", default=os.environ.get("SHOTSTAGE", "target/debug/shotstage"))
ap.add_argument("--live", action="store_true", help="drive the browser editor (needs dist/ from scripts/build-web.sh)")
args = ap.parse_args()

cmd = [args.bin, "mcp", "--scene", args.scene]
if args.live:
    cmd += ["--live", "--web", "dist"]
server = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
next_id = 0

def rpc(method, params=None):
    global next_id
    next_id += 1
    server.stdin.write(json.dumps({"jsonrpc": "2.0", "id": next_id, "method": method, "params": params or {}}) + "\n")
    server.stdin.flush()
    return json.loads(server.stdout.readline())

rpc("initialize", {"protocolVersion": "2025-06-18", "clientInfo": {"name": "example", "version": "1"}})
if args.live:
    input("Open http://127.0.0.1:7777/ in a browser, then press Enter… ")
out_dir = os.path.dirname(os.path.abspath(args.scene))
failed = 0
for i, step in enumerate(json.load(open(args.steps))):
    if step.get("note"):
        print(f"\n# {step['note']}")
    r = rpc("tools/call", {"name": step["tool"], "arguments": step.get("arguments", {})})["result"]
    texts = [c["text"] for c in r["content"] if c["type"] == "text"]
    images = [c for c in r["content"] if c["type"] == "image"]
    mark = "✗" if r.get("isError") else "✓"
    failed += bool(r.get("isError"))
    summary = texts[0].replace("\n", " ")[:150] if texts else ""
    print(f"{mark} {step['tool']}  {summary}")
    for k, img in enumerate(images):
        name = step.get("save") if len(images) == 1 and step.get("save") else f"step{i:02d}_{k}.png"
        with open(os.path.join(out_dir, name), "wb") as f:
            f.write(base64.b64decode(img["data"]))
        print(f"  picture → {os.path.join(out_dir, name)}")
server.stdin.close()
server.wait()
print(f"\n{'all steps ok' if not failed else f'{failed} step(s) failed'}; scene saved to {args.scene}")
sys.exit(1 if failed else 0)
