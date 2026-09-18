use std::{convert::Infallible, sync::atomic::Ordering, time::Duration};

use axum::{
    extract::{Extension, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response, Sse, sse::Event},
};
use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use thiserror::Error;
use tokio::time::{MissedTickBehavior, interval};
use uuid::Uuid;

use crate::{
    api::AppState,
    auth::AuthenticatedOwner,
    contracts::{ApiErrorBody, ApiErrorResponse},
};

const WORKSPACE_ID: &str = "workspace-default";
const EVENT_RETENTION: i64 = 2_000;
const EVENT_BATCH: i64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeEventKind {
    HostConnectionChanged,
    DiscoveryRunChanged,
    ProjectionChanged,
    OnboardingChanged,
    ProjectAgentChanged,
    MonitorScheduleChanged,
    MonitorRunChanged,
}

impl ChangeEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HostConnectionChanged => "host.connection.changed",
            Self::DiscoveryRunChanged => "discovery.run.changed",
            Self::ProjectionChanged => "projection.changed",
            Self::OnboardingChanged => "onboarding.changed",
            Self::ProjectAgentChanged => "project_agent.changed",
            Self::MonitorScheduleChanged => "monitor.schedule.changed",
            Self::MonitorRunChanged => "monitor.run.changed",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct ChangeEventPayload {
    kind: String,
    subject_ref: String,
    revision: i64,
    committed_at: String,
    summary: Value,
}

#[derive(Debug)]
struct StoredEvent {
    cursor: i64,
    kind: String,
    subject_ref: String,
    revision: i64,
    committed_at: String,
    summary: Value,
}

#[derive(Debug, Error)]
pub enum EventError {
    #[error("event cursor is invalid")]
    InvalidCursor,
    #[error("event storage is unavailable")]
    Storage(#[source] sqlx::Error),
}

impl IntoResponse for EventError {
    fn into_response(self) -> Response {
        let request_id = Uuid::new_v4().to_string();
        let (status, code, message) = match self {
            Self::InvalidCursor => (
                StatusCode::BAD_REQUEST,
                "INVALID_EVENT_CURSOR",
                "Last-Event-ID 必须是非负整数",
            ),
            Self::Storage(error) => {
                tracing::error!(%request_id, error = %error, "event storage failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "EVENT_STORAGE_UNAVAILABLE",
                    "变化通知暂不可用，请直接刷新快照",
                )
            }
        };
        (
            status,
            axum::Json(ApiErrorResponse {
                error: ApiErrorBody {
                    code: code.to_owned(),
                    message: message.to_owned(),
                    details: json!({}),
                    request_id,
                },
            }),
        )
            .into_response()
    }
}

pub async fn publish(
    pool: &SqlitePool,
    kind: ChangeEventKind,
    subject_ref: &str,
    revision: i64,
    summary: Value,
) -> Result<i64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let cursor = publish_in_transaction(&mut tx, kind, subject_ref, revision, summary).await?;
    tx.commit().await?;
    Ok(cursor)
}

pub(crate) async fn publish_in_transaction(
    tx: &mut Transaction<'_, Sqlite>,
    kind: ChangeEventKind,
    subject_ref: &str,
    revision: i64,
    summary: Value,
) -> Result<i64, sqlx::Error> {
    let result = sqlx::query(
        "INSERT INTO change_events(
            workspace_id, kind, subject_ref, revision, summary_json, committed_at
         ) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(WORKSPACE_ID)
    .bind(kind.as_str())
    .bind(subject_ref.chars().take(512).collect::<String>())
    .bind(revision)
    .bind(summary.to_string())
    .bind(now())
    .execute(&mut **tx)
    .await?;
    let cursor = result.last_insert_rowid();
    sqlx::query(
        "DELETE FROM change_events
         WHERE workspace_id = ? AND cursor <= (
             SELECT COALESCE(MAX(cursor), 0) - ? FROM change_events WHERE workspace_id = ?
         )",
    )
    .bind(WORKSPACE_ID)
    .bind(EVENT_RETENTION)
    .bind(WORKSPACE_ID)
    .execute(&mut **tx)
    .await?;
    Ok(cursor)
}

