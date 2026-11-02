# records every request body to /out/req-N.json and answers 400, so Claude Code stops after its first call
import http.server, json, os, itertools
n = itertools.count()
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        b = self.rfile.read(int(self.headers.get('content-length', 0)))
        open(f'/out/req-{next(n)}.json', 'wb').write(b)
        body = json.dumps({"type": "error", "error": {"type": "invalid_request_error", "message": "gate7 stop"}}).encode()
        self.send_response(400); self.send_header('content-type', 'application/json'); self.send_header('content-length', str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_GET(self):
        self.send_response(404); self.end_headers()
    def log_message(self, *a): pass
http.server.HTTPServer(('127.0.0.1', 8099), H).serve_forever()
