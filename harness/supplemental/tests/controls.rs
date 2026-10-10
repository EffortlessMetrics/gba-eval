use gba_supplemental::{bundle, fixtures, measure, model::*};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
fn temp() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "gba-supplemental-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root).unwrap();
    root
}
fn observed(variant: &str) -> (Vec<u8>, Vec<u8>, Observations) {
    let rom = fixtures::counter_rom();
    let wasm = wat::parse_str(fixtures::source(variant).unwrap()).unwrap();
    let mut case = Case::new(&rom);
    case.profile.fuel_per_call = 2_000_000;
    case.profile.fuel_lifecycle = 8_000_000;
    let o = measure::execute(case, &wasm, &rom, "abi_fixture").unwrap();
    (wasm, rom, o)
}
fn verdict(o: &Observations, id: &str) -> Verdict {
    measure::analyze(o)
        .unwrap()
        .into_iter()
        .find(|m| m.id == id)
        .unwrap()
        .verdict
}
#[test]
fn valid_and_relocating_controls_pass() {
    for name in ["good", "relocating"] {
        let (_, _, o) = observed(name);
        assert_eq!(verdict(&o, "reset-observable-restoration"), Verdict::Pass);
        assert_eq!(verdict(&o, "input-latch-exercised"), Verdict::Pass);
        assert_eq!(verdict(&o, "audio-drain-exercised"), Verdict::Pass);
        assert_eq!(verdict(&o, "frame-alpha-ff"), Verdict::Pass);
    }
}
#[test]
fn equality_is_not_functionality() {
    let (_, _, o) = observed("equal-wrong");
    assert_eq!(verdict(&o, "reset-vs-reload-frame"), Verdict::Pass);
    assert_eq!(verdict(&o, "reset-observable-restoration"), Verdict::Fail);
    let (_, _, o) = observed("blank");
    assert_eq!(verdict(&o, "reset-vs-reload-frame"), Verdict::Pass);
    assert_eq!(
        verdict(&o, "reset-observable-restoration"),
        Verdict::Indeterminate
    );
}
#[test]
fn named_faults_are_detected() {
    for (variant, id) in [
        ("no-op-reset", "reset-vs-reload-frame"),
        ("partial-reset", "reset-vs-reload-frame"),
        ("one-shot-keys", "input-latch-relation"),
        ("double-drain", "audio-second-drain-empty"),
        ("bad-alpha", "frame-alpha-ff"),
        ("audio-trap", "execution-audio-drain"),
        ("bad-reset-type", "execution-frame-format"),
        ("negative-audio", "execution-audio-drain"),
        ("bad-frame-pointer", "execution-frame-format"),
    ] {
        let (_, _, o) = observed(variant);
        assert_eq!(verdict(&o, id), Verdict::Fail, "{variant}: {id}");
    }
}
#[test]
fn fuel_exhaustion_is_not_fabricated_silence_or_nontermination_proof() {
    let (_, _, o) = observed("infinite-frame");
    assert_eq!(
        verdict(&o, "execution-frame-format"),
        Verdict::Indeterminate
    );
    assert!(o
        .traces
        .iter()
        .all(|t| t.failure.as_ref().unwrap().kind == FailureKind::GuestFuel));
}
#[test]
fn valid_failure_survives_later_missing_audio() {
    let (_, _, mut o) = observed("no-op-reset");
    let trace = o.traces.iter_mut().find(|t| t.id == "reset").unwrap();
    trace.captures.remove("end-audio");
    trace.failure = Some(Failure {
        kind: FailureKind::Instrument,
        operation: "capture".into(),
        message: "interrupted after frame".into(),
    });
    assert_eq!(verdict(&o, "reset-vs-reload-frame"), Verdict::Fail);
    assert_eq!(verdict(&o, "reset-vs-reload-audio"), Verdict::Indeterminate);
}
#[test]
fn schemas_reject_invalid_cases() {
    let mut case = Case::new(&fixtures::counter_rom());
    case.instruments.push(Instrument::Reset);
    assert!(case.validate().is_err());
    case.instruments.pop();
    case.probe_frames = 0;
    assert!(case.validate().is_err());
    case.probe_frames = 2;
    case.id = "../escape".into();
    assert!(case.validate().is_err());
    let mut value = serde_json::to_value(Case::new(&fixtures::counter_rom())).unwrap();
    value["model_judge"] = serde_json::json!(true);
    assert!(serde_json::from_value::<Case>(value).is_err());
}
#[test]
fn bundles_preserve_evidence_and_reject_tampering_and_replacement() {
    let root = temp();
    let out = root.join("run");
    let (wasm, rom, o) = observed("good");
    let seal = bundle::write_bundle(&out, &wasm, &rom, &o, None).unwrap();
    let (saved, ms, _) = bundle::validate_bundle(&out, Some(&seal)).unwrap();
    assert_eq!(saved, o);
    assert_eq!(ms, measure::analyze(&o).unwrap());
    assert!(bundle::write_bundle(&out, &wasm, &rom, &o, None).is_err());
    assert!(bundle::validate_bundle(&out, Some(&"0".repeat(64))).is_err());
    std::fs::write(out.join("candidate.wasm"), b"tampered").unwrap();
    assert!(bundle::validate_bundle(&out, None).is_err());
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn unsealed_and_symlink_bundles_are_rejected() {
    let root = temp();
    assert!(bundle::validate_bundle(&root, None).is_err());
    #[cfg(unix)]
    {
        let link = root.join("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        assert!(bundle::validate_bundle(&link, None).is_err());
    }
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn mismatched_input_is_not_a_candidate_failure() {
    let wasm = wat::parse_str(fixtures::source("good").unwrap()).unwrap();
    let case = Case::new(b"intended ROM");
    assert!(measure::execute(case, &wasm, b"different ROM", "candidate").is_err());
}
#[test]
fn hand_assembled_rom_has_consistent_header_literals_and_branches() {
    let rom = fixtures::counter_rom();
    assert_eq!(rom.len(), 512);
    assert_eq!(&rom[..4], &0xea00002eu32.to_le_bytes());
    assert_eq!(
        rom[0xa0..0xbe]
            .iter()
            .fold(0x19u8, |sum, b| sum.wrapping_add(*b)),
        0
    );
    for (at, target) in [(9usize, 7i32), (12, 10), (20, 7)] {
        let pos = 0xc0 + at * 4;
        let word = u32::from_le_bytes(rom[pos..pos + 4].try_into().unwrap());
        let disp = ((word << 8) as i32) >> 8;
        assert_eq!(at as i32 + 2 + disp, target);
    }
}
#[test]
fn hostile_schedules_are_bounded_before_allocating_validation_state() {
    let (_, _, mut o) = observed("good");
    o.traces[0].schedule.push(Action::Frames { count: u32::MAX });
    assert!(o.validate().is_err());
    let (_, _, mut o) = observed("good");
    o.traces[0].schedule.push(Action::Frame { tag: "end".into() });
    assert!(o.validate().is_err());
}
