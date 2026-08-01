#!/bin/bash
# Reproduce the TLS trust-store problem: custom CA accepted by the OS cert
# store but rejected by rustls's bundled webpki-roots.
#
# NO sudo, NO cargo/Rust required — uses openssl + curl + python3 only.
#
# Demonstrates:
#   Test A (curl --cacert): ✅ tool trusts CA when told explicitly
#   Test B (curl plain):    ❌ tool rejects CA when not in trust store
#
# This mirrors what happens behind a corporate TLS-inspecting VPN:
#   VPN CA is in the OS cert store → curl/R/Python work (like Test A)
#   rustls uses webpki-roots only, ignores OS store → uvr fails (like Test B)
#
# Note: uvr ≥ 0.4.4 resolves this by reading the OS trust store (#200/#201).
# This script remains useful as a self-contained TLS-inspection reproducer.
#
# Requirements: openssl, curl, python3
# Usage: bash tls-repro.sh

set -e
WORK=$(mktemp -d)
SERVER_PID=""

# Single-quote the trap so $WORK and $SERVER_PID are evaluated at exit time,
# not at registration time (when SERVER_PID is still empty).
trap 'rm -rf "$WORK"; [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true' EXIT

echo "=== Requirements ==="
for cmd in openssl curl python3; do
  command -v "$cmd" &>/dev/null \
    && echo "  ✅ $cmd" \
    || { echo "  ❌ $cmd not found"; exit 1; }
done
echo ""

echo "=== Step 1: Generate self-signed CA (not in webpki-roots or system store) ==="
openssl genrsa -out "$WORK/ca.key" 2048 2>/dev/null
openssl req -x509 -new -key "$WORK/ca.key" -out "$WORK/ca.crt" \
  -days 1 -nodes -subj "/CN=test-corporate-CA" 2>/dev/null
echo "Custom CA: $WORK/ca.crt  (CN=test-corporate-CA)"
echo ""

echo "=== Step 2: Generate server cert signed by custom CA ==="
openssl genrsa -out "$WORK/server.key" 2048 2>/dev/null
openssl req -new -key "$WORK/server.key" -out "$WORK/server.csr" \
  -subj "/CN=127.0.0.1" 2>/dev/null
openssl x509 -req -in "$WORK/server.csr" -CA "$WORK/ca.crt" -CAkey "$WORK/ca.key" \
  -CAcreateserial -out "$WORK/server.crt" -days 1 \
  -extfile <(echo "subjectAltName=IP:127.0.0.1") 2>/dev/null
echo "Server cert: signed by test-corporate-CA"
echo ""

echo "=== Step 3: Start local HTTPS server (no sudo) ==="
PORT=$(python3 -c "
import socket
s = socket.socket()
s.bind(('', 0))
p = s.getsockname()[1]
s.close()
print(p)
")

# Write the server script to a temp file so there is exactly one python3
# invocation whose PID we capture.
cat > "$WORK/server.py" << 'PYEOF'
import http.server, ssl, sys

work, port = sys.argv[1], int(sys.argv[2])

class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"Package: BiocStyle\nVersion: 2.38.0\n")
    def log_message(self, *a): pass

server = http.server.HTTPServer(("127.0.0.1", port), H)
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(f"{work}/server.crt", f"{work}/server.key")
server.socket = ctx.wrap_socket(server.socket, server_side=True)
server.serve_forever()
PYEOF

python3 "$WORK/server.py" "$WORK" "$PORT" &
SERVER_PID=$!
sleep 1
echo "HTTPS server on 127.0.0.1:$PORT  (cert signed by test-corporate-CA)"
echo ""

echo "=== Test A: curl WITH --cacert (explicit CA trust — simulates OS cert store) ==="
curl -sf --cacert "$WORK/ca.crt" "https://127.0.0.1:$PORT/" \
  && echo "result: ✅ SUCCESS — tool trusts custom CA" \
  || echo "result: ❌ FAIL"
echo ""

echo "=== Test B: curl WITHOUT --cacert (no custom CA — simulates rustls webpki-roots) ==="
curl -sf "https://127.0.0.1:$PORT/" \
  && echo "result: ✅ SUCCESS" \
  || echo "result: ❌ FAIL — cert rejected (CA not trusted)"
echo ""

echo "========================================"
echo "INTERPRETATION:"
echo "  Test A (curl --cacert): ✅ = CA trusted when provided explicitly"
echo "  Test B (curl plain):    ❌ = CA rejected when not in trust store"
echo ""
echo "Corporate TLS inspection:"
echo "  VPN CA is in OS cert store → curl/R/Python work (Test A)"
echo "  rustls uses webpki-roots only, ignores OS store → fails (Test B)"
echo ""
echo "Resolution: uvr ≥ 0.4.4 validates TLS against the OS trust store"
echo "in addition to webpki-roots, so corporate CAs are accepted."
echo "========================================"
