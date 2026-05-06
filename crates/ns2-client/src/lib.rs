/// HTTP client types for communicating with the ns2 server.
///
/// This crate owns the reqwest dependency for all ns2 server HTTP calls.
/// The `anthropic` crate owns reqwest for Anthropic API calls.
pub use reqwest::{Client, Error, Response, StatusCode};
