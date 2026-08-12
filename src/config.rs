use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use thiserror::Error;

use crate::xn_bundle::APRIL_MODEL_ID;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderOptions {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub model: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub voice: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub num_threads: Option<u32>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UtteranceOptions {
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
            .is_some_and(|model| model != APRIL_MODEL_ID)
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
        if self
            .num_threads
            .is_some_and(|value| !(1..=64).contains(&value))
        {
            return Err(ConfigError::Threads);
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
        Ok(())
    }

    /// Validate per-utterance controls against the selected runtime.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Model`] when an unsupported model is supplied.
    pub fn validate_for_model(&self, model_id: &str) -> Result<(), ConfigError> {
        if model_id != APRIL_MODEL_ID {
            return Err(ConfigError::Model);
        }
        self.validate()
    }

    #[must_use]
    pub fn effective_seed(&self) -> u32 {
        self.seed.unwrap_or(42)
    }
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
        "properties": {
            "model": {"type":"string", "const":APRIL_MODEL_ID},
            "voice": {"type":"string", "minLength":1, "maxLength":64, "pattern":"^[a-z0-9](?:[a-z0-9._-]{0,62}[a-z0-9])?$"},
            "num_threads": {"type":"integer", "minimum":1, "maximum":64}
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
    debug_assert_eq!(model_id, APRIL_MODEL_ID);
    utterance_options_schema()
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
    fn request_seed_uses_request_or_engine_default() {
        let utterance: UtteranceOptions = serde_json::from_value(json!({"seed":7})).unwrap();
        assert_eq!(utterance.effective_seed(), 7);
        assert_eq!(UtteranceOptions::default().effective_seed(), 42);
    }

    #[test]
    fn runtime_accepts_only_xn_controls_and_rejects_speed() {
        let options: ProviderOptions = serde_json::from_value(json!({
            "model": APRIL_MODEL_ID,
            "voice": "test",
            "num_threads": 4
        }))
        .unwrap();
        assert!(options.validate_runtime().is_ok());
        assert!(serde_json::from_value::<UtteranceOptions>(json!({"speed":1.2})).is_err());
        assert!(
            utterance_options_schema_for_model(APRIL_MODEL_ID)["properties"]["speed"].is_null()
        );
    }
}
