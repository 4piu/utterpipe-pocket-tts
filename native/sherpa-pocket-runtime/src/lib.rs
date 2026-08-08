//! Narrow safe binding to sherpa-onnx's Pocket TTS C API.
//!
//! ABI declarations and wrapper structure are derived from sherpa-onnx 1.13.4,
//! Copyright (c) 2022-2026 Next-gen Kaldi contributors and Copyright (c) 2023
//! Xiaomi Corporation, under Apache-2.0.

use std::{
    collections::HashMap,
    ffi::{CString, c_char, c_void},
    ptr, slice,
};

type ProgressCallback = dyn FnMut(&[f32], f32) -> bool;
type BoxedProgressCallback = Box<ProgressCallback>;

#[derive(Clone, Debug, Default)]
pub struct OfflineTtsPocketModelConfig {
    pub lm_flow: Option<String>,
    pub lm_main: Option<String>,
    pub encoder: Option<String>,
    pub decoder: Option<String>,
    pub text_conditioner: Option<String>,
    pub vocab_json: Option<String>,
    pub token_scores_json: Option<String>,
    pub voice_embedding_cache_capacity: i32,
}

#[derive(Clone, Debug, Default)]
pub struct OfflineTtsModelConfig {
    pub pocket: OfflineTtsPocketModelConfig,
    pub num_threads: i32,
    pub debug: bool,
}

#[derive(Clone, Debug, Default)]
pub struct OfflineTtsConfig {
    pub model: OfflineTtsModelConfig,
}

#[derive(Clone, Debug)]
pub struct GenerationConfig {
    pub silence_scale: f32,
    pub speed: f32,
    pub reference_audio: Option<Vec<f32>>,
    pub reference_sample_rate: i32,
    pub num_steps: i32,
    pub extra: Option<HashMap<String, serde_json::Value>>,
}

impl Default for GenerationConfig {
    fn default() -> Self {
        Self {
            silence_scale: 0.2,
            speed: 1.0,
            reference_audio: None,
            reference_sample_rate: 0,
            num_steps: 5,
            extra: None,
        }
    }
}

pub struct GeneratedAudio {
    pointer: *const raw::GeneratedAudio,
}

impl GeneratedAudio {
    #[must_use]
    pub fn samples(&self) -> &[f32] {
        // SAFETY: `pointer` is a live native allocation owned by this value.
        let audio = unsafe { &*self.pointer };
        if audio.samples.is_null() || audio.length <= 0 {
            &[]
        } else {
            // SAFETY: sherpa owns at least `length` samples until destruction.
            unsafe { slice::from_raw_parts(audio.samples, audio.length as usize) }
        }
    }

    #[must_use]
    pub fn sample_rate(&self) -> i32 {
        // SAFETY: `pointer` is a live native allocation owned by this value.
        unsafe { (*self.pointer).sample_rate }
    }
}

impl Drop for GeneratedAudio {
    fn drop(&mut self) {
        // SAFETY: the pointer came from sherpa and is destroyed exactly once.
        unsafe { raw::destroy_generated_audio(self.pointer) };
    }
}

pub struct OfflineTts {
    pointer: *const raw::OfflineTts,
}

// SAFETY: a Pocket runtime is used synchronously; sherpa documents independent
// instances as thread-safe and the provider serializes calls to each instance.
unsafe impl Send for OfflineTts {}
// SAFETY: shared access only invokes sherpa's const generation API, and the
// provider does not overlap generation calls on one runtime.
unsafe impl Sync for OfflineTts {}

impl OfflineTts {
    #[must_use]
    pub fn create(config: &OfflineTtsConfig) -> Option<Self> {
        let mut strings = Vec::new();
        let pocket = &config.model.pocket;
        let raw_pocket = raw::PocketModelConfig {
            lm_flow: to_c_pointer(&pocket.lm_flow, &mut strings)?,
            lm_main: to_c_pointer(&pocket.lm_main, &mut strings)?,
            encoder: to_c_pointer(&pocket.encoder, &mut strings)?,
            decoder: to_c_pointer(&pocket.decoder, &mut strings)?,
            text_conditioner: to_c_pointer(&pocket.text_conditioner, &mut strings)?,
            vocab_json: to_c_pointer(&pocket.vocab_json, &mut strings)?,
            token_scores_json: to_c_pointer(&pocket.token_scores_json, &mut strings)?,
            voice_embedding_cache_capacity: pocket.voice_embedding_cache_capacity,
        };
        // SAFETY: every field in these C configuration structs is an integer,
        // float, or pointer for which an all-zero value is valid and means off.
        let mut model: raw::ModelConfig = unsafe { std::mem::zeroed() };
        model.pocket = raw_pocket;
        model.num_threads = config.model.num_threads;
        model.debug = i32::from(config.model.debug);
        let raw_config = raw::Config {
            model,
            rule_fsts: ptr::null(),
            maximum_sentences: 0,
            rule_fars: ptr::null(),
            silence_scale: 0.2,
        };
        // SAFETY: pointers remain live for the synchronous constructor call.
        let pointer = unsafe { raw::create(&raw_config) };
        (!pointer.is_null()).then_some(Self { pointer })
    }

