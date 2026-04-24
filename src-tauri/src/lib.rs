mod ai;
mod db;
mod exporter;
mod models;
mod parser;
mod settings;

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use ai::{AiClient, QuickAnswer};
use anyhow::{anyhow, Context, Result};
use chrono::Local;
use models::{
    AppSettings, DashboardData, ImportRequest, ImportResponse, SaveSettingsInput, SolvedQuestion, SolveRequest,
    SolveResult, StatusEvent,
};
use parser::{chunk_text, parse_document};
use regex::Regex;
use rusqlite::Connection;
use tauri::{
    menu::{Menu, MenuItem},
    path::BaseDirectory,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tokio::time::sleep;
use tokio::sync::Mutex;
use uuid::Uuid;

const TOTAL_CAPTURE_STEPS: u8 = 6;

struct SharedState {
    service: Arc<Mutex<StudyService>>,
}

#[derive(Clone)]
struct PreparedSolveJob {
    task_id: String,
    settings: AppSettings,
    db_path: PathBuf,
    output_dir: PathBuf,
    course_name: String,
    request: SolveRequest,
    split_questions: Vec<String>,
    context: String,
    quick_preview: String,
}

struct StudyService {
    data_dir: PathBuf,
    db_path: PathBuf,
    settings_path: PathBuf,
    status: String,
    registered_capture_shortcut: Option<String>,
    registered_text_shortcut: Option<String>,
}

impl StudyService {
    fn new(data_dir: PathBuf, settings_path: PathBuf, db_path: PathBuf) -> Result<Self> {
        fs::create_dir_all(&data_dir)?;
        if let Some(parent) = settings_path.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let _ = db::open_database(&db_path)?;
        Ok(Self {
            data_dir,
            db_path,
            settings_path,
            status: "Idle".into(),
            registered_capture_shortcut: None,
            registered_text_shortcut: None,
        })
    }

    fn connection(&self) -> Result<Connection> {
        db::open_database(&self.db_path)
    }

    fn load_settings(&self) -> Result<AppSettings> {
        settings::load_settings(&self.settings_path)
    }

    fn save_settings(&self, input: SaveSettingsInput) -> Result<AppSettings> {
        let current = self.load_settings()?;
        let merged = settings::merge_settings(&current, input);
        settings::save_settings(&self.settings_path, &merged)?;
        Ok(merged)
    }

    fn output_dir(&self, settings: &AppSettings) -> PathBuf {
        if settings.output_dir.trim().is_empty() {
            return self.data_dir.join("exports");
        }
        PathBuf::from(&settings.output_dir)
    }

    fn load_dashboard(&self) -> Result<DashboardData> {
        let settings = self.load_settings()?;
        let connection = self.connection()?;
        Ok(DashboardData {
            courses: db::list_courses(&connection, settings.active_course_id.as_deref())?,
            recent_results: db::recent_results(&connection, 12)?,
            status: self.status.clone(),
            settings,
        })
    }

    async fn import_materials(&mut self, request: ImportRequest) -> Result<ImportResponse> {
        if request.course_name.trim().is_empty() {
            return Err(anyhow!("course name is required"));
        }
        if request.paths.is_empty() {
            return Err(anyhow!("select at least one document"));
        }

        self.status = format!("Importing {} documents…", request.paths.len());
        let settings = self.load_settings()?;
        let ai = AiClient::new(settings)?;
        let connection = self.connection()?;
        let course_id = db::upsert_course(&connection, request.course_name.trim())?;
        let mut imported_documents = 0usize;
        let mut imported_chunks = 0usize;

        for raw_path in request.paths {
            let parsed = parse_document(Path::new(&raw_path))?;
            let Some(document_id) = db::insert_document(&connection, &course_id, &parsed)? else {
                continue;
            };

            let chunks = chunk_text(&parsed.text, 1200, 180);
            for (index, chunk) in chunks.iter().enumerate() {
                let embedding = ai.embed(chunk).await?;
                db::insert_chunk(
                    &connection,
                    &course_id,
                    &document_id,
                    index as i64,
                    chunk,
                    &parsed.file_name,
                    &embedding,
                )?;
                imported_chunks += 1;
            }
            imported_documents += 1;
        }

        let mut updated = self.load_settings()?;
        updated.active_course_id = Some(course_id.clone());
        settings::save_settings(&self.settings_path, &updated)?;
        self.status = format!("Imported {imported_documents} documents");

        Ok(ImportResponse {
            course_id,
            imported_documents,
            imported_chunks,
        })
    }

    async fn capture_and_prepare(&mut self, app: &AppHandle) -> Result<PreparedSolveJob> {
        let settings = self.load_settings()?;
        let course_id = settings
            .active_course_id
            .clone()
            .ok_or_else(|| anyhow!("set an active course before capturing"))?;
        let screenshot_path = capture_to_temp(&self.data_dir).await?;
        self.prepare_solve_job_from_text(
            app,
            course_id,
            String::new(),
            screenshot_path.display().to_string(),
            "识题中",
            "正在从截图中提取题目内容",
            true,
        )
        .await
    }

    async fn clipboard_and_prepare(&mut self, app: &AppHandle) -> Result<PreparedSolveJob> {
        let settings = self.load_settings()?;
        let course_id = settings
            .active_course_id
            .clone()
            .ok_or_else(|| anyhow!("set an active course before solving"))?;
        let clipboard_text = read_clipboard_text().await?;
        self.prepare_solve_job_from_text(
            app,
            course_id,
            clipboard_text,
            String::new(),
            "读取文字中",
            "正在从剪贴板读取题目文字",
            true,
        )
        .await
    }

    async fn prepare_solve_job(&mut self, app: &AppHandle, request: SolveRequest) -> Result<PreparedSolveJob> {
        self.prepare_solve_job_from_text(
            app,
            request.course_id,
            String::new(),
            request.screenshot_path,
            "识题中",
            "正在从截图中提取题目内容",
            true,
        )
        .await
    }

    async fn prepare_solve_job_from_text(
        &mut self,
        app: &AppHandle,
        course_id: String,
        provided_question_text: String,
        screenshot_path: String,
        initial_status: &str,
        initial_detail: &str,
        takeover_progress: bool,
    ) -> Result<PreparedSolveJob> {
        let settings = self.load_settings()?;
        let ai = AiClient::new(settings.clone())?;
        let task_id = Uuid::new_v4().to_string();

        publish_status(
            app,
            &mut self.status,
            &task_id,
            initial_status,
            initial_detail,
            1,
            "",
            takeover_progress,
            false,
            false,
        );
        let question_text = if provided_question_text.trim().is_empty() {
            ai.extract_question_from_image(Path::new(&screenshot_path)).await?
        } else {
            provided_question_text.trim().to_string()
        };

        publish_status(
            app,
            &mut self.status,
            &task_id,
            "拆题中",
            "正在把截图内容拆成多道题目",
            2,
            "",
            true,
            false,
            false,
        );
        let split_questions = split_questions(&question_text);
        if split_questions.is_empty() {
            return Err(anyhow!("failed to split any questions from screenshot"));
        }

        publish_status(
            app,
            &mut self.status,
            &task_id,
            "检索课件中",
            "正在召回相关课件知识片段",
            3,
            "",
            true,
            false,
            false,
        );
        let connection = self.connection()?;
        let query_embedding = ai.embed(&question_text).await?;
        let retrieved = db::query_similar_chunks(&connection, &course_id, &query_embedding, 6)?;
        let context = retrieved
            .iter()
            .map(|chunk| format!("[{}]\n{}", chunk.source_file, chunk.content))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        publish_status(
            app,
            &mut self.status,
            &task_id,
            "快速求答中",
            "正在优先生成最终答案摘要",
            4,
            "",
            true,
            false,
            false,
        );
        let quick_answers = ai.solve_quick_answers(&split_questions, &context).await?;
        let quick_preview = build_quick_preview(&quick_answers, split_questions.len());
        publish_status(
            app,
            &mut self.status,
            &task_id,
            "已拿到答案",
            "已得到答案摘要，可以继续截图；完整解析会在后台继续生成",
            4,
            &quick_preview,
            true,
            false,
            false,
        );

        let course_name = db::course_name(&connection, &course_id)?;
        Ok(PreparedSolveJob {
            task_id,
            settings: settings.clone(),
            db_path: self.db_path.clone(),
            output_dir: self.output_dir(&settings),
            course_name,
            request: SolveRequest { course_id, screenshot_path },
            split_questions,
            context,
            quick_preview,
        })
    }
}

#[tauri::command]
async fn load_dashboard(state: State<'_, SharedState>) -> Result<DashboardData, String> {
    let service = state.service.lock().await;
    service.load_dashboard().map_err(error_to_string)
}

#[tauri::command]
async fn save_app_settings(
    app: AppHandle,
    state: State<'_, SharedState>,
    input: SaveSettingsInput,
) -> Result<DashboardData, String> {
    let shortcut = input.shortcut.clone();
    let text_shortcut = input.text_shortcut.clone();
    {
        let service = state.service.lock().await;
        service.save_settings(input).map_err(error_to_string)?;
    }
    refresh_shortcuts(&app, &shortcut, &text_shortcut).await.map_err(error_to_string)?;
    let service = state.service.lock().await;
    service.load_dashboard().map_err(error_to_string)
}

#[tauri::command]
async fn import_course_materials(
    state: State<'_, SharedState>,
    request: ImportRequest,
) -> Result<ImportResponse, String> {
    let mut service = state.service.lock().await;
    service.import_materials(request).await.map_err(error_to_string)
}

#[tauri::command]
async fn run_capture_and_solve(
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let prepared = {
        let mut service = state.service.lock().await;
        service.capture_and_prepare(&app).await
    };

    match prepared {
        Ok(job) => {
            spawn_finalize_job(app.clone(), job);
            Ok(())
        }
        Err(err) => {
            let mut status = "失败".to_string();
            publish_status(&app, &mut status, "", "失败", &err.to_string(), 6, "", true, true, true);
            Err(error_to_string(err))
        }
    }
}

#[tauri::command]
async fn run_clipboard_text_and_solve(
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let prepared = {
        let mut service = state.service.lock().await;
        service.clipboard_and_prepare(&app).await
    };

    match prepared {
        Ok(job) => {
            spawn_finalize_job(app.clone(), job);
            Ok(())
        }
        Err(err) => {
            let mut status = "失败".to_string();
            publish_status(&app, &mut status, "", "失败", &err.to_string(), 6, "", true, true, true);
            Err(error_to_string(err))
        }
    }
}

#[tauri::command]
async fn solve_from_screenshot(
    app: AppHandle,
    state: State<'_, SharedState>,
    request: SolveRequest,
) -> Result<(), String> {
    let prepared = {
        let mut service = state.service.lock().await;
        service.prepare_solve_job(&app, request).await
    };

    match prepared {
        Ok(job) => {
            spawn_finalize_job(app.clone(), job);
            Ok(())
        }
        Err(err) => {
            let mut status = "失败".to_string();
            publish_status(&app, &mut status, "", "失败", &err.to_string(), 6, "", true, true, true);
            Err(error_to_string(err))
        }
    }
}

#[tauri::command]
async fn set_active_course(
    state: State<'_, SharedState>,
    course_id: String,
) -> Result<DashboardData, String> {
    let service = state.service.lock().await;
    let current = service.load_settings().map_err(error_to_string)?;
        let merged = SaveSettingsInput {
            api_base: current.api_base,
            api_key: current.api_key,
            vision_model: current.vision_model,
            fast_answer_model: current.fast_answer_model,
            answer_model: current.answer_model,
            embedding_model: current.embedding_model,
            output_dir: current.output_dir,
            shortcut: current.shortcut,
            text_shortcut: current.text_shortcut,
            active_course_id: Some(course_id),
    };
    service.save_settings(merged).map_err(error_to_string)?;
    service.load_dashboard().map_err(error_to_string)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::default().level(log::LevelFilter::Info).build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let app_handle = app.handle().clone();
            let data_dir = app
                .path()
                .resolve("study-assistant", BaseDirectory::AppData)
                .context("failed to resolve app data directory")?;
            let settings_path = app
                .path()
                .resolve("study-assistant/settings.json", BaseDirectory::AppConfig)
                .context("failed to resolve settings path")?;
            let db_path = data_dir.join("assistant.sqlite");

            let service = StudyService::new(data_dir, settings_path, db_path)?;
            let settings = service.load_settings()?;
            app.manage(SharedState {
                service: Arc::new(Mutex::new(service)),
            });

            build_tray(&app_handle)?;
            setup_window_lifecycle(&app_handle, "main")?;
            setup_progress_window(&app_handle)?;

            if let Some(window) = app.get_webview_window("main") {
                if cfg!(target_os = "windows") {
                    let _ = window.show();
                    let _ = window.set_focus();
                } else {
                    let _ = window.hide();
                }
            }

            let shortcut = settings.shortcut.clone();
            let text_shortcut = settings.text_shortcut.clone();
            tauri::async_runtime::spawn(async move {
                let _ = refresh_shortcuts(&app_handle, &shortcut, &text_shortcut).await;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            load_dashboard,
            save_app_settings,
            import_course_materials,
            run_capture_and_solve,
            run_clipboard_text_and_solve,
            solve_from_screenshot,
            set_active_course
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn build_tray(app: &AppHandle) -> Result<()> {
    let open_item = MenuItem::with_id(app, "open", "Open Dashboard", true, None::<&str>)?;
    let capture_item = MenuItem::with_id(app, "capture", "Capture Question", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open_item, &capture_item, &quit_item])?;

    let icon = app.default_window_icon().cloned();
    let mut builder = TrayIconBuilder::with_id("study-assistant").menu(&menu).show_menu_on_left_click(false);
    if let Some(icon) = icon {
        builder = builder.icon(icon);
    }

    builder
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if let Some(window) = tray.app_handle().get_webview_window("main") {
                    if window.is_visible().unwrap_or(false) {
                        let _ = window.hide();
                    } else {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
            }
        })
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "capture" => spawn_capture_flow(app.clone()),
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;

    Ok(())
}

fn spawn_capture_flow(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<SharedState>();
        let prepared = {
            let mut service = state.service.lock().await;
            service.capture_and_prepare(&app).await
        };
        match prepared {
            Ok(job) => spawn_finalize_job(app.clone(), job),
            Err(err) => {
                let mut status = "失败".to_string();
                publish_status(&app, &mut status, "", "失败", &err.to_string(), 6, "", true, true, true);
            }
        }
    });
}

