//! ua-backend: LLM provider adapters for UnixAgent.
//!
//! This crate provides a unified interface for communicating with LLM APIs
//! (Anthropic, OpenAI, etc.) using streaming responses.

pub mod anthropic;
pub mod mock;
pub mod sse;

pub use anthropic::AnthropicClient;
pub use mock::{MockConfig, MockResponse};
