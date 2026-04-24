use std::{fs, path::Path, time::Duration};

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use tokio::time::sleep;

use crate::models::{AiSolvePayload, AiSolvedQuestion, AppSettings, SolveSource};

pub struct AiClient {
    http: Client,
    settings: AppSettings,
}

impl AiClient {
    pub fn new(settings: AppSettings) -> Result<Self> {
        let http = Client::builder().build()?;
        Ok(Self { http, settings })
    }

    pub async fn embed(&self, input: &str) -> Result<Vec<f32>> {
        let body = json!({
            "model": self.settings.embedding_model,
            "input": input,
        });

        let value = self.post("embeddings", body).await?;
        let vector = value["data"][0]["embedding"]
            .as_array()
            .ok_or_else(|| anyhow!("embedding response missing vector"))?
            .iter()
            .filter_map(|item| item.as_f64())
            .map(|item| item as f32)
            .collect::<Vec<_>>();

        if vector.is_empty() {
            return Err(anyhow!("embedding vector is empty"));
        }

        Ok(vector)
    }

    pub async fn extract_question_from_image(&self, image_path: &Path) -> Result<String> {
        let bytes = fs::read(image_path)?;
        let data_url = format!("data:image/png;base64,{}", STANDARD.encode(bytes));
        let body = json!({
            "model": self.settings.vision_model,
            "messages": [
                {
                    "role": "system",
                    "content": "You are an OCR and study helper. Extract all visible problem statements from the image in top-to-bottom order. Preserve numbering, answer choices, formulas, and line breaks as much as possible. Do not solve the problems."
                },
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "Return the question text exactly and cleanly, including multiple questions if present." },
                        { "type": "image_url", "image_url": { "url": data_url } }
                    ]
                }
            ],
            "temperature": 0.1
        });

        let text = self.chat_text(body).await?;
        let cleaned = text.trim().to_string();
        if !cleaned.is_empty() {
            return Ok(cleaned);
        }

        self.local_ocr_fallback(image_path)
            .await
            .context("vision extraction returned empty text")
    }

    pub async fn solve_quick_answers(&self, questions: &[String], context: &str) -> Result<Vec<QuickAnswer>> {
        let rendered_questions = render_questions(questions);
        let raw = match self
            .chat_text(quick_answer_body(&self.settings.fast_answer_model, &rendered_questions, context))
            .await
        {
            Ok(value) => value,
            Err(_) if self.settings.fast_answer_model != self.settings.answer_model => {
                self.chat_text(quick_answer_body(&self.settings.answer_model, &rendered_questions, context))
                    .await?
            }
            Err(err) => return Err(err),
        };
        match parse_quick_answers(&raw) {
            Ok(value) => Ok(value),
            Err(_) => {
                let retry_raw = self
                    .chat_text(quick_answer_body(&self.settings.fast_answer_model, &rendered_questions, context))
                    .await?;
                parse_quick_answers(&retry_raw)
            }
        }
    }

    pub async fn solve_questions(&self, questions: &[String], context: &str) -> Result<AiSolvePayload> {
        let rendered_questions = render_questions(questions);
        let body = json!({
            "model": self.settings.answer_model,
            "temperature": 0.2,
            "max_tokens": 2048,
            "response_format": { "type": "json_object" },
            "messages": [
                {
                    "role": "system",
                    "content": "You are a course study assistant. Return JSON with top-level key questions. Each item must contain: question, question_zh, answer, answer_brief, explanation, confidence, low_confidence, sources. Keep the same order as the input questions. question must preserve the original language. question_zh must be a natural Simplified Chinese translation. answer_brief must be extremely short for compact display. Always prefer the supplied course material. If evidence is weak, set low_confidence=true and say the answer needs human review. sources must be an array of objects with keys title and excerpt. Never invent page numbers or section names."
                },
                {
                    "role": "user",
                    "content": format!("Questions:\n{rendered_questions}\n\nCourse context:\n{context}")
                }
            ]
        });

        let raw = self.chat_text(body.clone()).await?;
        match parse_solve_payload(&raw) {
            Ok(value) => Ok(value),
            Err(_) => {
                let retry_raw = self.chat_text(body).await?;
                parse_solve_payload(&retry_raw)
            }
        }
    }

    async fn chat_text(&self, body: Value) -> Result<String> {
        let value = self.post("chat/completions", body).await?;
        let message = &value["choices"][0]["message"]["content"];

        if let Some(text) = message.as_str() {
            return Ok(text.to_string());
        }

        if let Some(parts) = message.as_array() {
            let text = parts
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                return Ok(text);
            }
        }

        Err(anyhow!("chat response missing content"))
    }

    async fn post(&self, endpoint: &str, body: Value) -> Result<Value> {
        if self.settings.api_key.trim().is_empty() {
            return Err(anyhow!("missing API key"));
        }

        let base = self.settings.api_base.trim_end_matches('/');
        let url = format!("{base}/{endpoint}");
        let mut last_error = None;

        for attempt in 0..3 {
            let response = self
                .http
                .post(&url)
                .bearer_auth(&self.settings.api_key)
                .json(&body)
                .send()
                .await?;

            let status = response.status();
            let value = response.json::<Value>().await?;
            if status.is_success() {
                return Ok(value);
            }

            let error = anyhow!(
                "model API error {status}: {}",
                serde_json::to_string(&value).unwrap_or_default()
            );

            if !is_retryable_status(status) || attempt == 2 {
                return Err(error);
            }

            last_error = Some(error);
            sleep(Duration::from_millis(400 * (attempt + 1) as u64)).await;
        }

        Err(last_error.unwrap_or_else(|| anyhow!("model API request failed")))
    }

    async fn local_ocr_fallback(&self, image_path: &Path) -> Result<String> {
        let output = tokio::process::Command::new("tesseract")
            .arg(image_path)
            .arg("stdout")
            .output()
            .await
            .context("tesseract is not available for OCR fallback")?;

        if !output.status.success() {
            return Err(anyhow!("tesseract OCR failed"));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

#[derive(Debug, Clone)]
pub struct QuickAnswer {
    pub question: String,
    pub answer_brief: String,
}

#[derive(Debug, Deserialize)]
struct JsonSolvePayload {
    questions: Vec<JsonSolvedQuestion>,
}

#[derive(Debug, Deserialize)]
struct JsonSolvedQuestion {
    question: String,
    question_zh: String,
    answer: String,
    answer_brief: String,
    explanation: String,
    confidence: f32,
    low_confidence: bool,
    #[serde(deserialize_with = "deserialize_sources")]
    sources: Vec<SolveSource>,
}

#[derive(Debug, Deserialize)]
struct JsonQuickPayload {
    questions: Vec<JsonQuickQuestion>,
}

#[derive(Debug, Deserialize)]
struct JsonQuickQuestion {
    question: String,
    answer_brief: String,
}

fn quick_answer_body(model: &str, rendered_questions: &str, context: &str) -> Value {
    json!({
        "model": model,
        "temperature": 0.1,
        "max_tokens": 256,
        "enable_thinking": false,
        "response_format": { "type": "json_object" },
        "messages": [
            {
                "role": "system",
                "content": "You are a fast answer extractor. Return JSON with top-level key questions. Each item must contain: question, answer_brief. Keep the same order as the input questions. answer_brief must be extremely short for compact display, for example 'A', 'BC', '42', 'True', or a 1-5 word short answer."
            },
            {
                "role": "user",
                "content": format!("Questions:\n{rendered_questions}\n\nCourse context:\n{context}")
            }
        ]
    })
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn parse_solve_payload(raw: &str) -> Result<AiSolvePayload> {
    let parsed = parse_json::<JsonSolvePayload>(raw)
        .or_else(|_| salvage_questions_payload::<JsonSolvePayload>(raw))?;
    let questions = parsed
        .questions
        .into_iter()
        .map(|item| AiSolvedQuestion {
            question: item.question,
            question_zh: item.question_zh,
            answer: item.answer,
            answer_brief: item.answer_brief,
            explanation: item.explanation,
            confidence: item.confidence.clamp(0.0, 1.0),
            low_confidence: item.low_confidence,
            sources: item.sources,
        })
        .collect::<Vec<_>>();

    Ok(AiSolvePayload { questions })
}

fn parse_quick_answers(raw: &str) -> Result<Vec<QuickAnswer>> {
    let parsed = parse_json::<JsonQuickPayload>(raw)
        .or_else(|_| salvage_questions_payload::<JsonQuickPayload>(raw))?;
    Ok(parsed
        .questions
        .into_iter()
        .map(|item| QuickAnswer {
            question: item.question,
            answer_brief: item.answer_brief,
        })
        .collect())
}

fn parse_json<T: for<'de> Deserialize<'de>>(raw: &str) -> Result<T> {
    let candidate = normalize_model_json(raw);
    if let Ok(parsed) = serde_json::from_str::<T>(&candidate) {
        return Ok(parsed);
    }

    let start = candidate.find('{').ok_or_else(|| anyhow!("model did not return JSON"))?;
    let sliced = &candidate[start..];
    let repaired = repair_truncated_json(sliced);
    if let Ok(parsed) = serde_json::from_str::<T>(&repaired) {
        return Ok(parsed);
    }

    let end = candidate.rfind('}').ok_or_else(|| anyhow!("model did not return JSON"))?;
    Ok(serde_json::from_str::<T>(&candidate[start..=end])?)
}

fn normalize_model_json(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(stripped) = trimmed.strip_prefix("```json").and_then(|value| value.strip_suffix("```")) {
        return stripped.trim().to_string();
    }
    if let Some(stripped) = trimmed.strip_prefix("```").and_then(|value| value.strip_suffix("```")) {
        return stripped.trim().to_string();
    }
    trimmed.to_string()
}

fn repair_truncated_json(raw: &str) -> String {
    let mut result = String::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut object_depth = 0usize;
    let mut array_depth = 0usize;

    for ch in raw.chars() {
        result.push(ch);

        if escaped {
            escaped = false;
            continue;
        }

        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '{' if !in_string => object_depth += 1,
            '}' if !in_string && object_depth > 0 => object_depth -= 1,
            '[' if !in_string => array_depth += 1,
            ']' if !in_string && array_depth > 0 => array_depth -= 1,
            _ => {}
        }
    }

    if in_string {
        result.push('"');
    }
    while array_depth > 0 {
        result.push(']');
        array_depth -= 1;
    }
    while object_depth > 0 {
        result.push('}');
        object_depth -= 1;
    }

    result
}

fn salvage_questions_payload<T: for<'de> Deserialize<'de>>(raw: &str) -> Result<T> {
    let normalized = normalize_model_json(raw);
    let key_index = normalized
        .find("\"questions\"")
        .ok_or_else(|| anyhow!("model did not return a questions payload"))?;
    let array_start = normalized[key_index..]
        .find('[')
        .map(|offset| key_index + offset)
        .ok_or_else(|| anyhow!("questions array missing"))?;
    let array_slice = &normalized[array_start + 1..];
    let objects = extract_complete_top_level_objects(array_slice);
    if objects.is_empty() {
        return Err(anyhow!("no complete question objects could be salvaged"));
    }

    let rebuilt = format!(r#"{{"questions":[{}]}}"#, objects.join(","));
    Ok(serde_json::from_str::<T>(&rebuilt)?)
}

fn extract_complete_top_level_objects(raw: &str) -> Vec<String> {
    let mut results = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut depth = 0usize;
    let mut start = None;

    for (index, ch) in raw.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }

        match ch {
            '\\' if in_string => {
                escaped = true;
            }
            '"' => {
                in_string = !in_string;
            }
            '{' if !in_string => {
                if depth == 0 {
                    start = Some(index);
                }
                depth += 1;
            }
            '}' if !in_string && depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if let Some(begin) = start.take() {
                        results.push(raw[begin..=index].to_string());
                    }
                }
            }
            ']' if !in_string && depth == 0 => break,
            _ => {}
        }
    }

    results
}

fn render_questions(questions: &[String]) -> String {
    questions
        .iter()
        .enumerate()
        .map(|(index, question)| format!("{}. {}", index + 1, question))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn deserialize_sources<'de, D>(deserializer: D) -> Result<Vec<SolveSource>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Vec::<serde_json::Value>::deserialize(deserializer)?;
    let mut sources = Vec::new();

    for item in raw {
        match item {
            serde_json::Value::Object(map) => {
                let title = map
                    .get("title")
                    .and_then(|value| value.as_str())
                    .unwrap_or("Course material")
                    .to_string();
                let excerpt = map
                    .get("excerpt")
                    .and_then(|value| value.as_str())
                    .or_else(|| map.get("text").and_then(|value| value.as_str()))
                    .unwrap_or("")
                    .to_string();
                sources.push(SolveSource { title, excerpt });
            }
            serde_json::Value::String(text) => {
                sources.push(SolveSource {
                    title: "Model note".to_string(),
                    excerpt: text,
                });
            }
            _ => {}
        }
    }

    Ok(sources)
}