fn spawn_clipboard_flow(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<SharedState>();
        let prepared = {
            let mut service = state.service.lock().await;
            service.clipboard_and_prepare(&app).await
        };
        match prepared {
            Ok(job) => spawn_finalize_job(app.clone(), job),
            Err(err) => {
                let mut status = "失败".to_string();
                publish_status(&app, &mut status, "", "失败", &err.to_string(), 6, "", true, true, true);
            }
        }
    });
}

fn setup_window_lifecycle(app: &AppHandle, label: &str) -> Result<()> {
    if let Some(window) = app.get_webview_window(label) {
        let app_handle = app.clone();
        let label = label.to_string();
        window.on_window_event(move |event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if let Some(window) = app_handle.get_webview_window(&label) {
                    let _ = window.hide();
                }
            }
        });
    }
    Ok(())
}

fn setup_progress_window(app: &AppHandle) -> Result<()> {
    if app.get_webview_window("progress").is_none() {
        let progress = WebviewWindowBuilder::new(
            app,
            "progress",
            WebviewUrl::App("index.html?view=progress".into()),
        )
        .title("生成进度")
        .inner_size(180.0, 78.0)
        .resizable(false)
        .visible(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .decorations(true)
        .build()?;

        let app_handle = app.clone();
        progress.on_window_event(move |event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if let Some(window) = app_handle.get_webview_window("progress") {
                    let _ = window.hide();
                }
            }
        });
    }

    Ok(())
}

