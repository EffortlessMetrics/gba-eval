//! Small versioned instruments; no arbitrary executable policy in case manifests.
use crate::{driver::Factory, model::*};
use anyhow::{ensure, Result};

fn frames(count: u32) -> Action {
    Action::Frames { count }
}
fn frame(tag: &str) -> Action {
    Action::Frame { tag: tag.into() }
}
fn audio(tag: &str) -> Action {
    Action::Audio { tag: tag.into() }
}
fn keys(value: u16) -> Action {
    Action::Keys { value }
}

pub fn schedules(case: &Case) -> Vec<(String, Vec<Action>)> {
    let mut out = Vec::new();
    for instrument in &case.instruments {
        match instrument {
            Instrument::Reset => {
                out.push((
                    "fresh".into(),
                    vec![
                        Action::Load,
                        frames(case.probe_frames),
                        frame("end"),
                        audio("end-audio"),
                    ],
                ));
                for (id, transition) in [
                    ("reset", Some(Action::Reset)),
                    ("reload", Some(Action::Load)),
                    ("continued", None),
                ] {
                    let mut actions =
                        vec![Action::Load, keys(case.keys), frames(case.prefix_frames)];
                    if let Some(a) = transition {
                        actions.push(a);
                    }
                    actions.extend([frames(case.probe_frames), frame("end"), audio("end-audio")]);
                    out.push((id.into(), actions));
                }
            }
            Instrument::InputLatch => {
                out.push((
                    "input-once".into(),
                    vec![
                        Action::Load,
                        keys(case.keys),
                        frames(case.probe_frames),
                        frame("end"),
                    ],
                ));
                let mut repeated = vec![Action::Load];
                for _ in 0..case.probe_frames {
                    repeated.extend([keys(case.keys), frames(1)]);
                }
                repeated.push(frame("end"));
                out.push(("input-repeated".into(), repeated));
                out.push((
                    "input-none".into(),
                    vec![Action::Load, frames(case.probe_frames), frame("end")],
                ));
            }
            Instrument::AudioDrain => {
                out.push((
                    "audio-drain".into(),
                    vec![
                        Action::Load,
                        frames(1),
                        audio("first"),
                        audio("second"),
                        frames(1),
                        audio("next"),
                    ],
                ));
            }
            Instrument::FrameFormat => {
                out.push((
                    "frame-format".into(),
                    vec![Action::Load, frames(case.probe_frames), frame("end")],
                ));
            }
        }
    }
    out
}

pub fn execute(
    case: Case,
    candidate: &[u8],
    rom: &[u8],
    subject_kind: &str,
) -> Result<Observations> {
    case.validate()?;
    ensure!(
        hash(rom) == case.rom_sha256,
        "ROM bytes do not match case identity"
    );
    ensure!(
        !rom.is_empty() && rom.len() <= MAX_ROM,
        "invalid host ROM size"
    );
    ensure!(
        matches!(subject_kind, "candidate" | "abi_fixture"),
        "unknown subject kind"
    );
    let factory = Factory::new(candidate, &case.profile);
    let mut traces = Vec::new();
    for (id, schedule) in schedules(&case) {
        traces.push(match &factory {
            Ok(f) => f.run(&id, rom, schedule),
            Err(e) => Trace {
                id,
                schedule,
                calls: Vec::new(),
                captures: Default::default(),
                failure: Some(e.clone()),
            },
        });
    }
    let observed = Observations {
        schema_version: VERSION,
        case,
        candidate_sha256: hash(candidate),
        subject_kind: subject_kind.into(),
        driver_version: env!("CARGO_PKG_VERSION").into(),
        wasmtime_version: "27.0.0".into(),
        traces,
    };
    observed.validate()?;
    Ok(observed)
}

