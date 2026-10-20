//! Compatibility name for the unified [`super::engine::AnthropicStreamEngine`].
//!
//! The FSM was historically split between this module and `translator.rs`.
//! Both adapters now delegate to the single engine.

pub use super::engine::AnthropicStreamEngine as AnthropicBlockStreamer;
