//! Supplemental contracts. Nothing here participates in the upstream score.
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const VERSION: u32 = 1;
pub const UPSTREAM: &str = "92b585e5a0f6c7533bf69b9a4348b517a1b735e0";
pub const FRAME_BYTES: usize = 240 * 160 * 4;
pub const MAX_ROM: usize = 32 * 1024 * 1024;
pub const MAX_MODULE: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub fuel_per_call: u64,
    pub fuel_lifecycle: u64,
    pub memory_bytes: usize,
    pub table_elements: usize,
    pub max_audio_pairs: usize,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            fuel_per_call: 500_000_000,
            fuel_lifecycle: 60_000_000_000,
            memory_bytes: 256 * 1024 * 1024,
            table_elements: 100_000,
            max_audio_pairs: 65_536,
        }
    }
}
impl Profile {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=60_000_000_000).contains(&self.fuel_per_call),
            "invalid per-call fuel"
        );
        ensure!(
            (1..=60_000_000_000).contains(&self.fuel_lifecycle),
            "invalid lifecycle fuel"
        );
        ensure!(
            (34 * 1024 * 1024..=512 * 1024 * 1024).contains(&self.memory_bytes),
            "invalid guest memory limit"
        );
        ensure!(
            (1..=1_000_000).contains(&self.table_elements),
            "invalid table limit"
        );
        ensure!(
            (1..=262_144).contains(&self.max_audio_pairs),
            "invalid audio safety limit"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Instrument {
    Reset,
    InputLatch,
    AudioDrain,
    FrameFormat,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub schema_version: u32,
    pub id: String,
    pub rom_sha256: String,
    pub prefix_frames: u32,
    pub probe_frames: u32,
    pub keys: u16,
    pub instruments: Vec<Instrument>,
    pub profile: Profile,
}
impl Case {
    pub fn new(rom: &[u8]) -> Self {
        Self {
            schema_version: VERSION,
            id: "abi-relations-v1".into(),
            rom_sha256: hash(rom),
            prefix_frames: 4,
            probe_frames: 2,
            keys: 1,
            instruments: vec![
                Instrument::Reset,
                Instrument::InputLatch,
                Instrument::AudioDrain,
                Instrument::FrameFormat,
            ],
            profile: Profile::default(),
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema_version == VERSION, "unsupported case version");
        ensure!(safe_id(&self.id), "unsafe case id");
        ensure!(is_hash(&self.rom_sha256), "invalid ROM hash");
        ensure!(
            (1..=120).contains(&self.prefix_frames),
            "prefix must be 1..120 frames"
        );
        ensure!(
            (2..=120).contains(&self.probe_frames),
            "probe must be 2..120 frames to exercise latching"
        );
        ensure!(
            (1..=1023).contains(&self.keys),
            "keys must use the documented ten-bit mapping"
        );
        ensure!(
            self.keys & 0x30 != 0x30 && self.keys & 0xc0 != 0xc0,
            "contradictory directions"
        );
        ensure!(
            !self.instruments.is_empty() && self.instruments.len() <= 4,
            "invalid instrument count"
        );
        let mut unique = self.instruments.clone();
        unique.sort();
        unique.dedup();
        ensure!(
            unique.len() == self.instruments.len(),
            "duplicate instrument"
        );
        self.profile.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Load,
    Reset,
    Keys { value: u16 },
    Frames { count: u32 },
    Frame { tag: String },
    Audio { tag: String },
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    Candidate,
    GuestFuel,
    SafetyLimit,
    Instrument,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    pub kind: FailureKind,
    pub operation: String,
    pub message: String,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.operation, self.message)
    }
}
impl std::error::Error for Failure {}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Call {
    pub name: String,
    pub arguments: Vec<i64>,
    pub returned: Option<String>,
    pub failure: Option<Failure>,
    pub fuel_before: u64,
    pub fuel_after: u64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub frame_le: Option<Vec<u8>>,
    pub pcm: Option<Vec<i16>>,
    pub sample_rate: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Trace {
    pub id: String,
    pub schedule: Vec<Action>,
    pub calls: Vec<Call>,
    pub captures: BTreeMap<String, Capture>,
    pub failure: Option<Failure>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
    Indeterminate,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Claim {
    Contract,
    Relational,
    Diagnostic,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Measurement {
    pub id: String,
    pub claim: Claim,
    pub verdict: Verdict,
    pub reason: String,
    pub evidence: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Observations {
    pub schema_version: u32,
    pub case: Case,
    pub candidate_sha256: String,
    pub subject_kind: String,
    pub driver_version: String,
    pub wasmtime_version: String,
    pub traces: Vec<Trace>,
}
impl Observations {
    pub fn validate(&self) -> Result<()> {
        self.case.validate()?;
        ensure!(
            self.schema_version == VERSION && is_hash(&self.candidate_sha256),
            "invalid observations identity"
        );
        ensure!(
            matches!(self.subject_kind.as_str(), "candidate" | "abi_fixture"),
            "unknown subject kind"
        );
        ensure!(
            self.driver_version == "0.1.0" && self.wasmtime_version == "27.0.0",
            "unsupported observation producer"
        );
        ensure!(
            !self.traces.is_empty() && self.traces.len() <= 10,
            "invalid trace count"
        );
        let mut ids = std::collections::BTreeSet::new();
        for t in &self.traces {
            ensure!(
                safe_id(&t.id) && ids.insert(&t.id),
                "duplicate/unsafe trace id"
            );
            ensure!(
                t.schedule.len() <= 260 && t.calls.len() <= 1024 && t.captures.len() <= 8,
                "trace exceeds bounds"
            );
            let mut scheduled_frames = 0_u32;
            let mut capture_tags = std::collections::BTreeSet::new();
            for action in &t.schedule {
                match action {
                    Action::Frames { count } => {
                        ensure!((1..=120).contains(count), "invalid scheduled frame count");
                        scheduled_frames = scheduled_frames
                            .checked_add(*count)
                            .ok_or_else(|| anyhow::anyhow!("frame count overflow"))?;
                        ensure!(scheduled_frames <= 240, "schedule exceeds frame budget");
                    }
                    Action::Keys { value } => ensure!(*value <= 1023, "unsupported key bits"),
                    Action::Frame { tag } | Action::Audio { tag } => {
                        ensure!(
                            safe_id(tag) && capture_tags.insert(tag),
                            "duplicate/unsafe capture tag"
                        );
                    }
                    _ => {}
                }
            }
            for call in &t.calls {
                ensure!(
                    call.returned.is_some() != call.failure.is_some(),
                    "call needs exactly one outcome"
                );
                ensure!(call.fuel_after <= call.fuel_before, "invalid fuel receipt");
            }
            let tags: BTreeMap<&str, bool> = t
                .schedule
                .iter()
                .filter_map(|a| match a {
                    Action::Frame { tag } => Some((tag.as_str(), true)),
                    Action::Audio { tag } => Some((tag.as_str(), false)),
                    _ => None,
                })
                .collect();
            for (id, c) in &t.captures {
                ensure!(
                    safe_id(id) && tags.contains_key(id.as_str()),
                    "unexpected/unsafe capture id"
                );
                ensure!(
                    tags[id.as_str()] == c.frame_le.is_some(),
                    "capture type disagrees with schedule"
                );
                ensure!(
                    c.frame_le.is_some() != c.pcm.is_some(),
                    "capture must contain exactly one observation kind"
                );
                if let Some(frame) = &c.frame_le {
                    ensure!(frame.len() == FRAME_BYTES, "incomplete frame");
                }
                if let Some(pcm) = &c.pcm {
                    ensure!(
                        pcm.len() % 2 == 0 && pcm.len() / 2 <= self.case.profile.max_audio_pairs,
                        "invalid stereo PCM"
                    );
                    ensure!(
                        c.sample_rate.is_some_and(|r| r > 0),
                        "missing/invalid audio rate"
                    );
                } else {
                    ensure!(c.sample_rate.is_none(), "rate without audio");
                }
                ensure!(c.frame_le.is_some() || c.pcm.is_some(), "empty capture");
            }
            if t.failure.is_none() {
                ensure!(
                    t.captures.len() == tags.len(),
                    "complete trace omits captures"
                );
                let mut expected = Vec::new();
                if t.calls.first().is_some_and(|c| c.name == "_initialize") {
                    expected.push("_initialize");
                }
                expected.push("emu_init");
                for action in &t.schedule {
                    match action {
                        Action::Load => expected.extend(["emu_rom_buffer", "emu_load_rom"]),
                        Action::Reset => expected.push("emu_reset"),
                        Action::Keys { .. } => expected.push("emu_set_keys"),
                        Action::Frames { count } => {
                            expected.extend(std::iter::repeat_n("emu_run_frame", *count as usize))
                        }
                        Action::Frame { .. } => expected.push("emu_framebuffer"),
                        Action::Audio { tag } => {
                            expected.push("emu_audio_samples");
                            if t.captures[tag].pcm.as_ref().is_some_and(|p| !p.is_empty()) {
                                expected.push("emu_audio_buffer");
                            }
                            expected.push("emu_audio_rate");
                        }
                    }
                }
                ensure!(
                    t.calls.iter().all(|c| c.failure.is_none()),
                    "complete trace contains a failed call"
                );
                ensure!(
                    t.calls
                        .iter()
                        .map(|c| c.name.as_str())
                        .eq(expected.into_iter()),
                    "complete trace has a missing/extra/reordered ABI call"
                );
            }
        }
        Ok(())
    }
}

pub fn hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
pub fn is_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
pub fn safe_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 80
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"-_".contains(&c))
}
pub fn read_bounded(path: &std::path::Path, max: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink(),
        "not a regular file: {}",
        path.display()
    );
    ensure!(meta.len() <= max, "file exceeds limit: {}", path.display());
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(max + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        bail!("file grew past limit");
    }
    Ok(bytes)
}
