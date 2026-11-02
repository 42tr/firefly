"""Local deterministic MCP fixture; supports stdio and Streamable HTTP JSON responses."""
import json
import os
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

calls = 0
initializations = 0
lock = threading.Lock()

def reply(message, auth=""):
    global calls, initializations
    method = message.get("method", "")
    if "id" not in message:
        return None
    params = message.get("params", {})
    if method == "initialize":
        with lock:
            initializations += 1
        time.sleep(float(os.environ.get("INIT_DELAY", "0")))
        result = {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}},
                  "serverInfo": {"name": "g-fixture", "version": "1"}}
    elif method == "tools/list":
        name = "echo" if not params.get("cursor") else "fail"
        result = {"tools": [{"name": name, "description": "fixture " + name,
            "inputSchema": {"type": "object", "properties": {"value": {"type": "string"}, "delay": {"type": "number"}}, "required": ["value"], "additionalProperties": False}}]}
        if name == "echo":
            result["nextCursor"] = "second"
    elif method == "tools/call":
        with lock:
            calls += 1
            count = calls
        args = params.get("arguments", {})
        time.sleep(args.get("delay", 0))
        data = {"value": args.get("value"), "auth": auth, "tag": os.environ.get("TAG", ""), "calls": count, "initializations": initializations}
        result = {"content": [{"type": "text", "text": json.dumps(data)}], "structuredContent": data, "isError": params["name"] == "fail"}
    elif method == "ping":
        result = {}
    else:
        return {"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32601, "message": "unsupported"}}
    return {"jsonrpc": "2.0", "id": message["id"], "result": result}

if len(sys.argv) > 1 and sys.argv[1] == "http":
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass
        def do_GET(self):
            if self.path == "/stats":
                data = json.dumps({"calls": calls, "initializations": initializations}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
            else:
                self.send_response(405)
                self.end_headers()
        def do_DELETE(self):
            self.send_response(200)
            self.end_headers()
        def do_POST(self):
            body = self.rfile.read(int(self.headers.get("content-length", "0")))
            message = json.loads(body)
            result = reply(message, self.headers.get("Authorization", ""))
            data = json.dumps(result).encode() if result is not None else b""
            expired = message.get("method") == "tools/call" and message.get("params", {}).get("arguments", {}).get("value") == "expire"
            self.send_response(404 if expired else (200 if result is not None else 202))
            if self.path == "/session" and message.get("method") == "initialize":
                self.send_header("Mcp-Session-Id", "fixture-session")
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            try:
                self.wfile.write(data)
            except BrokenPipeError:
                pass
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    print(server.server_port, flush=True)
    server.serve_forever()
else:
    for line in sys.stdin:
        result = reply(json.loads(line))
        if result is not None:
            print(json.dumps(result), flush=True)
