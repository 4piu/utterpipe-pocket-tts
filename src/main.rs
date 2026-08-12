use std::{
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use clap::{Args, Parser, Subcommand};
use utterpipe_pocket_tts::{
    PROVIDER_NAME, PROVIDER_SLUG, PROVIDER_VENDOR, PROVIDER_VERSION,
    direct_storage::resolve_direct_storage,
    protocol,
    store::{Store, VoiceProvenance},
    voice::{
        CURATED_VOICES, CuratedLicense, CuratedVoice, ExpectedDownload, VoiceSource,
        curated_download_url, curated_license_by_id, download_voice,
    },
    xn_bundle::{APRIL_MODEL_ID, verify_bundle as verify_xn_bundle},
    xn_prepare::{PrepareProgress, SourceOrigin, catalog_manifest, prepare_bundle_with_progress},
};

mod cli_catalog;
mod cli_confirm;

#[derive(Parser)]
#[command(name = "utterpipe-pocket-tts", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print provider identity and capabilities.
    Info,
    /// Validate local provider storage without loading the model.
    Doctor(Storage),
    /// Inspect, prepare, or remove the pinned model.
    Models {
        #[command(subcommand)]
        command: ModelCommand,
    },
    /// Inspect, install, import, or remove reference voices.
    Voices {
        #[command(subcommand)]
        command: VoiceCommand,
    },
    /// Run the framed UtterPipe protocol.
    Protocol {
        #[arg(long, required = true)]
        stdio: bool,
    },
}

