use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::Deserialize;
use serde_json::{Value, json};

const PLAN_SCHEMA: &str = "utterpipe.pocket-tts.xn-release-corpus/1";
const ACOUSTIC_SCHEMA: &str = "utterpipe.acoustic-manifest/1";
const MAX_PLAN_BYTES: u64 = 1_048_576;
const MAX_CASES: usize = 128;
const MAX_TEXT_CODE_POINTS: usize = 10_000;
const OUTPUT_GAIN: f64 = 0.65;

#[derive(Parser, Debug)]
#[command(
    name = "utterpipe-pocket-xn-release-corpus",
    about = "Generate a reproducible XN Q8/f32 acoustic release corpus"
)]
struct Args {
    /// Strict release-corpus JSON plan.
    #[arg(long)]
    plan: PathBuf,
    /// Provider-neutral utterpipe-benchmark executable with --output-wav.
    #[arg(long)]
    benchmark: PathBuf,
    /// Pocket TTS provider executable containing the XN adapter.
    #[arg(long)]
    provider: PathBuf,
    /// This package's utterpipe-pocket-xn-eval executable.
    #[arg(long)]
    evaluator: PathBuf,
    /// Installed provider data root containing the Q8 bundle and prepared voices.
    #[arg(long)]
    data_dir: PathBuf,
    /// Installed provider cache root paired with --data-dir.
    #[arg(long)]
    cache_dir: PathBuf,
    /// Official April model config used by the f32 control.
    #[arg(long)]
    config: PathBuf,
    /// Authorized official April f32 safetensors used by the control.
    #[arg(long)]
    fp32_weights: PathBuf,
    /// SentencePiece tokenizer matching the April model.
    #[arg(long)]
    tokenizer: PathBuf,
    /// VOICE_ID=PATH mapping to an XN-prepared voice state; repeat per plan voice.
    #[arg(long, value_name = "VOICE_ID=PATH")]
    voice_state: Vec<String>,
    /// New destination directory. Existing paths are never overwritten.
    #[arg(long)]
    output: PathBuf,
    /// XN worker threads for candidate and f32 control.
    #[arg(long, default_value_t = 4)]
    threads: u32,
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
struct CorpusSource {
    repository: String,
    revision: String,
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusCase {
    id: String,
    tags: Vec<String>,
    text: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let plan = load_plan(&args.plan)?;
    let voice_states = validate_args(&args, &plan)?;
    let parent = args
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .context("--output must have an existing parent")?;
    let staging = tempfile::Builder::new()
        .prefix(".xn-release-corpus-")
        .tempdir_in(parent)?;
    fs::create_dir(staging.path().join("audio"))?;
    fs::create_dir(staging.path().join("reports"))?;

    let mut acoustic_cases =
        Vec::with_capacity(plan.voices.len() * plan.seeds.len() * plan.cases.len());
    let provider_options = |voice: &str| {
        serde_json::to_string(&json!({
            "model":"pocket-tts-english-2026-04-q8",
            "voice":voice,
            "num_threads":args.threads
        }))
    };

    for voice in &plan.voices {
        let voice_state = voice_states
            .get(voice)
            .context("validated voice state mapping disappeared")?;
        for &seed in &plan.seeds {
            for case in &plan.cases {
                let id = format!("{}-{voice}-seed{seed}", case.id);
                eprintln!("generating {id}");
                let candidate_a = format!("audio/{id}-candidate-a.wav");
                let candidate_b = format!("audio/{id}-candidate-b.wav");
                let baseline = format!("audio/{id}-baseline.wav");
                let candidate_a_report = format!("reports/{id}-candidate-a.json");
                let candidate_b_report = format!("reports/{id}-candidate-b.json");
                let baseline_report = format!("reports/{id}-baseline.json");
                let utterance_options = serde_json::to_string(&json!({"seed":seed}))?;
                let provider_options = provider_options(voice)?;

                for (wave, report) in [
                    (&candidate_a, &candidate_a_report),
                    (&candidate_b, &candidate_b_report),
                ] {
                    let output = run_candidate(
                        &args,
                        &case.text,
                        &provider_options,
                        &utterance_options,
                        &staging.path().join(wave),
                    )?;
                    validate_candidate_report(&output.stdout)?;
                    fs::write(staging.path().join(report), output.stdout)?;
                }

                let output = run_baseline(
                    &args,
                    &case.text,
                    seed,
                    voice_state,
                    &staging.path().join(&baseline),
                )?;
                validate_baseline_report(&output.stdout, seed)?;
                fs::write(staging.path().join(&baseline_report), output.stdout)?;

                acoustic_cases.push(json!({
                    "id":id,
                    "voice_id":voice,
                    "seed":seed,
                    "tags":case.tags,
                    "candidate_wavs":[candidate_a,candidate_b],
                    "baseline_wavs":[baseline],
                    "sample_aligned":false
                }));
            }
        }
    }

    let required_tags: BTreeSet<_> = plan
        .cases
        .iter()
        .flat_map(|case| case.tags.iter().cloned())
        .collect();
    let manifest = json!({
        "schema":ACOUSTIC_SCHEMA,
            "candidate":{"id":"xn-april-q8-provider-gain065"},
            "baseline":{"id":"xn-april-fp32-control-gain065"},
        "requirements":{
            "minimum_cases":acoustic_cases.len(),
            "minimum_voices":plan.voices.len(),
            "minimum_seeds":plan.seeds.len(),
            "required_tags":required_tags,
            "candidate_replays_per_case":2,
            "require_transcripts":false,
            "require_sample_aligned_snr":false,
            "require_perceptual_metric":false
        },
        "policy":{
            "maximum_peak_fraction":0.95,
            "maximum_clipped_sample_fraction":0.0
        },
        "cases":acoustic_cases
    });
    fs::write(
        staging.path().join("acoustic-manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    fs::write(
        staging.path().join("corpus-provenance.json"),
        serde_json::to_vec_pretty(&json!({
            "schema":PLAN_SCHEMA,
            "source":{
                "repository":plan.source.repository,
                "revision":plan.source.revision,
                "path":plan.source.path
            },
            "case_count":manifest["cases"].as_array().map_or(0, Vec::len),
            "candidate_replays_per_case":2,
            "output_gain":OUTPUT_GAIN
        }))?,
    )?;

    let staging_path = staging.keep();
    fs::rename(staging_path, &args.output)?;
    eprintln!(
        "generated {} cases",
        manifest["cases"].as_array().map_or(0, Vec::len)
    );
    Ok(())
}

fn load_plan(path: &Path) -> Result<CorpusPlan> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_PLAN_BYTES {
        bail!("--plan must be a bounded regular file");
    }
    let plan: CorpusPlan = serde_json::from_slice(&fs::read(path)?)?;
    if plan.schema != PLAN_SCHEMA {
        bail!("unsupported release corpus schema");
    }
    Ok(plan)
}

fn validate_args(args: &Args, plan: &CorpusPlan) -> Result<BTreeMap<String, PathBuf>> {
    for (label, path) in [
        ("--plan", &args.plan),
        ("--benchmark", &args.benchmark),
        ("--provider", &args.provider),
        ("--evaluator", &args.evaluator),
        ("--config", &args.config),
        ("--fp32-weights", &args.fp32_weights),
        ("--tokenizer", &args.tokenizer),
    ] {
        require_absolute_regular(label, path)?;
    }
    for (label, path) in [
        ("--data-dir", &args.data_dir),
        ("--cache-dir", &args.cache_dir),
    ] {
        if !path.is_absolute() || !path.is_dir() {
            bail!("{label} must be an existing absolute directory");
        }
    }
    if !args.output.is_absolute() || args.output.exists() {
        bail!("--output must be an absent absolute path");
    }
    if !(1..=256).contains(&args.threads) {
        bail!("--threads must be in 1..=256");
    }
    if plan.voices.is_empty() || plan.seeds.is_empty() || plan.cases.is_empty() {
        bail!("release corpus dimensions must not be empty");
    }
    if !plan.asr_policy.maximum_wer_delta.is_finite()
        || plan.asr_policy.maximum_wer_delta < 0.0
        || !plan.asr_policy.maximum_cer_delta.is_finite()
        || plan.asr_policy.maximum_cer_delta < 0.0
        || !valid_id(&plan.asr_policy.calibrated_positive_control)
    {
        bail!("release corpus ASR policy is invalid");
    }
    if !valid_id(&plan.perceptual_policy.id)
        || plan.perceptual_policy.repository.is_empty()
        || !valid_id(&plan.perceptual_policy.revision)
        || plan.perceptual_policy.model_source.is_empty()
        || !valid_sha256(&plan.perceptual_policy.model_sha256)
        || !plan.perceptual_policy.minimum_case_improvement.is_finite()
        || !valid_id(&plan.perceptual_policy.calibrated_positive_control)
    {
        bail!("release corpus perceptual policy is invalid");
    }
    let expanded = plan
        .voices
        .len()
        .checked_mul(plan.seeds.len())
        .and_then(|value| value.checked_mul(plan.cases.len()))
        .context("release corpus size overflowed")?;
    if expanded > MAX_CASES {
        bail!("release corpus expands beyond {MAX_CASES} cases");
    }
    unique_ids(&plan.voices, "voice")?;
    let mut case_ids = Vec::with_capacity(plan.cases.len());
    for case in &plan.cases {
        if !valid_id(&case.id)
            || !(1..=MAX_TEXT_CODE_POINTS).contains(&case.text.chars().count())
            || case.tags.is_empty()
            || case.tags.iter().any(|tag| !valid_id(tag))
            || case.tags.iter().collect::<BTreeSet<_>>().len() != case.tags.len()
        {
            bail!("release corpus case is invalid");
        }
        case_ids.push(case.id.clone());
    }
    unique_ids(&case_ids, "case")?;
    if plan.seeds.iter().collect::<BTreeSet<_>>().len() != plan.seeds.len() {
        bail!("release corpus seeds must be unique");
    }

    let mut states = BTreeMap::new();
    for mapping in &args.voice_state {
        let (id, path) = mapping
            .split_once('=')
            .context("--voice-state must use VOICE_ID=PATH")?;
        if !valid_id(id) {
            bail!("--voice-state contains an invalid voice ID");
        }
        let path = PathBuf::from(path);
        require_absolute_regular("--voice-state path", &path)?;
        if states.insert(id.to_owned(), path).is_some() {
            bail!("--voice-state IDs must be unique");
        }
    }
    if states.keys().cloned().collect::<BTreeSet<_>>()
        != plan.voices.iter().cloned().collect::<BTreeSet<_>>()
    {
        bail!("--voice-state mappings must exactly match the plan voices");
    }
    Ok(states)
}

fn run_candidate(
    args: &Args,
    text: &str,
    provider_options: &str,
    utterance_options: &str,
    output_wav: &Path,
) -> Result<Output> {
    run_checked(
        Command::new(&args.benchmark)
            .args(["--provider"])
            .arg(&args.provider)
            .args(["--expected-provider", "pocket-tts"])
            .args(["--delivery", "incremental"])
            .args(["--format", "audio/pcm;codec=pcm_s16le"])
            .args(["--data-dir"])
            .arg(&args.data_dir)
            .args(["--cache-dir"])
            .arg(&args.cache_dir)
            .args(["--provider-options-json", provider_options])
            .args(["--utterance-options-json", utterance_options])
            .args(["--text", text])
            .args(["--warmups", "0", "--iterations", "1"])
            .args(["--steady-rss-window-ms", "1"])
            .args(["--output-wav"])
            .arg(output_wav),
        "candidate benchmark",
    )
}

fn run_baseline(
    args: &Args,
    text: &str,
    seed: u64,
    voice_state: &Path,
    output_wav: &Path,
) -> Result<Output> {
    run_checked(
        Command::new(&args.evaluator)
            .args(["--config"])
            .arg(&args.config)
            .args(["--weights"])
            .arg(&args.fp32_weights)
            .args(["--tokenizer"])
            .arg(&args.tokenizer)
            .args(["--voice-state"])
            .arg(voice_state)
            .args(["--precision", "fp32"])
            .args(["--text", text])
            .args(["--threads", &args.threads.to_string()])
            .args(["--warmups", "0", "--iterations", "1"])
            .args(["--cancellation-iterations", "0"])
            .args(["--temperature", "0.3"])
            .args(["--pad-with-spaces-for-short-inputs", "false"])
            .args(["--frames-after-eos-offset", "2"])
            .args(["--seed", &seed.to_string()])
            .args(["--output-gain", "0.65"])
            .args(["--output"])
            .arg(output_wav),
        "f32 control",
    )
}

fn run_checked(command: &mut Command, label: &str) -> Result<Output> {
    let output = command.output()?;
    if !output.status.success() {
        bail!("{label} failed with status {}", output.status);
    }
    Ok(output)
}

fn validate_candidate_report(bytes: &[u8]) -> Result<()> {
    let report: Value = serde_json::from_slice(bytes)?;
    if report["schema"] != "utterpipe.benchmark/1"
        || report["provider"]["slug"] != "pocket-tts"
        || report["configuration"]["delivery"]["mode"] != "incremental"
        || report["configuration"]["delivery"]["format"] != "audio/pcm;codec=pcm_s16le"
        || report["iterations"].as_array().map_or(0, Vec::len) != 1
    {
        bail!("candidate benchmark returned an unexpected report");
    }
    Ok(())
}

fn validate_baseline_report(bytes: &[u8], seed: u64) -> Result<()> {
    let report: Value = serde_json::from_slice(bytes)?;
    let gain = report["configuration"]["output_gain"].as_f64();
    if report["schema"] != "utterpipe_pocket_tts.xn_runtime_benchmark/1"
        || report["runtime"]["precision"] != "fp32"
        || report["configuration"]["seed"] != seed
        || gain.is_none_or(|gain| (gain - OUTPUT_GAIN).abs() > f64::from(f32::EPSILON))
        || report["runs"].as_array().map_or(0, Vec::len) != 1
    {
        bail!("f32 control returned an unexpected report");
    }
    Ok(())
}

fn require_absolute_regular(label: &str, path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("{label} could not be inspected"))?;
    if !path.is_absolute() || !metadata.file_type().is_file() {
        bail!("{label} must be an existing absolute regular file");
    }
    Ok(())
}

fn unique_ids(ids: &[String], label: &str) -> Result<()> {
    if ids.iter().any(|id| !valid_id(id)) || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
    {
        bail!("release corpus {label} IDs must be valid and unique");
    }
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
    fn ids_are_bounded_ascii_slugs() {
        assert!(valid_id("voice-zero-peter-yearsley"));
        assert!(!valid_id("-voice"));
        assert!(!valid_id("voice/path"));
        assert!(!valid_id("é"));
    }

    #[test]
    fn checked_in_plan_is_bounded_and_expands_to_eighty_four_cases() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("release-corpus.json");
        let plan = load_plan(&path).unwrap();
        assert_eq!(plan.voices.len() * plan.seeds.len() * plan.cases.len(), 84);
        assert_eq!(plan.source.repository, "kyutai-labs/pocket-tts");
        assert!(
            plan.cases
                .iter()
                .any(|case| case.tags.iter().any(|tag| tag == "long"))
        );
        assert!(
            plan.cases
                .iter()
                .any(|case| case.tags.iter().any(|tag| tag == "status"))
        );
    }
}
