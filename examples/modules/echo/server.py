"""Minimal standalone stdio template, Python 3 standard library only."""
import json
import sys

for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method, params = request.get("method"), request.get("params", {})
    if method == "initialize":
        result = {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}}, "serverInfo": {"name": "echo-example", "version": "0.1.0"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "echo", "description": "返回输入文本", "annotations": {"readOnlyHint": True}, "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"], "additionalProperties": False}}]}
    elif method == "tools/call" and params.get("name") == "echo":
        result = {"content": [{"type": "text", "text": params.get("arguments", {}).get("text", "")}]}
    elif method == "ping":
        result = {}
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32601, "message": "Unknown method"}}), flush=True)
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
