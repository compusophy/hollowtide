#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

echo "▸ building WASM client..."
(
  cd client
  wasm-pack build --release --target web --out-dir ../web/pkg --no-typescript
)

echo ""
echo "✓ client compiled to web/pkg/"
echo ""
echo "next:"
echo "  cd server && cargo run --release"
echo ""
echo "then open http://localhost:8080"
