use std::collections::BTreeSet;

use axum::{
    Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection, SqlitePool};
use uuid::Uuid;

use crate::{
    api::AppState,
    contracts::{
        AgentConfidence, AgentProposal, AgentProposalState, AgentQuestion, AgentQuestionState,
        AgentQuestionType, ApiErrorResponse, DiscoveryEvidence, Freshness, OnboardingAction,
        OnboardingMessageRequest, OnboardingSessionCreateRequest, OnboardingSessionData,
        OnboardingSessionResponse, OnboardingSessionState, ProjectionDraftData,
        ProjectionPatchOperation, ProjectionState,
    },
    model_provider::{
        M3Error, ModelCallError, call_configured_model, idempotency_key, m3_meta, payload_sha256,
        replay_mutation, request_id, store_mutation,
    },
    projection::{
        ProjectionUndoState, apply_agent_operations_in, load_projection_draft_data,
        undo_agent_operations_in, validate_projection_operations,
    },
};

const MAX_PROPOSALS: usize = 16;
const MAX_QUESTIONS: usize = 24;
const MAX_FACTS: usize = 128;
const MAX_WARNINGS: usize = 32;
const MAX_TEXT_CHARS: usize = 2_000;
const MAX_MESSAGE_CHARS: usize = 4_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelOutput {
    facts_used: Vec<String>,
    proposals: Vec<ModelProposal>,
    questions: Vec<ModelQuestion>,
    projection_patch: Vec<ProjectionPatchOperation>,
    warnings: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelProposal {
    title: String,
    reason: String,
    confidence: AgentConfidence,
    evidence_refs: Vec<String>,
    patch: Vec<ProjectionPatchOperation>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelQuestion {
    question_id: String,
    #[serde(rename = "type")]
    kind: AgentQuestionType,
    prompt: String,
    options: Vec<String>,
    evidence_refs: Vec<String>,
    blocking: bool,
}

#[derive(Debug, Default)]
struct GeneratedOutput {
    facts_used: Vec<String>,
    proposals: Vec<AgentProposal>,
    questions: Vec<AgentQuestion>,
    warnings: Vec<String>,
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn invalid_json(_error: JsonRejection) -> M3Error {
    M3Error::bad("INVALID_JSON", "请求正文不是符合契约的 JSON", json!({}))
}

fn enum_string<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn parse_enum<T: DeserializeOwned>(value: &str) -> Result<T, M3Error> {
    serde_json::from_value(Value::String(value.to_owned())).map_err(|_| M3Error::Internal)
}

fn clean_text(value: String, max_chars: usize) -> Result<String, ()> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().count() > max_chars
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(());
    }
    Ok(value.to_owned())
}

fn validate_refs(
    refs: Vec<String>,
    allowed: &BTreeSet<String>,
    allow_empty: bool,
) -> Result<Vec<String>, ()> {
    if (!allow_empty && refs.is_empty()) || refs.len() > MAX_FACTS {
        return Err(());
    }
    let mut unique = BTreeSet::new();
    for reference in refs {
        if reference.len() > 512 || !allowed.contains(&reference) || !unique.insert(reference) {
            return Err(());
        }
    }
    Ok(unique.into_iter().collect())
}

fn allowed_evidence_refs(
    draft: &ProjectionDraftData,
    evidence: &DiscoveryEvidence,
) -> BTreeSet<String> {
    let mut allowed = BTreeSet::from([format!("discovery:{}", draft.discovery_run_id)]);
    for item in evidence.items() {
        allowed.insert(format!("evidence:{}", item.external_id));
    }
    for node in &draft.snapshot.nodes {
        allowed.extend(node.source_refs.iter().cloned());
    }
    for edge in &draft.snapshot.edges {
        allowed.extend(edge.source_refs.iter().cloned());
    }
    allowed
}

fn validate_model_output(
    content: &str,
    draft: &ProjectionDraftData,
    allowed: &BTreeSet<String>,
) -> Result<GeneratedOutput, ()> {
    let output: ModelOutput = serde_json::from_str(content.trim()).map_err(|_| ())?;
    if output.proposals.len() > MAX_PROPOSALS
        || output.questions.len() > MAX_QUESTIONS
        || output.facts_used.len() > MAX_FACTS
        || output.warnings.len() > MAX_WARNINGS
    {
        return Err(());
    }
    let facts_used = validate_refs(output.facts_used, allowed, true)?;
    let mut proposals = Vec::with_capacity(output.proposals.len() + 1);
    for proposal in output.proposals {
        let title = clean_text(proposal.title, 160)?;
        let reason = clean_text(proposal.reason, MAX_TEXT_CHARS)?;
        let evidence_refs = validate_refs(proposal.evidence_refs, allowed, false)?;
        let patch = proposal.patch;
        validate_projection_operations(&draft.snapshot, &draft.draft_id, &patch).map_err(|_| ())?;
        proposals.push(AgentProposal {
            proposal_id: format!("proposal-{}", Uuid::new_v4()),
            title,
            reason,
            confidence: proposal.confidence,
            evidence_refs,
            requires_user_confirmation: true,
            patch,
            state: AgentProposalState::Pending,
            applied_revision: None,
        });
    }
    if !output.projection_patch.is_empty() {
        if facts_used.is_empty() {
            return Err(());
        }
        validate_projection_operations(&draft.snapshot, &draft.draft_id, &output.projection_patch)
            .map_err(|_| ())?;
        proposals.push(AgentProposal {
            proposal_id: format!("proposal-{}", Uuid::new_v4()),
            title: "整体投影整理建议".to_owned(),
            reason: "根据已引用事实生成的整体草稿补丁".to_owned(),
            confidence: AgentConfidence::Medium,
            evidence_refs: facts_used.clone(),
            requires_user_confirmation: true,
            patch: output.projection_patch,
            state: AgentProposalState::Pending,
            applied_revision: None,
        });
    }
    if proposals.len() > MAX_PROPOSALS {
        return Err(());
    }

    let mut model_question_ids = BTreeSet::new();
    let mut questions = Vec::with_capacity(output.questions.len());
    for question in output.questions {
        let model_question_id = clean_text(question.question_id, 120)?;
        if !model_question_ids.insert(model_question_id) {
            return Err(());
        }
        let prompt = clean_text(question.prompt, MAX_TEXT_CHARS)?;
        let evidence_refs = validate_refs(question.evidence_refs, allowed, false)?;
        let options = question
            .options
            .into_iter()
            .map(|option| clean_text(option, 240))
            .collect::<Result<Vec<_>, _>>()?;
        match question.kind {
            AgentQuestionType::Choice if !(2..=12).contains(&options.len()) => return Err(()),
            AgentQuestionType::Text | AgentQuestionType::Confirm if !options.is_empty() => {
                return Err(());
            }
            _ => {}
        }
        questions.push(AgentQuestion {
            question_id: format!("question-{}", Uuid::new_v4()),
            kind: question.kind,
            prompt,
            options,
            evidence_refs,
            blocking: question.blocking,
            state: AgentQuestionState::Pending,
            answer: None,
        });
    }
    let warnings = output
        .warnings
        .into_iter()
        .map(|warning| clean_text(warning, MAX_TEXT_CHARS))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(GeneratedOutput {
        facts_used,
        proposals,
        questions,
        warnings,
    })
}

fn model_messages(
    draft: &ProjectionDraftData,
    evidence: &DiscoveryEvidence,
    allowed: &BTreeSet<String>,
) -> Vec<Value> {
    let schema = json!({
        "facts_used": ["evidence:EXTERNAL_ID"],
        "proposals": [{
            "title": "string",
            "reason": "string",
            "confidence": "low | medium | high",
            "evidence_refs": ["evidence:EXTERNAL_ID"],
            "patch": [{"op": "rename", "node_id": "NODE_ID", "label": "NAME"}]
        }],
        "questions": [{
            "question_id": "local-question-name",
            "type": "choice | text | confirm",
            "prompt": "string",
            "options": ["choice-a", "choice-b"],
            "evidence_refs": ["evidence:EXTERNAL_ID"],
            "blocking": false
        }],
        "projection_patch": [],
        "warnings": []
    });
    let input = json!({
        "draft": draft,
        "evidence": evidence,
        "allowed_evidence_refs": allowed,
    });
    vec![
        json!({
            "role": "system",
            "content": format!(
                "You assist a visual project map. Return only one JSON object matching this exact schema: {}. Use only allowed evidence references. Each proposal patch must be independently valid against the supplied draft. Never claim that a patch is applied; every proposal requires explicit user confirmation. For text and confirm questions, options must be empty.",
                schema
            )
        }),
        json!({
            "role": "user",
            "content": serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_owned())
        }),
    ]
}

