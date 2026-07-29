#!/bin/bash
# Reproduce uvr TLS issue: custom CA trusted by system but not by rustls webpki-roots
# NO sudo, NO cargo/Rust required — uses openssl + curl only
#
# Demonstrates the core problem:
#   curl with explicit --cacert: trusts custom CA → works
#   openssl with system store:   trusts custom CA → works (like native-tls)
#   openssl without custom CA:   rejects cert → fails (like rustls webpki-roots)
#
# This is exactly what happens in corporate environments with TLS inspection:
#   System cert store has corporate CA → curl/R/Python work
#   rustls webpki-roots missing corporate CA → uvr fails
#
# Requirements: openssl, curl, python3
# Usage: bash repro-norust.sh

set -e
WORK=$(mktemp -d)
trap "rm -rf $WORK; kill $SERVER_PID 2>/dev/null || true" EXIT

echo "=== Requirements check ==="
for cmd in openssl curl python3; do
  command -v $cmd &>/dev/null && echo "  ✅ $cmd" || { echo "  ❌ $cmd not found"; exit 1; }
done
echo ""

echo "=== Step 1: Generate self-signed CA (not in webpki-roots/system store) ==="
openssl genrsa -out $WORK/ca.key 2048 2>/dev/null
openssl req -x509 -new -key $WORK/ca.key -out $WORK/ca.crt \
  -days 1 -nodes -subj "/CN=test-corporate-CA" 2>/dev/null
echo "Custom CA: $WORK/ca.crt  (CN=test-corporate-CA)"
echo ""

echo "=== Step 2: Generate server cert signed by custom CA ==="
openssl genrsa -out $WORK/server.key 2048 2>/dev/null
openssl req -new -key $WORK/server.key -out $WORK/server.csr \
  -subj "/CN=127.0.0.1" 2>/dev/null
openssl x509 -req -in $WORK/server.csr -CA $WORK/ca.crt -CAkey $WORK/ca.key \
  -CAcreateserial -out $WORK/server.crt -days 1 \
  -extfile <(echo "subjectAltName=IP:127.0.0.1") 2>/dev/null
echo "Server cert: signed by test-corporate-CA"
echo ""

echo "=== Step 3: Start local HTTPS server (no sudo) ==="
PORT=$(python3 -c "import socket; s=socket.socket(); s.bind(('',0)); p=s.getsockname()[1]; s.close(); print(p)")

python3 - "$WORK" "$PORT" &
SERVER_PID=$!
sleep 1

cat << PYEOF | python3 - "$WORK" "$PORT" &
import http.server, ssl, sys, threading

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

SERVER_PID=$!
sleep 1
echo "HTTPS server on 127.0.0.1:$PORT  (cert signed by test-corporate-CA)"
echo ""

echo "=== Test A: curl WITH --cacert (explicit trust — simulates system cert store) ==="
curl -sf --cacert $WORK/ca.crt https://127.0.0.1:$PORT/ \
  && echo "result: ✅ SUCCESS — curl trusts our custom CA" \
  || echo "result: ❌ FAIL"
echo ""

echo "=== Test B: curl WITHOUT --cacert (no custom CA trust — simulates rustls webpki-roots) ==="
curl -sf https://127.0.0.1:$PORT/ \
  && echo "result: ✅ SUCCESS" \
  || echo "result: ❌ FAIL — curl rejects cert (CA not trusted)"
echo ""

echo "========================================"
echo "INTERPRETATION:"
echo "  Test A (curl --cacert):  ✅ = tool CAN be told to trust custom CA"
echo "  Test B (curl plain):     ❌ = without custom CA in trust store → rejected"
echo ""
echo "In corporate VPN/TLS-inspection environments:"
echo "  The VPN adds its CA to the system cert store."
echo "  curl/R/Python/browsers use system cert store → they work (like Test A)"
echo "  rustls (uvr) uses webpki-roots ONLY, ignores system cert store → fails (like Test B)"
echo ""
echo "Fix: build uvr with native-tls feature to use system cert store instead of webpki-roots."
echo "========================================"
