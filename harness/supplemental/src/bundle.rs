//! New-directory, seal-last evidence bundles. Hashes are integrity, not attestation.
use crate::{measure::{analyze, markdown}, model::*};
use anyhow::{ensure, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{collections::{BTreeMap, BTreeSet}, fs::{self, OpenOptions}, io::Write, path::Path};

const FILE_LIMIT: u64 = 128 * 1024 * 1024;
const TOTAL_LIMIT: u64 = 256 * 1024 * 1024;
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry { sha256: String, bytes: u64 }
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest { schema_version: u32, baseline_commit: String, files: BTreeMap<String, Entry> }

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(bytes)?; file.sync_all()?; Ok(())
}
fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>> { Ok(serde_json::to_vec(value)?) }
fn decode<T: DeserializeOwned + Serialize>(bytes: &[u8]) -> Result<T> {
    let value: T = serde_json::from_slice(bytes)?;
    ensure!(canonical(&value)? == bytes, "bundle JSON is not in its canonical producer format (including duplicate/unknown keys)");
    Ok(value)
}

pub fn write_bundle(out: &Path, candidate: &[u8], rom: &[u8], observed: &Observations, upstream: Option<&[u8]>) -> Result<String> {
    observed.validate()?;
    ensure!(hash(candidate) == observed.candidate_sha256 && hash(rom) == observed.case.rom_sha256, "input identity mismatch");
    let measures = analyze(observed)?;
    let mut files = BTreeMap::new();
    files.insert("candidate.wasm".to_string(), candidate.to_vec());
    files.insert("rom.gba".to_string(), rom.to_vec());
    files.insert("case.json".to_string(), canonical(&observed.case)?);
    files.insert("observations.json".to_string(), canonical(observed)?);
    files.insert("measurements.json".to_string(), canonical(&measures)?);
    let mut report = markdown(observed, &measures);
    if let Some(raw) = upstream {
        let value = validate_upstream(raw, &observed.candidate_sha256)?;
        report.push_str(&format!("\n## Imported upstream output\n\nOriginal reported overall: `{value}`. Raw bytes are preserved in `upstream-summary.json`. This attachment alone does **not** establish baseline parity, complete corpus execution, or comparable agent conditions.\n"));
        files.insert("upstream-summary.json".into(), raw.to_vec());
    } else {
        report.push_str("\n## Upstream output\n\nNot attached. No upstream score was computed by the supplemental driver.\n");
    }
    files.insert("report.md".into(), report.into_bytes());
    ensure!(files.values().all(|v| v.len() as u64 <= FILE_LIMIT), "artifact too large");
    ensure!(files.values().map(|v| v.len() as u64).sum::<u64>() <= TOTAL_LIMIT, "bundle too large");
    // Atomic reservation: no replacement of an existing, even incomplete, run.
    fs::create_dir(out).with_context(|| format!("reserve new output directory {}", out.display()))?;
    let mut manifest = Manifest { schema_version: VERSION, baseline_commit: UPSTREAM.into(), files: BTreeMap::new() };
    for (name, bytes) in files {
        write_new(&out.join(&name), &bytes)?;
        manifest.files.insert(name, Entry { sha256: hash(&bytes), bytes: bytes.len() as u64 });
    }
    let bytes = canonical(&manifest)?;
    write_new(&out.join("manifest.json"), &bytes)?;
    let seal = hash(&bytes);
    // The only completeness marker. Interrupted earlier writes leave no seal.
    write_new(&out.join("seal.sha256"), format!("{seal}\n").as_bytes())?;
    #[cfg(unix)]
    fs::File::open(out)?.sync_all()?;
    Ok(seal)
}

fn validate_upstream(raw: &[u8], candidate_sha: &str) -> Result<f64> {
    ensure!(raw.len() <= 4 * 1024 * 1024, "upstream summary exceeds limit");
    let v: serde_json::Value = serde_json::from_slice(raw)?;
    ensure!(v.get("candidate_sha256").and_then(|s| s.as_str()) == Some(candidate_sha), "upstream summary belongs to a different candidate");
    let overall = v.get("overall").and_then(|n| n.as_f64()).context("missing original overall score")?;
    ensure!(overall.is_finite() && (0.0..=1.0).contains(&overall), "invalid original overall score");
    Ok(overall)
}

pub fn validate_bundle(root: &Path, expected_seal: Option<&str>) -> Result<(Observations, Vec<Measurement>, String)> {
    let metadata = fs::symlink_metadata(root)?;
    ensure!(metadata.is_dir() && !metadata.file_type().is_symlink(), "bundle root must be a real directory");
    let manifest_bytes = read_bounded(&root.join("manifest.json"), 64 * 1024)?;
    let seal = hash(&manifest_bytes);
    let recorded = read_bounded(&root.join("seal.sha256"), 65)?;
    ensure!(recorded == format!("{seal}\n").as_bytes(), "missing/invalid completeness seal");
    if let Some(expected) = expected_seal { ensure!(expected == seal, "bundle differs from externally recorded seal"); }
    let manifest: Manifest = decode(&manifest_bytes)?;
    ensure!(manifest.schema_version == VERSION && manifest.baseline_commit == UPSTREAM, "unsupported manifest identity");
    let required = ["candidate.wasm", "rom.gba", "case.json", "observations.json", "measurements.json", "report.md"];
    let allowed: BTreeSet<&str> = required.iter().copied().chain(["upstream-summary.json"]).collect();
    ensure!(required.iter().all(|p| manifest.files.contains_key(*p)), "manifest omits required artifacts");
    ensure!(manifest.files.keys().all(|p| allowed.contains(p.as_str())), "unknown/unsafe artifact path");
    let expected: BTreeSet<String> = manifest.files.keys().cloned().chain(["manifest.json".into(),"seal.sha256".into()]).collect();
    let actual: BTreeSet<String> = fs::read_dir(root)?.map(|e| Ok(e?.file_name().into_string().map_err(|_| anyhow::anyhow!("non-UTF8 artifact name"))?)).collect::<Result<_>>()?;
    ensure!(actual == expected, "unexpected or missing bundle entries");
    let mut total = 0_u64;
    let mut data = BTreeMap::new();
    for (name, entry) in &manifest.files {
        ensure!(entry.bytes <= FILE_LIMIT && is_hash(&entry.sha256), "invalid artifact declaration");
        total = total.checked_add(entry.bytes).context("size overflow")?;
        ensure!(total <= TOTAL_LIMIT, "bundle exceeds bound");
        let bytes = read_bounded(&root.join(name), entry.bytes)?;
        ensure!(bytes.len() as u64 == entry.bytes && hash(&bytes) == entry.sha256, "artifact integrity failure: {name}");
        data.insert(name.as_str(), bytes);
    }
    let observed: Observations = decode(&data["observations.json"])?;
    observed.validate()?;
    let case: Case = decode(&data["case.json"])?;
    ensure!(case == observed.case, "case/observations mismatch");
    ensure!(hash(&data["candidate.wasm"]) == observed.candidate_sha256, "candidate identity mismatch");
    ensure!(hash(&data["rom.gba"]) == case.rom_sha256, "ROM identity mismatch");
    let measured: Vec<Measurement> = decode(&data["measurements.json"])?;
    ensure!(analyze(&observed)? == measured, "saved measurements do not follow the recorded evidence and rule version");
    if let Some(upstream) = data.get("upstream-summary.json") { validate_upstream(upstream, &observed.candidate_sha256)?; }
    Ok((observed, measured, seal))
}