async fn refresh_shortcuts(app: &AppHandle, shortcut: &str, text_shortcut: &str) -> Result<()> {
    let state = app.state::<SharedState>();
    let mut service = state.service.lock().await;
    if let Some(previous) = service.registered_capture_shortcut.take() {
        let _ = app.global_shortcut().unregister(previous.as_str());
    }
    if let Some(previous) = service.registered_text_shortcut.take() {
        let _ = app.global_shortcut().unregister(previous.as_str());
    }

    let capture_trigger = shortcut.trim().to_string();
    let text_trigger = text_shortcut.trim().to_string();

    let app_handle = app.clone();
    if !capture_trigger.is_empty() {
        app.global_shortcut().on_shortcut(capture_trigger.as_str(), move |_, _, event| {
            if !matches!(event.state, ShortcutState::Pressed) {
                return;
            }
            spawn_capture_flow(app_handle.clone());
        })?;
        service.registered_capture_shortcut = Some(capture_trigger);
    }

    if !text_trigger.is_empty() {
        let app_handle = app.clone();
        app.global_shortcut().on_shortcut(text_trigger.as_str(), move |_, _, event| {
            if !matches!(event.state, ShortcutState::Pressed) {
                return;
            }
            spawn_clipboard_flow(app_handle.clone());
        })?;
        service.registered_text_shortcut = Some(text_trigger);
    }

    Ok(())
}

