// Created: 2026-07-09 by Constructor Tech
//! Async job and batch models — from `schemas/async/`.
//!
//! Covers background jobs and batches wrapping the core request/response types.
//! Timestamps are RFC 3339 `date-time` strings, modeled as `DateTime<Utc>`.

use chrono::{DateTime, Utc};

use crate::models::core::{CreateResponseBody, ResponseResource};

// ---------------------------------------------------------------------------
// Job
// ---------------------------------------------------------------------------

/// State of an async (background) job.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Job {
    /// Unique job identifier.
    pub id: String,
    /// Current job status.
    pub status: JobStatus,
    /// Original request body.
    pub request: CreateResponseBody,
    /// Response resource, once the job completes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ResponseResource>,
    /// Error details, if the job failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<AsyncError>,
    /// Job creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Result expiration timestamp.
    pub expires_at: DateTime<Utc>,
}

/// Status of an async job.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

// ---------------------------------------------------------------------------
// Batch
// ---------------------------------------------------------------------------

/// State of a batch of requests.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Batch {
    /// Unique batch identifier.
    pub id: String,
    /// Current batch status.
    pub status: BatchStatus,
    /// Requests in the batch.
    pub requests: Vec<BatchRequest>,
    /// Batch creation timestamp.
    pub created_at: DateTime<Utc>,
}

/// An individual request within a batch.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct BatchRequest {
    /// Consumer-provided identifier for correlation.
    pub custom_id: String,
    /// The request body.
    pub request: CreateResponseBody,
    /// Response resource, once this request completes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ResponseResource>,
    /// Error details, if this request failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<AsyncError>,
}

/// Status of a batch.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BatchStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

// ---------------------------------------------------------------------------
// AsyncError
// ---------------------------------------------------------------------------

/// Error information for a failed job or batch request.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct AsyncError {
    /// Error code.
    pub code: String,
    /// Human-readable error message.
    pub message: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::core::CreateResponseBody;

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn job_roundtrips_with_nested_request() {
        let job = Job {
            id: "job_1".into(),
            status: JobStatus::Running,
            request: CreateResponseBody {
                model: Some("gpt".into()),
                ..Default::default()
            },
            result: None,
            error: Some(AsyncError {
                code: "rate_limited".into(),
                message: "slow down".into(),
            }),
            created_at: ts("2026-07-09T00:00:00Z"),
            expires_at: ts("2026-07-10T00:00:00Z"),
        };
        let value = serde_json::to_value(&job).unwrap();
        let back: Job = serde_json::from_value(value).unwrap();
        assert_eq!(job, back);
    }

    #[test]
    fn batch_roundtrips_with_nested_requests() {
        let batch = Batch {
            id: "batch_1".into(),
            status: BatchStatus::InProgress,
            requests: vec![BatchRequest {
                custom_id: "c1".into(),
                request: CreateResponseBody {
                    model: Some("gpt".into()),
                    ..Default::default()
                },
                result: None,
                error: None,
            }],
            created_at: ts("2026-07-09T00:00:00Z"),
        };
        let value = serde_json::to_value(&batch).unwrap();
        let back: Batch = serde_json::from_value(value).unwrap();
        assert_eq!(batch, back);
    }
}