pub fn event_kind_for_request(method: &Method, path: &str) -> Option<ChangeEventKind> {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
        || path.contains("/auth/")
        || path.contains("/exports/")
    {
        return None;
    }
    if path.contains("discovery-runs") {
        return Some(ChangeEventKind::DiscoveryRunChanged);
    }
    if path.contains("project-agents") || path.ends_with("/agent") {
        return Some(ChangeEventKind::ProjectAgentChanged);
    }
    if path.contains("technical-projects")
        || path.contains("project-targets")
        || path.contains("deployment-candidates")
        || path.contains("/deployments/")
    {
        return Some(ChangeEventKind::ProjectionChanged);
    }
    // Monitoring handlers publish their own state transitions so idempotent
    // HTTP replays do not create false change events.
    if path.contains("monitor-schedules") || path.contains("monitor-runs") {
        return None;
    }
    if path.contains("connection-tests")
        || path.contains("host-key-confirmations")
        || path.ends_with("/hosts")
        || (method == Method::PATCH && path.contains("/hosts/"))
        || (method == Method::DELETE && path.contains("/hosts/"))
    {
        return Some(ChangeEventKind::HostConnectionChanged);
    }
    if path.contains("projection-drafts")
        || path.contains("layouts")
        || path.contains("ignore-rules")
        || path.contains("/projects/")
        || path.ends_with("/workspace")
    {
        return Some(ChangeEventKind::ProjectionChanged);
    }
    if path.contains("onboarding") || path.contains("model-provider") {
        return Some(ChangeEventKind::OnboardingChanged);
    }
    None
}

#[utoipa::path(
    get,
    path = "/api/v1/events/stream",
    tag = "m4",
    params(("Last-Event-ID" = Option<i64>, Header, description = "Last committed event cursor")),
    responses(
        (status = 200, description = "Authenticated SSE change summary stream", content_type = "text/event-stream"),
        (status = 400, body = ApiErrorResponse),
        (status = 401, body = ApiErrorResponse)
    )
)]
pub async fn stream(
    State(state): State<AppState>,
    Extension(owner): Extension<AuthenticatedOwner>,
    headers: HeaderMap,
) -> Result<Response, EventError> {
    let requested = last_event_id(&headers)?;
    let (minimum, maximum) = cursor_bounds(&state.pool).await?;
    let expired = requested > 0 && minimum.is_some_and(|value| requested < value.saturating_sub(1));
    let pool = state.pool.clone();
    let shutdown_requested = state.shutdown_requested.clone();
    let session_id = owner.session_id;
    let stream = async_stream::stream! {
        let mut cursor = requested;
        if expired {
            cursor = maximum.unwrap_or(requested);
            let reset = json!({
                "kind": "stream.reset",
                "reason": "cursor_expired",
                "snapshot_required": true,
                "latest_cursor": cursor,
            });
            yield Ok::<Event, Infallible>(Event::default()
                .id(cursor.to_string())
                .event("stream.reset")
                .json_data(reset)
                .expect("reset payload is serializable"));
        }

        let mut poll = interval(Duration::from_millis(500));
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut heartbeat = interval(Duration::from_secs(15));
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
        poll.tick().await;
        heartbeat.tick().await;
        loop {
            if shutdown_requested.load(Ordering::Acquire) {
                break;
            }
            tokio::select! {
                _ = poll.tick() => {
                    if let Some(session_id) = session_id.as_deref() {
                        match session_is_active(&pool, session_id).await {
                            Ok(true) => {}
                            Ok(false) => break,
                            Err(error) => {
                                tracing::error!(error = %error, "SSE session revalidation failed");
                                break;
                            }
                        }
                    }
                    match load_after(&pool, cursor).await {
                        Ok(events) => {
                            for stored in events {
                                cursor = stored.cursor;
                                let payload = ChangeEventPayload {
                                    kind: stored.kind.clone(),
                                    subject_ref: stored.subject_ref,
                                    revision: stored.revision,
                                    committed_at: stored.committed_at,
                                    summary: stored.summary,
                                };
                                yield Ok(Event::default()
                                    .id(stored.cursor.to_string())
                                    .event(stored.kind)
                                    .json_data(payload)
                                    .expect("change payload is serializable"));
                            }
                        }
                        Err(error) => {
                            tracing::error!(error = %error, "SSE polling failed; client will retry from its last cursor");
                            break;
                        }
                    }
                }
                _ = heartbeat.tick() => {
                    yield Ok(Event::default().comment("heartbeat"));
                }
            }
        }
    };
    let mut response = Sse::new(stream).into_response();
    response
        .headers_mut()
        .insert("x-accel-buffering", "no".parse().expect("static header"));
    Ok(response)
}

