//! Original MIT qualification fixtures. ABI fixtures are NOT GBA emulators.
use crate::{
    bundle::write_bundle,
    measure::{analyze, execute},
    model::*,
};
use anyhow::{ensure, Result};
use serde::Serialize;
use std::{collections::BTreeMap, fs, path::Path};

pub const VARIANTS: &[&str] = &[
    "good",
    "relocating",
    "no-op-reset",
    "partial-reset",
    "equal-wrong",
    "blank",
    "one-shot-keys",
    "ignored-keys",
    "double-drain",
    "frame-trap",
    "audio-trap",
    "bad-alpha",
    "bad-reset-type",
    "infinite-frame",
    "negative-audio",
    "bad-frame-pointer",
];

pub fn source(variant: &str) -> Result<String> {
    ensure!(VARIANTS.contains(&variant), "unknown fixture variant");
    let load = if variant == "equal-wrong" {
        ""
    } else {
        "call $clear"
    };
    let reset = match variant {
        "no-op-reset" | "equal-wrong" => "",
        "partial-reset" => "i32.const 0 global.set $frame i32.const 0 global.set $pairs",
        _ => "call $clear",
    };
    let reset_function = if variant == "bad-reset-type" {
        "(func (export \"emu_reset\") (result i64) i64.const 1)".to_string()
    } else {
        format!("(func (export \"emu_reset\") (result i32) {reset} i32.const 1)")
    };
    let setter = if variant == "ignored-keys" {
        ""
    } else {
        "local.get 0 global.set $keys"
    };
    let run = match variant {
        "blank" => "".into(),
        "frame-trap" => "unreachable".into(),
        "infinite-frame" => "(loop $forever br $forever)".into(),
        _ => {
            let relocate = if variant == "relocating" {
                "global.get $fb i32.const 262144 i32.xor global.set $fb"
            } else {
                ""
            };
            let alpha = if variant == "bad-alpha" {
                "0"
            } else {
                "-16777216"
            };
            let unlatch = if variant == "one-shot-keys" {
                "i32.const 0 global.set $keys"
            } else {
                ""
            };
            format!(
                r#"
                global.get $frame i32.const 1 i32.add global.set $frame
                {relocate}
                global.get $fb i32.const 255 i32.const 153600 memory.fill
                global.get $fb
                i32.const {alpha} global.get $frame i32.const 10 i32.shl i32.or global.get $keys i32.or i32.store
                i32.const 700000 global.get $frame i32.store16
                i32.const 700002 i32.const 0 global.get $frame i32.sub i32.store16
                i32.const 1 global.set $pairs
                {unlatch}
            "#
            )
        }
    };
    let drain = match variant {
        "audio-trap" => "unreachable",
        "negative-audio" => "i32.const -1",
        "double-drain" => "global.get $pairs",
        _ => "global.get $pairs i32.const 0 global.set $pairs",
    };
    let framebuffer = if variant == "bad-frame-pointer" {
        "i32.const -1"
    } else {
        "global.get $fb"
    };
    Ok(format!(
        r#"(module
        (memory (export "memory") 528)
        (global $frame (mut i32) (i32.const 0))
        (global $keys (mut i32) (i32.const 0))
        (global $pairs (mut i32) (i32.const 0))
        (global $fb (mut i32) (i32.const 0))
        (func $clear
            i32.const 0 global.set $frame
            i32.const 0 global.set $keys
            i32.const 0 global.set $pairs
            global.get $fb i32.const 255 i32.const 153600 memory.fill)
        (func (export "emu_init") (result i32) call $clear i32.const 1)
        (func (export "emu_rom_buffer") (result i32) i32.const 1048576)
        (func (export "emu_load_rom") (param i32) (result i32) {load} i32.const 1)
        {reset_function}
        (func (export "emu_set_keys") (param i32) {setter})
        (func (export "emu_run_frame") {run})
        (func (export "emu_framebuffer") (result i32) {framebuffer})
        (func (export "emu_audio_samples") (result i32) {drain})
        (func (export "emu_audio_buffer") (result i32) i32.const 700000)
        (func (export "emu_audio_rate") (result i32) i32.const 32768)
    )"#
    ))
}

