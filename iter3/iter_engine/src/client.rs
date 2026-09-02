//! Blocking HTTP client for the iter_data API.

use serde_json::Value;
use std::time::Duration;

#[derive(Clone)]
pub struct Api {
    pub base: String,
    pub token: String,
    http: reqwest::blocking::Client,
}

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub body: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}: {}", self.status, self.body)
    }
}

impl Api {
    pub fn new(base: &str, token: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("http client"),
        }
    }

    fn handle(resp: reqwest::blocking::Response) -> Result<Value, ApiError> {
        let status = resp.status().as_u16();
        let body = resp.text().unwrap_or_default();
        if (200..300).contains(&status) {
            Ok(serde_json::from_str(&body).unwrap_or(Value::Null))
        } else {
            Err(ApiError { status, body })
        }
    }

    pub fn get(&self, path: &str) -> Result<Value, ApiError> {
        let resp = self
            .http
            .get(format!("{}{}", self.base, path))
            .bearer_auth(&self.token)
            .send()
            .map_err(|e| ApiError { status: 0, body: e.to_string() })?;
        Self::handle(resp)
    }

    pub fn put(&self, path: &str, body: &Value) -> Result<Value, ApiError> {
        let resp = self
            .http
            .put(format!("{}{}", self.base, path))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .map_err(|e| ApiError { status: 0, body: e.to_string() })?;
        Self::handle(resp)
    }

    pub fn post(&self, path: &str, body: &Value) -> Result<Value, ApiError> {
        let resp = self
            .http
            .post(format!("{}{}", self.base, path))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .map_err(|e| ApiError { status: 0, body: e.to_string() })?;
        Self::handle(resp)
    }
}