async fn capture_to_temp(data_dir: &Path) -> Result<PathBuf> {
    let capture_dir = data_dir.join("captures");
    fs::create_dir_all(&capture_dir)?;
    let path = capture_dir.join(format!("capture-{}.png", Local::now().timestamp_millis()));
    if cfg!(target_os = "windows") {
        capture_to_temp_windows(&path).await?;
    } else {
        let status = tokio::process::Command::new("screencapture")
            .arg("-i")
            .arg(&path)
            .status()
            .await
            .context("failed to run macOS screencapture")?;

        if !status.success() || !path.exists() {
            return Err(anyhow!("capture was cancelled or failed"));
        }
    }

    Ok(path)
}

async fn read_clipboard_text() -> Result<String> {
    let text = if cfg!(target_os = "windows") {
        let output = tokio::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", "Get-Clipboard -Raw"])
            .output()
            .await
            .context("failed to read Windows clipboard")?;

        if !output.status.success() {
            return Err(anyhow!("failed to read clipboard text"));
        }

        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        let output = tokio::process::Command::new("pbpaste")
            .output()
            .await
            .context("failed to read macOS clipboard")?;

        if !output.status.success() {
            return Err(anyhow!("failed to read clipboard text"));
        }

        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    if text.is_empty() {
        return Err(anyhow!("clipboard does not contain readable text"));
    }

    Ok(text)
}

async fn capture_to_temp_windows(path: &Path) -> Result<()> {
    let clear_status = tokio::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-STA",
            "-Command",
            "Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.Clipboard]::Clear()",
        ])
        .status()
        .await
        .context("failed to clear Windows clipboard before capture")?;

    if !clear_status.success() {
        return Err(anyhow!("failed to clear clipboard before starting capture"));
    }

    let launch_status = tokio::process::Command::new("cmd")
        .args(["/C", "start", "", "ms-screenclip:"])
        .status()
        .await
        .context("failed to launch Windows screen snipping")?;

    if !launch_status.success() {
        return Err(anyhow!("failed to launch Windows screen snipping"));
    }

    for _ in 0..180 {
        if save_windows_clipboard_image(path).await? {
            return Ok(());
        }
        sleep(Duration::from_millis(500)).await;
    }

    Err(anyhow!("capture was cancelled or no image was copied to the clipboard"))
}

