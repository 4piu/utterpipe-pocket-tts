use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{engine::EngineOptions, model::MODEL_ID, xn_bundle::APRIL_MODEL_ID};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderOptions {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub model: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub voice: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub num_threads: Option<u32>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub max_reference_audio_seconds: Option<f64>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub voice_embedding_cache_capacity: Option<u32>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UtteranceOptions {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub speed: Option<f64>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub seed: Option<u32>,
}

fn optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("model must select the pinned Pocket TTS model")]
    Model,
    #[error("voice must be a valid imported voice ID")]
    Voice,
    #[error("num_threads must be from 1 through 64")]
    Threads,
    #[error("speed must be from 0.5 through 2.0")]
    Speed,
    #[error("speed is unavailable for the XN model")]
    XnSpeed,
    #[error("max_reference_audio_seconds must be from 1 through 30")]
    ReferenceLength,
    #[error("voice_embedding_cache_capacity must be from 1 through 128")]
    VoiceCache,
    #[error("reference-length and sherpa voice-cache options are unavailable for the XN model")]
    XnOptions,
    #[error("runtime provider options require model and voice")]
    Incomplete,
}

impl ProviderOptions {
    /// Validate supplied fixed provider options without requiring runtime selectors.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when any supplied option is invalid.
    pub fn validate_partial(&self) -> Result<(), ConfigError> {
        if self
            .model
            .as_deref()
            .is_some_and(|model| !matches!(model, MODEL_ID | APRIL_MODEL_ID))
        {
            return Err(ConfigError::Model);
        }
        if self
            .voice
            .as_ref()
            .is_some_and(|voice| !valid_voice_id(voice))
        {
            return Err(ConfigError::Voice);
        }
        validate_engine_controls(
            self.num_threads,
            self.max_reference_audio_seconds,
            self.voice_embedding_cache_capacity,
        )?;
        if self.model.as_deref() == Some(APRIL_MODEL_ID)
            && (self.max_reference_audio_seconds.is_some()
                || self.voice_embedding_cache_capacity.is_some())
        {
            return Err(ConfigError::XnOptions);
        }
        Ok(())
    }

    /// Validate the complete configuration required to start synthesis.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when a selector is absent or an option is invalid.
    pub fn validate_runtime(&self) -> Result<(), ConfigError> {
        self.validate_partial()?;
        if self.model.is_none() || self.voice.is_none() {
            return Err(ConfigError::Incomplete);
        }
        Ok(())
    }

    #[must_use]
    pub fn engine_options(&self) -> EngineOptions {
        let defaults = EngineOptions::default();
        EngineOptions {
            num_threads: self.num_threads.unwrap_or(defaults.num_threads),
            max_reference_audio_seconds: self
                .max_reference_audio_seconds
                .unwrap_or(defaults.max_reference_audio_seconds),
            voice_embedding_cache_capacity: self
                .voice_embedding_cache_capacity
                .unwrap_or(defaults.voice_embedding_cache_capacity),
        }
    }

    #[must_use]
    pub fn xn_num_threads(&self) -> u32 {
        self.num_threads
            .unwrap_or(if cfg!(target_arch = "aarch64") { 4 } else { 8 })
    }
}

impl UtteranceOptions {
    /// Validate per-utterance controls.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when a supplied control is outside its schema.
    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_request_controls(self.speed)
    }

    /// Validate per-utterance controls against the selected runtime.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::XnSpeed`] when XN receives a speed field.
    pub fn validate_for_model(&self, model_id: &str) -> Result<(), ConfigError> {
        self.validate()?;
        if model_id == APRIL_MODEL_ID && self.speed.is_some() {
            return Err(ConfigError::XnSpeed);
        }
        Ok(())
    }

    #[must_use]
    pub fn effective_controls(&self) -> (f64, u32) {
        (self.speed.unwrap_or(1.0), self.seed.unwrap_or(42))
    }
}

fn validate_engine_controls(
    num_threads: Option<u32>,
    max_reference_audio_seconds: Option<f64>,
    voice_embedding_cache_capacity: Option<u32>,
) -> Result<(), ConfigError> {
    if num_threads.is_some_and(|value| !(1..=64).contains(&value)) {
        return Err(ConfigError::Threads);
    }
    if max_reference_audio_seconds
        .is_some_and(|value| !value.is_finite() || !(1.0..=30.0).contains(&value))
    {
        return Err(ConfigError::ReferenceLength);
    }
    if voice_embedding_cache_capacity.is_some_and(|value| !(1..=128).contains(&value)) {
        return Err(ConfigError::VoiceCache);
    }
    Ok(())
}

