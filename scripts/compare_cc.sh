#!/bin/bash
# Side-by-side comparison: official CC vs clawed
set -e
PROJ="/Users/sqb/Documents/GitHub/clawed-code"
CC="/Users/sqb/.local/bin/claude"
OUT="$PROJ/scripts/comparisons"
mkdir -p "$OUT"

prompt="用中文回复：写一段包含###标题、-列表、>引用和|A|B|表格的markdown"

echo "=== Official CC ==="
timeout 30 "$CC" --print --output-format text "$prompt" > "$OUT/cc_output.txt" 2>/dev/null || true

echo "=== Clawed ==="
(cd "$PROJ" && timeout 30 cargo run -- --print --output-format text "$prompt" > "$OUT/clawed_output.txt" 2>/dev/null) || true

echo "CC lines: $(wc -l < "$OUT/cc_output.txt")"
echo "Clawed lines: $(wc -l < "$OUT/clawed_output.txt")"
echo ""
echo "--- CC output ---"
head -20 "$OUT/cc_output.txt"
echo ""
echo "--- Clawed output ---"
head -20 "$OUT/clawed_output.txt"
