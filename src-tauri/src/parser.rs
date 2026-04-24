use std::{
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use quick_xml::{events::Event, Reader};
use regex::Regex;
use sha2::{Digest, Sha256};
use zip::ZipArchive;

#[derive(Debug, Clone)]
pub struct ParsedDocument {
    pub path: PathBuf,
    pub file_name: String,
    pub kind: String,
    pub sha256: String,
    pub text: String,
}

pub fn parse_document(path: &Path) -> Result<ParsedDocument> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("invalid file name"))?
        .to_string();
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let text = match ext.as_str() {
        "txt" | "md" => String::from_utf8(bytes.clone()).context("document is not valid utf-8")?,
        "pdf" => pdf_extract::extract_text_from_mem(&bytes).context("failed to extract PDF text")?,
        "docx" => extract_docx_text(&bytes)?,
        "pptx" => extract_pptx_text(&bytes)?,
        other => return Err(anyhow!("unsupported file type: {other}")),
    };

    Ok(ParsedDocument {
        path: path.to_path_buf(),
        file_name,
        kind: ext,
        sha256,
        text: normalize_text(&text),
    })
}

pub fn chunk_text(text: &str, max_chars: usize, overlap: usize) -> Vec<String> {
    let paragraphs = text
        .split("\n\n")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();

    let mut chunks = Vec::new();
    let mut current = String::new();

    for paragraph in paragraphs {
        if current.len() + paragraph.len() + 2 > max_chars && !current.is_empty() {
            chunks.push(current.trim().to_string());
            let carry = current
                .chars()
                .rev()
                .take(overlap)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>();
            current = carry;
        }

        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(paragraph);
    }

    if !current.trim().is_empty() {
        chunks.push(current.trim().to_string());
    }

    chunks
}

fn extract_docx_text(bytes: &[u8]) -> Result<String> {
    let cursor = Cursor::new(bytes);
    let mut archive = ZipArchive::new(cursor)?;
    let mut xml = String::new();
    archive
        .by_name("word/document.xml")?
        .read_to_string(&mut xml)
        .context("failed to read DOCX XML")?;
    extract_xml_text(&xml)
}

fn extract_pptx_text(bytes: &[u8]) -> Result<String> {
    let cursor = Cursor::new(bytes);
    let mut archive = ZipArchive::new(cursor)?;
    let mut names = archive.file_names().map(|name| name.to_string()).collect::<Vec<_>>();
    names.sort();

    let mut output = Vec::new();
    for name in names {
        if name.starts_with("ppt/slides/slide") && name.ends_with(".xml") {
            let mut xml = String::new();
            archive.by_name(&name)?.read_to_string(&mut xml)?;
            output.push(extract_xml_text(&xml)?);
        }
    }

    Ok(output.join("\n\n"))
}

fn extract_xml_text(xml: &str) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut fragments = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Text(text)) => fragments.push(text.decode()?.into_owned()),
            Ok(Event::CData(text)) => fragments.push(text.decode()?.into_owned()),
            Ok(Event::Eof) => break,
            Err(err) => return Err(err.into()),
            _ => {}
        }
        buffer.clear();
    }

    Ok(fragments.join("\n"))
}

fn normalize_text(input: &str) -> String {
    let whitespace = Regex::new(r"[ \t]+").expect("valid regex");
    let blank_lines = Regex::new(r"\n{3,}").expect("valid regex");
    let text = input.replace("\r\n", "\n");
    let text = whitespace.replace_all(&text, " ");
    blank_lines.replace_all(text.trim(), "\n\n").to_string()
}