fn validate_request_controls(speed: Option<f64>) -> Result<(), ConfigError> {
    if speed.is_some_and(|value| !value.is_finite() || !(0.5..=2.0).contains(&value)) {
        return Err(ConfigError::Speed);
    }
    Ok(())
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

#[must_use]
pub fn provider_options_schema() -> Value {
    let mut schema = management_options_schema();
    schema["required"] = json!(["model", "voice"]);
    schema
}

#[must_use]
pub fn management_options_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "allOf": [{
            "if": {
                "properties": {"model": {"const": APRIL_MODEL_ID}},
                "required": ["model"]
            },
            "then": {
                "properties": {
                    "max_reference_audio_seconds": false,
                    "voice_embedding_cache_capacity": false
                }
            }
        }],
        "properties": {
            "model": {"type":"string", "enum":[MODEL_ID, APRIL_MODEL_ID]},
            "voice": {"type":"string", "minLength":1, "maxLength":64, "pattern":"^[a-z0-9](?:[a-z0-9._-]{0,62}[a-z0-9])?$"},
            "num_threads": {"type":"integer", "minimum":1, "maximum":64},
            "max_reference_audio_seconds": {"type":"number", "minimum":1.0, "maximum":30.0, "default":10.0},
            "voice_embedding_cache_capacity": {"type":"integer", "minimum":1, "maximum":128, "default":16}
        }
    })
}

#[must_use]
pub fn utterance_options_schema() -> Value {
    json!({
        "$schema":"https://json-schema.org/draft/2020-12/schema",
        "type":"object",
        "additionalProperties":false,
        "maxProperties":64,
        "properties":{
            "speed":{
                "type":"number", "minimum":0.5, "maximum":2.0,
                "title":"Speaking speed",
                "description":"Sets the Pocket TTS speaking-speed multiplier for this utterance.",
                "x-utterpipe":{
                    "default_behavior":"Omission uses the provider's 1.0 speed.",
                    "use_when":"Use when this utterance should be spoken faster or slower.",
                    "omit_when":"Omit when normal speaking speed is suitable.",
                    "unit":"multiplier"
                }
            },
            "seed":{
                "type":"integer", "minimum":0, "maximum":4294967295_u64,
                "title":"Generation seed",
                "description":"Sets the Pocket TTS sampling seed for this utterance.",
                "x-utterpipe":{
                    "default_behavior":"Omission uses the provider's seed 42.",
                    "use_when":"Use when a repeatable alternative rendering is wanted.",
                    "omit_when":"Omit unless generation variation or reproducibility matters.",
                    "effects":["Changing the seed may change pronunciation timing and vocal detail."]
                }
            }
        }
    })
}

#[must_use]
pub fn utterance_options_schema_for_model(model_id: &str) -> Value {
    let mut schema = utterance_options_schema();
    if model_id == APRIL_MODEL_ID {
        schema["properties"]
            .as_object_mut()
            .expect("static schema properties must be an object")
            .remove("speed");
    }
    schema
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_requires_selectors_but_management_accepts_empty_options() {
        let empty: ProviderOptions = serde_json::from_value(json!({})).unwrap();
        assert!(empty.validate_partial().is_ok());
        assert!(matches!(
            empty.validate_runtime(),
            Err(ConfigError::Incomplete)
        ));
    }

    #[test]
    fn request_controls_use_request_or_engine_defaults() {
        let utterance: UtteranceOptions = serde_json::from_value(json!({"speed":1.2})).unwrap();
        assert_eq!(utterance.effective_controls(), (1.2, 42));
        assert_eq!(UtteranceOptions::default().effective_controls(), (1.0, 42));
    }

    #[test]
    fn xn_rejects_sherpa_only_controls_and_non_neutral_speed() {
        let options: ProviderOptions = serde_json::from_value(json!({
            "model": APRIL_MODEL_ID,
            "voice": "test",
            "voice_embedding_cache_capacity": 4
        }))
        .unwrap();
        assert!(matches!(
            options.validate_runtime(),
            Err(ConfigError::XnOptions)
        ));
        let utterance: UtteranceOptions = serde_json::from_value(json!({"speed":1.2})).unwrap();
        assert!(matches!(
            utterance.validate_for_model(APRIL_MODEL_ID),
            Err(ConfigError::XnSpeed)
        ));
        let neutral: UtteranceOptions = serde_json::from_value(json!({"speed":1.0})).unwrap();
        assert!(matches!(
            neutral.validate_for_model(APRIL_MODEL_ID),
            Err(ConfigError::XnSpeed)
        ));
        assert!(
            utterance_options_schema_for_model(APRIL_MODEL_ID)["properties"]["speed"].is_null()
        );
    }
}
