# The model stand-in (decision 17's resume check): records each request body to OUT/req-N.json. A streamed request
# that offers AskUserQuestion and has no tool_result yet is answered with an AskUserQuestion tool_use; every other
# request with the text "done", so Claude Code ends the turn.
import http.server, json, itertools, os, sys
OUT = sys.argv[1]; PORT = int(sys.argv[2])
n = itertools.count()
def sse(events):
    return b"".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n".encode() for e in events)
def message(blocks, stop):
    ev = [{"type": "message_start", "message": {"id": "msg_ask", "type": "message", "role": "assistant",
           "model": "claude-sonnet-4-5", "content": [], "stop_reason": None, "stop_sequence": None,
           "usage": {"input_tokens": 1, "output_tokens": 1}}}]
    for i, b in enumerate(blocks):
        if b["type"] == "tool_use":
            ev.append({"type": "content_block_start", "index": i,
                       "content_block": {"type": "tool_use", "id": b["id"], "name": b["name"], "input": {}}})
            ev.append({"type": "content_block_delta", "index": i,
                       "delta": {"type": "input_json_delta", "partial_json": json.dumps(b["input"])}})
        else:
            ev.append({"type": "content_block_start", "index": i, "content_block": {"type": "text", "text": ""}})
            ev.append({"type": "content_block_delta", "index": i, "delta": {"type": "text_delta", "text": b["text"]}})
        ev.append({"type": "content_block_stop", "index": i})
    ev.append({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": None}, "usage": {"output_tokens": 5}})
    ev.append({"type": "message_stop"})
    return sse(ev)
ASK = {"questions": [{"question": "Which colour?", "header": "Colour", "multiSelect": False,
        "options": [{"label": "Red", "description": "red"}, {"label": "Blue", "description": "blue"}]}]}
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        b = self.rfile.read(int(self.headers.get('content-length', 0)))
        i = next(n); open(f'{OUT}/req-{i}.json', 'wb').write(b)
        try: req = json.loads(b)
        except ValueError: req = {}
        tools = [t.get("name") for t in req.get("tools", []) if isinstance(t, dict)]
        answered = any(isinstance(c, dict) and c.get("type") == "tool_result" for m in req.get("messages", []) if isinstance(m.get("content"), list) for c in m["content"])
        if req.get("stream") and "AskUserQuestion" in tools and not answered:
            body = message([{"type": "tool_use", "id": "toolu_ask17", "name": "AskUserQuestion", "input": ASK}], "tool_use")
            ctype = "text/event-stream"
        elif req.get("stream"):
            body, ctype = message([{"type": "text", "text": "done"}], "end_turn"), "text/event-stream"
        else:
            body = json.dumps({"id": "msg_ask", "type": "message", "role": "assistant", "model": "claude-sonnet-4-5",
                "content": [{"type": "text", "text": "done"}], "stop_reason": "end_turn", "stop_sequence": None,
                "usage": {"input_tokens": 1, "output_tokens": 1}}).encode()
            ctype = "application/json"
        self.send_response(200); self.send_header('content-type', ctype)
        self.send_header('content-length', str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_GET(self): self.send_response(404); self.end_headers()
    def log_message(self, *a): pass
http.server.HTTPServer(('127.0.0.1', PORT), H).serve_forever()