async fn session_is_active(pool: &SqlitePool, session_id: &str) -> Result<bool, sqlx::Error> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM owner_sessions
         WHERE session_id = ? AND revoked_at IS NULL AND expires_at > ?",
    )
    .bind(session_id)
    .bind(now())
    .fetch_one(pool)
    .await?;
    Ok(count == 1)
}

async fn cursor_bounds(pool: &SqlitePool) -> Result<(Option<i64>, Option<i64>), EventError> {
    let row = sqlx::query(
        "SELECT MIN(cursor) AS minimum, MAX(cursor) AS maximum
         FROM change_events WHERE workspace_id = ?",
    )
    .bind(WORKSPACE_ID)
    .fetch_one(pool)
    .await
    .map_err(EventError::Storage)?;
    Ok((
        row.try_get("minimum").map_err(EventError::Storage)?,
        row.try_get("maximum").map_err(EventError::Storage)?,
    ))
}

async fn load_after(pool: &SqlitePool, cursor: i64) -> Result<Vec<StoredEvent>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT cursor, kind, subject_ref, revision, summary_json, committed_at
         FROM change_events
         WHERE workspace_id = ? AND cursor > ?
         ORDER BY cursor ASC LIMIT ?",
    )
    .bind(WORKSPACE_ID)
    .bind(cursor)
    .bind(EVENT_BATCH)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let summary: String = row.try_get("summary_json")?;
            Ok(StoredEvent {
                cursor: row.try_get("cursor")?,
                kind: row.try_get("kind")?,
                subject_ref: row.try_get("subject_ref")?,
                revision: row.try_get("revision")?,
                committed_at: row.try_get("committed_at")?,
                summary: serde_json::from_str(&summary).unwrap_or_else(|_| json!({})),
            })
        })
        .collect()
}

fn last_event_id(headers: &HeaderMap) -> Result<i64, EventError> {
    let Some(value) = headers.get("last-event-id") else {
        return Ok(0);
    };
    let parsed = value
        .to_str()
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value >= 0)
        .ok_or(EventError::InvalidCursor)?;
    Ok(parsed)
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_paths_map_to_domain_event_types() {
        assert_eq!(
            event_kind_for_request(&Method::POST, "/api/v1/hosts/h/discovery-runs"),
            Some(ChangeEventKind::DiscoveryRunChanged)
        );
        assert_eq!(
            event_kind_for_request(&Method::POST, "/api/v1/hosts/h/connection-tests"),
            Some(ChangeEventKind::HostConnectionChanged)
        );
        assert_eq!(
            event_kind_for_request(&Method::PATCH, "/api/v1/projection-drafts/d"),
            Some(ChangeEventKind::ProjectionChanged)
        );
        assert_eq!(
            event_kind_for_request(&Method::POST, "/api/v1/onboarding-sessions"),
            Some(ChangeEventKind::OnboardingChanged)
        );
        assert_eq!(
            event_kind_for_request(&Method::POST, "/api/v1/technical-projects/project-1/agent"),
            Some(ChangeEventKind::ProjectAgentChanged)
        );
        assert_eq!(
            event_kind_for_request(&Method::POST, "/api/v1/hosts/h/monitor-schedules"),
            None
        );
        assert_eq!(
            event_kind_for_request(&Method::POST, "/api/v1/hosts/h/monitor-runs"),
            None
        );
    }
}