    #[must_use]
    pub fn sample_rate(&self) -> i32 {
        // SAFETY: `pointer` is live for the lifetime of this value.
        unsafe { raw::sample_rate(self.pointer) }
    }

    pub fn generate_with_config<F>(
        &self,
        text: &str,
        config: &GenerationConfig,
        callback: Option<F>,
    ) -> Option<GeneratedAudio>
    where
        F: FnMut(&[f32], f32) -> bool + 'static,
    {
        let text = CString::new(text).ok()?;
        let extra = CString::new(
            config
                .extra
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .ok()?
                .unwrap_or_else(|| "{}".to_owned()),
        )
        .ok()?;
        let (reference_audio, reference_audio_length) =
            config
                .reference_audio
                .as_ref()
                .map_or((ptr::null(), 0), |samples| {
                    i32::try_from(samples.len())
                        .map(|length| (samples.as_ptr(), length))
                        .unwrap_or((ptr::null(), -1))
                });
        if reference_audio_length < 0 {
            return None;
        }
        let raw_config = raw::GenerationConfig {
            silence_scale: config.silence_scale,
            speed: config.speed,
            speaker_id: 0,
            reference_audio,
            reference_audio_length,
            reference_sample_rate: config.reference_sample_rate,
            reference_text: ptr::null(),
            steps: config.num_steps,
            extra: extra.as_ptr(),
        };
        let (callback, callback_argument) = callback.map_or((None, ptr::null_mut()), |callback| {
            let boxed: Box<BoxedProgressCallback> = Box::new(Box::new(callback));
            (
                Some(progress_trampoline as unsafe extern "C" fn(_, _, _, _) -> _),
                Box::into_raw(boxed).cast::<c_void>(),
            )
        });
        // SAFETY: all input buffers and the callback allocation remain live for
        // this synchronous call; sherpa does not retain them afterward.
        let pointer = unsafe {
            raw::generate(
                self.pointer,
                text.as_ptr(),
                &raw_config,
                callback,
                callback_argument,
            )
        };
        if !callback_argument.is_null() {
            // SAFETY: this reconstructs the one allocation created above after
            // the synchronous native call has stopped using it.
            unsafe {
                drop(Box::from_raw(
                    callback_argument.cast::<BoxedProgressCallback>(),
                ))
            };
        }
        (!pointer.is_null()).then_some(GeneratedAudio { pointer })
    }
}

impl Drop for OfflineTts {
    fn drop(&mut self) {
        // SAFETY: the pointer came from sherpa and is destroyed exactly once.
        unsafe { raw::destroy(self.pointer) };
    }
}

fn to_c_pointer(value: &Option<String>, storage: &mut Vec<CString>) -> Option<*const c_char> {
    let Some(value) = value else {
        return Some(ptr::null());
    };
    let value = CString::new(value.as_str()).ok()?;
    let pointer = value.as_ptr();
    storage.push(value);
    Some(pointer)
}

unsafe extern "C" fn progress_trampoline(
    samples: *const f32,
    length: i32,
    progress: f32,
    argument: *mut c_void,
) -> i32 {
    if argument.is_null() {
        return 0;
    }
    // SAFETY: `argument` is the boxed callback allocated for this call.
    let callback = unsafe { &mut *argument.cast::<BoxedProgressCallback>() };
    let samples = if samples.is_null() || length <= 0 {
        &[]
    } else {
        // SAFETY: sherpa guarantees the callback slice for this invocation.
        unsafe { slice::from_raw_parts(samples, length as usize) }
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(samples, progress)))
        .ok()
        .is_some_and(|keep_going| keep_going)
        .into()
}

