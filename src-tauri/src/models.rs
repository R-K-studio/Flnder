use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub api_base: String,
    pub api_key: String,
    pub vision_model: String,
    #[serde(default = "default_fast_answer_model")]
    pub fast_answer_model: String,
    pub answer_model: String,
    pub embedding_model: String,
    pub output_dir: String,
    pub shortcut: String,
    #[serde(default = "default_text_shortcut")]
    pub text_shortcut: String,
    pub active_course_id: Option<String>,
}

impl AppSettings {
    pub fn defaults() -> Self {
        Self {
            api_base: "https://api.siliconflow.cn/v1".into(),
            api_key: String::new(),
            vision_model: "THUDM/GLM-4.1V-9B-Thinking".into(),
            fast_answer_model: default_fast_answer_model(),
            answer_model: "Pro/zai-org/GLM-5.1".into(),
            embedding_model: "BAAI/bge-m3".into(),
            output_dir: String::new(),
            shortcut: "CommandOrControl+Shift+1".into(),
            text_shortcut: default_text_shortcut(),
            active_course_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Course {
    pub id: String,
    pub name: String,
    pub document_count: i64,
    pub chunk_count: i64,
    pub updated_at: String,
    pub is_active: bool,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDocument {
    pub id: String,
    pub course_id: String,
    pub file_name: String,
    pub path: String,
    pub kind: String,
    pub imported_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeChunk {
    pub id: String,
    pub course_id: String,
    pub document_id: String,
    pub ordinal: i64,
    pub content: String,
    pub source_file: String,
    pub similarity: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SolveRequest {
    pub course_id: String,
    pub screenshot_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SolveSource {
    pub title: String,
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SolvedQuestion {
    pub ordinal: usize,
    pub question: String,
    pub question_zh: String,
    pub answer: String,
    pub answer_brief: String,
    pub explanation: String,
    pub sources: Vec<SolveSource>,
    pub confidence: f32,
    pub low_confidence: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SolveResult {
    pub id: String,
    pub course_id: String,
    pub item_count: usize,
    pub items: Vec<SolvedQuestion>,
    pub answer_preview: String,
    pub confidence: f32,
    pub low_confidence: bool,
    pub suggested_file_stem: String,
    pub output_path: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocxExportJob {
    pub output_path: String,
    pub written_questions: usize,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardData {
    pub settings: AppSettings,
    pub courses: Vec<Course>,
    pub recent_results: Vec<SolveResult>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSettingsInput {
    pub api_base: String,
    pub api_key: String,
    pub vision_model: String,
    pub fast_answer_model: String,
    pub answer_model: String,
    pub embedding_model: String,
    pub output_dir: String,
    pub shortcut: String,
    pub text_shortcut: String,
    pub active_course_id: Option<String>,
}

fn default_fast_answer_model() -> String {
    "Qwen/Qwen3-32B".into()
}

fn default_text_shortcut() -> String {
    "CommandOrControl+Shift+2".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRequest {
    pub course_name: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResponse {
    pub course_id: String,
    pub imported_documents: usize,
    pub imported_chunks: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusEvent {
    pub task_id: String,
    pub status: String,
    pub detail: String,
    pub current_step: u8,
    pub total_steps: u8,
    pub answer_preview: String,
    pub takeover_progress: bool,
    pub is_terminal: bool,
    pub is_error: bool,
    pub when: DateTime<Local>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSolvedQuestion {
    pub question: String,
    pub question_zh: String,
    pub answer: String,
    pub answer_brief: String,
    pub explanation: String,
    pub confidence: f32,
    pub low_confidence: bool,
    pub sources: Vec<SolveSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSolvePayload {
    pub questions: Vec<AiSolvedQuestion>,
}
