//! Generates SWIFT MT text from swift-mt-message's datafake scenarios.
//!
//! swift-mt-message ships no MT text at all: its corpus is 74 datafake scenario
//! files across the four types quackiso reads, and a message exists only as the
//! return value of `SwiftMessage::to_mt_message()`, which writes the whole
//! `{1:}`..`{5:}` envelope. The published MT corpora - wolph/mt940 and
//! prowide-core - are real bank traffic and cover the readers unevenly, so this
//! is the tier that hands them a message per scenario instead.
//!
//! Output is nondeterministic, for the same reason mxgen's is: datafake-rs 0.2
//! exposes no seed, and the bic8, uuid and date operators produce different bytes
//! every run. A file that trips a sweep rule cannot be regenerated, which is why
//! the sweep copies its findings out.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use swift_mt_message::messages::{MT101, MT103, MT104, MT202, MT940, MT942};
use swift_mt_message::{SampleGenerator, ScenarioConfig};

const USAGE: &str = "usage: mtgen --scenarios <dir> --out <dir> [--per-scenario <n>]";

/// The scenario directories worth generating: the five types quackiso reads. The
/// crate ships thirty, and a message with no reader behind it would only be a
/// file the sweep routes nowhere.
const TYPES: [&str; 6] = ["mt101", "mt103", "mt104", "mt202", "mt940", "mt942"];

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

/// The `scenarios` array of one `index.json`. The index decides what gets
/// generated and the directory listing does not, the same rule mxgen follows.
fn listed_scenarios(index: &Path) -> Result<Vec<String>, String> {
    let source = fs::read_to_string(index).map_err(|error| format!("{error}"))?;
    let document: serde_json::Value =
        serde_json::from_str(&source).map_err(|error| format!("{error}"))?;
    let listed = document
        .get("scenarios")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "index has no scenarios array".to_string())?;
    Ok(listed
        .iter()
        .filter_map(|entry| entry.get("file")?.as_str().map(str::to_owned))
        .collect())
}

/// One scenario to one MT message. The type parameter has to be concrete, so the
/// arms are the dispatch: `generate` deserializes the generated JSON into
/// `SwiftMessage<T>`, and only `T` knows which fields that is.
///
/// `stem` and not the file name: the generator joins `{stem}.json` itself.
fn render(generator: &SampleGenerator, message_type: &str, stem: &str) -> Result<String, String> {
    let text = match message_type {
        "mt101" => generator
            .generate::<MT101>(message_type, Some(stem))
            .map(|message| message.to_mt_message()),
        "mt103" => generator
            .generate::<MT103>(message_type, Some(stem))
            .map(|message| message.to_mt_message()),
        "mt104" => generator
            .generate::<MT104>(message_type, Some(stem))
            .map(|message| message.to_mt_message()),
        "mt202" => generator
            .generate::<MT202>(message_type, Some(stem))
            .map(|message| message.to_mt_message()),
        "mt940" => generator
            .generate::<MT940>(message_type, Some(stem))
            .map(|message| message.to_mt_message()),
        "mt942" => generator
            .generate::<MT942>(message_type, Some(stem))
            .map(|message| message.to_mt_message()),
        other => return Err(format!("no reader covers {other}")),
    };
    text.map_err(|error| error.to_string())
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("mtgen: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    if let Err(error) = fs::create_dir_all(&args.out) {
        eprintln!("mtgen: {}: {error}", args.out.display());
        return ExitCode::from(2);
    }

    if !args.scenarios.is_dir() {
        eprintln!("mtgen: {}: not a directory", args.scenarios.display());
        return ExitCode::from(2);
    }

    // with_paths and not with_path: the latter appends, leaving the crate's own
    // `test_scenarios` and `../test_scenarios` ahead of the pinned corpus, and
    // $SWIFT_SCENARIO_PATH ahead of both.
    let generator =
        SampleGenerator::with_config(ScenarioConfig::with_paths(vec![args.scenarios.clone()]));

    let mut scenarios = 0usize;
    let mut written = 0usize;
    let mut failed = 0usize;

    for message_type in TYPES {
        let directory = args.scenarios.join(message_type);
        let listed = match listed_scenarios(&directory.join("index.json")) {
            Ok(listed) => listed,
            Err(error) => {
                eprintln!("mtgen: {message_type}/index.json: {error}");
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
                match render(&generator, message_type, &stem) {
                    Ok(text) => {
                        let target = args
                            .out
                            .join(format!("{message_type}__{stem}__{run:02}.txt"));
                        if let Err(error) = fs::write(&target, text) {
                            eprintln!("mtgen: {message_type}/{file}: {error}");
                            failed += 1;
                        } else {
                            written += 1;
                        }
                    }
                    Err(error) => {
                        eprintln!("mtgen: {message_type}/{file}: {error}");
                        failed += 1;
                    }
                }
            }
        }
    }

    println!("mtgen: wrote {written} files from {scenarios} scenarios, {failed} failed");

    if written > 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
