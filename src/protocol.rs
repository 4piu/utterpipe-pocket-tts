use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use futures_util::StreamExt;
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    task::JoinHandle,
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use crate::{
    PROVIDER_NAME, PROVIDER_SLUG, PROVIDER_VENDOR, PROVIDER_VERSION,
    audio::{MAX_RIFF_REFERENCE_BYTES, pcm16_wav_bytes},
    config::{
        ProviderOptions, UtteranceOptions, management_options_schema, provider_options_schema,
        utterance_options_schema,
    },
    engine::{EngineError, GenerationOptions, GenerationSummary, PocketEngine, SAMPLE_RATE},
    model::{
        ARCHIVE_BYTES, ARCHIVE_SHA256, INSTALLED_BYTES, LICENSE_IDS, MODEL_ID, SOURCE_URL,
        licenses, model_descriptor,
    },
    store::{RuntimeAssets, Store, StoreError},
};

const CONTROL_KIND: u8 = 1;
const AUDIO_KIND: u8 = 2;
const MAX_CONTROL_BYTES: usize = 1_048_576;
const MAX_AUDIO_BYTES: usize = 268_435_456;
const MAX_TEXT_CODE_POINTS: usize = 4_096;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const WAV_FORMAT: &str = "audio/wav;codec=pcm_s16le";
const PCM_FORMAT: &str = "audio/pcm;codec=pcm_s16le";
const CANCELLATION_GRACE: Duration = Duration::from_secs(2);
const UTTERANCE_SCHEMA_PROFILE: &str = "utterpipe.utterance-options/1";
static PLAN_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Error)]
pub enum ProtocolFailure {
    #[error("protocol input failed")]
    Input,
    #[error("protocol output failed")]
    Output,
    #[error("invalid protocol frame: {0}")]
    Frame(&'static str),
    #[error("invalid control message")]
    InvalidControl,
    #[error("provider worker failed internally")]
    Worker,
    #[error("provider worker did not stop after cancellation")]
    CancellationStuck,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Session {
    Inspect,
    Runtime,
    Management,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Delivery {
    Complete,
    Incremental,
}

#[derive(Debug)]
struct Request {
    id: String,
    method: String,
    params: Value,
}

struct RuntimeState {
    engine: Arc<PocketEngine>,
    assets: RuntimeAssets,
    audio_deliveries: Vec<AudioDelivery>,
    max_text_code_points: usize,
    max_audio_bytes: usize,
    timeout: Duration,
}

struct ManagementState {
    store: Store,
    provider_options: ProviderOptions,
}

struct ActiveSynthesis {
    id: String,
    delivery: Delivery,
    cancellation: Arc<AtomicBool>,
    chunks: tokio::sync::mpsc::Receiver<Vec<u8>>,
    chunks_closed: bool,
    task: JoinHandle<Result<GenerationSummary, EngineError>>,
    pcm: Vec<u8>,
    began: bool,
    sent_bytes: usize,
    sent_frames: usize,
    deadline: Instant,
    forced_error: Option<WireError>,
    cancellation_deadline: Option<Instant>,
}

struct ActiveManagement {
    id: String,
    cancellation: CancellationToken,
    task: JoinHandle<Result<Value, WireError>>,
}

#[derive(Clone)]
struct PreparePlan {
    id: String,
    allow_network: bool,
    status: String,
}

#[derive(Clone)]
struct RemovePlan {
    id: String,
    artifacts: Vec<String>,
    tokens: HashMap<String, String>,
}

enum InputEvent {
    Control(Value),
    Eof,
    Fatal(ProtocolFailure),
}

enum LoopEvent {
    Input(Option<InputEvent>),
    Chunk(Option<Vec<u8>>),
    SynthesisDone(Result<Result<GenerationSummary, EngineError>, tokio::task::JoinError>),
    ManagementDone(Result<Result<Value, WireError>, tokio::task::JoinError>),
    Deadline,
    CancellationExpired,
}

#[derive(Debug, Clone)]
struct WireError {
    code: &'static str,
    message: String,
}

impl WireError {
    fn new(code: &'static str, message: impl AsRef<str>) -> Self {
        let mut message = message.as_ref().replace(['\r', '\n'], " ");
        if message.chars().count() > 512 {
            message = message.chars().take(509).collect::<String>();
            message.push_str("...");
        }
        Self { code, message }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HelloParams {
    protocol: String,
    versions: Vec<u64>,
    expected_provider: String,
    session: String,
    host: HostIdentity,
    utterance_schema_profiles: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostIdentity {
    name: String,
    version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeInitializeParams {
    data_dir: String,
    cache_dir: String,
    provider_options: Map<String, Value>,
    limits: Limits,
    accepted_audio_deliveries: Vec<AudioDelivery>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagementInitializeParams {
    data_dir: String,
    cache_dir: String,
    provider_options: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct AudioDelivery {
    mode: String,
    format: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Limits {
    max_text_code_points: u64,
    max_audio_bytes: u64,
    synthesis_timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SynthesisParams {
    text: String,
    audio_delivery: AudioDelivery,
    #[serde(default)]
    utterance_options: Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelParams {
    request_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogItemsParams {
    catalog_id: String,
    scope: String,
    refresh: bool,
    limit: u16,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparePlanParams {
    refresh: bool,
    allow_network: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareApplyParams {
    plan_id: String,
    accepted_licenses: Vec<String>,
    operation_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemovePlanParams {
    artifacts: Vec<String>,
    purge: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveApplyParams {
    plan_id: String,
    operation_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssetImportParams {
    kind: String,
    source_path: String,
    requested_id: String,
    consent_confirmed: bool,
    operation_id: String,
}

/// Run one `UtterPipe` provider process over inherited standard I/O.
///
/// # Errors
///
/// Returns [`ProtocolFailure`] after fatal framing/I/O failure, an internal
/// protocol invariant failure, or a worker that cannot stop within the required
/// cancellation grace. Synthesis worker panics become terminal wire errors.
#[allow(clippy::too_many_lines)]
pub async fn run_stdio() -> Result<(), ProtocolFailure> {
    let (input_sender, mut input_receiver) = tokio::sync::mpsc::channel(8);
    let input_task = tokio::spawn(read_input(tokio::io::stdin(), input_sender));
    let mut output = tokio::io::stdout();
    let mut session = None;
    let mut initialized = false;
    let mut runtime: Option<RuntimeState> = None;
    let mut management: Option<ManagementState> = None;
    let mut synthesis: Option<ActiveSynthesis> = None;
    let mut mutation: Option<ActiveManagement> = None;
    let mut prepare_plan: Option<PreparePlan> = None;
    let mut remove_plan: Option<RemovePlan> = None;

    loop {
        let event = next_event(&mut input_receiver, synthesis.as_mut(), mutation.as_mut()).await;
        match event {
            LoopEvent::Chunk(Some(chunk)) => {
                let active = synthesis.as_mut().ok_or(ProtocolFailure::Worker)?;
                if active.forced_error.is_some() {
                    continue;
                }
                match active.delivery {
                    Delivery::Complete => active.pcm.extend_from_slice(&chunk),
                    Delivery::Incremental => {
                        if !active.began {
                            write_event(
                                &mut output,
                                "synthesis.audio_begin",
                                json!({
                                    "request_id": active.id,
                                    "format": PCM_FORMAT,
                                    "sample_rate_hz": SAMPLE_RATE,
                                    "channels": 1
                                }),
                            )
                            .await?;
                            active.began = true;
                        }
                        write_frame(&mut output, AUDIO_KIND, &chunk).await?;
                        active.sent_bytes += chunk.len();
                        active.sent_frames += 1;
                    }
                }
            }
            LoopEvent::Chunk(None) => {
                if let Some(active) = &mut synthesis {
                    active.chunks_closed = true;
                }
            }
            LoopEvent::SynthesisDone(completion) => {
                let active = synthesis.take().ok_or(ProtocolFailure::Worker)?;
                let outcome = normalize_synthesis_completion(completion);
                finish_synthesis(&mut output, active, outcome).await?;
            }
            LoopEvent::ManagementDone(completion) => {
                let active = mutation.take().ok_or(ProtocolFailure::Worker)?;
                match completion.unwrap_or_else(|_| {
                    Err(WireError::new(
                        "internal",
                        "management worker failed internally",
                    ))
                }) {
                    Ok(result) => write_result(&mut output, &active.id, result).await?,
                    Err(error) => write_error(&mut output, &active.id, &error).await?,
                }
            }
            LoopEvent::Deadline => {
                let active = synthesis.as_mut().ok_or(ProtocolFailure::Worker)?;
                active.cancellation.store(true, Ordering::Release);
                active.forced_error =
                    Some(WireError::new("timeout", "synthesis exceeded its deadline"));
                active.cancellation_deadline = Some(Instant::now() + CANCELLATION_GRACE);
            }
            LoopEvent::CancellationExpired => {
                if let Some(active) = synthesis.take() {
                    active.task.abort();
                }
                input_task.abort();
                return Err(ProtocolFailure::CancellationStuck);
            }
            LoopEvent::Input(None | Some(InputEvent::Eof)) => {
                cancel_workers(&mut synthesis, &mut mutation).await;
                input_task.abort();
                return Ok(());
            }
            LoopEvent::Input(Some(InputEvent::Fatal(error))) => {
                cancel_workers(&mut synthesis, &mut mutation).await;
                input_task.abort();
                return Err(error);
            }
            LoopEvent::Input(Some(InputEvent::Control(value))) => {
                let request = match parse_request(&value) {
                    Ok(request) => request,
                    Err((Some(id), error)) => {
                        write_error(&mut output, &id, &error).await?;
                        continue;
                    }
                    Err((None, _)) => return Err(ProtocolFailure::InvalidControl),
                };
                if let Some(active_synthesis) = &mut synthesis {
                    if handle_synthesis_control(&mut output, &request, active_synthesis).await?
                        && request.method == "session.shutdown"
                    {
                        let active = synthesis.take().ok_or(ProtocolFailure::Worker)?;
                        let active_id = active.id.clone();
                        await_synthesis_stop(active).await?;
                        write_error(
                            &mut output,
                            &active_id,
                            &WireError::new("cancelled", "synthesis was cancelled"),
                        )
                        .await?;
                        write_result(&mut output, &request.id, json!({"accepted": true})).await?;
                        input_task.abort();
                        return Ok(());
                    }
                    continue;
                }
                if mutation.is_some() {
                    if request.method == "session.shutdown" {
                        if let Err(error) = require_empty(&request.params) {
                            write_error(&mut output, &request.id, &error).await?;
                            continue;
                        }
                        let active = mutation.take().ok_or(ProtocolFailure::Worker)?;
                        active.cancellation.cancel();
                        let id = active.id.clone();
                        let completion = tokio::time::timeout(CANCELLATION_GRACE, active.task)
                            .await
                            .map_err(|_| ProtocolFailure::CancellationStuck)?;
                        match completion {
                            Ok(Ok(result)) => write_result(&mut output, &id, result).await?,
                            Ok(Err(error)) => write_error(&mut output, &id, &error).await?,
                            Err(_) => {
                                write_error(
                                    &mut output,
                                    &id,
                                    &WireError::new(
                                        "internal",
                                        "management worker failed internally",
                                    ),
                                )
                                .await?;
                            }
                        }
                        write_result(&mut output, &request.id, json!({"accepted": true})).await?;
                        input_task.abort();
                        return Ok(());
                    }
                    write_error(
                        &mut output,
                        &request.id,
                        &WireError::new("busy", "a management operation is active"),
                    )
                    .await?;
                    continue;
                }

                if request.method == "protocol.hello" {
                    if session.is_some() {
                        write_error(
                            &mut output,
                            &request.id,
                            &WireError::new(
                                "invalid_state",
                                "protocol hello was already completed",
                            ),
                        )
                        .await?;
                    } else {
                        match hello(&request.params) {
                            Ok((chosen, result)) => {
                                session = Some(chosen);
                                write_result(&mut output, &request.id, result).await?;
                            }
                            Err(error) => write_error(&mut output, &request.id, &error).await?,
                        }
                    }
                    continue;
                }
                let Some(chosen_session) = session else {
                    write_error(
                        &mut output,
                        &request.id,
                        &WireError::new("invalid_state", "protocol hello must be first"),
                    )
                    .await?;
                    continue;
                };
                if request.method == "session.shutdown" {
                    if let Err(error) = require_empty(&request.params) {
                        write_error(&mut output, &request.id, &error).await?;
                        continue;
                    }
                    write_result(&mut output, &request.id, json!({"accepted": true})).await?;
                    input_task.abort();
                    return Ok(());
                }
                if request.method == "session.initialize" {
                    if chosen_session == Session::Inspect {
                        write_error(
                            &mut output,
                            &request.id,
                            &WireError::new(
                                "wrong_session",
                                "initialization is unavailable in an inspect session",
                            ),
                        )
                        .await?;
                        continue;
                    }
                    if initialized {
                        write_error(
                            &mut output,
                            &request.id,
                            &WireError::new(
                                "invalid_state",
                                "session cannot be initialized in this state",
                            ),
                        )
                        .await?;
                        continue;
                    }
                    match initialize(chosen_session, &request.params).await {
                        Ok(Initialized::Runtime(state)) => {
                            let audio_deliveries = state.audio_deliveries.clone();
                            let schema = utterance_options_schema();
                            let digest = utterance_schema_digest(&schema)
                                .map_err(|_| ProtocolFailure::Worker)?;
                            runtime = Some(state);
                            initialized = true;
                            write_result(
                                &mut output,
                                &request.id,
                                initialize_runtime_result(audio_deliveries, schema, digest),
                            )
                            .await?;
                        }
                        Ok(Initialized::Management(state)) => {
                            management = Some(state);
                            initialized = true;
                            write_result(&mut output, &request.id, json!({"ready":true})).await?;
                        }
                        Err(error) => write_error(&mut output, &request.id, &error).await?,
                    }
                    continue;
                }
                if chosen_session == Session::Inspect {
                    let error = if is_runtime_method(&request.method)
                        || is_management_method(&request.method)
                    {
                        WireError::new(
                            "wrong_session",
                            "method is unavailable in an inspect session",
                        )
                    } else {
                        WireError::new("method_not_supported", "unknown protocol method")
                    };
                    write_error(&mut output, &request.id, &error).await?;
                    continue;
                }
                if !initialized {
                    write_error(
                        &mut output,
                        &request.id,
                        &WireError::new("invalid_state", "session is not initialized"),
                    )
                    .await?;
                    continue;
                }

                match (chosen_session, request.method.as_str()) {
                    (Session::Runtime, "runtime.health") => match require_empty(&request.params) {
                        Ok(()) => {
                            write_result(&mut output, &request.id, json!({"status": "ready"}))
                                .await?
                        }
                        Err(error) => write_error(&mut output, &request.id, &error).await?,
                    },
                    (Session::Runtime, "synthesis.start") => {
                        let params: SynthesisParams = match decode(&request.params) {
                            Ok(params) => params,
                            Err(error) => {
                                write_error(&mut output, &request.id, &error).await?;
                                continue;
                            }
                        };
                        let state = runtime.as_ref().ok_or(ProtocolFailure::Worker)?;
                        let Some(delivery) =
                            selected_delivery(&state.audio_deliveries, &params.audio_delivery)
                        else {
                            write_error(
                                &mut output,
                                &request.id,
                                &WireError::new(
                                    "unsupported_audio_delivery",
                                    "requested audio delivery is unavailable in this runtime",
                                ),
                            )
                            .await?;
                            continue;
                        };
                        if params.text.is_empty()
                            || params.text.contains('\0')
                            || params.text.chars().count() > state.max_text_code_points
                        {
                            write_error(
                                &mut output,
                                &request.id,
                                &WireError::new(
                                    "invalid_text",
                                    "text is empty, contains NUL, or exceeds the negotiated limit",
                                ),
                            )
                            .await?;
                            continue;
                        }
                        let utterance: UtteranceOptions =
                            match serde_json::from_value(Value::Object(params.utterance_options)) {
                                Ok(options) => options,
                                Err(_) => {
                                    write_error(
                                        &mut output,
                                        &request.id,
                                        &WireError::new(
                                            "invalid_utterance_options",
                                            "utterance options are invalid",
                                        ),
                                    )
                                    .await?;
                                    continue;
                                }
                            };
                        if let Err(error) = utterance.validate() {
                            write_error(
                                &mut output,
                                &request.id,
                                &WireError::new("invalid_utterance_options", error.to_string()),
                            )
                            .await?;
                            continue;
                        }
                        synthesis = Some(start_synthesis(
                            request.id,
                            params.text,
                            &utterance,
                            delivery,
                            state,
                        ));
                    }
                    (Session::Runtime, "synthesis.cancel") => {
                        match decode_cancel(&request.params) {
                            Ok(_) => {
                                write_result(&mut output, &request.id, json!({"accepted": false}))
                                    .await?
                            }
                            Err(error) => write_error(&mut output, &request.id, &error).await?,
                        }
                    }
                    (Session::Management, method) => {
                        let state = management.as_ref().ok_or(ProtocolFailure::Worker)?;
                        match method {
                            "provider.validate" => {
                                if let Err(error) = require_empty(&request.params) {
                                    write_error(&mut output, &request.id, &error).await?;
                                } else {
                                    write_result(
                                        &mut output,
                                        &request.id,
                                        validation_result(state),
                                    )
                                    .await?;
                                }
                            }
                            "catalog.items" => match catalog_items(&request.params, state) {
                                Ok(result) => {
                                    write_result(&mut output, &request.id, result).await?
                                }
                                Err(error) => write_error(&mut output, &request.id, &error).await?,
                            },
                            "prepare.plan" => match make_prepare_plan(&request.params, state) {
                                Ok((plan, result)) => {
                                    prepare_plan = Some(plan);
                                    write_result(&mut output, &request.id, result).await?;
                                }
                                Err(error) => write_error(&mut output, &request.id, &error).await?,
                            },
                            "prepare.apply" => match start_prepare_apply(
                                &request.params,
                                state,
                                prepare_plan.as_ref(),
                            ) {
                                Ok(active) => {
                                    mutation = Some(ActiveManagement {
                                        id: request.id,
                                        ..active
                                    })
                                }
                                Err(error) => write_error(&mut output, &request.id, &error).await?,
                            },
                            "remove.plan" => match make_remove_plan(&request.params, state) {
                                Ok((plan, result)) => {
                                    remove_plan = Some(plan);
                                    write_result(&mut output, &request.id, result).await?;
                                }
                                Err(error) => write_error(&mut output, &request.id, &error).await?,
                            },
                            "remove.apply" => match start_remove_apply(
                                &request.params,
                                state,
                                remove_plan.as_ref(),
                            ) {
                                Ok(active) => {
                                    mutation = Some(ActiveManagement {
                                        id: request.id,
                                        ..active
                                    })
                                }
                                Err(error) => write_error(&mut output, &request.id, &error).await?,
                            },
                            "asset.import" => match start_asset_import(&request.params, state) {
                                Ok(active) => {
                                    mutation = Some(ActiveManagement {
                                        id: request.id,
                                        ..active
                                    })
                                }
                                Err(error) => write_error(&mut output, &request.id, &error).await?,
                            },
                            _ if is_runtime_method(method) => {
                                write_error(
                                    &mut output,
                                    &request.id,
                                    &WireError::new(
                                        "wrong_session",
                                        "runtime method is unavailable in a management session",
                                    ),
                                )
                                .await?
                            }
                            _ => {
                                write_error(
                                    &mut output,
                                    &request.id,
                                    &WireError::new(
                                        "method_not_supported",
                                        "unknown protocol method",
                                    ),
                                )
                                .await?
                            }
                        }
                    }
                    (Session::Runtime, method) if is_management_method(method) => {
                        write_error(
                            &mut output,
                            &request.id,
                            &WireError::new(
                                "wrong_session",
                                "management method is unavailable in a runtime session",
                            ),
                        )
                        .await?;
                    }
                    _ => {
                        write_error(
                            &mut output,
                            &request.id,
                            &WireError::new("method_not_supported", "unknown protocol method"),
                        )
                        .await?
                    }
                }
            }
        }
    }
}

async fn next_event(
    input: &mut tokio::sync::mpsc::Receiver<InputEvent>,
    synthesis: Option<&mut ActiveSynthesis>,
    management: Option<&mut ActiveManagement>,
) -> LoopEvent {
    if let Some(active) = synthesis {
        if let Some(cancel_deadline) = active.cancellation_deadline {
            tokio::select! {
                biased;
                () = tokio::time::sleep_until(cancel_deadline) => LoopEvent::CancellationExpired,
                message = input.recv() => LoopEvent::Input(message),
                completion = &mut active.task => LoopEvent::SynthesisDone(completion),
                chunk = active.chunks.recv(), if !active.chunks_closed => LoopEvent::Chunk(chunk),
            }
        } else {
            tokio::select! {
                biased;
                () = tokio::time::sleep_until(active.deadline) => LoopEvent::Deadline,
                message = input.recv() => LoopEvent::Input(message),
                completion = &mut active.task => LoopEvent::SynthesisDone(completion),
                chunk = active.chunks.recv(), if !active.chunks_closed => LoopEvent::Chunk(chunk),
            }
        }
    } else if let Some(active) = management {
        tokio::select! {
            biased;
            completion = &mut active.task => LoopEvent::ManagementDone(completion),
            message = input.recv() => LoopEvent::Input(message),
        }
    } else {
        LoopEvent::Input(input.recv().await)
    }
}

fn normalize_synthesis_completion(
    completion: Result<Result<GenerationSummary, EngineError>, tokio::task::JoinError>,
) -> Result<GenerationSummary, EngineError> {
    completion.unwrap_or(Err(EngineError::Failed))
}

fn start_synthesis(
    id: String,
    text: String,
    utterance: &UtteranceOptions,
    delivery: Delivery,
    state: &RuntimeState,
) -> ActiveSynthesis {
    let (sender, chunks) = tokio::sync::mpsc::channel(4);
    let cancellation = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancellation);
    let engine = Arc::clone(&state.engine);
    let reference = state.assets.reference.clone();
    let timeout = state.timeout;
    let (speed, seed) = utterance.effective_controls();
    let max_audio_bytes = match delivery {
        Delivery::Complete => state.max_audio_bytes.saturating_sub(44),
        Delivery::Incremental => state.max_audio_bytes,
    };
    let task = tokio::task::spawn_blocking(move || {
        engine.generate(
            &text,
            &reference,
            GenerationOptions {
                speed,
                seed,
                timeout,
                max_audio_bytes,
            },
            worker_cancel,
            sender,
        )
    });
    ActiveSynthesis {
        id,
        delivery,
        cancellation,
        chunks,
        chunks_closed: false,
        task,
        pcm: Vec::new(),
        began: false,
        sent_bytes: 0,
        sent_frames: 0,
        deadline: Instant::now() + timeout,
        forced_error: None,
        cancellation_deadline: None,
    }
}

async fn finish_synthesis<W: AsyncWrite + Unpin>(
    output: &mut W,
    active: ActiveSynthesis,
    outcome: Result<GenerationSummary, EngineError>,
) -> Result<(), ProtocolFailure> {
    if let Some(error) = active.forced_error {
        return write_error(output, &active.id, &error).await;
    }
    match outcome {
        Err(error) => write_error(output, &active.id, &engine_error(error)).await,
        Ok(summary) => match active.delivery {
            Delivery::Complete => {
                if active.pcm.len() != summary.byte_length || summary.frame_count == 0 {
                    return write_error(
                        output,
                        &active.id,
                        &WireError::new(
                            "synthesis_failed",
                            "engine callback counts are inconsistent",
                        ),
                    )
                    .await;
                }
                let Some(wav) = pcm16_wav_bytes(SAMPLE_RATE, &active.pcm) else {
                    return write_error(
                        output,
                        &active.id,
                        &WireError::new("synthesis_failed", "could not encode generated audio"),
                    )
                    .await;
                };
                write_result(
                    output,
                    &active.id,
                    json!({"audio": {
                        "format": WAV_FORMAT, "byte_length": wav.len(),
                        "sample_rate_hz": SAMPLE_RATE, "channels": 1
                    }}),
                )
                .await?;
                write_frame(output, AUDIO_KIND, &wav).await
            }
            Delivery::Incremental => {
                if !active.began
                    || active.sent_bytes != summary.byte_length
                    || active.sent_frames != summary.frame_count
                {
                    return write_error(
                        output,
                        &active.id,
                        &WireError::new(
                            "synthesis_failed",
                            "engine produced no audio or inconsistent callback counts",
                        ),
                    )
                    .await;
                }
                write_result(
                    output,
                    &active.id,
                    json!({"audio": {
                        "format": PCM_FORMAT, "byte_length": active.sent_bytes,
                        "sample_rate_hz": SAMPLE_RATE, "channels": 1,
                        "frame_count": active.sent_frames
                    }}),
                )
                .await
            }
        },
    }
}

async fn handle_synthesis_control<W: AsyncWrite + Unpin>(
    output: &mut W,
    request: &Request,
    active: &mut ActiveSynthesis,
) -> Result<bool, ProtocolFailure> {
    match request.method.as_str() {
        "synthesis.cancel" => {
            let params: CancelParams = match decode_cancel(&request.params) {
                Ok(params) => params,
                Err(error) => {
                    write_error(output, &request.id, &error).await?;
                    return Ok(false);
                }
            };
            if params.request_id == active.id {
                active.cancellation.store(true, Ordering::Release);
                active.forced_error = Some(WireError::new("cancelled", "synthesis was cancelled"));
                active.cancellation_deadline = Some(Instant::now() + CANCELLATION_GRACE);
                write_result(output, &request.id, json!({"accepted": true})).await?;
            } else {
                write_result(output, &request.id, json!({"accepted": false})).await?;
            }
            Ok(false)
        }
        "session.shutdown" => {
            if let Err(error) = require_empty(&request.params) {
                write_error(output, &request.id, &error).await?;
                return Ok(false);
            }
            active.cancellation.store(true, Ordering::Release);
            active.forced_error = Some(WireError::new("cancelled", "synthesis was cancelled"));
            Ok(true)
        }
        _ => {
            write_error(
                output,
                &request.id,
                &WireError::new("busy", "a synthesis request is active"),
            )
            .await?;
            Ok(false)
        }
    }
}

async fn await_synthesis_stop(mut active: ActiveSynthesis) -> Result<(), ProtocolFailure> {
    let deadline = Instant::now() + CANCELLATION_GRACE;
    loop {
        tokio::select! {
            chunk = active.chunks.recv(), if !active.chunks_closed => {
                if chunk.is_none() { active.chunks_closed = true; }
            },
            result = &mut active.task => return result.map(|_| ()).map_err(|_| ProtocolFailure::Worker),
            () = tokio::time::sleep_until(deadline) => {
                active.task.abort();
                return Err(ProtocolFailure::CancellationStuck);
            }
        }
    }
}

async fn cancel_workers(
    synthesis: &mut Option<ActiveSynthesis>,
    mutation: &mut Option<ActiveManagement>,
) {
    if let Some(mut active) = synthesis.take() {
        active.cancellation.store(true, Ordering::Release);
        let _ = tokio::time::timeout(CANCELLATION_GRACE, async {
            loop {
                tokio::select! {
                    _ = active.chunks.recv() => {},
                    _ = &mut active.task => return,
                }
            }
        })
        .await;
        active.task.abort();
    }
    if let Some(active) = mutation.take() {
        active.cancellation.cancel();
        active.task.abort();
    }
}

enum Initialized {
    Runtime(RuntimeState),
    Management(ManagementState),
}

async fn initialize(session: Session, value: &Value) -> Result<Initialized, WireError> {
    if session == Session::Management {
        let params: ManagementInitializeParams = decode(value)?;
        let store = Store::new(
            PathBuf::from(params.data_dir),
            PathBuf::from(params.cache_dir),
        )
        .map_err(store_error)?;
        let options = decode_provider_options(params.provider_options, false)?;
        store.validate_local().map_err(store_error)?;
        return Ok(Initialized::Management(ManagementState {
            store,
            provider_options: options,
        }));
    }

    let params: RuntimeInitializeParams = decode(value)?;
    let store = Store::new(
        PathBuf::from(params.data_dir),
        PathBuf::from(params.cache_dir),
    )
    .map_err(store_error)?;
    let options = decode_provider_options(params.provider_options, true)?;
    if params.limits.max_text_code_points == 0
        || params.limits.max_audio_bytes == 0
        || params.limits.synthesis_timeout_ms == 0
        || params.limits.max_text_code_points > MAX_SAFE_INTEGER
        || params.limits.max_audio_bytes > MAX_SAFE_INTEGER
        || params.limits.synthesis_timeout_ms > MAX_SAFE_INTEGER
    {
        return Err(WireError::new(
            "invalid_message",
            "negotiated limits are invalid",
        ));
    }
    let audio_deliveries = resolve_deliveries(&params.accepted_audio_deliveries)?;
    let max_text_code_points = usize::try_from(
        params
            .limits
            .max_text_code_points
            .min(MAX_TEXT_CODE_POINTS as u64),
    )
    .map_err(|_| WireError::new("invalid_message", "text limit is unsupported"))?;
    let max_audio_bytes =
        usize::try_from(params.limits.max_audio_bytes.min(MAX_AUDIO_BYTES as u64))
            .map_err(|_| WireError::new("invalid_message", "audio limit is unsupported"))?;
    let timeout = Duration::from_millis(params.limits.synthesis_timeout_ms);
    if std::time::Instant::now().checked_add(timeout).is_none() {
        return Err(WireError::new(
            "invalid_message",
            "synthesis timeout is too large",
        ));
    }
    let voice_id = options
        .voice
        .clone()
        .ok_or_else(|| WireError::new("invalid_provider_options", "runtime voice is missing"))?;
    let model_id = options
        .model
        .clone()
        .ok_or_else(|| WireError::new("invalid_provider_options", "runtime model is missing"))?;
    let store_copy = store.clone();
    let assets =
        tokio::task::spawn_blocking(move || store_copy.acquire_runtime(&model_id, &voice_id))
            .await
            .map_err(|_| WireError::new("internal", "runtime initialization failed"))?
            .map_err(store_error)?;
    let model_path = assets.model_dir.clone();
    let engine_options = options.engine_options();
    let engine =
        tokio::task::spawn_blocking(move || PocketEngine::create(&model_path, engine_options))
            .await
            .map_err(|_| WireError::new("internal", "engine initialization failed"))?
            .map_err(engine_error)?;
    Ok(Initialized::Runtime(RuntimeState {
        engine: Arc::new(engine),
        assets,
        audio_deliveries,
        max_text_code_points,
        max_audio_bytes,
        timeout,
    }))
}

fn decode_provider_options(
    options: Map<String, Value>,
    runtime: bool,
) -> Result<ProviderOptions, WireError> {
    let options: ProviderOptions = serde_json::from_value(Value::Object(options))
        .map_err(|_| WireError::new("invalid_provider_options", "provider options are invalid"))?;
    let validation = if runtime {
        options.validate_runtime()
    } else {
        options.validate_partial()
    };
    validation.map_err(|error| WireError::new("invalid_provider_options", error.to_string()))?;
    Ok(options)
}

fn initialize_runtime_result(
    audio_deliveries: Vec<AudioDelivery>,
    schema: Value,
    digest: String,
) -> Value {
    json!({
        "ready":true,
        "audio_deliveries":audio_deliveries,
        "utterance_options_schema":schema,
        "utterance_options_schema_digest":digest
    })
}

fn resolve_deliveries(deliveries: &[AudioDelivery]) -> Result<Vec<AudioDelivery>, WireError> {
    if deliveries.is_empty()
        || deliveries
            .iter()
            .enumerate()
            .any(|(index, item)| deliveries[..index].contains(item))
    {
        return Err(WireError::new(
            "invalid_message",
            "accepted audio deliveries must be nonempty and unique",
        ));
    }
    if deliveries.iter().any(|delivery| {
        !matches!(
            (delivery.mode.as_str(), delivery.format.as_str()),
            ("incremental", PCM_FORMAT) | ("complete", WAV_FORMAT)
        )
    }) {
        return Err(WireError::new(
            "invalid_message",
            "accepted audio deliveries contain an unsupported pair",
        ));
    }
    Ok(deliveries.to_vec())
}

fn selected_delivery(available: &[AudioDelivery], requested: &AudioDelivery) -> Option<Delivery> {
    if !available.contains(requested) {
        return None;
    }
    match (requested.mode.as_str(), requested.format.as_str()) {
        ("incremental", PCM_FORMAT) => Some(Delivery::Incremental),
        ("complete", WAV_FORMAT) => Some(Delivery::Complete),
        _ => None,
    }
}

fn hello(value: &Value) -> Result<(Session, Value), WireError> {
    let params: HelloParams = decode(value)?;
    if params.protocol != "utterpipe.tts"
        || !params.versions.contains(&1)
        || params
            .versions
            .iter()
            .any(|version| *version > MAX_SAFE_INTEGER)
    {
        return Err(WireError::new(
            "unsupported_protocol",
            "UtterPipe protocol major 1 was not offered",
        ));
    }
    if params.expected_provider != PROVIDER_SLUG {
        return Err(WireError::new(
            "provider_mismatch",
            "expected provider does not match this provider",
        ));
    }
    if !params
        .utterance_schema_profiles
        .iter()
        .any(|profile| profile == UTTERANCE_SCHEMA_PROFILE)
    {
        return Err(WireError::new(
            "unsupported_schema_profile",
            "utterpipe.utterance-options/1 was not offered",
        ));
    }
    if params.host.name.is_empty() || params.host.version.is_empty() {
        return Err(WireError::new(
            "invalid_message",
            "hello identity fields are empty",
        ));
    }
    let session = match params.session.as_str() {
        "inspect" => Session::Inspect,
        "runtime" => Session::Runtime,
        "management" => Session::Management,
        _ => return Err(WireError::new("invalid_message", "session type is invalid")),
    };
    Ok((
        session,
        json!({
            "protocol": "utterpipe.tts", "version": 1, "framing":"UTP1",
            "provider": {"slug": PROVIDER_SLUG, "name": PROVIDER_NAME, "vendor": PROVIDER_VENDOR, "version": PROVIDER_VERSION},
            "capabilities": ["synthesis", "synthesis.cancel", "catalog", "prepare", "remove", "asset.import"],
            "audio_deliveries": [
                {"mode":"complete", "format":WAV_FORMAT},
                {"mode":"incremental", "format":PCM_FORMAT}
            ],
            "utterance_schema_profile":UTTERANCE_SCHEMA_PROFILE,
            "provider_options_schema":provider_options_schema(),
            "management_options_schema":management_options_schema(),
            "catalogs":[
                {"id":"models", "name":"Models", "description":"Pocket TTS model artifacts usable by this provider.", "item_kind":"model", "patchable_provider_options":["model"], "patchable_utterance_options":[]},
                {"id":"voices", "name":"Voices", "description":"Imported consented reference voices usable by Pocket TTS.", "item_kind":"voice", "patchable_provider_options":["voice"], "patchable_utterance_options":[]}
            ],
            "import_kinds":[
                {"id":"voice", "name":"Voice reference", "media_types":["audio/wav"], "max_source_bytes":MAX_RIFF_REFERENCE_BYTES, "patchable_provider_options":["voice"], "patchable_utterance_options":[]}
            ]
        }),
    ))
}

fn validation_result(state: &ManagementState) -> Value {
    let mut issues = Vec::new();
    let model = state.provider_options.model.as_deref();
    let voice = state.provider_options.voice.as_deref();
    if model.is_none() {
        issues.push(json!({"severity":"error","code":"model_option_missing","message":"no Pocket TTS model is selected","remediation":"select a model from the models catalog"}));
    } else if state.store.model_status() != "installed" {
        issues.push(json!({"severity":"error","code":"model_missing","message":"the selected model is not installed","remediation":"run the host preparation command"}));
    }
    if let Some(voice) = voice {
        let voice_present = state
            .store
            .voice_catalog()
            .is_ok_and(|voices| voices.iter().any(|item| item["id"] == voice));
        if !voice_present {
            issues.push(json!({"severity":"error","code":"voice_missing","message":"the selected voice is not installed","remediation":"import a consented reference voice"}));
        }
    } else {
        issues.push(json!({"severity":"error","code":"voice_option_missing","message":"no Pocket TTS voice is selected","remediation":"select or import a consented reference voice"}));
    }
    let status = if issues.is_empty() {
        "ready"
    } else {
        "incomplete"
    };
    json!({"status": status, "issues": issues})
}

fn catalog_items(value: &Value, state: &ManagementState) -> Result<Value, WireError> {
    if value.get("cursor").is_some_and(Value::is_null) {
        return Err(WireError::new(
            "invalid_message",
            "catalog cursor is invalid",
        ));
    }
    let params: CatalogItemsParams = decode(value)?;
    validate_scope(&params.scope)?;
    if !(1..=256).contains(&params.limit) {
        return Err(WireError::new(
            "invalid_message",
            "catalog limit must be from 1 through 256",
        ));
    }
    let _ = params.refresh;
    let items = match params.catalog_id.as_str() {
        "models" => model_catalog_items(&params.scope, state),
        "voices" => voice_catalog_items(&params.scope, state)?,
        _ => return Err(WireError::new("invalid_message", "catalog ID is unknown")),
    };
    let offset = parse_catalog_cursor(params.cursor.as_deref(), items.len())?;
    let end = (offset + usize::from(params.limit)).min(items.len());
    let page = items[offset..end].to_vec();
    let mut result = json!({"items":page});
    if end < items.len() {
        result["next_cursor"] = Value::String(format!("offset:{end}"));
    }
    Ok(result)
}

fn model_catalog_items(scope: &str, state: &ManagementState) -> Vec<Value> {
    let status = state.store.model_status();
    if (scope == "installed" && status != "installed")
        || (scope == "available" && status != "available")
    {
        return Vec::new();
    }
    let mut item = model_descriptor(status);
    if let Some(object) = item.as_object_mut() {
        object.remove("version");
    }
    item["description"] = Value::String("Pinned English Pocket TTS int8 model.".into());
    item["provider_options_patch"] = json!({"model":MODEL_ID});
    item["utterance_options_patch"] = json!({});
    item["artifacts"] = json!([format!("model:{MODEL_ID}")]);
    vec![item]
}

fn voice_catalog_items(scope: &str, state: &ManagementState) -> Result<Vec<Value>, WireError> {
    if scope == "available" {
        return Ok(Vec::new());
    }
    state
        .store
        .voice_catalog()
        .map_err(store_error)
        .map(|mut voices| {
            for voice in &mut voices {
                let curated = voice["kind"] == "curated";
                if let Some(object) = voice.as_object_mut() {
                    object.remove("kind");
                }
                let id = voice["id"].clone();
                voice["description"] = Value::String(
                    if curated {
                        "Verified curated Pocket TTS reference voice."
                    } else {
                        "User-imported Pocket TTS reference voice."
                    }
                    .into(),
                );
                voice["provider_options_patch"] = json!({"voice":id});
                voice["utterance_options_patch"] = json!({});
                voice["artifacts"] = json!([format!(
                    "voice:{}",
                    voice["id"].as_str().unwrap_or_default()
                )]);
            }
            voices
        })
}

fn parse_catalog_cursor(cursor: Option<&str>, length: usize) -> Result<usize, WireError> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    cursor
        .strip_prefix("offset:")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|offset| *offset <= length)
        .ok_or_else(|| WireError::new("invalid_message", "catalog cursor is invalid"))
}

fn validate_scope(scope: &str) -> Result<(), WireError> {
    if matches!(scope, "all" | "available" | "installed") {
        Ok(())
    } else {
        Err(WireError::new(
            "invalid_message",
            "catalog scope is invalid",
        ))
    }
}

fn make_prepare_plan(
    value: &Value,
    state: &ManagementState,
) -> Result<(PreparePlan, Value), WireError> {
    let params: PreparePlanParams = decode(value)?;
    let _ = params.refresh;
    let status = state.store.model_status().to_owned();
    let id = format!("prepare-{}", PLAN_COUNTER.fetch_add(1, Ordering::Relaxed));
    let actions = if status == "installed" {
        Vec::new()
    } else {
        vec![json!({
            "kind":"download", "artifact":format!("model:{MODEL_ID}"), "source":SOURCE_URL,
            "download_bytes":ARCHIVE_BYTES, "installed_bytes":INSTALLED_BYTES, "sha256":ARCHIVE_SHA256
        })]
    };
    let summary = if status == "installed" {
        "Pinned Pocket TTS model is already installed"
    } else {
        "Install pinned Pocket TTS model; import the selected voice separately if needed"
    };
    Ok((
        PreparePlan {
            id: id.clone(),
            allow_network: params.allow_network,
            status,
        },
        json!({
            "plan_id": id, "summary": summary, "actions": actions, "licenses": licenses()
        }),
    ))
}

fn start_prepare_apply(
    value: &Value,
    state: &ManagementState,
    plan: Option<&PreparePlan>,
) -> Result<ActiveManagement, WireError> {
    let params: PrepareApplyParams = decode(value)?;
    if params.operation_id.is_empty() {
        return Err(WireError::new("invalid_message", "operation ID is empty"));
    }
    let plan = plan
        .filter(|plan| plan.id == params.plan_id)
        .ok_or_else(|| WireError::new("plan_stale", "prepare plan is absent or stale"))?;
    if state.store.model_status() != plan.status {
        return Err(WireError::new(
            "plan_stale",
            "installed model state changed",
        ));
    }
    let accepted: HashSet<_> = params
        .accepted_licenses
        .iter()
        .map(String::as_str)
        .collect();
    if !LICENSE_IDS.iter().all(|license| accepted.contains(license)) {
        return Err(WireError::new(
            "license_required",
            "all three model disclosures must be accepted",
        ));
    }
    if plan.status != "installed" && !plan.allow_network {
        return Err(WireError::new(
            "network_error",
            "prepare plan did not authorize network access",
        ));
    }
    let store = state.store.clone();
    let licenses = params.accepted_licenses;
    let mutation = store.begin_mutation().map_err(store_error)?;
    if store.model_status() != plan.status {
        return Err(WireError::new(
            "plan_stale",
            "model state changed while acquiring the mutation lease",
        ));
    }
    let cancellation = CancellationToken::new();
    let task_cancel = cancellation.clone();
    let task = tokio::spawn(async move {
        if store.model_status() != "installed" {
            let archive = download_archive(&store, task_cancel.clone()).await?;
            let install_archive = archive.clone();
            let install_store = store.clone();
            let install_cancel = task_cancel.clone();
            tokio::task::spawn_blocking(move || {
                install_store.install_model_from_archive_locked(
                    &install_archive,
                    &licenses,
                    &mutation,
                    || install_cancel.is_cancelled(),
                )
            })
            .await
            .map_err(|_| WireError::new("internal", "model installation worker failed"))?
            .map_err(store_error)?;
            if archive.starts_with(store.cache_dir().join("tmp")) {
                let _ = tokio::fs::remove_file(archive).await;
            }
        }
        Ok(json!({"status":"ready","installed":[format!("model:{MODEL_ID}")]}))
    });
    Ok(ActiveManagement {
        id: String::new(),
        cancellation,
        task,
    })
}

async fn download_archive(
    store: &Store,
    cancellation: CancellationToken,
) -> Result<PathBuf, WireError> {
    let cached = store
        .cache_dir()
        .join("downloads")
        .join("sha256")
        .join(ARCHIVE_SHA256);
    if cached.is_file() {
        return Ok(cached);
    }
    let temporary_root = store.cache_dir().join("tmp");
    tokio::fs::create_dir_all(&temporary_root)
        .await
        .map_err(|_| WireError::new("network_error", "could not create download cache"))?;
    let temporary = temporary_root.join(format!(
        "download-{}-{}.tmp",
        std::process::id(),
        PLAN_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let redirect = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 5 {
            return attempt.error("redirect limit exceeded");
        }
        let allowed = attempt.url().scheme() == "https"
            && attempt.url().host_str().is_some_and(|host| {
                host == "github.com"
                    || host == "release-assets.githubusercontent.com"
                    || host.ends_with(".githubusercontent.com")
            });
        if allowed {
            attempt.follow()
        } else {
            attempt.stop()
        }
    });
    let client = reqwest::Client::builder()
        .tls_backend_rustls()
        .redirect(redirect)
        .connect_timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| WireError::new("network_error", "could not create model downloader"))?;
    let operation = async {
        let response = client
            .get(SOURCE_URL)
            .send()
            .await
            .map_err(|_| WireError::new("network_error", "model download failed"))?;
        if !response.status().is_success() {
            return Err(WireError::new(
                "network_error",
                "model source returned an error",
            ));
        }
        if response
            .content_length()
            .is_some_and(|size| size > ARCHIVE_BYTES + 65_536)
        {
            return Err(WireError::new(
                "integrity_error",
                "model download is larger than declared",
            ));
        }
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await
            .map_err(|_| WireError::new("network_error", "could not create model download"))?;
        let mut stream = response.bytes_stream();
        let mut total = 0_u64;
        let mut digest = Sha256::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk
                .map_err(|_| WireError::new("network_error", "model download was interrupted"))?;
            total = total.saturating_add(chunk.len() as u64);
            if total > ARCHIVE_BYTES + 65_536 {
                return Err(WireError::new(
                    "integrity_error",
                    "model download is larger than declared",
                ));
            }
            digest.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|_| WireError::new("network_error", "could not store model download"))?;
        }
        file.sync_all()
            .await
            .map_err(|_| WireError::new("network_error", "could not sync model download"))?;
        if format!("{:x}", digest.finalize()) != ARCHIVE_SHA256 || total != ARCHIVE_BYTES {
            return Err(WireError::new(
                "integrity_error",
                "model download checksum or size is wrong",
            ));
        }
        Ok(temporary.clone())
    };
    let result = tokio::select! {
        () = cancellation.cancelled() => Err(WireError::new("cancelled", "model download was cancelled")),
        result = tokio::time::timeout(Duration::from_secs(600), operation) => result.map_err(|_| WireError::new("timeout", "model download timed out"))?,
    };
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}

fn make_remove_plan(
    value: &Value,
    state: &ManagementState,
) -> Result<(RemovePlan, Value), WireError> {
    let params: RemovePlanParams = decode(value)?;
    if params.purge || params.artifacts.is_empty() {
        return Err(WireError::new(
            "invalid_message",
            "purge is not supported and artifacts must be nonempty",
        ));
    }
    let unique: HashSet<_> = params.artifacts.iter().collect();
    if unique.len() != params.artifacts.len() {
        return Err(WireError::new(
            "invalid_message",
            "remove artifacts must be unique",
        ));
    }
    let mut tokens = HashMap::new();
    let mut actions = Vec::new();
    for artifact in &params.artifacts {
        let token = state.store.artifact_token(artifact).map_err(store_error)?;
        tokens.insert(artifact.clone(), token);
        let reclaimed = if artifact.starts_with("model:") {
            INSTALLED_BYTES
        } else {
            0
        };
        actions.push(json!({"kind":"remove","artifact":artifact,"reclaimed_bytes":reclaimed}));
    }
    let id = format!("remove-{}", PLAN_COUNTER.fetch_add(1, Ordering::Relaxed));
    Ok((
        RemovePlan {
            id: id.clone(),
            artifacts: params.artifacts,
            tokens,
        },
        json!({
            "plan_id":id,"summary":"Remove selected provider assets","actions":actions
        }),
    ))
}

fn start_remove_apply(
    value: &Value,
    state: &ManagementState,
    plan: Option<&RemovePlan>,
) -> Result<ActiveManagement, WireError> {
    let params: RemoveApplyParams = decode(value)?;
    if params.operation_id.is_empty() {
        return Err(WireError::new("invalid_message", "operation ID is empty"));
    }
    let plan = plan
        .filter(|plan| plan.id == params.plan_id)
        .ok_or_else(|| WireError::new("plan_stale", "remove plan is absent or stale"))?;
    for (artifact, token) in &plan.tokens {
        if !matches!(state.store.artifact_token(artifact), Ok(current) if current == *token) {
            return Err(WireError::new(
                "plan_stale",
                "asset state changed after planning",
            ));
        }
    }
    let store = state.store.clone();
    let mutation = store.begin_mutation().map_err(store_error)?;
    for (artifact, token) in &plan.tokens {
        if !matches!(store.artifact_token(artifact), Ok(current) if current == *token) {
            return Err(WireError::new(
                "plan_stale",
                "asset state changed while acquiring the mutation lease",
            ));
        }
    }
    let artifacts = plan.artifacts.clone();
    let cancellation = CancellationToken::new();
    let task_cancel = cancellation.clone();
    let task = tokio::task::spawn_blocking(move || {
        let removed = store
            .remove_artifacts_locked(&artifacts, &mutation, || task_cancel.is_cancelled())
            .map_err(store_error)?;
        Ok(json!({"status":"ready","removed":removed}))
    });
    Ok(ActiveManagement {
        id: String::new(),
        cancellation,
        task,
    })
}

fn start_asset_import(
    value: &Value,
    state: &ManagementState,
) -> Result<ActiveManagement, WireError> {
    let params: AssetImportParams = decode(value)?;
    if params.operation_id.is_empty() {
        return Err(WireError::new("invalid_message", "operation ID is empty"));
    }
    if params.kind != "voice" {
        return Err(WireError::new(
            "invalid_message",
            "asset import kind is unknown",
        ));
    }
    if !params.consent_confirmed {
        return Err(WireError::new(
            "license_required",
            "voice import requires consent confirmation",
        ));
    }
    let source = PathBuf::from(params.source_path);
    if !source.is_absolute() || !valid_voice_id(&params.requested_id) {
        return Err(WireError::new(
            "invalid_message",
            "voice source or ID is invalid",
        ));
    }
    let id = params.requested_id;
    let result_id = id.clone();
    let store = state.store.clone();
    let mutation = store.begin_mutation().map_err(store_error)?;
    let source_bytes = std::fs::symlink_metadata(&source)
        .map_err(|_| WireError::new("resource_missing", "voice source is unavailable"))?;
    if !source_bytes.file_type().is_file() || source_bytes.len() > MAX_RIFF_REFERENCE_BYTES {
        return Err(WireError::new(
            "invalid_message",
            "voice source must be a regular classic RIFF/WAVE file",
        ));
    }
    let cancellation = CancellationToken::new();
    let task_cancel = cancellation.clone();
    let task = tokio::task::spawn_blocking(move || {
        store
            .import_voice_locked(&source, &id, true, 30.0, &mutation, || {
                task_cancel.is_cancelled()
            })
            .map_err(store_error)?;
        Ok(json!({
            "artifact_id":format!("voice:{result_id}"),
            "status":"installed",
            "provider_options_patch":{"voice":result_id},
            "utterance_options_patch":{}
        }))
    });
    Ok(ActiveManagement {
        id: String::new(),
        cancellation,
        task,
    })
}

fn engine_error(error: EngineError) -> WireError {
    match error {
        EngineError::InvalidOptions => {
            WireError::new("invalid_provider_options", error.to_string())
        }
        EngineError::Unavailable => WireError::new("engine_unavailable", error.to_string()),
        EngineError::Cancelled => WireError::new("cancelled", error.to_string()),
        EngineError::Timeout => WireError::new("timeout", error.to_string()),
        EngineError::OutputTooLarge => WireError::new("output_too_large", error.to_string()),
        EngineError::Failed => WireError::new("synthesis_failed", error.to_string()),
    }
}

fn store_error(error: StoreError) -> WireError {
    let code = match error {
        StoreError::InvalidPaths => "invalid_message",
        StoreError::ModelMissing | StoreError::VoiceMissing => "resource_missing",
        StoreError::Integrity | StoreError::UnsafeArchive => "integrity_error",
        StoreError::ResourceBusy => "resource_busy",
        StoreError::InvalidVoiceId | StoreError::VoiceConflict => "invalid_provider_options",
        StoreError::ConsentRequired | StoreError::LicenseRequired => "license_required",
        StoreError::InvalidAudio => "invalid_message",
        StoreError::Cancelled => "cancelled",
        StoreError::Schema | StoreError::Io => "engine_unavailable",
    };
    WireError::new(code, error.to_string())
}

fn valid_voice_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    let edge = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    (1..=64).contains(&bytes.len())
        && bytes.first().is_some_and(edge)
        && bytes.last().is_some_and(edge)
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn decode_cancel(value: &Value) -> Result<CancelParams, WireError> {
    let params: CancelParams = decode(value)?;
    if !valid_request_id(&params.request_id) {
        return Err(WireError::new(
            "invalid_message",
            "cancel request ID is invalid",
        ));
    }
    Ok(params)
}

/// Download and install the pinned model for an explicit direct CLI command.
///
/// # Errors
///
/// Returns a redacted human-facing message for download, integrity, lock, license,
/// extraction, or installation failures.
pub async fn prepare_model_network(store: Store, accepted: Vec<String>) -> Result<(), String> {
    let mutation = store.begin_mutation().map_err(|error| error.to_string())?;
    let cancellation = CancellationToken::new();
    let archive = download_archive(&store, cancellation.clone())
        .await
        .map_err(|error| error.message)?;
    let install_archive = archive.clone();
    let store_copy = store.clone();
    tokio::task::spawn_blocking(move || {
        store_copy.install_model_from_archive_locked(&install_archive, &accepted, &mutation, || {
            cancellation.is_cancelled()
        })
    })
    .await
    .map_err(|_| "model installation worker failed".to_owned())?
    .map_err(|error| error.to_string())?;
    if archive.starts_with(store.cache_dir().join("tmp")) {
        let _ = tokio::fs::remove_file(archive).await;
    }
    Ok(())
}

fn is_runtime_method(method: &str) -> bool {
    matches!(
        method,
        "runtime.health" | "synthesis.start" | "synthesis.cancel"
    )
}

fn is_management_method(method: &str) -> bool {
    matches!(
        method,
        "provider.validate"
            | "catalog.items"
            | "prepare.plan"
            | "prepare.apply"
            | "remove.plan"
            | "remove.apply"
            | "asset.import"
    )
}

fn utterance_schema_digest(schema: &Value) -> Result<String, serde_json::Error> {
    let canonical = serde_json_canonicalizer::to_vec(schema)?;
    let digest = Sha256::digest(canonical);
    Ok(format!("sha256:{digest:x}"))
}

async fn read_input<R: AsyncRead + Unpin>(
    mut input: R,
    sender: tokio::sync::mpsc::Sender<InputEvent>,
) {
    loop {
        match read_frame(&mut input).await {
            Ok(Some(value)) => {
                if sender.send(InputEvent::Control(value)).await.is_err() {
                    return;
                }
            }
            Ok(None) => {
                let _ = sender.send(InputEvent::Eof).await;
                return;
            }
            Err(error) => {
                let _ = sender.send(InputEvent::Fatal(error)).await;
                return;
            }
        }
    }
}

async fn read_frame<R: AsyncRead + Unpin>(input: &mut R) -> Result<Option<Value>, ProtocolFailure> {
    let mut header = [0_u8; 12];
    let count = input
        .read(&mut header[..1])
        .await
        .map_err(|_| ProtocolFailure::Input)?;
    if count == 0 {
        return Ok(None);
    }
    input
        .read_exact(&mut header[1..])
        .await
        .map_err(|_| ProtocolFailure::Frame("truncated header"))?;
    if &header[..4] != b"UTP1" || header[4] != CONTROL_KIND || header[5..8] != [0, 0, 0] {
        return Err(ProtocolFailure::Frame("invalid header"));
    }
    let length = u32::from_be_bytes([header[8], header[9], header[10], header[11]]) as usize;
    if !(2..=MAX_CONTROL_BYTES).contains(&length) {
        return Err(ProtocolFailure::Frame("control length is out of bounds"));
    }
    let mut payload = vec![0_u8; length];
    input
        .read_exact(&mut payload)
        .await
        .map_err(|_| ProtocolFailure::Frame("truncated payload"))?;
    decode_strict_json(&payload).map(Some)
}

async fn write_result<W: AsyncWrite + Unpin>(
    output: &mut W,
    id: &str,
    result: Value,
) -> Result<(), ProtocolFailure> {
    write_control(output, &json!({"kind":"response","id":id,"result":result})).await
}

async fn write_error<W: AsyncWrite + Unpin>(
    output: &mut W,
    id: &str,
    error: &WireError,
) -> Result<(), ProtocolFailure> {
    write_control(
        output,
        &json!({"kind":"response","id":id,"error":{"code":error.code,"message":error.message}}),
    )
    .await
}

async fn write_event<W: AsyncWrite + Unpin>(
    output: &mut W,
    event: &str,
    params: Value,
) -> Result<(), ProtocolFailure> {
    write_control(
        output,
        &json!({"kind":"event","event":event,"params":params}),
    )
    .await
}

async fn write_control<W: AsyncWrite + Unpin>(
    output: &mut W,
    value: &Value,
) -> Result<(), ProtocolFailure> {
    let payload = serde_json::to_vec(value).map_err(|_| ProtocolFailure::Output)?;
    if payload.len() > MAX_CONTROL_BYTES {
        return Err(ProtocolFailure::Output);
    }
    write_frame(output, CONTROL_KIND, &payload).await
}

async fn write_frame<W: AsyncWrite + Unpin>(
    output: &mut W,
    kind: u8,
    payload: &[u8],
) -> Result<(), ProtocolFailure> {
    let length = u32::try_from(payload.len()).map_err(|_| ProtocolFailure::Output)?;
    let mut header = [0_u8; 12];
    header[..4].copy_from_slice(b"UTP1");
    header[4] = kind;
    header[8..].copy_from_slice(&length.to_be_bytes());
    output
        .write_all(&header)
        .await
        .map_err(|_| ProtocolFailure::Output)?;
    output
        .write_all(payload)
        .await
        .map_err(|_| ProtocolFailure::Output)?;
    output.flush().await.map_err(|_| ProtocolFailure::Output)
}

fn parse_request(value: &Value) -> Result<Request, (Option<String>, WireError)> {
    let Some(object) = value.as_object() else {
        return Err((
            None,
            WireError::new("invalid_message", "control payload must be an object"),
        ));
    };
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| valid_request_id(id))
        .map(str::to_owned);
    if object.get("kind").and_then(Value::as_str) != Some("request") {
        return Err((
            id,
            WireError::new("invalid_message", "control kind must be request"),
        ));
    }
    let Some(id) = id else {
        return Err((
            None,
            WireError::new("invalid_message", "request ID is invalid"),
        ));
    };
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Err((
            Some(id),
            WireError::new("invalid_message", "method is missing"),
        ));
    };
    let Some(params) = object.get("params").filter(|value| value.is_object()) else {
        return Err((
            Some(id),
            WireError::new("invalid_message", "params must be an object"),
        ));
    };
    Ok(Request {
        id,
        method: method.to_owned(),
        params: params.clone(),
    })
}

fn valid_request_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn decode<T: for<'de> Deserialize<'de>>(value: &Value) -> Result<T, WireError> {
    serde_json::from_value(value.clone())
        .map_err(|_| WireError::new("invalid_message", "request parameters are invalid"))
}

fn require_empty(value: &Value) -> Result<(), WireError> {
    if value.as_object().is_some_and(Map::is_empty) {
        Ok(())
    } else {
        Err(WireError::new(
            "invalid_message",
            "request parameters must be empty",
        ))
    }
}

fn decode_strict_json(payload: &[u8]) -> Result<Value, ProtocolFailure> {
    let mut decoder = serde_json::Deserializer::from_slice(payload);
    let value =
        StrictValue::deserialize(&mut decoder).map_err(|_| ProtocolFailure::InvalidControl)?;
    decoder.end().map_err(|_| ProtocolFailure::InvalidControl)?;
    Ok(value.0)
}

struct StrictValue(Value);
impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        decoder.deserialize_any(StrictVisitor)
    }
}
struct StrictVisitor;
impl<'de> Visitor<'de> for StrictVisitor {
    type Value = StrictValue;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JSON without duplicate keys")
    }
    fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(v)))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(v))))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(v))))
    }
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
        Number::from_f64(v)
            .map(Value::Number)
            .map(StrictValue)
            .ok_or_else(|| E::custom("non-finite number"))
    }
    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(v.to_owned())))
    }
    fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(v)))
    }
    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }
    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(v) = seq.next_element::<StrictValue>()? {
            values.push(v.0);
        }
        Ok(StrictValue(Value::Array(values)))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut values = Map::new();
        while let Some((k, v)) = map.next_entry::<String, StrictValue>()? {
            if values.insert(k, v.0).is_some() {
                return Err(serde::de::Error::custom("duplicate key"));
            }
        }
        Ok(StrictValue(Value::Object(values)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_json_is_rejected() {
        assert!(decode_strict_json(br#"{"a":1,"a":2}"#).is_err());
    }
    #[test]
    fn delivery_set_preserves_host_order_and_request_selects_exactly() {
        let offered = vec![
            AudioDelivery {
                mode: "complete".into(),
                format: WAV_FORMAT.into(),
            },
            AudioDelivery {
                mode: "incremental".into(),
                format: PCM_FORMAT.into(),
            },
        ];
        assert_eq!(resolve_deliveries(&offered).unwrap(), offered);
        assert_eq!(
            selected_delivery(&offered, &offered[1]),
            Some(Delivery::Incremental)
        );
    }

    #[tokio::test]
    async fn blocking_worker_panic_becomes_synthesis_failure() {
        let completion =
            tokio::task::spawn_blocking(|| -> Result<GenerationSummary, EngineError> {
                panic!("synthetic engine panic")
            })
            .await;
        assert!(matches!(
            normalize_synthesis_completion(completion),
            Err(EngineError::Failed)
        ));
    }

    #[test]
    fn cancellation_target_must_be_a_wire_request_id() {
        assert!(decode_cancel(&json!({"request_id":"synth-1"})).is_ok());
        assert!(decode_cancel(&json!({"request_id":""})).is_err());
        assert!(decode_cancel(&json!({"request_id":"has space"})).is_err());
    }
}