#[derive(Args, Clone)]
struct Storage {
    /// Override the platform-standard provider data directory.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Override the platform-standard provider cache directory.
    #[arg(long)]
    cache_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum ModelCommand {
    List(Storage),
    Prepare {
        #[command(flatten)]
        storage: Storage,
        /// Prepare from a directory containing the exact pinned model.safetensors and tokenizer.model.
        #[arg(long)]
        source_dir: Option<PathBuf>,
        /// Required disclosure ID; repeat for every displayed ID.
        #[arg(long = "accept")]
        accepted: Vec<String>,
        /// Confirm the displayed plan and terms.
        #[arg(long)]
        yes: bool,
    },
    /// Import a complete self-describing XN bundle from a local directory.
    Import {
        /// Directory containing manifest.json, model.gguf, config.json, and one supported tokenizer.
        source: PathBuf,
        #[command(flatten)]
        storage: Storage,
        /// Required bundle disclosure ID; repeat for every displayed ID.
        #[arg(long = "accept")]
        accepted: Vec<String>,
        /// Confirm the displayed plan and terms.
        #[arg(long)]
        yes: bool,
    },
    Remove {
        id: String,
        #[command(flatten)]
        storage: Storage,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum VoiceCommand {
    /// List voices already imported into provider storage.
    List(Storage),
    /// List the small, pinned catalog available for verified download.
    Available(Storage),
    /// Download and import one or more voices from the pinned catalog.
    Install {
        /// Catalog numbers or numeric ranges. Omit for an interactive chooser.
        selections: Vec<String>,
        /// Override the installed voice ID.
        #[arg(long)]
        id: Option<String>,
        /// Required upstream license ID.
        #[arg(long = "accept")]
        accepted: Vec<String>,
        /// Confirm the displayed plan.
        #[arg(long)]
        yes: bool,
        #[command(flatten)]
        storage: Storage,
    },
    /// Import a local path or an explicit HTTP(S) URL.
    Import {
        /// Relative/absolute file path or explicit HTTP(S) URL.
        source: OsString,
        /// Installed voice ID.
        #[arg(long)]
        id: String,
        /// Confirm permitted, consented use without an interactive prompt.
        #[arg(long)]
        consent_confirmed: bool,
        #[command(flatten)]
        storage: Storage,
    },
    Remove {
        /// Installed voice ID.
        id: String,
        #[command(flatten)]
        storage: Storage,
        /// Confirm removal without an interactive prompt.
        #[arg(long)]
        yes: bool,
    },
}

#[tokio::main]
async fn main() {
    let exit_code = match run().await {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("error: {message}");
            1
        }
    };
    // A native inference worker may be inside foreign code that cannot observe a
    // cancellation callback. Exiting explicitly avoids waiting indefinitely for
    // Tokio's blocking pool after the protocol grace period or host EOF.
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(exit_code);
}

async fn run() -> Result<(), String> {
    match Cli::parse().command {
        Command::Info => {
            println!("{PROVIDER_NAME}");
            println!("slug: {PROVIDER_SLUG}");
            println!("vendor: {PROVIDER_VENDOR}");
            println!("version: {PROVIDER_VERSION}");
            println!("protocol: utterpipe.tts v1");
            println!("delivery: complete PCM16 WAV, incremental PCM16");
            println!("engine: XN 0.1.21 (native Rust, Q8)");
            Ok(())
        }
        Command::Doctor(storage) => {
            let store = store(storage)?;
            store.validate_local().map_err(public_store_error)?;
            println!("model: {}", store.xn_model_status());
            println!(
                "imported voices: {}",
                store.voice_catalog().map_err(public_store_error)?.len()
            );
            Ok(())
        }
        Command::Models { command } => match command {
            ModelCommand::List(storage) => {
                let store = store(storage)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!([{
                        "id": APRIL_MODEL_ID,
                        "name": "Pocket TTS English April 2026 Q8",
                        "status": store.xn_model_status(),
                        "languages": ["en"],
                        "source": "pinned official model with local Q8 conversion"
                    }]))
                    .map_err(|_| "could not encode model descriptor".to_owned())?
                );
                Ok(())
            }
            ModelCommand::Prepare {
                storage,
                source_dir,
                mut accepted,
                yes,
            } => {
                let manifest = catalog_manifest();
                println!("Plan: prepare and install {APRIL_MODEL_ID}");
                println!("Disclosures:");
                for license in &manifest.licenses {
                    println!("- {}", license.name);
                    println!("  {}", license.url);
                }
                cli_confirm::model_bundle_import(&manifest.licenses, &mut accepted, yes)?;
                let store = store(storage)?;
                let source_dir = source_dir.map(absolute_path).transpose()?;
                let cancellation = cancellation_on_ctrl_c();
                let progress = Arc::new(Mutex::new(CliPrepareProgress::default()));
                let progress_reporter = progress.clone();
                let bundle = prepare_bundle_with_progress(
                    store.cache_dir(),
                    source_dir.as_deref(),
                    cancellation.clone(),
                    move |event| {
                        if let Ok(mut reporter) = progress_reporter.lock() {
                            reporter.report(event);
                        }
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
                eprintln!("Installing verified model bundle...");
                let install_store = store.clone();
                let install_source = bundle.path().to_owned();
                let mut install = tokio::task::spawn_blocking(move || {
                    install_store.install_xn_bundle_from_directory(&install_source, &accepted)
                });
                tokio::select! {
                    result = &mut install => result
                        .map_err(|_| "model installation worker failed".to_owned())?
                        .map_err(public_store_error)?,
                    () = cancellation.cancelled() => install
                        .await
                        .map_err(|_| "model installation worker failed".to_owned())?
                        .map_err(public_store_error)?,
                }
                eprintln!("Model bundle installed.");
                let voices = store.voice_catalog().map_err(public_store_error)?;
                if !voices.is_empty() {
                    eprintln!("Preparing {} installed voice(s)...", voices.len());
                }
                for voice in voices {
                    let Some(voice_id) = voice["id"].as_str() else {
                        continue;
                    };
                    store
                        .prepare_xn_voice(APRIL_MODEL_ID, voice_id, default_xn_threads())
                        .map_err(public_store_error)?;
                }
                println!("installed: model:{APRIL_MODEL_ID}");
                Ok(())
            }
            ModelCommand::Import {
                source,
                storage,
                mut accepted,
                yes,
            } => {
                let source = absolute_path(source)?;
                let bundle =
                    verify_xn_bundle(&source, || false).map_err(|error| error.to_string())?;
                println!("Plan: import {}", bundle.manifest.model_id);
                println!("Name: {}", bundle.manifest.name);
                println!(
                    "Source: {} at {}",
                    bundle.manifest.source_repository, bundle.manifest.source_revision
                );
                println!(
                    "Runtime: {} {} ({})",
                    bundle.manifest.runtime.engine,
                    bundle.manifest.runtime.revision,
                    bundle.manifest.runtime.precision
                );
                println!("Disclosures:");
                for license in &bundle.manifest.licenses {
                    println!("- {} ({}) {}", license.name, license.id, license.url);
                }
                cli_confirm::model_bundle_import(&bundle.manifest.licenses, &mut accepted, yes)?;
                let store = store(storage)?;
                store
                    .install_xn_bundle_from_directory(&source, &accepted)
                    .map_err(public_store_error)?;
                for voice in store.voice_catalog().map_err(public_store_error)? {
                    let Some(voice_id) = voice["id"].as_str() else {
                        continue;
                    };
                    store
                        .prepare_xn_voice(APRIL_MODEL_ID, voice_id, default_xn_threads())
                        .map_err(public_store_error)?;
                }
                println!("installed: model:{}", bundle.manifest.model_id);
                Ok(())
            }
            ModelCommand::Remove { id, storage, yes } => {
                if id != APRIL_MODEL_ID {
                    return Err("unknown model ID".to_owned());
                }
                let artifact = format!("model:{id}");
                println!("Plan: remove {artifact}");
                cli_confirm::removal(yes, &artifact)?;
                store(storage)?
                    .remove_artifacts(std::slice::from_ref(&artifact))
                    .map_err(public_store_error)?;
                println!("removed: {artifact}");
                Ok(())
            }
        },
        Command::Voices { command } => match command {
            VoiceCommand::List(storage) => {
                for voice in store(storage)?
                    .voice_catalog()
                    .map_err(public_store_error)?
                {
                    println!(
                        "{}",
                        serde_json::to_string(&voice)
                            .map_err(|_| "could not encode voice descriptor".to_owned())?
                    );
                }
                Ok(())
            }
            VoiceCommand::Available(storage) => {
                let installed = store(storage)?
                    .voice_catalog()
                    .map_err(public_store_error)?;
                cli_catalog::print_available(
                    CURATED_VOICES,
                    &curated_installed_state(CURATED_VOICES, &installed),
                )
            }
            VoiceCommand::Install {
                selections,
                id,
                mut accepted,
                yes,
                storage,
            } => {
                let store = store(storage)?;
                let installed = store.voice_catalog().map_err(public_store_error)?;
                let installed_state = curated_installed_state(CURATED_VOICES, &installed);
                let selected = if selections.is_empty() {
                    cli_catalog::choose_interactively(CURATED_VOICES, &installed_state)?
                } else {
                    cli_catalog::resolve_selections(&selections, CURATED_VOICES)?
                };
                if id.is_some() && selected.len() != 1 {
                    return Err("--id can only be used when installing one voice".to_owned());
                }
                let targets: Vec<_> = selected
                    .iter()
                    .map(|voice| {
                        let installed_id = id.clone().unwrap_or_else(|| voice.id.to_owned());
                        (*voice, installed_id)
                    })
                    .collect();
                println!("Plan: download and import {} voice(s)", targets.len());
                for (voice, installed_id) in &targets {
                    println!(
                        "- {} ({}) -> voice:{} [{}]",
                        voice.name, voice.id, installed_id, voice.license_id
                    );
                    println!(
                        "  Source: https://huggingface.co/{}/blob/{}/{}",
                        voice.repository, voice.revision, voice.path
                    );
                    println!("  Attribution: {}", voice.attribution);
                }
                let licenses = selected_licenses(&selected)?;
                println!("Licenses:");
                for license in &licenses {
                    println!("- {} ({}) {}", license.name, license.id, license.url);
                    println!("  {}", license.notice);
                }
                cli_confirm::curated_voice_install(&licenses, &mut accepted, yes)?;
                let cancellation = cancellation_on_ctrl_c();
                let mut downloads = Vec::with_capacity(targets.len());
                for (voice, installed_id) in targets {
                    let staged = download_voice(
                        curated_download_url(voice),
                        store.cache_dir(),
                        Some(ExpectedDownload {
                            bytes: voice.bytes,
                            sha256: voice.sha256,
                        }),
                        cancellation.clone(),
                        warn_large_input,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                    downloads.push((voice, installed_id, staged));
                }
                for (voice, installed_id, staged) in downloads {
                    let provenance = VoiceProvenance {
                        kind: "curated".to_owned(),
                        name: voice.name.to_owned(),
                        source_url: curated_download_url(voice).to_string(),
                        repository: voice.repository.to_owned(),
                        revision: voice.revision.to_owned(),
                        path: voice.path.to_owned(),
                        license_id: voice.license_id.to_owned(),
                        license_url: voice.license_url.to_owned(),
                        attribution: voice.attribution.to_owned(),
                    };
                    import_voice_path(
                        store.clone(),
                        staged.path().to_owned(),
                        installed_id.clone(),
                        Some(provenance),
                        cancellation.clone(),
                    )
                    .await?;
                    println!("installed: voice:{installed_id}");
                }
                Ok(())
            }
            VoiceCommand::Import {
                source,
                id,
                consent_confirmed,
                storage,
            } => {
                println!("Plan: normalize and import voice:{id}");
                cli_confirm::voice_import(consent_confirmed)?;
                let store = store(storage)?;
                let cancellation = cancellation_on_ctrl_c();
                match VoiceSource::parse(source).map_err(|error| error.to_string())? {
                    VoiceSource::File(path) => {
                        let path = absolute_path(path)?;
                        warn_large_local_file(&path);
                        import_voice_path(store, path, id.clone(), None, cancellation).await?;
                    }
                    VoiceSource::Url(url) => {
                        let staged = download_voice(
                            url,
                            store.cache_dir(),
                            None,
                            cancellation.clone(),
                            warn_large_input,
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                        import_voice_path(
                            store,
                            staged.path().to_owned(),
                            id.clone(),
                            None,
                            cancellation,
                        )
                        .await?;
                    }
                }
                println!("installed: voice:{id}");
                Ok(())
            }
            VoiceCommand::Remove { id, storage, yes } => {
                let artifact = format!("voice:{id}");
                println!("Plan: remove {artifact}");
                cli_confirm::removal(yes, &artifact)?;
                store(storage)?
                    .remove_artifacts(std::slice::from_ref(&artifact))
                    .map_err(public_store_error)?;
                println!("removed: {artifact}");
                Ok(())
            }
        },
        Command::Protocol { stdio } => {
            debug_assert!(stdio);
            protocol::run_stdio()
                .await
                .map_err(|error| error.to_string())
        }
    }
}

fn store(storage: Storage) -> Result<Store, String> {
    let storage = resolve_direct_storage(storage.data_dir, storage.cache_dir)
        .map_err(|error| error.to_string())?;
    Store::new(storage.data_dir, storage.cache_dir).map_err(public_store_error)
}

fn public_store_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn curated_installed_state(voices: &[CuratedVoice], installed: &[serde_json::Value]) -> Vec<bool> {
    voices
        .iter()
        .map(|voice| {
            installed.iter().any(|item| {
                item["id"] == voice.id
                    || (item["source"]["repository"] == voice.repository
                        && item["source"]["revision"] == voice.revision
                        && item["source"]["path"] == voice.path)
            })
        })
        .collect()
}

fn selected_licenses(selected: &[&CuratedVoice]) -> Result<Vec<CuratedLicense>, String> {
    let mut licenses = Vec::new();
    for voice in selected {
        if licenses
            .iter()
            .any(|license: &CuratedLicense| license.id == voice.license_id)
        {
            continue;
        }
        licenses.push(
            curated_license_by_id(voice.license_id)
                .ok_or_else(|| "curated voice has an unknown license".to_owned())?,
        );
    }
    Ok(licenses)
}

fn absolute_path(path: PathBuf) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path)
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(path))
            .map_err(|_| "could not resolve the voice source path".to_owned())
    }
}