fn capture<'a>(o: &'a Observations, trace: &str, tag: &str) -> Option<&'a Capture> {
    o.traces.iter().find(|t| t.id == trace)?.captures.get(tag)
}
fn image<'a>(o: &'a Observations, trace: &str) -> Option<&'a [u8]> {
    capture(o, trace, "end")?.frame_le.as_deref()
}
fn measurement(
    id: &str,
    claim: Claim,
    truth: Option<bool>,
    pass: &str,
    fail: &str,
    evidence: &[&str],
) -> Measurement {
    let (verdict, reason) = match truth {
        Some(true) => (Verdict::Pass, pass),
        Some(false) => (Verdict::Fail, fail),
        None => (
            Verdict::Indeterminate,
            "required observations unavailable; inspect execution findings",
        ),
    };
    Measurement {
        id: id.into(),
        claim,
        verdict,
        reason: reason.into(),
        evidence: evidence.iter().map(|s| (*s).into()).collect(),
    }
}
fn compare_frames(o: &Observations, id: &str, claim: Claim, a: &str, b: &str) -> Measurement {
    let truth = image(o, a).zip(image(o, b)).map(|(a, b)| a == b);
    measurement(
        id,
        claim,
        truth,
        "captured frames are exactly equal",
        "captured frames differ",
        &[a, b],
    )
}

