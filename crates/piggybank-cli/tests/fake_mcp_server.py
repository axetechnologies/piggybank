#!/usr/bin/env python3
"""Minimal fake MCP server for proxy integration tests.

Speaks newline-delimited JSON-RPC 2.0 over stdio.
Responds to initialize, notifications/initialized, tools/list, tools/call.

Usage:
  python3 fake_mcp_server.py [--content-length]

With --content-length, switches to HTTP-style Content-Length framing.
"""
import sys
import json

USE_CONTENT_LENGTH = "--content-length" in sys.argv

TOOLS = [
    {
        "name": "echo",
        "description": "Echo the input back. Returns whatever text you send.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "text": {"type": "string", "description": "Text to echo back."}
            },
            "required": ["text"],
        },
    },
    {
        "name": "big_response",
        "description": "Return a large text payload for compression testing.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "size": {"type": "integer", "description": "Approximate response size in bytes."}
            },
            "required": ["size"],
        },
    },
    {
        "name": "error_tool",
        "description": "Always returns an isError response.",
        "inputSchema": {"type": "object", "properties": {}},
    },
]


def send(obj):
    text = json.dumps(obj)
    if USE_CONTENT_LENGTH:
        header = f"Content-Length: {len(text)}\r\n\r\n"
        sys.stdout.buffer.write(header.encode() + text.encode())
    else:
        sys.stdout.write(text + "\n")
    sys.stdout.flush()


def recv():
    if USE_CONTENT_LENGTH:
        content_length = None
        while True:
            line = sys.stdin.buffer.readline().decode("utf-8", errors="replace")
            if not line:
                return None
            stripped = line.strip()
            if not stripped:
                break
            lower = stripped.lower()
            if lower.startswith("content-length:"):
                content_length = int(lower.split(":", 1)[1].strip())
        if content_length is None:
            return None
        body = sys.stdin.buffer.read(content_length)
        return json.loads(body)
    else:
        line = sys.stdin.readline()
        if not line:
            return None
        return json.loads(line.strip())


def handle(msg):
    method = msg.get("method", "")
    msg_id = msg.get("id")

    if method == "initialize":
        send({
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fake-mcp", "version": "0.0.1"},
            },
        })

    elif method == "notifications/initialized":
        pass  # no response for notifications

    elif method == "tools/list":
        send({
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": {"tools": TOOLS},
        })

    elif method == "tools/call":
        params = msg.get("params", {})
        name = params.get("name", "")
        args = params.get("arguments", {})

        if name == "echo":
            text = args.get("text", "")
            send({
                "jsonrpc": "2.0",
                "id": msg_id,
                "result": {"content": [{"type": "text", "text": text}]},
            })
        elif name == "big_response":
            size = int(args.get("size", 8192))
            # Repeated lines to make compression effective
            line = "The quick brown fox jumps over the lazy dog. " * 4 + "\n"
            payload = (line * (size // len(line) + 1))[:size]
            send({
                "jsonrpc": "2.0",
                "id": msg_id,
                "result": {"content": [{"type": "text", "text": payload}]},
            })
        elif name == "error_tool":
            send({
                "jsonrpc": "2.0",
                "id": msg_id,
                "result": {
                    "content": [{"type": "text", "text": "deliberate error"}],
                    "isError": True,
                },
            })
        else:
            send({
                "jsonrpc": "2.0",
                "id": msg_id,
                "error": {"code": -32601, "message": f"unknown tool: {name}"},
            })
    else:
        if msg_id is not None:
            send({
                "jsonrpc": "2.0",
                "id": msg_id,
                "error": {"code": -32601, "message": f"unknown method: {method}"},
            })


def main():
    while True:
        msg = recv()
        if msg is None:
            break
        handle(msg)


if __name__ == "__main__":
    main()
