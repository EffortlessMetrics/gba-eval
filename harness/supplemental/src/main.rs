use anyhow::{bail, ensure, Context, Result};
use gba_supplemental::{bundle, fixtures, measure, model::*};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

fn usage() -> &'static str {
    "gba-supplemental commands:\n  case --rom ROM\n  counter-rom OUTPUT\n  fixture NAME OUTPUT.wasm\n  run --candidate WASM --rom ROM --case CASE.json --out NEW_DIR [--upstream-summary SUMMARY.json]\n  qualify NEW_DIR\n  verify BUNDLE [EXPECTED_SEAL]\n  report BUNDLE\n  reanalyze BUNDLE NEW_JSON\n\nThe binary bounds guest resources. Use the documented container wrapper to bound the host and deny network access. Provision/build separately."
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new().create_new(true).write(true).open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}
fn flags(args: &[String], allowed: &[&str]) -> Result<BTreeMap<String, PathBuf>> {
    ensure!(args.len() % 2 == 0, "flags require values");
    let mut out = BTreeMap::new();
    for pair in args.chunks_exact(2) {
        ensure!(
            allowed.contains(&pair[0].as_str()),
            "unknown flag {}",
            pair[0]
        );
        ensure!(
            out.insert(pair[0].clone(), PathBuf::from(&pair[1]))
                .is_none(),
            "duplicate flag {}",
            pair[0]
        );
    }
    Ok(out)
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e:#}");
        std::process::exit(2);
    }
}
fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        bail!("{}", usage());
    };
    match command.as_str() {
        "--help" | "help" => {
            println!("{}", usage());
        }
        "counter-rom" => {
            ensure!(args.len() == 2, "{}", usage());
            let bytes = fixtures::counter_rom();
            write_new(Path::new(&args[1]), &bytes)?;
            println!("{}", hash(&bytes));
        }
        "fixture" => {
            ensure!(args.len() == 3, "{}", usage());
            let source = fixtures::source(&args[1])?;
            let bytes = wat::parse_str(&source)?;
            write_new(Path::new(&args[2]), &bytes)?;
            println!("{}", hash(&bytes));
        }
        "case" => {
            let f = flags(&args[1..], &["--rom"])?;
            let bytes = read_bounded(f.get("--rom").context("--rom required")?, MAX_ROM as u64)?;
            ensure!(!bytes.is_empty(), "empty ROM");
            println!("{}", serde_json::to_string_pretty(&Case::new(&bytes))?);
        }
        "run" => {
            let f = flags(
                &args[1..],
                &[
                    "--candidate",
                    "--rom",
                    "--case",
                    "--out",
                    "--upstream-summary",
                ],
            )?;
            let candidate = read_bounded(
                f.get("--candidate").context("--candidate required")?,
                MAX_MODULE as u64,
            )?;
            let rom = read_bounded(f.get("--rom").context("--rom required")?, MAX_ROM as u64)?;
            let case_bytes = read_bounded(f.get("--case").context("--case required")?, 65536)?;
            let case: Case = serde_json::from_slice(&case_bytes)?;
            let out = f.get("--out").context("--out required")?;
            let upstream = f
                .get("--upstream-summary")
                .map(|p| read_bounded(p, 4 * 1024 * 1024))
                .transpose()?;
            let observed = measure::execute(case, &candidate, &rom, "candidate")?;
            let seal = bundle::write_bundle(out, &candidate, &rom, &observed, upstream.as_deref())?;
            println!("bundle={} seal={seal}", out.display());
            println!("Completed capture. Inspect measurements.json for behavioral failures and indeterminate cases; exit success is not emulator correctness.");
        }
        "qualify" => {
            ensure!(args.len() == 2, "{}", usage());
            let matched = fixtures::qualify(Path::new(&args[1]))?;
            ensure!(matched, "qualification controls disagreed with their declared expectations; all results retained");
            println!("All {} named ABI-fixture controls matched their declared expectations. Not a general emulator qualification.", fixtures::VARIANTS.len());
        }
        "verify" => {
            ensure!((2..=3).contains(&args.len()), "{}", usage());
            let (_, _, seal) =
                bundle::validate_bundle(Path::new(&args[1]), args.get(2).map(String::as_str))?;
            println!("valid bundle seal={seal}");
        }
        "report" => {
            ensure!(args.len() == 2, "{}", usage());
            bundle::validate_bundle(Path::new(&args[1]), None)?;
            let report = read_bounded(&Path::new(&args[1]).join("report.md"), 1024 * 1024)?;
            print!("{}", std::str::from_utf8(&report)?);
        }
        "reanalyze" => {
            ensure!(args.len() == 3, "{}", usage());
            let (o, _, seal) = bundle::validate_bundle(Path::new(&args[1]), None)?;
            let data = serde_json::json!({"schema_version":VERSION,"source_seal":seal,"rule_version":1,"measurements":measure::analyze(&o)?});
            write_new(Path::new(&args[2]), &serde_json::to_vec_pretty(&data)?)?;
        }
        _ => bail!("unknown command {command}\n{}", usage()),
    }
    Ok(())
}
