use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const PLAN_SCHEMA: &str = "utterpipe.pocket-tts.xn-release-corpus/1";
const MANIFEST_SCHEMA: &str = "utterpipe.acoustic-manifest/1";
const SCORES_SCHEMA: &str = "utterpipe.pocket-tts.xn-utmos22-scores/1";
const PROVENANCE_SCHEMA: &str = "utterpipe.pocket-tts.xn-perceptual-provenance/1";
const MAX_JSON_BYTES: u64 = 1_048_576;
const MAX_CASES: usize = 128;

#[derive(Parser, Debug)]
#[command(
    name = "utterpipe-pocket-xn-perceptual-corpus",
    about = "Add pinned UTMOS perceptual evidence to an existing XN release corpus"
)]
struct Args {
    #[arg(long)]
    plan: PathBuf,
    #[arg(long)]
    manifest: PathBuf,
    #[arg(long)]
    python: PathBuf,
    #[arg(long)]
    scorer: PathBuf,
    #[arg(long)]
    speechmos_source: PathBuf,
    #[arg(long)]
    model: PathBuf,
    #[arg(long)]
    output_manifest: PathBuf,
    #[arg(long)]
    output_provenance: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusPlan {
    schema: String,
    source: CorpusSource,
    asr_policy: AsrPolicy,
    perceptual_policy: PerceptualPolicy,
    voices: Vec<String>,
    seeds: Vec<u64>,
    cases: Vec<CorpusCase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusSource {
    repository: String,
    revision: String,
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AsrPolicy {
    maximum_wer_delta: f64,
    maximum_cer_delta: f64,
    calibrated_positive_control: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PerceptualPolicy {
    id: String,
    repository: String,
    revision: String,
    model_source: String,
    model_sha256: String,
    minimum_case_improvement: f64,
    calibrated_positive_control: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusCase {
    id: String,
    tags: Vec<String>,
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScoreSet {
    schema: String,
    cases: Vec<CaseScore>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseScore {
    id: String,
    candidate: f64,
    baseline: f64,
}

fn main() -> Result<()> {
    let args = Args::parse();
    validate_args(&args)?;
    let plan: CorpusPlan = load_bounded_json(&args.plan, "--plan")?;
    validate_plan(&plan)?;
    verify_speechmos_checkout(&args.speechmos_source, &plan.perceptual_policy)?;
    let model_sha256 = format!("sha256:{}", sha256_file(&args.model)?);
    if model_sha256 != plan.perceptual_policy.model_sha256 {
        bail!("perceptual model SHA-256 does not match the release plan");
    }

    let mut manifest: Value = load_bounded_json(&args.manifest, "--manifest")?;
    let expected_ids = expected_case_ids(&plan);
    validate_manifest(&manifest, &expected_ids)?;
    let scores = run_scorer(&args)?;
    let scores = validate_scores(scores, &expected_ids)?;
    annotate_manifest(&mut manifest, &plan.perceptual_policy, &scores)?;

    let scorer_sha256 = format!("sha256:{}", sha256_file(&args.scorer)?);
    let python_version = python_version(&args.python)?;
    let provenance = json!({
        "schema":PROVENANCE_SCHEMA,
        "metric":{
            "id":plan.perceptual_policy.id,
            "repository":plan.perceptual_policy.repository,
            "revision":plan.perceptual_policy.revision,
            "model_source":plan.perceptual_policy.model_source,
            "model_sha256":model_sha256,
            "direction":"higher_is_better"
        },
        "scorer_sha256":scorer_sha256,
        "python_version":python_version,
        "case_count":scores.len(),
        "policy":{
            "minimum_case_improvement":plan.perceptual_policy.minimum_case_improvement,
            "calibrated_positive_control":plan.perceptual_policy.calibrated_positive_control
        }
    });
    persist_json(&args.output_provenance, &provenance)?;
    if let Err(error) = persist_json(&args.output_manifest, &manifest) {
        let _ = fs::remove_file(&args.output_provenance);
        return Err(error);
    }
    eprintln!("annotated {} perceptual cases", scores.len());
    Ok(())
}

fn validate_args(args: &Args) -> Result<()> {
    for (label, path) in [
        ("--plan", &args.plan),
        ("--manifest", &args.manifest),
        ("--scorer", &args.scorer),
        ("--model", &args.model),
    ] {
        require_absolute_regular(label, path)?;
    }
    if !args.python.is_absolute()
        || !fs::metadata(&args.python).is_ok_and(|metadata| metadata.is_file())
    {
        bail!("--python must resolve from an absolute path to an existing file");
    }
    if !args.speechmos_source.is_absolute() || !args.speechmos_source.is_dir() {
        bail!("--speechmos-source must be an existing absolute directory");
    }
    for (label, path) in [
        ("--output-manifest", &args.output_manifest),
        ("--output-provenance", &args.output_provenance),
    ] {
        if !path.is_absolute() || path.exists() || !path.parent().is_some_and(Path::is_dir) {
            bail!("{label} must be an absent absolute path with an existing parent");
        }
    }
    if args.output_manifest == args.output_provenance {
        bail!("perceptual manifest and provenance outputs must differ");
    }
    Ok(())
}

fn validate_plan(plan: &CorpusPlan) -> Result<()> {
    if plan.schema != PLAN_SCHEMA
        || plan.source.repository.is_empty()
        || plan.source.revision.is_empty()
        || plan.source.path.is_empty()
        || !plan.asr_policy.maximum_wer_delta.is_finite()
        || !plan.asr_policy.maximum_cer_delta.is_finite()
        || plan.asr_policy.calibrated_positive_control.is_empty()
        || plan.perceptual_policy.id != "utmos22-strong"
        || plan.perceptual_policy.repository != "tarepan/SpeechMOS"
        || !valid_id(&plan.perceptual_policy.revision)
        || plan.perceptual_policy.model_source.is_empty()
        || !valid_sha256(&plan.perceptual_policy.model_sha256)
        || !plan.perceptual_policy.minimum_case_improvement.is_finite()
        || !valid_id(&plan.perceptual_policy.calibrated_positive_control)
        || plan.voices.is_empty()
        || plan.seeds.is_empty()
        || plan.cases.is_empty()
        || plan.voices.len() * plan.seeds.len() * plan.cases.len() > MAX_CASES
    {
        bail!("release corpus plan is invalid for perceptual evaluation");
    }
    for case in &plan.cases {
        if !valid_id(&case.id) || case.tags.is_empty() || case.text.is_empty() {
            bail!("release corpus case is invalid");
        }
    }
    Ok(())
}

fn expected_case_ids(plan: &CorpusPlan) -> BTreeSet<String> {
    plan.voices
        .iter()
        .flat_map(|voice| {
            plan.seeds.iter().flat_map(move |seed| {
                plan.cases
                    .iter()
                    .map(move |case| format!("{}-{voice}-seed{seed}", case.id))
            })
        })
        .collect()
}

fn validate_manifest(manifest: &Value, expected: &BTreeSet<String>) -> Result<()> {
    if manifest["schema"] != MANIFEST_SCHEMA || manifest.get("perceptual_metric").is_some() {
        bail!("input must be an unannotated acoustic manifest");
    }
    let cases = manifest["cases"]
        .as_array()
        .context("acoustic manifest cases must be an array")?;
    let ids = cases
        .iter()
        .map(|case| {
            if case.get("perceptual_scores").is_some() {
                bail!("input manifest already contains perceptual scores");
            }
            Ok(case["id"]
                .as_str()
                .context("manifest case ID is missing")?
                .to_owned())
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if ids != *expected || cases.len() != expected.len() {
        bail!("acoustic manifest does not exactly match the release plan");
    }
    Ok(())
}

fn run_scorer(args: &Args) -> Result<ScoreSet> {
    let output = Command::new(&args.python)
        .arg(&args.scorer)
        .args(["--manifest"])
        .arg(&args.manifest)
        .args(["--speechmos-source"])
        .arg(&args.speechmos_source)
        .args(["--model"])
        .arg(&args.model)
        .output()?;
    if !output.status.success() {
        bail!("perceptual scorer failed with status {}", output.status);
    }
    if output.stdout.len() as u64 > MAX_JSON_BYTES {
        bail!("perceptual scorer output exceeded 1 MiB");
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn validate_scores(
    scores: ScoreSet,
    expected: &BTreeSet<String>,
) -> Result<BTreeMap<String, (f64, f64)>> {
    if scores.schema != SCORES_SCHEMA || scores.cases.len() != expected.len() {
        bail!("perceptual scorer returned an unexpected result set");
    }
    let mut mapped = BTreeMap::new();
    for score in scores.cases {
        if !expected.contains(&score.id)
            || !score.candidate.is_finite()
            || !score.baseline.is_finite()
            || mapped
                .insert(score.id, (score.candidate, score.baseline))
                .is_some()
        {
            bail!("perceptual scorer returned an invalid case score");
        }
    }
    Ok(mapped)
}

fn annotate_manifest(
    manifest: &mut Value,
    policy: &PerceptualPolicy,
    scores: &BTreeMap<String, (f64, f64)>,
) -> Result<()> {
    manifest
        .as_object_mut()
        .context("manifest must be an object")?
        .insert(
            "perceptual_metric".to_owned(),
            json!({
                "id":policy.id,
                "revision":policy.revision,
                "model_sha256":policy.model_sha256,
                "direction":"higher_is_better"
            }),
        );
    manifest["requirements"]
        .as_object_mut()
        .context("manifest requirements must be an object")?
        .insert("require_perceptual_metric".to_owned(), json!(true));
    manifest["policy"]
        .as_object_mut()
        .context("manifest policy must be an object")?
        .insert(
            "minimum_perceptual_case_improvement".to_owned(),
            json!(policy.minimum_case_improvement),
        );
    for case in manifest["cases"]
        .as_array_mut()
        .context("manifest cases must be an array")?
    {
        let id = case["id"].as_str().context("manifest case ID is missing")?;
        let (candidate, baseline) = scores.get(id).context("perceptual score disappeared")?;
        case.as_object_mut()
            .context("manifest case must be an object")?
            .insert(
                "perceptual_scores".to_owned(),
                json!({"candidate":candidate,"baseline":baseline}),
            );
    }
    Ok(())
}

fn verify_speechmos_checkout(path: &Path, policy: &PerceptualPolicy) -> Result<()> {
    let revision = git_output(path, &["rev-parse", "HEAD"])?;
    if revision != policy.revision {
        bail!("SpeechMOS checkout revision does not match the release plan");
    }
    if !git_output(path, &["status", "--porcelain"])?.is_empty() {
        bail!("SpeechMOS checkout must be clean, including untracked files");
    }
    Ok(())
}

fn git_output(path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()?;
    if !output.status.success() || !output.stderr.is_empty() || output.stdout.len() > 4096 {
        bail!("could not verify the pinned SpeechMOS checkout");
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn python_version(python: &Path) -> Result<String> {
    let output = Command::new(python).arg("--version").output()?;
    let bytes = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    let value = String::from_utf8(bytes.clone())?.trim().to_owned();
    if !output.status.success()
        || value.is_empty()
        || value.len() > 80
        || value.chars().any(char::is_control)
    {
        bail!("could not determine a bounded Python version");
    }
    Ok(value)
}

fn load_bounded_json<T: serde::de::DeserializeOwned>(path: &Path, label: &str) -> Result<T> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_JSON_BYTES {
        bail!("{label} must be a bounded regular JSON file");
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn require_absolute_regular(label: &str, path: &Path) -> Result<()> {
    if !path.is_absolute()
        || !fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
    {
        bail!("{label} must be an existing absolute regular file");
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut input = File::open(path)?;
    let mut hasher = Sha256::new();
    io::copy(&mut input.by_ref().take(u64::MAX), &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn persist_json(path: &Path, value: &Value) -> Result<()> {
    let parent = path.parent().context("output must have a parent")?;
    let mut temp = tempfile::Builder::new()
        .prefix(".xn-perceptual-json-")
        .tempfile_in(parent)?;
    serde_json::to_writer_pretty(temp.as_file_mut(), value)?;
    temp.persist_noclobber(path)?;
    Ok(())
}

fn valid_id(value: &str) -> bool {
    (1..=80).contains(&value.len())
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && b"._-".contains(&byte))
        })
}

fn valid_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores_must_exactly_cover_expected_cases() {
        let expected = BTreeSet::from(["one".to_owned()]);
        let scores = ScoreSet {
            schema: SCORES_SCHEMA.to_owned(),
            cases: vec![CaseScore {
                id: "one".to_owned(),
                candidate: 4.0,
                baseline: 3.0,
            }],
        };
        assert_eq!(
            validate_scores(scores, &expected).unwrap()["one"],
            (4.0, 3.0)
        );
    }
}
