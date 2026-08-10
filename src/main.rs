use std::{io::Write, path::PathBuf};

use clap::{Args, Parser, Subcommand};
use utterpipe_pocket_tts::{
    PROVIDER_NAME, PROVIDER_SLUG, PROVIDER_VENDOR, PROVIDER_VERSION,
    direct_storage::resolve_direct_storage,
    model::{MODEL_ID, licenses, model_descriptor},
    protocol,
    store::Store,
};

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
    /// Inspect, import, or remove user-provided reference voices.
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
        /// Install from this already-downloaded pinned archive instead of networking.
        #[arg(long)]
        archive: Option<PathBuf>,
        /// Required disclosure ID; repeat for all three IDs.
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
    List(Storage),
    Import {
        wav: PathBuf,
        #[arg(long)]
        id: String,
        #[arg(long)]
        consent_confirmed: bool,
        #[command(flatten)]
        storage: Storage,
    },
    Remove {
        id: String,
        #[command(flatten)]
        storage: Storage,
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
            println!("engine: sherpa-onnx 1.13.4 (static)");
            Ok(())
        }
        Command::Doctor(storage) => {
            let store = store(storage)?;
            store.validate_local().map_err(public_store_error)?;
            println!("model: {}", store.model_status());
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
                    serde_json::to_string_pretty(&model_descriptor(store.model_status()))
                        .map_err(|_| "could not encode model descriptor".to_owned())?
                );
                Ok(())
            }
            ModelCommand::Prepare {
                storage,
                archive,
                mut accepted,
                yes,
            } => {
                println!("Plan: install {MODEL_ID}");
                println!(
                    "{}",
                    serde_json::to_string_pretty(&licenses())
                        .map_err(|_| "could not encode model disclosures".to_owned())?
                );
                cli_confirm::prepare(&mut accepted, yes)?;
                let store = store(storage)?;
                if let Some(archive) = archive {
                    store
                        .install_model_from_archive(&archive, &accepted)
                        .map_err(public_store_error)?;
                } else {
                    protocol::prepare_model_network(store, accepted).await?;
                }
                println!("installed: model:{MODEL_ID}");
                Ok(())
            }
            ModelCommand::Remove { id, storage, yes } => {
                if id != MODEL_ID {
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
            VoiceCommand::Import {
                wav,
                id,
                consent_confirmed,
                storage,
            } => {
                println!("Plan: normalize and import voice:{id}");
                let consent_confirmed = cli_confirm::voice_import(consent_confirmed)?;
                store(storage)?
                    .import_voice(&wav, &id, consent_confirmed, 30.0)
                    .map_err(public_store_error)?;
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
