# Supplemental GBA evaluation

This is an additive evaluation path. It does not import or change the original scoring kernel, corpus, thresholds, or summary format. The root workspace adds one crate. No model is called during measurement, qualification, validation, or reporting.

The first instruments measure reset-versus-reload relationships, input latching, audio count draining, and framebuffer alpha conformance. They report individual findings, not a replacement leaderboard score. Fresh-start equivalence is diagnostic: the published ABI's reset/reload relationship and assumptions about host input/audio state are not silently collapsed into one claim.

## Build and use

```sh
cargo +1.87.0 build -p gba-supplemental
cargo +1.87.0 test -p gba-supplemental
BIN=target/debug/gba-supplemental
$BIN counter-rom /tmp/counter.gba
$BIN case --rom /tmp/counter.gba > /tmp/case.json
$BIN run --candidate candidates/gba-core/gba_core_shim.wasm \
  --rom /tmp/counter.gba --case /tmp/case.json --out /tmp/new-run
$BIN verify /tmp/new-run
$BIN report /tmp/new-run
$BIN reanalyze /tmp/new-run /tmp/new-analysis.json
$BIN qualify /tmp/new-qualification
```

These are low-level native commands. The binary limits guest fuel, guest memory/table growth, module/ROM size, and capture sizes. Those limits do not contain compilation, total host memory, or process lifetime. Run untrusted candidates only inside a disposable, bounded process/container. The `Supplemental evidence` workflow provisions first, then executes qualification and the shipped specimens with network denied, a read-only source mount, no model keys, bounded memory/CPU/PIDs, a watchdog, and a separate writable output mount. Do not mount credentials or a container socket.

`run` exits successfully when it has produced a complete evidence bundle, even when the candidate violates a tested contract. Inspect `measurements.json`. Infrastructure/input failures are nonzero exits. `qualify` is nonzero if a declared control expectation is contradicted. All completed control evidence is retained.

## What is in the bundle

The exact candidate, ROM, case, ABI schedules and call results, raw captured framebuffer bytes and PCM, derived measurements, and Markdown report are preserved. Writes reserve a new directory and publish the completeness seal last. Existing output directories are never replaced. Partial unsealed directories remain incomplete; use a new destination for another attempt.

`verify` checks required files, bounded sizes, no unexpected paths or symlinks, canonical JSON, content hashes, input identity, schedules, and re-derived measurements. Record the printed seal outside the bundle to detect replacement of the entire self-consistent bundle. Hashes are not producer authentication. `reanalyze` writes a new linked result and does not rerun the emulator.

Only the scheduled snapshots are captured. This is lossless evidence for these comparisons, not a complete video trace or support for arbitrary future temporal rescoring. Missing required observations remain indeterminate. A valid observed failure is not erased by a later unrelated capture failure.

An optional `--upstream-summary` attachment preserves original bytes and verifies candidate identity. It is explicitly unqualified on its own: attaching an old score does not demonstrate complete corpus availability, stock-score parity, or comparable agent development conditions.

## Qualification and current boundaries

Sixteen original, source-built WAT controls include conforming and relocating buffers, no-op/partial reset, equally wrong reset/load, blank output, one-shot or ignored inputs, double draining, traps, wrong reset signature, invalid pointers/counts/alpha, and fuel exhaustion. These are **ABI fixtures, not GBA emulators**. Their matrix qualifies the named instrumentation and comparisons against these controls; it is not a population error rate.

The original 512-byte ARM ROM in `fixtures.rs` configures mode 3, writes a VBlank counter to pixel zero, and input bits 0–7 to pixel one. Its header, literals, and branch offsets are checked mechanically. The CI observations against shipped Mesen and gba-core are a separate reference-qualification step. Neither implementation is assumed to pass every obligation, and a source-built ROM is not an independently validated oracle merely because it compiles.

There is no blanket ABI-compliant or emulator-correct badge. Input mapping/release, broader gameplay outcomes, arbitrary audio fidelity, historical trajectories, costs, hidden-pack isolation, and the separate SDLC experiments require their own qualified instruments. The roadmap remains in issue #1; a passing synthetic matrix does not close that roadmap.

## License

Original supplemental source and fixtures are MIT, following the repository's MIT portion. Existing reference, shim, ROM, and third-party license boundaries remain unchanged. The ARM fixture contains no commercial ROM or BIOS dump. This fork's additions are not a claim of upstream endorsement.
