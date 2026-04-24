use std::{cmp::Ordering, path::Path};

use anyhow::{anyhow, Result};
use chrono::Local;
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::models::{Course, KnowledgeChunk, SolvedQuestion, SolveResult, SolveSource};
use crate::parser::ParsedDocument;

pub fn open_database(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)?;
    initialize(&connection)?;
    Ok(connection)
}

fn initialize(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;

        CREATE TABLE IF NOT EXISTS courses (
          id TEXT PRIMARY KEY,
          name TEXT NOT NULL UNIQUE,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS source_documents (
          id TEXT PRIMARY KEY,
          course_id TEXT NOT NULL,
          path TEXT NOT NULL,
          file_name TEXT NOT NULL,
          kind TEXT NOT NULL,
          sha256 TEXT NOT NULL,
          imported_at TEXT NOT NULL,
          UNIQUE(course_id, sha256)
        );

        CREATE TABLE IF NOT EXISTS knowledge_chunks (
          id TEXT PRIMARY KEY,
          course_id TEXT NOT NULL,
          document_id TEXT NOT NULL,
          ordinal INTEGER NOT NULL,
          content TEXT NOT NULL,
          source_file TEXT NOT NULL,
          embedding_json TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS solve_results (
          id TEXT PRIMARY KEY,
          course_id TEXT NOT NULL,
          question TEXT NOT NULL DEFAULT '',
          question_zh TEXT NOT NULL DEFAULT '',
          answer TEXT NOT NULL DEFAULT '',
          explanation TEXT NOT NULL DEFAULT '',
          sources_json TEXT NOT NULL DEFAULT '[]',
          items_json TEXT NOT NULL DEFAULT '[]',
          answer_preview TEXT NOT NULL DEFAULT '',
          confidence REAL NOT NULL DEFAULT 0,
          low_confidence INTEGER NOT NULL DEFAULT 0,
          suggested_file_stem TEXT NOT NULL DEFAULT '',
          output_path TEXT NOT NULL,
          created_at TEXT NOT NULL
        );
        "#,
    )?;

    ensure_column(connection, "solve_results", "question_zh", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(connection, "solve_results", "items_json", "TEXT NOT NULL DEFAULT '[]'")?;
    ensure_column(connection, "solve_results", "answer_preview", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(
        connection,
        "solve_results",
        "suggested_file_stem",
        "TEXT NOT NULL DEFAULT ''",
    )?;

    Ok(())
}

fn ensure_column(connection: &Connection, table: &str, column: &str, definition: &str) -> Result<()> {
    let mut stmt = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    if columns.iter().any(|existing| existing == column) {
        return Ok(());
    }

    connection.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
        [],
    )?;
    Ok(())
}

pub fn upsert_course(connection: &Connection, name: &str) -> Result<String> {
    let existing = connection
        .query_row(
            "SELECT id FROM courses WHERE name = ?1",
            params![name],
            |row| row.get::<_, String>(0),
        )
        .optional()?;

    if let Some(id) = existing {
        connection.execute(
            "UPDATE courses SET updated_at = ?1 WHERE id = ?2",
            params![Local::now().to_rfc3339(), id],
        )?;
        return Ok(id);
    }

    let id = Uuid::new_v4().to_string();
    let now = Local::now().to_rfc3339();
    connection.execute(
        "INSERT INTO courses (id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
        params![id, name, now],
    )?;
    Ok(id)
}

pub fn insert_document(
    connection: &Connection,
    course_id: &str,
    document: &ParsedDocument,
) -> Result<Option<String>> {
    let exists = connection
        .query_row(
            "SELECT id FROM source_documents WHERE course_id = ?1 AND sha256 = ?2",
            params![course_id, document.sha256],
            |row| row.get::<_, String>(0),
        )
        .optional()?;

    if exists.is_some() {
        return Ok(None);
    }

    let id = Uuid::new_v4().to_string();
    connection.execute(
        "INSERT INTO source_documents (id, course_id, path, file_name, kind, sha256, imported_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            course_id,
            document.path.display().to_string(),
            document.file_name,
            document.kind,
            document.sha256,
            Local::now().to_rfc3339(),
        ],
    )?;
    Ok(Some(id))
}

pub fn insert_chunk(
    connection: &Connection,
    course_id: &str,
    document_id: &str,
    ordinal: i64,
    content: &str,
    source_file: &str,
    embedding: &[f32],
) -> Result<()> {
    let id = Uuid::new_v4().to_string();
    let embedding_json = serde_json::to_string(embedding)?;
    connection.execute(
        "INSERT INTO knowledge_chunks (id, course_id, document_id, ordinal, content, source_file, embedding_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, course_id, document_id, ordinal, content, source_file, embedding_json],
    )?;
    Ok(())
}

pub fn list_courses(connection: &Connection, active_course_id: Option<&str>) -> Result<Vec<Course>> {
    let mut stmt = connection.prepare(
        r#"
        SELECT c.id, c.name, c.updated_at,
          (SELECT COUNT(*) FROM source_documents d WHERE d.course_id = c.id) AS document_count,
          (SELECT COUNT(*) FROM knowledge_chunks k WHERE k.course_id = c.id) AS chunk_count
        FROM courses c
        ORDER BY c.updated_at DESC
        "#,
    )?;

    let courses = stmt
        .query_map([], |row| {
            let id = row.get::<_, String>(0)?;
            Ok(Course {
                is_active: active_course_id == Some(id.as_str()),
                id,
                name: row.get(1)?,
                updated_at: row.get(2)?,
                document_count: row.get(3)?,
                chunk_count: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(courses)
}

pub fn query_similar_chunks(
    connection: &Connection,
    course_id: &str,
    query_embedding: &[f32],
    limit: usize,
) -> Result<Vec<KnowledgeChunk>> {
    let mut stmt = connection.prepare(
        "SELECT id, document_id, ordinal, content, source_file, embedding_json
         FROM knowledge_chunks
         WHERE course_id = ?1",
    )?;

    let chunks = stmt
        .query_map(params![course_id], |row| {
            let embedding_json = row.get::<_, String>(5)?;
            let embedding = serde_json::from_str::<Vec<f32>>(&embedding_json).unwrap_or_default();
            Ok((
                KnowledgeChunk {
                    id: row.get(0)?,
                    course_id: course_id.to_string(),
                    document_id: row.get(1)?,
                    ordinal: row.get(2)?,
                    content: row.get(3)?,
                    source_file: row.get(4)?,
                    similarity: Some(cosine_similarity(query_embedding, &embedding)),
                },
                embedding,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(|(chunk, _)| chunk)
        .collect::<Vec<_>>();

    let mut ranked = chunks;
    ranked.sort_by(|left, right| {
        right
            .similarity
            .partial_cmp(&left.similarity)
            .unwrap_or(Ordering::Equal)
    });
    ranked.truncate(limit);
    Ok(ranked)
}

pub fn save_result(connection: &Connection, result: &SolveResult) -> Result<()> {
    let primary = result.items.first().cloned();
    let question = primary.as_ref().map(|item| item.question.clone()).unwrap_or_default();
    let question_zh = primary.as_ref().map(|item| item.question_zh.clone()).unwrap_or_default();
    let answer = primary.as_ref().map(|item| item.answer.clone()).unwrap_or_default();
    let explanation = primary
        .as_ref()
        .map(|item| item.explanation.clone())
        .unwrap_or_default();
    let sources = primary.map(|item| item.sources).unwrap_or_default();

    connection.execute(
        "INSERT INTO solve_results (id, course_id, question, question_zh, answer, explanation, sources_json, items_json, answer_preview, confidence, low_confidence, suggested_file_stem, output_path, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            result.id,
            result.course_id,
            question,
            question_zh,
            answer,
            explanation,
            serde_json::to_string(&sources)?,
            serde_json::to_string(&result.items)?,
            result.answer_preview,
            result.confidence,
            result.low_confidence as i64,
            result.suggested_file_stem,
            result.output_path,
            result.created_at,
        ],
    )?;
    Ok(())
}

pub fn recent_results(connection: &Connection, limit: usize) -> Result<Vec<SolveResult>> {
    let mut stmt = connection.prepare(
        "SELECT id, course_id, question, question_zh, answer, explanation, sources_json, items_json, answer_preview, confidence, low_confidence, suggested_file_stem, output_path, created_at
         FROM solve_results
         ORDER BY created_at DESC
         LIMIT ?1",
    )?;

    let items = stmt
        .query_map(params![limit as i64], row_to_result)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(items)
}

pub fn course_name(connection: &Connection, course_id: &str) -> Result<String> {
    connection
        .query_row(
            "SELECT name FROM courses WHERE id = ?1",
            params![course_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("course not found"))
}

fn row_to_result(row: &rusqlite::Row<'_>) -> rusqlite::Result<SolveResult> {
    let sources_json = row.get::<_, String>(6)?;
    let sources = serde_json::from_str::<Vec<SolveSource>>(&sources_json).unwrap_or_default();
    let items_json = row.get::<_, String>(7)?;
    let mut items = serde_json::from_str::<Vec<SolvedQuestion>>(&items_json).unwrap_or_default();

    if items.is_empty() {
        items.push(SolvedQuestion {
            ordinal: 1,
            question: row.get(2)?,
            question_zh: row.get(3)?,
            answer: row.get(4)?,
            answer_brief: row.get::<_, String>(4)?,
            explanation: row.get(5)?,
            sources,
            confidence: row.get(9)?,
            low_confidence: row.get::<_, i64>(10)? == 1,
        });
    }

    Ok(SolveResult {
        id: row.get(0)?,
        course_id: row.get(1)?,
        item_count: items.len(),
        items,
        answer_preview: row.get(8)?,
        confidence: row.get(9)?,
        low_confidence: row.get::<_, i64>(10)? == 1,
        suggested_file_stem: row.get(11)?,
        output_path: row.get(12)?,
        created_at: row.get(13)?,
    })
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0;
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;

    for (a, b) in left.iter().zip(right.iter()) {
        dot += a * b;
        left_norm += a * a;
        right_norm += b * b;
    }

    if left_norm == 0.0 || right_norm == 0.0 {
        return 0.0;
    }

    dot / (left_norm.sqrt() * right_norm.sqrt())
}