/// Original 512-byte ARM program, not a commercial ROM/BIOS dump.
/// Mode 3: pixel 0 = VBlank counter mod 32; pixel 1 = active-low key bits 0..7.
/// Header branches to 0xC0. All literal addresses and branches are derived below.
/// This program needs independent reference execution before being used as an oracle.
pub fn counter_rom() -> Vec<u8> {
    let mut words = [
        0,          // ldr r0, =0x04000000
        0xe3a01b01, // mov r1, #0x400
        0xe3811003, // orr r1, r1, #3
        0xe1c010b0, // strh r1, [r0]
        0xe3a02000, // mov r2, #0
        0,          // ldr r3, =0x06000000
        0,          // ldr r7, =0x04000130
        0xe1d040b6, // visible: ldrh r4, [r0, #6]
        0xe35400a0, // cmp r4, #160
        0,          // bhs visible
        0xe1d040b6, // vblank: ldrh r4, [r0, #6]
        0xe35400a0, // cmp r4, #160
        0,          // blo vblank
        0xe2822001, // add r2, r2, #1
        0xe202501f, // and r5, r2, #31
        0xe1c350b0, // strh r5, [r3]
        0xe1d760b0, // ldrh r6, [r7]
        0xe1e06006, // mvn r6, r6
        0xe20660ff, // and r6, r6, #255
        0xe1c360b2, // strh r6, [r3, #2]
        0,          // b visible
        0x04000000, 0x06000000, 0x04000130,
    ];
    for (instruction, literal, register) in [(0usize, 21usize, 0u32), (5, 22, 3), (6, 23, 7)] {
        words[instruction] =
            0xe59f0000 | (register << 12) | ((literal - instruction - 2) as u32 * 4);
    }
    for (at, target, condition) in [(9usize, 7usize, 0x2u32), (12, 10, 0x3), (20, 7, 0xe)] {
        let displacement = target as i32 - at as i32 - 2;
        words[at] = (condition << 28) | 0x0a000000 | (displacement as u32 & 0x00ffffff);
    }
    let mut rom = vec![0; 512];
    rom[..4].copy_from_slice(&0xea00002eu32.to_le_bytes());
    rom[0xa0..0xac].copy_from_slice(b"EM RESET V1 ");
    rom[0xb2] = 0x96;
    let checksum = rom[0xa0..0xbd]
        .iter()
        .fold(0x19u8, |sum, b| sum.wrapping_add(*b));
    rom[0xbd] = 0u8.wrapping_sub(checksum);
    for (i, word) in words.iter().enumerate() {
        rom[0xc0 + i * 4..0xc4 + i * 4].copy_from_slice(&word.to_le_bytes());
    }
    rom
}

fn expected(variant: &str) -> Vec<(&'static str, Verdict)> {
    match variant {
        "good" | "relocating" => vec![
            ("reset-observable-restoration", Verdict::Pass),
            ("input-latch-exercised", Verdict::Pass),
            ("audio-drain-exercised", Verdict::Pass),
            ("frame-alpha-ff", Verdict::Pass),
        ],
        "no-op-reset" | "partial-reset" => vec![("reset-vs-reload-frame", Verdict::Fail)],
        "equal-wrong" => vec![
            ("reset-vs-reload-frame", Verdict::Pass),
            ("reset-observable-restoration", Verdict::Fail),
        ],
        "blank" => vec![
            ("reset-vs-reload-frame", Verdict::Pass),
            ("reset-observable-restoration", Verdict::Indeterminate),
            ("audio-drain-exercised", Verdict::Indeterminate),
        ],
        "one-shot-keys" => vec![("input-latch-relation", Verdict::Fail)],
        "ignored-keys" => vec![("input-latch-exercised", Verdict::Indeterminate)],
        "double-drain" => vec![("audio-second-drain-empty", Verdict::Fail)],
        "bad-alpha" => vec![("frame-alpha-ff", Verdict::Fail)],
        "infinite-frame" => vec![("execution-frame-format", Verdict::Indeterminate)],
        "audio-trap" | "negative-audio" => vec![("execution-audio-drain", Verdict::Fail)],
        _ => vec![("execution-frame-format", Verdict::Fail)],
    }
}

#[derive(Serialize)]
pub struct ControlResult {
    pub source_sha256: String,
    pub bundle_seal: String,
    pub expected: BTreeMap<String, Verdict>,
    pub observed: BTreeMap<String, Verdict>,
    pub matches: bool,
}
pub fn qualify(out: &Path) -> Result<bool> {
    fs::create_dir(out)?;
    let rom = counter_rom();
    let mut matrix = BTreeMap::new();
    let mut all_match = true;
    for variant in VARIANTS {
        let wat = source(variant)?;
        let wasm = wat::parse_str(&wat)?;
        let mut case = Case::new(&rom);
        case.profile.fuel_per_call = 2_000_000;
        case.profile.fuel_lifecycle = 8_000_000;
        let observed = execute(case, &wasm, &rom, "abi_fixture")?;
        let measures = analyze(&observed)?;
        let expected: BTreeMap<String, Verdict> = expected(variant)
            .into_iter()
            .map(|(k, v)| (k.into(), v))
            .collect();
        let actual: BTreeMap<String, Verdict> =
            measures.iter().map(|m| (m.id.clone(), m.verdict)).collect();
        let matches = expected.iter().all(|(k, v)| actual.get(k) == Some(v));
        all_match &= matches;
        fs::write(out.join(format!("{variant}.wat")), &wat)?;
        let seal = write_bundle(&out.join(variant), &wasm, &rom, &observed, None)?;
        matrix.insert(
            (*variant).to_string(),
            ControlResult {
                source_sha256: hash(wat.as_bytes()),
                bundle_seal: seal,
                expected,
                observed: actual,
                matches,
            },
        );
    }
    fs::write(
        out.join("qualification.json"),
        serde_json::to_vec_pretty(&matrix)?,
    )?;
    fs::write(out.join("README.md"), "# ABI-driver qualification\n\nThese source-built WAT fixtures are not GBA emulators. This matrix qualifies named host-driver and relational-comparison behaviors against the selected controls. It does not qualify arbitrary GBA hardware behavior or population error rates. Every attempted control and bundle is retained, including mismatches. The original ARM ROM is included as inert input to these ABI fixtures; reference execution is a separate qualification step.\n")?;
    Ok(all_match)
}
