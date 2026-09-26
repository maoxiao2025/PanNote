// 笔尖APP - 数据模型

use serde::{Deserialize, Serialize};

// ==================== 通用 ====================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub timestamp: String,
    pub services: serde_json::Value,
}

// ==================== AI 对话 ====================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    pub id: String,
    pub title: String,
    pub role: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub rating: i32,
    pub ts: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateSessionRequest {
    pub title: Option<String>,
    pub role: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SendMessageRequest {
    pub session_id: String,
    pub message: String,
    pub role: Option<String>,
    pub model: Option<String>,
    pub enable_tools: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SendMessageResponse {
    pub message_id: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RateMessageRequest {
    pub rating: i32,
}

// ==================== 笔记 ====================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub content: String,
    pub tags: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateNoteRequest {
    pub title: String,
    pub content: String,
    pub tags: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateNoteRequest {
    pub title: Option<String>,
    pub content: Option<String>,
    pub tags: Option<String>,
}

// ==================== 会议 ====================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meeting {
    pub id: String,
    pub title: String,
    pub status: String,
    pub duration: i64,
    pub created_at: String,
    pub updated_at: String,
    pub session_dir: String,
    /// v2.4: 会话归属的笔记 id；空 = 独立录音（老数据行为不变）
    #[serde(default)]
    pub note_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptChunk {
    pub id: String,
    pub meeting_id: String,
    pub start_time: f64,
    pub end_time: f64,
    pub text: String,
    pub speaker: String,
    pub confidence: f64,
    pub word_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub id: String,
    pub meeting_id: String,
    pub summary_type: String,
    pub content: String,
    pub evidence: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EditTranscriptRequest {
    pub text: String,
}

// ==================== 搜索 ====================

#[derive(Debug, Clone, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub source: String,
    pub title: String,
    pub snippet: String,
    pub url: Option<String>,
}

// ==================== 进化系统 ====================

#[derive(Debug, Clone, Serialize)]
pub struct EvolutionInsight {
    pub id: String,
    pub insight_type: String,
    pub content: String,
    pub confidence: f64,
    pub created_at: String,
}

// ==================== 流式响应 ====================

#[derive(Debug, Clone, Serialize)]
pub struct StreamEvent {
    pub event_type: String,
    pub data: serde_json::Value,
}

// ==================== ASR ====================

#[derive(Debug, Clone, Serialize)]
pub struct AsrBackendStatus {
    pub name: String,
    pub available: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AsrStatusResponse {
    pub backends: Vec<AsrBackendStatus>,
    pub primary: String,
    /// 任一 ASR 引擎可用即 true（兼容旧前端 asrStatus.running 判断）
    pub running: bool,
}

// ==================== 录音 ====================

#[derive(Debug, Clone, Serialize)]
pub struct RecordingStatus {
    pub recording: bool,
    pub file_path: Option<String>,
    pub duration: f64,
}