async fn save_windows_clipboard_image(path: &Path) -> Result<bool> {
    let escaped_path = path.display().to_string().replace('\'', "''");
    let script = format!(
        "$ErrorActionPreference = 'Stop'; \
         Add-Type -AssemblyName System.Windows.Forms; \
         Add-Type -AssemblyName System.Drawing; \
         if ([System.Windows.Forms.Clipboard]::ContainsImage()) {{ \
             $img = [System.Windows.Forms.Clipboard]::GetImage(); \
             $img.Save('{escaped_path}', [System.Drawing.Imaging.ImageFormat]::Png); \
             Write-Output 'saved'; \
         }}"
    );

    let output = tokio::process::Command::new("powershell")
        .args(["-NoProfile", "-STA", "-Command", &script])
        .output()
        .await
        .context("failed to inspect Windows clipboard image")?;

    if !output.status.success() {
        return Err(anyhow!(
            "failed to read screenshot from Windows clipboard: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).contains("saved") && path.exists())
}

fn spawn_finalize_job(app: AppHandle, job: PreparedSolveJob) {
    tauri::async_runtime::spawn(async move {
        if let Err(err) = finalize_solve_job(app.clone(), job).await {
            publish_shared_status(
                &app,
                "",
                "失败",
                &format!("后台完整解析失败：{err}"),
                6,
                "",
                false,
                true,
                true,
            )
            .await;
        }
    });
}

async fn finalize_solve_job(app: AppHandle, job: PreparedSolveJob) -> Result<()> {
    publish_shared_status(
        &app,
        &job.task_id,
        "完整求答中",
        "正在后台生成中文翻译、解析和完整答案；你现在可以继续截图下一题",
        5,
        &job.quick_preview,
        false,
        false,
        false,
    )
    .await;

    let ai = AiClient::new(job.settings.clone())?;
    let solved = ai.solve_questions(&job.split_questions, &job.context).await?;
    if solved.questions.is_empty() {
        return Err(anyhow!("model did not return any solved questions"));
    }

    let items = solved
        .questions
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            let answer_brief = normalize_answer_brief(&item.answer_brief, &item.answer);
            SolvedQuestion {
                ordinal: index + 1,
                question: item.question,
                question_zh: item.question_zh,
                answer: item.answer,
                answer_brief,
                explanation: item.explanation,
                sources: item.sources,
                confidence: item.confidence,
                low_confidence: item.low_confidence,
            }
        })
        .collect::<Vec<_>>();

    let answer_preview = if job.quick_preview.trim().is_empty() {
        build_answer_preview(&items)
    } else {
        job.quick_preview.clone()
    };

    let file_stem = suggest_file_stem(&items);
    let output_path = ensure_unique_output_path(&job.output_dir, &file_stem);
    let result = SolveResult {
        id: Uuid::new_v4().to_string(),
        course_id: job.request.course_id.clone(),
        item_count: items.len(),
        confidence: average_confidence(&items),
        low_confidence: items.iter().any(|item| item.low_confidence),
        items,
        answer_preview: answer_preview.clone(),
        suggested_file_stem: file_stem,
        output_path: output_path.display().to_string(),
        created_at: Local::now().to_rfc3339(),
    };

    publish_shared_status(
        &app,
        &job.task_id,
        "写入文档中",
        "后台正在写入完整解析文档",
        6,
        &answer_preview,
        false,
        false,
        false,
    )
    .await;

    exporter::write_result_docx(&job.course_name, &output_path, &result)?;
    let connection = db::open_database(&job.db_path)?;
    db::save_result(&connection, &result)?;

    publish_shared_status(
        &app,
        &job.task_id,
        "已完成",
        "后台完整解析和文档已生成，你可以继续截图下一题",
        6,
        &answer_preview,
        false,
        true,
        false,
    )
    .await;

    Ok(())
}

async fn publish_shared_status(
    app: &AppHandle,
    task_id: &str,
    status: &str,
    detail: &str,
    current_step: u8,
    answer_preview: &str,
    takeover_progress: bool,
    is_terminal: bool,
    is_error: bool,
) {
    let state = app.state::<SharedState>();
    let mut service = state.service.lock().await;
    publish_status(
        app,
        &mut service.status,
        task_id,
        status,
        detail,
        current_step,
        answer_preview,
        takeover_progress,
        is_terminal,
        is_error,
    );
}

fn publish_status(
    app: &AppHandle,
    stored_status: &mut String,
    task_id: &str,
    status: &str,
    detail: &str,
    current_step: u8,
    answer_preview: &str,
    takeover_progress: bool,
    is_terminal: bool,
    is_error: bool,
) {
    *stored_status = status.to_string();
    if let Some(window) = app.get_webview_window("progress") {
        let _ = window.show();
    }
    let _ = app.emit(
        "study://status",
        StatusEvent {
            task_id: task_id.to_string(),
            status: status.to_string(),
            detail: detail.to_string(),
            current_step,
            total_steps: TOTAL_CAPTURE_STEPS,
            answer_preview: answer_preview.to_string(),
            takeover_progress,
            is_terminal,
            is_error,
            when: Local::now(),
        },
    );
}

fn split_questions(text: &str) -> Vec<String> {
    let normalized = text.replace("\r\n", "\n");
    let numbered = Regex::new(r"(?m)^(?:Q(?:uestion)?\s*\d+|第\s*\d+\s*题|\d+\s*[\.\):、])").expect("valid regex");
    if numbered.find_iter(&normalized).count() >= 2 {
        return split_by_markers(&normalized, &numbered);
    }

    if looks_like_single_choice_question(&normalized) {
        let single = normalized.trim();
        return if single.is_empty() {
            Vec::new()
        } else {
            vec![single.to_string()]
        };
    }

    let choice_heads = Regex::new(r"(?m)^(?:[A-D][\.\):、]|[①②③④])").expect("valid regex");
    let blank_separated = normalized
        .split("\n\n")
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if blank_separated.len() >= 2
        && blank_separated.iter().filter(|s| choice_heads.is_match(s)).count() < blank_separated.len()
        && !blank_separated.iter().skip(1).all(|segment| is_choice_option_only(segment))
    {
        return blank_separated.into_iter().map(ToString::to_string).collect();
    }

    vec![normalized.trim().to_string()]
}

fn looks_like_single_choice_question(text: &str) -> bool {
    let option_line = Regex::new(r"(?m)^\s*[A-D][\.\):、]\s+.+$").expect("valid regex");
    let option_count = option_line.find_iter(text).count();
    if option_count < 2 {
        return false;
    }

    let numbered = Regex::new(r"(?m)^(?:Q(?:uestion)?\s*\d+|第\s*\d+\s*题|\d+\s*[\.\):、])").expect("valid regex");
    numbered.find_iter(text).count() <= 1
}

fn is_choice_option_only(text: &str) -> bool {
    let option_line = Regex::new(r"(?m)^\s*[A-D][\.\):、]\s+.+$").expect("valid regex");
    let non_empty_lines = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>();
    !non_empty_lines.is_empty() && non_empty_lines.iter().all(|line| option_line.is_match(line))
}

fn split_by_markers(text: &str, pattern: &Regex) -> Vec<String> {
    let starts = pattern.find_iter(text).map(|m| m.start()).collect::<Vec<_>>();
    let mut items = Vec::new();
    for (index, start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(text.len());
        let part = text[*start..end].trim();
        if !part.is_empty() {
            items.push(part.to_string());
        }
    }
    items
}

fn build_quick_preview(answers: &[QuickAnswer], expected_len: usize) -> String {
    let mut normalized = answers
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let brief = normalize_answer_brief(&item.answer_brief, &item.answer_brief);
            let number = extract_question_number(&item.question).unwrap_or_else(|| (index + 1).to_string());
            format!("{number}.{brief}")
        })
        .collect::<Vec<_>>();

    if normalized.is_empty() && expected_len > 0 {
        normalized = (1..=expected_len).map(|index| format!("{index}.?")).collect();
    }

    normalized.join("  ")
}