#[allow(non_camel_case_types)]
mod raw {
    use super::{c_char, c_void};

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct VitsModelConfig {
        model: *const c_char,
        lexicon: *const c_char,
        tokens: *const c_char,
        data_dir: *const c_char,
        noise_scale: f32,
        noise_scale_w: f32,
        length_scale: f32,
        dict_dir: *const c_char,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct MatchaModelConfig {
        acoustic_model: *const c_char,
        vocoder: *const c_char,
        lexicon: *const c_char,
        tokens: *const c_char,
        data_dir: *const c_char,
        noise_scale: f32,
        length_scale: f32,
        dict_dir: *const c_char,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct KokoroModelConfig {
        model: *const c_char,
        voices: *const c_char,
        tokens: *const c_char,
        data_dir: *const c_char,
        length_scale: f32,
        dict_dir: *const c_char,
        lexicon: *const c_char,
        lang: *const c_char,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct KittenModelConfig {
        model: *const c_char,
        voices: *const c_char,
        tokens: *const c_char,
        data_dir: *const c_char,
        length_scale: f32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct ZipvoiceModelConfig {
        tokens: *const c_char,
        encoder: *const c_char,
        decoder: *const c_char,
        vocoder: *const c_char,
        data_dir: *const c_char,
        lexicon: *const c_char,
        feature_scale: f32,
        time_shift: f32,
        target_rms: f32,
        guidance_scale: f32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct PocketModelConfig {
        pub lm_flow: *const c_char,
        pub lm_main: *const c_char,
        pub encoder: *const c_char,
        pub decoder: *const c_char,
        pub text_conditioner: *const c_char,
        pub vocab_json: *const c_char,
        pub token_scores_json: *const c_char,
        pub voice_embedding_cache_capacity: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct SupertonicModelConfig {
        duration_predictor: *const c_char,
        text_encoder: *const c_char,
        vector_estimator: *const c_char,
        vocoder: *const c_char,
        tts_json: *const c_char,
        unicode_indexer: *const c_char,
        voice_style: *const c_char,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct ModelConfig {
        vits: VitsModelConfig,
        pub num_threads: i32,
        pub debug: i32,
        provider: *const c_char,
        matcha: MatchaModelConfig,
        kokoro: KokoroModelConfig,
        kitten: KittenModelConfig,
        zipvoice: ZipvoiceModelConfig,
        pub pocket: PocketModelConfig,
        supertonic: SupertonicModelConfig,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Config {
        pub model: ModelConfig,
        pub rule_fsts: *const c_char,
        pub maximum_sentences: i32,
        pub rule_fars: *const c_char,
        pub silence_scale: f32,
    }

    #[repr(C)]
    pub struct GenerationConfig {
        pub silence_scale: f32,
        pub speed: f32,
        pub speaker_id: i32,
        pub reference_audio: *const f32,
        pub reference_audio_length: i32,
        pub reference_sample_rate: i32,
        pub reference_text: *const c_char,
        pub steps: i32,
        pub extra: *const c_char,
    }

    #[repr(C)]
    pub struct GeneratedAudio {
        pub samples: *const f32,
        pub length: i32,
        pub sample_rate: i32,
    }

    #[repr(C)]
    pub struct OfflineTts {
        private: [u8; 0],
    }

    pub type Callback = Option<unsafe extern "C" fn(*const f32, i32, f32, *mut c_void) -> i32>;

    unsafe extern "C" {
        #[link_name = "SherpaOnnxCreateOfflineTts"]
        pub fn create(config: *const Config) -> *const OfflineTts;
        #[link_name = "SherpaOnnxDestroyOfflineTts"]
        pub fn destroy(tts: *const OfflineTts);
        #[link_name = "SherpaOnnxOfflineTtsSampleRate"]
        pub fn sample_rate(tts: *const OfflineTts) -> i32;
        #[link_name = "SherpaOnnxOfflineTtsGenerateWithConfig"]
        pub fn generate(
            tts: *const OfflineTts,
            text: *const c_char,
            config: *const GenerationConfig,
            callback: Callback,
            argument: *mut c_void,
        ) -> *const GeneratedAudio;
        #[link_name = "SherpaOnnxDestroyOfflineTtsGeneratedAudio"]
        pub fn destroy_generated_audio(audio: *const GeneratedAudio);
    }
}
