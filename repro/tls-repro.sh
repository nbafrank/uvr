#!/bin/bash
# Reproduction script for uvr rustls TLS issue
# NO sudo required — uses a temporary user-level cert store
#
# Demonstrates that reqwest with rustls-tls does NOT use the system cert store,
# while reqwest with native-tls does. This explains why corporate VPN/TLS-inspection
# causes UnknownIssuer in uvr but not in curl/Python/R.
#
# Requirements: cargo (rustup), openssl, python3
# Usage: bash repro-noroot.sh

set -e
WORK=$(mktemp -d)

# Check requirements
if ! command -v cargo &>/dev/null; then
  echo "❌ cargo not found. Install Rust: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
  exit 1
fi
if ! command -v openssl &>/dev/null; then
  echo "❌ openssl not found"
  exit 1
fi
if ! command -v python3 &>/dev/null; then
  echo "❌ python3 not found"
  exit 1
fi
trap "rm -rf $WORK" EXIT

echo "=== Step 1: Generate a self-signed CA (not in webpki-roots) ==="
openssl genrsa -out $WORK/ca.key 2048 2>/dev/null
openssl req -x509 -new -key $WORK/ca.key -out $WORK/ca.crt \
  -days 1 -nodes -subj "/CN=test-corporate-CA" 2>/dev/null
echo "Custom CA: $WORK/ca.crt"

echo ""
echo "=== Step 2: Generate server cert signed by our CA ==="
openssl genrsa -out $WORK/server.key 2048 2>/dev/null
openssl req -new -key $WORK/server.key -out $WORK/server.csr \
  -subj "/CN=localhost" 2>/dev/null
openssl x509 -req -in $WORK/server.csr -CA $WORK/ca.crt -CAkey $WORK/ca.key \
  -CAcreateserial -out $WORK/server.crt -days 1 2>/dev/null
echo "Server cert signed by custom CA (NOT trusted by webpki-roots)"

echo ""
echo "=== Step 3: Start local HTTPS server (no sudo needed) ==="
python3 - "$WORK" << 'PYEOF' &
import http.server, ssl, sys

work = sys.argv[1]

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b'Package: BiocStyle\nVersion: 2.38.0\n')
    def log_message(self, *a): pass

server = http.server.HTTPServer(('127.0.0.1', 14443), Handler)
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(f'{work}/server.crt', f'{work}/server.key')
server.socket = ctx.wrap_socket(server.socket, server_side=True)
server.serve_forever()
PYEOF
SERVER_PID=$!
sleep 1

echo ""
echo "=== Step 4: curl with --cacert (simulates system cert store trusting our CA) ==="
curl -s --cacert $WORK/ca.crt https://localhost:14443/ \
  && echo "curl --cacert: ✅ SUCCESS" \
  || echo "curl --cacert: ❌ FAIL"

echo ""
echo "=== Step 5: Build and run reqwest test (no sudo, isolated cargo project) ==="
mkdir -p $WORK/rust-test/src

cat > $WORK/rust-test/Cargo.toml << 'TOML'
[package]
name = "tls-test"
version = "0.1.0"
edition = "2021"

[dependencies]
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "blocking"] }
TOML

cat > $WORK/rust-test/src/main.rs << 'RUST'
fn main() {
    // rustls-tls: uses webpki-roots only — does NOT trust system/custom CAs
    let result = reqwest::blocking::get("https://localhost:14443/");
    match result {
        Ok(r)  => println!("reqwest rustls-tls: ✅ OK ({})", r.status()),
        Err(e) => println!("reqwest rustls-tls: ❌ FAIL\n  Error: {e}"),
    }
}
RUST

echo "Building reqwest rustls-tls test (first build ~30s, subsequent instant)..."
cd $WORK/rust-test && cargo run 2>&1

echo ""
echo "=== Step 6: Same test with native-tls (uses system cert store) ==="
sed -i '' 's/rustls-tls/native-tls/' $WORK/rust-test/Cargo.toml 2>/dev/null || \
  sed -i 's/rustls-tls/native-tls/' $WORK/rust-test/Cargo.toml
# native-tls needs to trust our CA — pass it via SSL_CERT_FILE (macOS/Linux)
export SSL_CERT_FILE=$WORK/ca.crt
cd $WORK/rust-test && cargo run 2>&1

kill $SERVER_PID 2>/dev/null || true

echo ""
echo "========================================"
echo "EXPECTED RESULT:"
echo "  curl --cacert:      ✅ (explicitly trusts our CA)"
echo "  reqwest rustls-tls: ❌ UnknownIssuer (webpki-roots doesn't have our CA)"
echo "  reqwest native-tls: ✅ (uses system cert store / SSL_CERT_FILE)"
echo ""
echo "This is exactly what corporate TLS-inspection does:"
echo "  System cert store: trusts corporate CA ✅"
echo "  rustls webpki-roots: doesn't know corporate CA ❌"
echo "========================================"
