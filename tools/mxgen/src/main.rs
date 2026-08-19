//! Generates ISO 20022 XML from MXMessage's datafake scenarios.
//!
//! MXMessage ships no XML at all: its corpus is 172 datafake scenario files, and
//! XML exists only as the return value of `MxMessage::to_xml()`. It also covers
//! 14 of quackiso's 29 readers, more than any other published source, so
//! scripts/sweep_foreign_corpora.py needs a generator to have anything to feed
//! them.
//!
//! Output is nondeterministic. datafake-rs 0.2.1 exposes no seed and its
//! iso8601_datetime operator calls Utc::now(), so two runs never produce the
//! same bytes. A file that trips a sweep rule cannot be regenerated, which is
//! why the sweep copies its findings out.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use datafake_rs::DataGenerator;
use mx_message::MxMessage;

const USAGE: &str = "usage: mxgen --scenarios <dir> --out <dir> [--per-scenario <n>]";

struct Args {
    scenarios: PathBuf,
    out: PathBuf,
    per_scenario: u32,
}

fn value(argv: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    argv.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn parse_args() -> Result<Args, String> {
    let mut scenarios: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut per_scenario: u32 = 1;

    let mut argv = std::env::args().skip(1);
    while let Some(flag) = argv.next() {
        match flag.as_str() {
            "--scenarios" => scenarios = Some(value(&mut argv, &flag)?.into()),
            "--out" => out = Some(value(&mut argv, &flag)?.into()),
            "--per-scenario" => {
                per_scenario = value(&mut argv, &flag)?
                    .parse()
                    .map_err(|error| format!("--per-scenario: {error}"))?;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }

    Ok(Args {
        scenarios: scenarios.ok_or_else(|| "--scenarios is required".to_string())?,
        out: out.ok_or_else(|| "--out is required".to_string())?,
        per_scenario,
    })
}

/// The `scenarios` array of one `index.json`. Two `.json` files upstream are
/// listed by no index at all, so the index decides what gets generated and the
/// directory listing does not.
fn listed_scenarios(index: &Path) -> Result<Vec<String>, String> {
    let source = fs::read_to_string(index).map_err(|error| format!("{error:?}"))?;
    let document: serde_json::Value =
        serde_json::from_str(&source).map_err(|error| format!("{error:?}"))?;
    let listed = document
        .get("scenarios")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "index has no scenarios array".to_string())?;
    Ok(listed
        .iter()
        .filter_map(|entry| entry.get("file")?.as_str().map(str::to_owned))
        .collect())
}

/// ISO 20022 allows an amount at most five fraction digits. datafake-rs hands
/// back raw f64 samples and `MxMessage::to_xml` writes them out at full float
/// precision, so 101 of the 170 scenarios carried something like
/// `37718.049892138275` and every reader that parses an amount refused the file.
/// Rounding the generated values puts the documents inside the schema the sweep
/// takes them to be in, which is what leaves a reader error meaning something.
/// Integers are untouched: `NbOfTxs` is a count.
fn round_to_iso_scale(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Number(number) => {
            if number.as_i64().is_some() || number.as_u64().is_some() {
                return;
            }
            if let Some(float) = number.as_f64() {
                let scaled = (float * 100_000.0).round() / 100_000.0;
                if let Some(rounded) = serde_json::Number::from_f64(scaled) {
                    *number = rounded;
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(round_to_iso_scale),
        serde_json::Value::Object(entries) => entries.values_mut().for_each(round_to_iso_scale),
        _ => {}
    }
}

/// Scenario JSON to XML. `MxError` does not implement `Display` for every
/// variant, so every step here formats with `{:?}`.
fn render(scenario: &Path) -> Result<String, String> {
    let source = fs::read_to_string(scenario).map_err(|error| format!("{error:?}"))?;
    let template: serde_json::Value =
        serde_json::from_str(&source).map_err(|error| format!("{error:?}"))?;
    let mut generated = DataGenerator::from_value(template)
        .map_err(|error| format!("{error:?}"))?
        .generate()
        .map_err(|error| format!("{error:?}"))?;
    round_to_iso_scale(&mut generated);
    let json = serde_json::to_string(&generated).map_err(|error| format!("{error:?}"))?;
    MxMessage::from_json(&json)
        .map_err(|error| format!("{error:?}"))?
        .to_xml()
        .map_err(|error| format!("{error:?}"))
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("mxgen: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    if let Err(error) = fs::create_dir_all(&args.out) {
        eprintln!("mxgen: {}: {error}", args.out.display());
        return ExitCode::from(2);
    }

    let entries = match fs::read_dir(&args.scenarios) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("mxgen: {}: {error}", args.scenarios.display());
            return ExitCode::from(2);
        }
    };

    let mut types: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join("index.json").is_file())
        .collect();
    types.sort();

    let mut scenarios = 0usize;
    let mut written = 0usize;
    let mut failed = 0usize;

    for directory in &types {
        let message_type = directory
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        let listed = match listed_scenarios(&directory.join("index.json")) {
            Ok(listed) => listed,
            Err(error) => {
                eprintln!("mxgen: {message_type}/index.json: {error}");
                failed += 1;
                continue;
            }
        };

        for file in listed {
            scenarios += 1;
            let stem = Path::new(&file)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();

            for run in 1..=args.per_scenario {
                match render(&directory.join(&file)) {
                    Ok(xml) => {
                        let target = args
                            .out
                            .join(format!("{message_type}__{stem}__{run:02}.xml"));
                        if let Err(error) = fs::write(&target, xml) {
                            eprintln!("mxgen: {message_type}/{file}: {error:?}");
                            failed += 1;
                        } else {
                            written += 1;
                        }
                    }
                    Err(error) => {
                        eprintln!("mxgen: {message_type}/{file}: {error}");
                        failed += 1;
                    }
                }
            }
        }
    }

    println!("mxgen: wrote {written} files from {scenarios} scenarios, {failed} failed");

    if written > 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