fn warn_large_local_file(path: &Path) {
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && metadata.len() > utterpipe_pocket_tts::audio::LARGE_REFERENCE_WARNING_BYTES
    {
        warn_large_input(metadata.len());
    }
}

fn warn_large_input(bytes: u64) {
    eprintln!(
        "warning: voice source is unusually large ({bytes} bytes); import will continue and remains cancellable"
    );
}

fn cancellation_on_ctrl_c() -> tokio_util::sync::CancellationToken {
    let cancellation = tokio_util::sync::CancellationToken::new();
    let signal = cancellation.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal.cancel();
        }
    });
    cancellation
}

async fn import_voice_path(
    store: Store,
    path: PathBuf,
    id: String,
    provenance: Option<VoiceProvenance>,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<(), String> {
    let preparation_store = store.clone();
    let preparation_id = id.clone();
    let mutation = store.begin_mutation().map_err(public_store_error)?;
    let task_cancellation = cancellation.clone();
    let mut task = tokio::task::spawn_blocking(move || {
        if let Some(provenance) = provenance {
            store.import_curated_voice_locked(&path, &id, true, 30.0, &mutation, provenance, || {
                task_cancellation.is_cancelled()
            })
        } else {
            store.import_voice_locked(&path, &id, true, 30.0, &mutation, || {
                task_cancellation.is_cancelled()
            })
        }
    });
    tokio::select! {
        result = &mut task => result
            .map_err(|_| "voice import worker failed".to_owned())?
            .map_err(public_store_error),
        () = cancellation.cancelled() => {
            task.await
                .map_err(|_| "voice import worker failed".to_owned())?
                .map_err(public_store_error)
        }
    }?;
    if preparation_store.xn_model_status() == "installed" {
        let preparation_mutation = preparation_store
            .begin_mutation()
            .map_err(public_store_error)?;
        let preparation_cancellation = cancellation.clone();
        let mut preparation = tokio::task::spawn_blocking(move || {
            preparation_store.prepare_xn_voice_locked(
                APRIL_MODEL_ID,
                &preparation_id,
                default_xn_threads(),
                &preparation_mutation,
                || preparation_cancellation.is_cancelled(),
            )
        });
        tokio::select! {
            result = &mut preparation => result
                .map_err(|_| "XN voice preparation worker failed".to_owned())?
                .map_err(public_store_error)?,
            () = cancellation.cancelled() => preparation
                .await
                .map_err(|_| "XN voice preparation worker failed".to_owned())?
                .map_err(public_store_error)?,
        }
    }
    Ok(())
}

fn default_xn_threads() -> u32 {
    if cfg!(target_arch = "aarch64") { 4 } else { 8 }
}

#[derive(Default)]
struct CliPrepareProgress {
    download_name: Option<&'static str>,
    download_bucket: Option<u64>,
    conversion_bucket: Option<usize>,
}

impl CliPrepareProgress {
    fn report(&mut self, event: PrepareProgress) {
        match event {
            PrepareProgress::CheckingSource { name, bytes } => {
                self.download_name = None;
                self.download_bucket = None;
                eprintln!("Checking {name} ({})...", human_bytes(bytes));
            }
            PrepareProgress::Downloading {
                name,
                downloaded,
                total,
            } => {
                let percentage = downloaded.saturating_mul(100) / total.max(1);
                let bucket = if percentage == 100 {
                    100
                } else {
                    (percentage / 10) * 10
                };
                if self.download_name != Some(name) || self.download_bucket != Some(bucket) {
                    eprintln!(
                        "Downloading {name}: {percentage:>3}% ({}/{})",
                        human_bytes(downloaded),
                        human_bytes(total)
                    );
                    self.download_name = Some(name);
                    self.download_bucket = Some(bucket);
                }
            }
            PrepareProgress::SourceReady { name, origin } => {
                let origin = match origin {
                    SourceOrigin::Local => "local source",
                    SourceOrigin::Cache => "verified cache",
                    SourceOrigin::Download => "verified download",
                };
                eprintln!("Ready: {name} ({origin}).");
            }
            PrepareProgress::Converting { completed, total } => {
                let percentage = completed.saturating_mul(100) / total.max(1);
                let bucket = if percentage == 100 {
                    100
                } else {
                    (percentage / 10) * 10
                };
                if self.conversion_bucket != Some(bucket) {
                    eprintln!("Converting model to Q8: {percentage:>3}%");
                    self.conversion_bucket = Some(bucket);
                }
            }
            PrepareProgress::VerifyingBundle => {
                eprintln!("Verifying prepared Q8 bundle...");
            }
        }
    }
}

fn human_bytes(bytes: u64) -> String {
    const KIB: u64 = 1_024;
    const MIB: u64 = KIB * 1_024;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}