pub fn analyze(o: &Observations) -> Result<Vec<Measurement>> {
    o.validate()?;
    let expected = schedules(&o.case);
    ensure!(expected.len() == o.traces.len(), "missing/extra trace");
    for (id, schedule) in expected {
        let trace = o
            .traces
            .iter()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow::anyhow!("missing trace {id}"))?;
        ensure!(
            trace.schedule == schedule,
            "schedule differs from instrument v1 for {id}"
        );
    }
    let mut out = Vec::new();
    for t in &o.traces {
        let (claim, verdict, reason) = match &t.failure {
            None => (
                Claim::Contract,
                Verdict::Pass,
                "required scheduled ABI calls completed under the recorded profile".into(),
            ),
            Some(e) if e.kind == FailureKind::Candidate => {
                (Claim::Contract, Verdict::Fail, e.to_string())
            }
            Some(e) => (Claim::Diagnostic, Verdict::Indeterminate, e.to_string()),
        };
        out.push(Measurement {
            id: format!("execution-{}", t.id),
            claim,
            verdict,
            reason,
            evidence: vec![t.id.clone()],
        });
    }
    for instrument in &o.case.instruments {
        match instrument {
            Instrument::Reset => {
                out.push(compare_frames(
                    o,
                    "reset-vs-reload-frame",
                    Claim::Relational,
                    "reset",
                    "reload",
                ));
                out.push(compare_frames(
                    o,
                    "reset-vs-fresh-frame",
                    Claim::Diagnostic,
                    "reset",
                    "fresh",
                ));
                let audio_pair =
                    capture(o, "reset", "end-audio").zip(capture(o, "reload", "end-audio"));
                let truth = audio_pair.and_then(|(a, b)| {
                    a.pcm
                        .as_ref()
                        .zip(b.pcm.as_ref())
                        .map(|(pa, pb)| pa == pb && a.sample_rate == b.sample_rate)
                });
                out.push(measurement(
                    "reset-vs-reload-audio",
                    Claim::Relational,
                    truth,
                    "post-transition audio and rates agree",
                    "post-transition audio or rates differ",
                    &["reset/end-audio", "reload/end-audio"],
                ));
                let mut active = measurement(
                    "reset-observable-restoration",
                    Claim::Relational,
                    None,
                    "",
                    "",
                    &["fresh/end", "continued/end", "reset/end", "reload/end"],
                );
                if let (Some(fresh), Some(continued), Some(reset), Some(reload)) = (
                    image(o, "fresh"),
                    image(o, "continued"),
                    image(o, "reset"),
                    image(o, "reload"),
                ) {
                    if fresh == continued {
                        active.reason = "no fresh-versus-continued contrast; equality does not establish functional reset".into();
                    } else if reset == reload && reset != continued {
                        active.verdict = Verdict::Pass;
                        active.reason = "reset matched reload and changed the observed continuation; not universal emulator correctness".into();
                    } else {
                        active.verdict = Verdict::Fail;
                        active.reason = "observable prefix state existed, but reset did not demonstrate reload-equivalent restoration".into();
                    }
                }
                out.push(active);
            }
            Instrument::InputLatch => {
                out.push(compare_frames(
                    o,
                    "input-latch-relation",
                    Claim::Relational,
                    "input-once",
                    "input-repeated",
                ));
                let mut active = measurement(
                    "input-latch-exercised",
                    Claim::Relational,
                    None,
                    "",
                    "",
                    &["input-once/end", "input-repeated/end", "input-none/end"],
                );
                if let (Some(once), Some(repeated), Some(none)) = (
                    image(o, "input-once"),
                    image(o, "input-repeated"),
                    image(o, "input-none"),
                ) {
                    if repeated == none {
                        active.reason = "stimulus did not expose input effects; no demonstrated input-latching claim".into();
                    } else if once == repeated {
                        active.verdict = Verdict::Pass;
                        active.reason =
                            "observed input effect persisted without resending the setter".into();
                    } else {
                        active.verdict = Verdict::Fail;
                        active.reason = "setting once disagrees with repeated setting of the same latched input".into();
                    }
                }
                out.push(active);
            }
            Instrument::AudioDrain => {
                let first = capture(o, "audio-drain", "first").and_then(|c| c.pcm.as_ref());
                let second = capture(o, "audio-drain", "second").and_then(|c| c.pcm.as_ref());
                out.push(measurement(
                    "audio-second-drain-empty",
                    Claim::Contract,
                    second.map(|s| s.is_empty()),
                    "second drain contains no already-drained samples",
                    "second drain returned samples with no intervening frame",
                    &["audio-drain/first", "audio-drain/second"],
                ));
                let mut active = measurement(
                    "audio-drain-exercised",
                    Claim::Relational,
                    None,
                    "",
                    "",
                    &["audio-drain/first", "audio-drain/second"],
                );
                if let (Some(first), Some(second)) = (first, second) {
                    if first.is_empty() {
                        active.reason = "no generated samples were observed; empty-versus-empty is not drain qualification".into();
                    } else {
                        active.verdict = if second.is_empty() {
                            Verdict::Pass
                        } else {
                            Verdict::Fail
                        };
                        active.reason =
                            "nonempty first drain compared with immediate second drain".into();
                    }
                }
                out.push(active);
                let next = capture(o, "audio-drain", "next").and_then(|c| c.pcm.as_ref());
                let mut m = measurement("audio-next-frame-produced-samples", Claim::Diagnostic, next.map(|s| !s.is_empty()), "a subsequent frame produced samples", "no samples observed after the next frame; not by itself an audio correctness failure", &["audio-drain/next"]);
                if next.is_some_and(|s| s.is_empty()) {
                    m.verdict = Verdict::Indeterminate;
                }
                out.push(m);
            }
            Instrument::FrameFormat => {
                let truth =
                    image(o, "frame-format").map(|f| f.chunks_exact(4).all(|p| p[3] == 255));
                out.push(measurement(
                    "frame-alpha-ff",
                    Claim::Contract,
                    truth,
                    "all captured alpha bytes are FF as specified",
                    "captured framebuffer violates the specified FF alpha",
                    &["frame-format/end"],
                ));
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

fn escape(text: &str) -> String {
    text.chars()
        .take(2048)
        .flat_map(|c| match c {
            '<' => "&lt;".chars().collect::<Vec<_>>(),
            '>' => "&gt;".chars().collect(),
            '&' => "&amp;".chars().collect(),
            '\n' | '\r' => vec![' '],
            '`' | '*' | '_' | '[' | ']' | '|' | '\\' => vec!['\\', c],
            _ => vec![c],
        })
        .collect()
}
pub fn markdown(o: &Observations, measures: &[Measurement]) -> String {
    use std::fmt::Write;
    let mut s = format!("# Supplemental GBA measurements\n\nCase: `{}`. Subject: `{}` (`{}`).\n\nThese are separate measurements, not a replacement GBA Eval score. No universal emulator/ABI correctness claim is made.\n\n", o.case.id, o.candidate_sha256, o.subject_kind);
    s.push_str("| Measurement | Claim | Verdict | Evidence and reason |\n|---|---|---|---|\n");
    for m in measures {
        let _ = writeln!(
            s,
            "| {} | {:?} | {:?} | {} — {} |",
            escape(&m.id),
            m.claim,
            m.verdict,
            escape(&m.evidence.join(", ")),
            escape(&m.reason)
        );
    }
    s.push_str("\nExact schedules, call outcomes, raw little-endian framebuffer bytes, and interleaved PCM are in `observations.json`. `candidate.wasm`, `rom.gba`, and `case.json` preserve the inputs. `manifest.json` and `seal.sha256` check content integrity, not producer authenticity. The executed binary identity and host containment must come from the invocation receipt.\n");
    s
}
