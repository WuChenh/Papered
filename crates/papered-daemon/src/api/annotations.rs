//! User annotations on papers: star ratings and free-text comments.
//!
//! These endpoints back the paper-detail "Rating" and "Comments" widgets. A
//! paper has at most one rating (1–5, replaceable/deletable) and any number
//! of comments.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use papered::store::vector::PaperComment;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::types::{
    ApiResult, ApiStatusResult, bad_request_msg, map_err, require_paper, validate_paper_id,
};
use crate::AppState;

/// Minimum/maximum allowed star rating.
const MIN_RATING: i64 = 1;
const MAX_RATING: i64 = 5;

/// Maximum comment length in Unicode characters (not bytes) — generous for
/// real notes, small enough to keep the list view and DB rows sane.
const MAX_COMMENT_CHARS: usize = 4000;

#[derive(Debug, Deserialize)]
pub struct SetRatingRequest {
    pub rating: i64,
}

#[derive(Debug, Serialize)]
pub struct RatingResponse {
    pub paper_id: String,
    /// `None` when the paper has not been rated yet.
    pub rating: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct AddCommentRequest {
    pub content: String,
}

/// `GET /api/v1/papers/{id}/rating`
pub async fn get_rating(
    State(state): State<Arc<AppState>>,
    Path(paper_id): Path<String>,
) -> ApiResult<RatingResponse> {
    validate_paper_id(&paper_id)?;
    let rating = state
        .store
        .get_paper_rating(&paper_id)
        .await
        .map_err(map_err)?;
    Ok(Json(RatingResponse { paper_id, rating }))
}

/// `PUT /api/v1/papers/{id}/rating` — create or replace the star rating.
pub async fn set_rating(
    State(state): State<Arc<AppState>>,
    Path(paper_id): Path<String>,
    Json(req): Json<SetRatingRequest>,
) -> ApiResult<RatingResponse> {
    require_paper(&state, &paper_id).await?;
    if !(MIN_RATING..=MAX_RATING).contains(&req.rating) {
        return Err(bad_request_msg(format!(
            "Rating must be between {MIN_RATING} and {MAX_RATING}, got {}",
            req.rating
        )));
    }
    state
        .store
        .set_paper_rating(&paper_id, req.rating)
        .await
        .map_err(map_err)?;
    Ok(Json(RatingResponse {
        paper_id,
        rating: Some(req.rating),
    }))
}

/// `DELETE /api/v1/papers/{id}/rating` — clear the rating.
pub async fn delete_rating(
    State(state): State<Arc<AppState>>,
    Path(paper_id): Path<String>,
) -> ApiStatusResult {
    validate_paper_id(&paper_id)?;
    state
        .store
        .delete_paper_rating(&paper_id)
        .await
        .map_err(map_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/papers/{id}/comments` — list comments, oldest first.
pub async fn list_comments(
    State(state): State<Arc<AppState>>,
    Path(paper_id): Path<String>,
) -> ApiResult<Vec<PaperComment>> {
    validate_paper_id(&paper_id)?;
    let comments = state
        .store
        .list_paper_comments(&paper_id)
        .await
        .map_err(map_err)?;
    Ok(Json(comments))
}

/// `POST /api/v1/papers/{id}/comments` — add a comment, returning the stored
/// record (with generated id and timestamp).
pub async fn add_comment(
    State(state): State<Arc<AppState>>,
    Path(paper_id): Path<String>,
    Json(req): Json<AddCommentRequest>,
) -> ApiResult<PaperComment> {
    require_paper(&state, &paper_id).await?;
    let content = req.content.trim();
    if content.is_empty() {
        return Err(bad_request_msg("Comment content must not be empty"));
    }
    let char_count = content.chars().count();
    if char_count > MAX_COMMENT_CHARS {
        return Err(bad_request_msg(format!(
            "Comment is too long: {char_count} characters exceeds the {MAX_COMMENT_CHARS}-character limit"
        )));
    }
    let comment = state
        .store
        .add_paper_comment(&paper_id, content)
        .await
        .map_err(map_err)?;
    Ok(Json(comment))
}

/// `DELETE /api/v1/papers/{id}/comments/{comment_id}`
pub async fn delete_comment(
    State(state): State<Arc<AppState>>,
    Path((paper_id, comment_id)): Path<(String, i64)>,
) -> ApiStatusResult {
    validate_paper_id(&paper_id)?;
    state
        .store
        .delete_paper_comment(&paper_id, comment_id)
        .await
        .map_err(map_err)?;
    Ok(StatusCode::NO_CONTENT)
}