async fn load_evidence(
    pool: &SqlitePool,
    draft: &ProjectionDraftData,
) -> Result<Option<DiscoveryEvidence>, M3Error> {
    let row = sqlx::query(
        "SELECT evidence_json FROM discovery_runs
         WHERE run_id = ?
           AND state IN ('evidence_ready', 'discovery_complete', 'discovery_partial')",
    )
    .bind(&draft.discovery_run_id)
    .fetch_optional(pool)
    .await
    .map_err(M3Error::Storage)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let payload: Option<String> = row.try_get("evidence_json").map_err(M3Error::Storage)?;
    payload
        .map(|payload| serde_json::from_str(&payload).map_err(|_| M3Error::Internal))
        .transpose()
}

fn session_response(
    data: OnboardingSessionData,
    revision: i64,
    request_id: String,
) -> OnboardingSessionResponse {
    let freshness = match data.state {
        OnboardingSessionState::Ready => Freshness::Fresh,
        OnboardingSessionState::Degraded => Freshness::Stale,
        OnboardingSessionState::Unavailable => Freshness::Unavailable,
    };
    OnboardingSessionResponse {
        data,
        meta: m3_meta(request_id, revision, freshness),
    }
}

async fn load_session_data_in(
    connection: &mut SqliteConnection,
    session_id: &str,
) -> Result<(OnboardingSessionData, i64), M3Error> {
    let row = sqlx::query(
        "SELECT session.session_id, session.draft_id, session.discovery_run_id, session.state,
                session.facts_used_json, session.warnings_json, session.error_code,
                session.created_at, session.updated_at, draft.revision
         FROM onboarding_sessions session
         JOIN projection_drafts draft ON draft.draft_id = session.draft_id
         WHERE session.session_id = ?",
    )
    .bind(session_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(M3Error::Storage)?
    .ok_or_else(|| M3Error::not_found("onboarding_session", session_id))?;
    let facts_used_json: String = row.try_get("facts_used_json").map_err(M3Error::Storage)?;
    let warnings_json: String = row.try_get("warnings_json").map_err(M3Error::Storage)?;

    let proposal_rows = sqlx::query(
        "SELECT proposal_id, title, reason, confidence, evidence_refs_json, patch_json,
                state, applied_revision
         FROM agent_proposals WHERE session_id = ? ORDER BY rowid",
    )
    .bind(session_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(M3Error::Storage)?;
    let mut proposals = Vec::with_capacity(proposal_rows.len());
    for proposal in proposal_rows {
        let evidence_refs_json: String = proposal
            .try_get("evidence_refs_json")
            .map_err(M3Error::Storage)?;
        let patch_json: String = proposal.try_get("patch_json").map_err(M3Error::Storage)?;
        let confidence: String = proposal.try_get("confidence").map_err(M3Error::Storage)?;
        let state: String = proposal.try_get("state").map_err(M3Error::Storage)?;
        proposals.push(AgentProposal {
            proposal_id: proposal.try_get("proposal_id").map_err(M3Error::Storage)?,
            title: proposal.try_get("title").map_err(M3Error::Storage)?,
            reason: proposal.try_get("reason").map_err(M3Error::Storage)?,
            confidence: parse_enum(&confidence)?,
            evidence_refs: serde_json::from_str(&evidence_refs_json)
                .map_err(|_| M3Error::Internal)?,
            requires_user_confirmation: true,
            patch: serde_json::from_str(&patch_json).map_err(|_| M3Error::Internal)?,
            state: parse_enum(&state)?,
            applied_revision: proposal
                .try_get("applied_revision")
                .map_err(M3Error::Storage)?,
        });
    }

    let question_rows = sqlx::query(
        "SELECT question_id, kind, prompt, options_json, evidence_refs_json, blocking,
                state, answer_json
         FROM agent_questions WHERE session_id = ? ORDER BY rowid",
    )
    .bind(session_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(M3Error::Storage)?;
    let mut questions = Vec::with_capacity(question_rows.len());
    for question in question_rows {
        let kind: String = question.try_get("kind").map_err(M3Error::Storage)?;
        let state: String = question.try_get("state").map_err(M3Error::Storage)?;
        let options_json: String = question.try_get("options_json").map_err(M3Error::Storage)?;
        let evidence_refs_json: String = question
            .try_get("evidence_refs_json")
            .map_err(M3Error::Storage)?;
        let answer_json: Option<String> =
            question.try_get("answer_json").map_err(M3Error::Storage)?;
        let blocking: i64 = question.try_get("blocking").map_err(M3Error::Storage)?;
        questions.push(AgentQuestion {
            question_id: question.try_get("question_id").map_err(M3Error::Storage)?,
            kind: parse_enum(&kind)?,
            prompt: question.try_get("prompt").map_err(M3Error::Storage)?,
            options: serde_json::from_str(&options_json).map_err(|_| M3Error::Internal)?,
            evidence_refs: serde_json::from_str(&evidence_refs_json)
                .map_err(|_| M3Error::Internal)?,
            blocking: blocking != 0,
            state: parse_enum(&state)?,
            answer: answer_json
                .map(|answer| serde_json::from_str(&answer).map_err(|_| M3Error::Internal))
                .transpose()?,
        });
    }

    let state_value: String = row.try_get("state").map_err(M3Error::Storage)?;
    Ok((
        OnboardingSessionData {
            session_id: row.try_get("session_id").map_err(M3Error::Storage)?,
            draft_id: row.try_get("draft_id").map_err(M3Error::Storage)?,
            discovery_run_id: row.try_get("discovery_run_id").map_err(M3Error::Storage)?,
            state: parse_enum(&state_value)?,
            facts_used: serde_json::from_str(&facts_used_json).map_err(|_| M3Error::Internal)?,
            proposals,
            questions,
            warnings: serde_json::from_str(&warnings_json).map_err(|_| M3Error::Internal)?,
            error_code: row.try_get("error_code").map_err(M3Error::Storage)?,
            created_at: row.try_get("created_at").map_err(M3Error::Storage)?,
            updated_at: row.try_get("updated_at").map_err(M3Error::Storage)?,
        },
        row.try_get("revision").map_err(M3Error::Storage)?,
    ))
}

async fn load_session_data(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<(OnboardingSessionData, i64), M3Error> {
    let mut connection = pool.acquire().await.map_err(M3Error::Storage)?;
    load_session_data_in(&mut connection, session_id).await
}

#[utoipa::path(
    post,
    path = "/api/v1/onboarding-sessions",
    tag = "m3",
    params(("Idempotency-Key" = String, Header, description = "Stable key for safe request retries")),
    request_body = OnboardingSessionCreateRequest,
    responses(
        (status = 201, body = OnboardingSessionResponse),
        (status = 200, body = OnboardingSessionResponse),
        (status = 400, body = ApiErrorResponse)
    )
)]
pub async fn create_onboarding_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<OnboardingSessionCreateRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<OnboardingSessionResponse>), M3Error> {
    let Json(request) = payload.map_err(invalid_json)?;
    let key = idempotency_key(&headers)?;
    let request_hash = payload_sha256(serde_json::to_vec(&request).map_err(|_| M3Error::Internal)?);
    if let Some(response) = replay_mutation::<OnboardingSessionResponse>(
        &state.pool,
        "onboarding_create",
        &request.draft_id,
        &key,
        &request_hash,
    )
    .await?
    {
        return Ok((StatusCode::OK, Json(response)));
    }
    if let Some(row) = sqlx::query(
        "SELECT session_id FROM onboarding_sessions
         WHERE draft_id = ? AND state = 'ready' ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(&request.draft_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(M3Error::Storage)?
    {
        let session_id: String = row.try_get("session_id").map_err(M3Error::Storage)?;
        let (data, revision) = load_session_data(&state.pool, &session_id).await?;
        return Ok((
            StatusCode::OK,
            Json(session_response(data, revision, request_id(&headers))),
        ));
    }

    let draft = load_projection_draft_data(&state.pool, &request.draft_id).await?;
    if draft.state != ProjectionState::Draft {
        return Err(M3Error::Conflict {
            code: "DRAFT_NOT_EDITABLE",
            message: "只有未确认草稿可以启动 Agent 辅助",
            details: json!({"draft_id": request.draft_id}),
        });
    }
    let evidence = load_evidence(&state.pool, &draft).await?;
    let session_id = format!("session-{}", Uuid::new_v4());
    let created_at = now();
    let mut generated = GeneratedOutput::default();
    let mut session_state = OnboardingSessionState::Unavailable;
    let mut error_code = Some("EVIDENCE_NOT_AVAILABLE".to_owned());
    let mut invocation_state = "failed";
    let mut model_revision = None;

    if let Some(evidence) = &evidence {
        let allowed = allowed_evidence_refs(&draft, evidence);
        match call_configured_model(&state, model_messages(&draft, evidence, &allowed)).await {
            Ok((config, content)) => {
                model_revision = Some(config.revision);
                match validate_model_output(&content, &draft, &allowed) {
                    Ok(output) => {
                        generated = output;
                        session_state = OnboardingSessionState::Ready;
                        error_code = None;
                        invocation_state = "succeeded";
                    }
                    Err(()) => {
                        session_state = OnboardingSessionState::Degraded;
                        error_code = Some("PROPOSAL_INVALID".to_owned());
                        generated
                            .warnings
                            .push("模型响应未通过证据与补丁校验，未保存其中的建议".to_owned());
                    }
                }
            }
            Err(error) => {
                session_state = if error == ModelCallError::InvalidResponse {
                    OnboardingSessionState::Degraded
                } else {
                    OnboardingSessionState::Unavailable
                };
                error_code = Some(error.code().to_owned());
                generated
                    .warnings
                    .push("Agent 暂不可用；确定性草稿和手动编辑保持可用".to_owned());
            }
        }
    } else {
        generated
            .warnings
            .push("当前扫描仅保留摘要，无法生成新的 Agent 建议".to_owned());
    }

    let current_draft = load_projection_draft_data(&state.pool, &request.draft_id).await?;
    if current_draft.revision != draft.revision {
        generated = GeneratedOutput {
            warnings: vec!["模型调用期间草稿已变化，请重新创建辅助会话".to_owned()],
            ..GeneratedOutput::default()
        };
        session_state = OnboardingSessionState::Degraded;
        error_code = Some("DRAFT_CHANGED_DURING_MODEL_CALL".to_owned());
        invocation_state = "failed";
    }

    let data = OnboardingSessionData {
        session_id: session_id.clone(),
        draft_id: request.draft_id.clone(),
        discovery_run_id: draft.discovery_run_id.clone(),
        state: session_state.clone(),
        facts_used: generated.facts_used.clone(),
        proposals: generated.proposals.clone(),
        questions: generated.questions.clone(),
        warnings: generated.warnings.clone(),
        error_code: error_code.clone(),
        created_at: created_at.clone(),
        updated_at: created_at.clone(),
    };
    let response = session_response(data, current_draft.revision, request_id(&headers));
    let mut tx = state.pool.begin().await.map_err(M3Error::Storage)?;
    sqlx::query(
        "INSERT INTO onboarding_sessions(
            session_id, workspace_id, draft_id, discovery_run_id, state, facts_used_json,
            warnings_json, error_code, created_at, updated_at
         ) VALUES (?, 'workspace-default', ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&session_id)
    .bind(&request.draft_id)
    .bind(&draft.discovery_run_id)
    .bind(enum_string(&session_state))
    .bind(serde_json::to_string(&generated.facts_used).map_err(|_| M3Error::Internal)?)
    .bind(serde_json::to_string(&generated.warnings).map_err(|_| M3Error::Internal)?)
    .bind(&error_code)
    .bind(&created_at)
    .bind(&created_at)
    .execute(&mut *tx)
    .await
    .map_err(M3Error::Storage)?;
    for proposal in &generated.proposals {
        sqlx::query(
            "INSERT INTO agent_proposals(
                proposal_id, session_id, title, reason, confidence, evidence_refs_json,
                patch_json, state, before_snapshot_json, applied_revision, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', NULL, NULL, ?, ?)",
        )
        .bind(&proposal.proposal_id)
        .bind(&session_id)
        .bind(&proposal.title)
        .bind(&proposal.reason)
        .bind(enum_string(&proposal.confidence))
        .bind(serde_json::to_string(&proposal.evidence_refs).map_err(|_| M3Error::Internal)?)
        .bind(serde_json::to_string(&proposal.patch).map_err(|_| M3Error::Internal)?)
        .bind(&created_at)
        .bind(&created_at)
        .execute(&mut *tx)
        .await
        .map_err(M3Error::Storage)?;
    }
    for question in &generated.questions {
        sqlx::query(
            "INSERT INTO agent_questions(
                question_id, session_id, kind, prompt, options_json, evidence_refs_json,
                blocking, state, answer_json, created_at, answered_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', NULL, ?, NULL)",
        )
        .bind(&question.question_id)
        .bind(&session_id)
        .bind(enum_string(&question.kind))
        .bind(&question.prompt)
        .bind(serde_json::to_string(&question.options).map_err(|_| M3Error::Internal)?)
        .bind(serde_json::to_string(&question.evidence_refs).map_err(|_| M3Error::Internal)?)
        .bind(i64::from(question.blocking))
        .bind(&created_at)
        .execute(&mut *tx)
        .await
        .map_err(M3Error::Storage)?;
    }
    sqlx::query(
        "INSERT INTO model_invocations(
            invocation_id, session_id, state, error_code, result_summary_json, created_at, finished_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&session_id)
    .bind(invocation_state)
    .bind(&error_code)
    .bind(
        serde_json::to_string(&json!({
            "model_revision": model_revision,
            "facts": generated.facts_used.len(),
            "proposals": generated.proposals.len(),
            "questions": generated.questions.len(),
            "warnings": generated.warnings.len()
        }))
        .map_err(|_| M3Error::Internal)?,
    )
    .bind(&created_at)
    .bind(&created_at)
    .execute(&mut *tx)
    .await
    .map_err(M3Error::Storage)?;
    store_mutation(
        &mut tx,
        "onboarding_create",
        &request.draft_id,
        &key,
        &request_hash,
        &response,
    )
    .await?;
    tx.commit().await.map_err(M3Error::Storage)?;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/onboarding-sessions/{session_id}",
    tag = "m3",
    params(("session_id" = String, Path, description = "Onboarding session identifier")),
    responses((status = 200, body = OnboardingSessionResponse), (status = 404, body = ApiErrorResponse))
)]
pub async fn get_onboarding_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Result<Json<OnboardingSessionResponse>, M3Error> {
    let (data, revision) = load_session_data(&state.pool, &session_id).await?;
    Ok(Json(session_response(data, revision, request_id(&headers))))
}

#[utoipa::path(
    get,
    path = "/api/v1/discovery-runs/{run_id}/proposal",
    tag = "m3",
    params(("run_id" = String, Path, description = "Discovery run identifier")),
    responses((status = 200, body = OnboardingSessionResponse), (status = 404, body = ApiErrorResponse))
)]
pub async fn get_discovery_proposal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Result<Json<OnboardingSessionResponse>, M3Error> {
    let row = sqlx::query(
        "SELECT session_id FROM onboarding_sessions
         WHERE discovery_run_id = ? ORDER BY updated_at DESC, session_id DESC LIMIT 1",
    )
    .bind(&run_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(M3Error::Storage)?
    .ok_or_else(|| M3Error::not_found("discovery_proposal", &run_id))?;
    let session_id: String = row.try_get("session_id").map_err(M3Error::Storage)?;
    let (data, revision) = load_session_data(&state.pool, &session_id).await?;
    Ok(Json(session_response(data, revision, request_id(&headers))))
}

fn require_base_revision(request: &OnboardingMessageRequest) -> Result<i64, M3Error> {
    request.base_revision.ok_or_else(|| {
        M3Error::bad(
            "BASE_REVISION_REQUIRED",
            "采用、修改或撤销建议必须提供当前草稿修订",
            json!({}),
        )
    })
}

fn validate_message(message: &Option<String>) -> Result<(), M3Error> {
    if message
        .as_ref()
        .is_some_and(|value| value.chars().count() > MAX_MESSAGE_CHARS)
    {
        return Err(M3Error::bad(
            "MESSAGE_TOO_LARGE",
            "会话消息不能超过 4000 个字符",
            json!({}),
        ));
    }
    Ok(())
}

fn validate_answer(
    kind: &AgentQuestionType,
    options: &[String],
    answer: &Value,
) -> Result<(), M3Error> {
    let value = answer
        .get("value")
        .ok_or_else(|| M3Error::bad("INVALID_ANSWER", "回答必须包含 value 字段", json!({})))?;
    let valid = match kind {
        AgentQuestionType::Choice => value
            .as_str()
            .is_some_and(|selected| options.iter().any(|option| option == selected)),
        AgentQuestionType::Text => value.as_str().is_some_and(|text| {
            !text.trim().is_empty() && text.chars().count() <= MAX_MESSAGE_CHARS
        }),
        AgentQuestionType::Confirm => value.is_boolean(),
    };
    if !valid {
        return Err(M3Error::bad(
            "INVALID_ANSWER",
            "回答类型或选项与问题不匹配",
            json!({}),
        ));
    }
    Ok(())
}

fn ensure_proposal_state(state: &str, allowed: &[&str]) -> Result<(), M3Error> {
    if allowed.contains(&state) {
        return Ok(());
    }
    Err(M3Error::Conflict {
        code: "PROPOSAL_STATE_CONFLICT",
        message: "建议当前状态不允许该操作",
        details: json!({"state": state}),
    })
}

#[utoipa::path(
    post,
    path = "/api/v1/onboarding-sessions/{session_id}/messages",
    tag = "m3",
    params(
        ("session_id" = String, Path, description = "Onboarding session identifier"),
        ("Idempotency-Key" = String, Header, description = "Stable key for safe request retries")
    ),
    request_body = OnboardingMessageRequest,
    responses(
        (status = 200, body = OnboardingSessionResponse),
        (status = 409, body = ApiErrorResponse),
        (status = 412, body = ApiErrorResponse)
    )
)]
pub async fn send_onboarding_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    payload: Result<Json<OnboardingMessageRequest>, JsonRejection>,
) -> Result<Json<OnboardingSessionResponse>, M3Error> {
    let Json(request) = payload.map_err(invalid_json)?;
    validate_message(&request.message)?;
    let key = idempotency_key(&headers)?;
    let request_hash = payload_sha256(serde_json::to_vec(&request).map_err(|_| M3Error::Internal)?);
    if let Some(response) = replay_mutation::<OnboardingSessionResponse>(
        &state.pool,
        "onboarding_message",
        &session_id,
        &key,
        &request_hash,
    )
    .await?
    {
        return Ok(Json(response));
    }

    let mut tx = state.pool.begin().await.map_err(M3Error::Storage)?;
    let session = sqlx::query("SELECT draft_id FROM onboarding_sessions WHERE session_id = ?")
        .bind(&session_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(M3Error::Storage)?
        .ok_or_else(|| M3Error::not_found("onboarding_session", &session_id))?;
    let draft_id: String = session.try_get("draft_id").map_err(M3Error::Storage)?;
    let timestamp = now();
    let summary = match request.action {
        OnboardingAction::Adopt
        | OnboardingAction::Modify
        | OnboardingAction::Reject
        | OnboardingAction::Undo => {
            let proposal_id = request.proposal_id.as_ref().ok_or_else(|| {
                M3Error::bad(
                    "PROPOSAL_ID_REQUIRED",
                    "该操作必须指定 proposal_id",
                    json!({}),
                )
            })?;
            let proposal = sqlx::query(
                "SELECT state, patch_json, before_snapshot_json, applied_revision
                 FROM agent_proposals WHERE proposal_id = ? AND session_id = ?",
            )
            .bind(proposal_id)
            .bind(&session_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(M3Error::Storage)?
            .ok_or_else(|| M3Error::not_found("agent_proposal", proposal_id))?;
            let proposal_state: String = proposal.try_get("state").map_err(M3Error::Storage)?;
            match request.action {
                OnboardingAction::Adopt | OnboardingAction::Modify => {
                    ensure_proposal_state(&proposal_state, &["pending"])?;
                    let operations = if request.action == OnboardingAction::Adopt {
                        let patch_json: String =
                            proposal.try_get("patch_json").map_err(M3Error::Storage)?;
                        serde_json::from_str::<Vec<ProjectionPatchOperation>>(&patch_json)
                            .map_err(|_| M3Error::Internal)?
                    } else {
                        request.operations.clone().ok_or_else(|| {
                            M3Error::bad(
                                "OPERATIONS_REQUIRED",
                                "修改建议必须提供 operations",
                                json!({}),
                            )
                        })?
                    };
                    let applied = apply_agent_operations_in(
                        &mut tx,
                        &draft_id,
                        require_base_revision(&request)?,
                        &operations,
                    )
                    .await?;
                    let new_state = if request.action == OnboardingAction::Adopt {
                        "adopted"
                    } else {
                        "modified"
                    };
                    sqlx::query(
                        "UPDATE agent_proposals SET state = ?, before_snapshot_json = ?,
                                applied_revision = ?, updated_at = ? WHERE proposal_id = ?",
                    )
                    .bind(new_state)
                    .bind(serde_json::to_string(&applied.undo).map_err(|_| M3Error::Internal)?)
                    .bind(applied.data.revision)
                    .bind(&timestamp)
                    .bind(proposal_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(M3Error::Storage)?;
                    json!({
                        "action": new_state,
                        "proposal_id": proposal_id,
                        "draft_revision": applied.data.revision,
                        "operation_count": operations.len()
                    })
                }
                OnboardingAction::Reject => {
                    ensure_proposal_state(&proposal_state, &["pending"])?;
                    sqlx::query(
                        "UPDATE agent_proposals SET state = 'rejected', updated_at = ?
                         WHERE proposal_id = ?",
                    )
                    .bind(&timestamp)
                    .bind(proposal_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(M3Error::Storage)?;
                    json!({"action": "rejected", "proposal_id": proposal_id})
                }
                OnboardingAction::Undo => {
                    ensure_proposal_state(&proposal_state, &["adopted", "modified"])?;
                    let before_json: Option<String> = proposal
                        .try_get("before_snapshot_json")
                        .map_err(M3Error::Storage)?;
                    let applied_revision: Option<i64> = proposal
                        .try_get("applied_revision")
                        .map_err(M3Error::Storage)?;
                    let undo: ProjectionUndoState =
                        serde_json::from_str(before_json.as_deref().ok_or(M3Error::Internal)?)
                            .map_err(|_| M3Error::Internal)?;
                    let restored = undo_agent_operations_in(
                        &mut tx,
                        &draft_id,
                        require_base_revision(&request)?,
                        applied_revision.ok_or(M3Error::Internal)?,
                        &undo,
                    )
                    .await?;
                    sqlx::query(
                        "UPDATE agent_proposals SET state = 'undone', updated_at = ?
                         WHERE proposal_id = ?",
                    )
                    .bind(&timestamp)
                    .bind(proposal_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(M3Error::Storage)?;
                    json!({
                        "action": "undone",
                        "proposal_id": proposal_id,
                        "draft_revision": restored.revision
                    })
                }
                OnboardingAction::Answer => unreachable!(),
            }
        }
        OnboardingAction::Answer => {
            let question_id = request.question_id.as_ref().ok_or_else(|| {
                M3Error::bad(
                    "QUESTION_ID_REQUIRED",
                    "回答必须指定 question_id",
                    json!({}),
                )
            })?;
            let question = sqlx::query(
                "SELECT kind, options_json, state FROM agent_questions
                 WHERE question_id = ? AND session_id = ?",
            )
            .bind(question_id)
            .bind(&session_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(M3Error::Storage)?
            .ok_or_else(|| M3Error::not_found("agent_question", question_id))?;
            let question_state: String = question.try_get("state").map_err(M3Error::Storage)?;
            if question_state != "pending" {
                return Err(M3Error::Conflict {
                    code: "QUESTION_ALREADY_ANSWERED",
                    message: "该问题已经回答",
                    details: json!({"question_id": question_id}),
                });
            }
            let kind: String = question.try_get("kind").map_err(M3Error::Storage)?;
            let kind: AgentQuestionType = parse_enum(&kind)?;
            let options_json: String =
                question.try_get("options_json").map_err(M3Error::Storage)?;
            let options: Vec<String> =
                serde_json::from_str(&options_json).map_err(|_| M3Error::Internal)?;
            let answer = request
                .answer
                .as_ref()
                .ok_or_else(|| M3Error::bad("ANSWER_REQUIRED", "回答内容不能为空", json!({})))?;
            validate_answer(&kind, &options, answer)?;
            sqlx::query(
                "UPDATE agent_questions SET state = 'answered', answer_json = ?, answered_at = ?
                 WHERE question_id = ?",
            )
            .bind(serde_json::to_string(answer).map_err(|_| M3Error::Internal)?)
            .bind(&timestamp)
            .bind(question_id)
            .execute(&mut *tx)
            .await
            .map_err(M3Error::Storage)?;
            json!({"action": "answered", "question_id": question_id})
        }
    };

    sqlx::query("UPDATE onboarding_sessions SET updated_at = ? WHERE session_id = ?")
        .bind(&timestamp)
        .bind(&session_id)
        .execute(&mut *tx)
        .await
        .map_err(M3Error::Storage)?;
    sqlx::query(
        "INSERT INTO onboarding_messages(
            message_id, session_id, action, proposal_id, question_id, summary_json, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&session_id)
    .bind(enum_string(&request.action))
    .bind(&request.proposal_id)
    .bind(&request.question_id)
    .bind(serde_json::to_string(&summary).map_err(|_| M3Error::Internal)?)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await
    .map_err(M3Error::Storage)?;
    let (data, revision) = load_session_data_in(&mut tx, &session_id).await?;
    let response = session_response(data, revision, request_id(&headers));
    store_mutation(
        &mut tx,
        "onboarding_message",
        &session_id,
        &key,
        &request_hash,
        &response,
    )
    .await?;
    tx.commit().await.map_err(M3Error::Storage)?;
    Ok(Json(response))
}
