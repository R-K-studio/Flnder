use std::{fs, path::Path};

use anyhow::Result;
use chrono::{DateTime, Local};
use docx_rs::*;

use crate::models::{DocxExportJob, SolveResult};

pub fn write_result_docx(course_name: &str, output_path: &Path, result: &SolveResult) -> Result<DocxExportJob> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut doc = Docx::new().add_paragraph(
        Paragraph::new().add_run(
            Run::new()
                .add_text(format!("{course_name} - 多题答案单"))
                .bold()
                .size(36),
        ),
    );

    for item in &result.items {
        let created_at = DateTime::parse_from_rfc3339(&result.created_at)
            .map(|value| value.with_timezone(&Local))
            .unwrap_or_else(|_| Local::now());

        doc = doc
            .add_paragraph(
                Paragraph::new().add_run(
                    Run::new()
                        .add_text(format!("第 {} 题", item.ordinal))
                        .bold()
                        .size(30),
                ),
            )
            .add_paragraph(paragraph("原题", &item.question))
            .add_paragraph(paragraph("题目中文翻译", &item.question_zh))
            .add_paragraph(paragraph("答案", &item.answer))
            .add_paragraph(paragraph("简短答案", &item.answer_brief))
            .add_paragraph(paragraph("解析", &item.explanation))
            .add_paragraph(paragraph("置信度", &format!("{:.0}%", item.confidence * 100.0)))
            .add_paragraph(paragraph(
                "人工复核",
                if item.low_confidence { "需要" } else { "暂不需要" },
            ));

        for source in &item.sources {
            doc = doc.add_paragraph(paragraph("依据来源", &format!("{}：{}", source.title, source.excerpt)));
        }

        doc = doc
            .add_paragraph(paragraph(
                "生成时间",
                &created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            ))
            .add_paragraph(Paragraph::new().add_run(Run::new().add_break(BreakType::Page)));
    }

    let file = fs::File::create(output_path)?;
    doc.build().pack(file)?;

    Ok(DocxExportJob {
        output_path: output_path.display().to_string(),
        written_questions: result.items.len(),
        created_at: Local::now().to_rfc3339(),
    })
}

fn paragraph(label: &str, value: &str) -> Paragraph {
    Paragraph::new()
        .add_run(Run::new().add_text(format!("{label}：")).bold())
        .add_run(Run::new().add_text(value))
}
