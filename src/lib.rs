pub mod audio;
pub mod engine;
pub mod model;
pub mod protocol;
pub mod store;

pub const PROVIDER_SLUG: &str = "pocket-tts";
pub const PROVIDER_NAME: &str = "Pocket TTS provider";
pub const PROVIDER_VENDOR: &str = "UtterPipe contributors";
pub const PROVIDER_VERSION: &str = env!("CARGO_PKG_VERSION");
