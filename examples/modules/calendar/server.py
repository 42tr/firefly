"""Self-contained MCP stdio example. Python 3.9+, no third-party dependencies.

The intentionally small protocol implementation handles this example's tools and
HTML resource. For larger modules use an official MCP SDK. stdout is protocol only.
"""
import datetime
import json
import os
from pathlib import Path
import sqlite3
import sys
from zoneinfo import ZoneInfo

ROOT = Path(__file__).resolve().parent
DATA = Path(os.environ["FIREFLY_MODULE_DATA_DIR"])
CONFIG = json.loads(os.environ.get("FIREFLY_MODULE_CONFIG", "{}"))
UI = "ui://personal-calendar/calendar"


def schema(properties=None, required=None):
    return {"type": "object", "properties": properties or {}, "required": required or [], "additionalProperties": False}


TOOLS = [
    {"name": "list_dates", "description": "列出用户保存的公历重要日期", "inputSchema": schema(), "annotations": {"readOnlyHint": True}, "_meta": {"ui": {"resourceUri": UI}}},
    {"name": "save_date", "description": "按稳定 ID 新增或更新一个公历重要日期", "inputSchema": schema({"id": {"type": "string", "minLength": 1, "maxLength": 100}, "name": {"type": "string", "minLength": 1, "maxLength": 200}, "date": {"type": "string", "pattern": "^[0-9]{2}-[0-9]{2}$"}}, ["id", "name", "date"]), "annotations": {"idempotentHint": True}},
    {"name": "delete_date", "description": "删除指定 ID 的重要日期", "inputSchema": schema({"id": {"type": "string"}}, ["id"]), "annotations": {"idempotentHint": True, "destructiveHint": True}},
    {"name": "check_reminders", "description": "检查今天的公历重要日期；结果保存在宿主执行记录中", "inputSchema": schema(), "annotations": {"readOnlyHint": True}},
]


def call(name, arguments):
    DATA.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(DATA / "calendar.sqlite3") as database:
        database.execute("CREATE TABLE IF NOT EXISTS dates (id TEXT PRIMARY KEY, name TEXT NOT NULL, date TEXT NOT NULL)")
        if name == "save_date":
            datetime.datetime.strptime("2000-" + arguments["date"], "%Y-%m-%d")
            database.execute("INSERT INTO dates VALUES (?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,date=excluded.date", (arguments["id"], arguments["name"], arguments["date"]))
        elif name == "delete_date":
            database.execute("DELETE FROM dates WHERE id=?", (arguments["id"],))
        elif name not in {"list_dates", "check_reminders"}:
            raise ValueError("Unknown tool")
        dates = [dict(zip(("id", "name", "date"), row)) for row in database.execute("SELECT id,name,date FROM dates ORDER BY date,name")]
    today = datetime.datetime.now(ZoneInfo(CONFIG.get("timezone", "Asia/Shanghai"))).strftime("%m-%d")
    value = {"dates": dates, "today": today}
    if name == "check_reminders":
        value["dates"] = [date for date in dates if date["date"] == today]
    return {"content": [{"type": "text", "text": json.dumps(value, ensure_ascii=False)}], "structuredContent": value}


def handle(message):
    method, params = message.get("method"), message.get("params") or {}
    if method == "initialize":
        return {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}, "resources": {}}, "serverInfo": {"name": "personal-calendar", "version": "0.1.0"}}
    if method == "ping":
        return {}
    if method == "tools/list":
        return {"tools": TOOLS}
    if method == "tools/call":
        try:
            return call(params["name"], params.get("arguments", {}))
        except (KeyError, ValueError, sqlite3.Error) as error:
            return {"isError": True, "content": [{"type": "text", "text": str(error)}]}
    if method == "resources/list":
        return {"resources": [{"uri": UI, "name": "Calendar", "mimeType": "text/html;profile=mcp-app"}]}
    if method == "resources/read" and params.get("uri") == UI:
        return {"contents": [{"uri": UI, "mimeType": "text/html;profile=mcp-app", "text": (ROOT / "calendar.html").read_text()}]}
    raise ValueError("Unsupported method")


def serve():
    for line in sys.stdin:
        try:
            message = json.loads(line)
            if "id" not in message:
                continue
            try:
                response = {"jsonrpc": "2.0", "id": message["id"], "result": handle(message)}
            except Exception as error:
                print(str(error), file=sys.stderr, flush=True)
                response = {"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32603, "message": "Module request failed"}}
            print(json.dumps(response, ensure_ascii=False), flush=True)
        except json.JSONDecodeError:
            print("Invalid JSON request", file=sys.stderr, flush=True)


if __name__ == "__main__":
    serve()
