use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::error::ApiError;
use crate::session::AuthenticatedSession;
use crate::state::AppState;

#[derive(Serialize)]
pub struct WebhookResponse {
    pub id: String,
    pub url: String,
    pub events: Vec<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize)]
pub struct CreateWebhookRequest {
    pub url: String,
    pub events: Vec<String>,
    pub secret: Option<String>,
}

pub async fn list_webhooks(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedSession>,
) -> Result<Json<Vec<WebhookResponse>>, ApiError> {
    let records = sqlx::query(
        r#"
        SELECT id, url, events, created_at
        FROM webhooks
        WHERE user_id = $1
        ORDER BY created_at DESC
        "#
    )
    .bind(auth.user.id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let responses = records
        .into_iter()
        .map(|r| WebhookResponse {
            id: r.get::<uuid::Uuid, _>("id").to_string(),
            url: r.get("url"),
            events: r.get("events"),
            created_at: r.get("created_at"),
        })
        .collect();

    Ok(Json(responses))
}

pub async fn create_webhook(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedSession>,
    Json(req): Json<CreateWebhookRequest>,
) -> Result<Json<WebhookResponse>, ApiError> {
    if req.url.is_empty() || req.events.is_empty() {
        return Err(ApiError::BadRequest("url and events are required".into()));
    }
    if let Err(reason) = crate::webhooks::validate_webhook_url(&req.url).await {
        return Err(ApiError::BadRequest(format!("invalid webhook url: {reason}")));
    }

    let record = sqlx::query(
        r#"
        INSERT INTO webhooks (user_id, url, events, secret)
        VALUES ($1, $2, $3, $4)
        RETURNING id, url, events, created_at
        "#
    )
    .bind(auth.user.id)
    .bind(&req.url)
    .bind(&req.events)
    .bind(&req.secret)
    .fetch_one(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(Json(WebhookResponse {
        id: record.get::<uuid::Uuid, _>("id").to_string(),
        url: record.get("url"),
        events: record.get("events"),
        created_at: record.get("created_at"),
    }))
}

pub async fn delete_webhook(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedSession>,
    Path(id): Path<String>,
) -> Result<Json<()>, ApiError> {
    let webhook_id = Uuid::parse_str(&id)
        .map_err(|_| ApiError::BadRequest("invalid webhook id".into()))?;

    let res = sqlx::query(
        "DELETE FROM webhooks WHERE id = $1 AND user_id = $2"
    )
    .bind(webhook_id)
    .bind(auth.user.id)
    .execute(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound("webhook not found"));
    }

    Ok(Json(()))
}