fn build_answer_preview(items: &[SolvedQuestion]) -> String {
    items
        .iter()
        .map(|item| format!("{}.{}", item.ordinal, normalize_answer_brief(&item.answer_brief, &item.answer)))
        .collect::<Vec<_>>()
        .join("  ")
}

fn normalize_answer_brief(answer_brief: &str, answer: &str) -> String {
    let trimmed = answer_brief.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }

    let choice = Regex::new(r"(?i)\b([A-D]{1,4})\b").expect("valid regex");
    if let Some(captures) = choice.captures(answer) {
        if let Some(value) = captures.get(1) {
            return value.as_str().to_uppercase();
        }
    }

    answer.split_whitespace().take(3).collect::<Vec<_>>().join(" ")
}

fn average_confidence(items: &[SolvedQuestion]) -> f32 {
    if items.is_empty() {
        return 0.0;
    }
    items.iter().map(|item| item.confidence).sum::<f32>() / items.len() as f32
}

fn suggest_file_stem(items: &[SolvedQuestion]) -> String {
    let numbers = items
        .iter()
        .filter_map(|item| extract_question_number(&item.question))
        .collect::<Vec<_>>();

    if let (Some(first), Some(last)) = (numbers.first(), numbers.last()) {
        if numbers.len() > 1 {
            return format!("第{}-{}题", first, last);
        }
        return format!("第{}题", first);
    }

    if let Some(first) = items.first() {
        let token_pattern = Regex::new(r"[\p{L}\p{N}]+").expect("valid regex");
        let tokens = token_pattern
            .find_iter(&first.question)
            .take(3)
            .map(|item| item.as_str())
            .collect::<Vec<_>>();

        if !tokens.is_empty() {
            return sanitize_file_name(&tokens.join("-"));
        }
    }

    format!("questions-{}", Local::now().timestamp())
}

