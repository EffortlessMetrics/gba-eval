#!/usr/bin/env bash
# Full original-corpus parity. Inputs and scores remain untouched.
set -euo pipefail
ROOT="$(pwd)"
OUT="$ROOT/_baseline-receipts"
IMAGE=ubuntu@sha256:534baea6a22c03a63003dbc8dbe78fe34bc0d7e595d9a9dc9834884ff530eb55
CID=""
cleanup() {
    if [ -n "$CID" ] && [ -f "$CID" ]; then
        docker rm -f "$(cat "$CID")" >/dev/null 2>&1 || true
        rm -f "$CID"
    fi
}
trap cleanup EXIT INT TERM
for specimen in gba_core_shim blank; do
    if [ "$specimen" = gba_core_shim ]; then
        WASM="$ROOT/candidates/gba-core/gba_core_shim.wasm"
    else
        WASM="$OUT/blank.wasm"
    fi
    for repetition in stock-a stock-b stock-c fork; do
        if [ "$repetition" = fork ]; then SOURCE="$ROOT"; else SOURCE="$ROOT/_baseline-stock"; fi
        NAME="$specimen-$repetition"
        mkdir "$OUT/$NAME"
        CID="$OUT/$NAME.cid"
        timeout --kill-after=15s 600 docker run --rm --cidfile "$CID" \
            --network none --read-only --cap-drop ALL --security-opt no-new-privileges \
            --memory 5g --memory-swap 5g --cpus 2 --pids-limit 64 \
            --tmpfs /tmp:rw,noexec,nosuid,size=64m --user "$(id -u):$(id -g)" \
            --env RAYON_NUM_THREADS=1 --workdir /repo \
            -v "$SOURCE:/repo:ro" -v "$WASM:/candidate.wasm:ro" -v "$OUT/$NAME:/out:rw" \
            "$IMAGE" /repo/target/debug/grader /candidate.wasm corpus/ /out \
            > "$OUT/$NAME.log" 2>&1
        cleanup
        CID=""
    done
    python3 supplemental/baseline.py compare "$OUT/$specimen-fork" "$OUT/$specimen-parity.json" \
        --stock "$OUT/$specimen-stock-a" --stock "$OUT/$specimen-stock-b" --stock "$OUT/$specimen-stock-c" \
        --manifest corpus/testcases.json
    cat "$OUT/$specimen-parity.json"
done