fn extract_question_number(question: &str) -> Option<String> {
    let patterns = [
        Regex::new(r"(?i)\bq(?:uestion)?\s*([0-9]{1,3})\b").expect("valid regex"),
        Regex::new(r"第\s*([0-9]{1,3})\s*题").expect("valid regex"),
        Regex::new(r"^\s*([0-9]{1,3})\s*[\.\):、]").expect("valid regex"),
    ];

    for pattern in patterns {
        if let Some(captures) = pattern.captures(question) {
            if let Some(value) = captures.get(1) {
                return Some(value.as_str().to_string());
            }
        }
    }
    None
}

fn ensure_unique_output_path(base_dir: &Path, file_stem: &str) -> PathBuf {
    let safe_stem = sanitize_file_name(file_stem).trim().to_string();
    let stem = if safe_stem.is_empty() {
        format!("questions-{}", Local::now().timestamp())
    } else {
        safe_stem
    };

    let mut attempt = 1usize;
    loop {
        let candidate = if attempt == 1 {
            base_dir.join(format!("{stem}.docx"))
        } else {
            base_dir.join(format!("{stem}-{attempt}.docx"))
        };

        if !candidate.exists() {
            return candidate;
        }
        attempt += 1;
    }
}

fn sanitize_file_name(name: &str) -> String {
    name.chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => ch,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn error_to_string(error: impl std::fmt::Display) -> String {
    error.to_string()
}
