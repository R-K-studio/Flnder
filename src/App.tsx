import { startTransition, useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { open } from '@tauri-apps/plugin-dialog'
import './App.css'

type AppSettings = {
  apiBase: string
  apiKey: string
  visionModel: string
  fastAnswerModel: string
  answerModel: string
  embeddingModel: string
  outputDir: string
  shortcut: string
  textShortcut: string
  activeCourseId: string | null
}

type Course = {
  id: string
  name: string
  documentCount: number
  chunkCount: number
  updatedAt: string
  isActive: boolean
}

type SolveSource = {
  title: string
  excerpt: string
}

type SolvedQuestion = {
  ordinal: number
  question: string
  questionZh: string
  answer: string
  answerBrief: string
  explanation: string
  sources: SolveSource[]
  confidence: number
  lowConfidence: boolean
}

type SolveResult = {
  id: string
  courseId: string
  itemCount: number
  items: SolvedQuestion[]
  answerPreview: string
  confidence: number
  lowConfidence: boolean
  suggestedFileStem: string
  outputPath: string
  createdAt: string
}

type DashboardData = {
  settings: AppSettings
  courses: Course[]
  recentResults: SolveResult[]
  status: string
}

type StatusEvent = {
  taskId: string
  status: string
  detail: string
  currentStep: number
  totalSteps: number
  answerPreview: string
  takeoverProgress: boolean
  isTerminal: boolean
  isError: boolean
}

const emptySettings: AppSettings = {
  apiBase: 'https://api.siliconflow.cn/v1',
  apiKey: '',
  visionModel: 'THUDM/GLM-4.1V-9B-Thinking',
  fastAnswerModel: 'Qwen/Qwen3-32B',
  answerModel: 'Pro/zai-org/GLM-5.1',
  embeddingModel: 'BAAI/bge-m3',
  outputDir: '',
  shortcut: 'CommandOrControl+Shift+1',
  textShortcut: 'CommandOrControl+Shift+2',
  activeCourseId: null,
}

const initialProgress: StatusEvent = {
  taskId: '',
  status: '待命中',
  detail: '截图后会在这里显示多题进度。',
  currentStep: 0,
  totalSteps: 6,
  answerPreview: '',
  takeoverProgress: false,
  isTerminal: false,
  isError: false,
}

function App() {
  const view = new URLSearchParams(window.location.search).get('view')
  return view === 'progress' ? <ProgressView /> : <DashboardView />
}

function DashboardView() {
  const [dashboard, setDashboard] = useState<DashboardData | null>(null)
  const [settings, setSettings] = useState<AppSettings>(emptySettings)
  const [courseName, setCourseName] = useState('')
  const [selectedPaths, setSelectedPaths] = useState<string[]>([])
  const [busy, setBusy] = useState('')
  const [message, setMessage] = useState('')

  function applySiliconFlowPreset() {
    startTransition(() => {
      setSettings((current) => ({
        ...current,
        apiBase: 'https://api.siliconflow.cn/v1',
        visionModel: 'THUDM/GLM-4.1V-9B-Thinking',
        fastAnswerModel: 'Qwen/Qwen3-32B',
        answerModel: 'Pro/zai-org/GLM-5.1',
        embeddingModel: 'BAAI/bge-m3',
        textShortcut: 'CommandOrControl+Shift+2',
      }))
      setMessage('已套用 SiliconFlow 推荐预设。')
    })
  }

  useEffect(() => {
    void refreshDashboard()

    const unlistenPromise = listen<StatusEvent>('study://status', (event) => {
      startTransition(() => {
        setMessage(event.payload.detail)
      })
      void refreshDashboard()
    })

    return () => {
      void unlistenPromise.then((unlisten) => unlisten())
    }
  }, [])

  async function refreshDashboard() {
    const next = await invoke<DashboardData>('load_dashboard')
    startTransition(() => {
      setDashboard(next)
      setSettings(next.settings)
    })
  }

  async function handleSaveSettings() {
    setBusy('settings')
    setMessage('')
    try {
      const next = await invoke<DashboardData>('save_app_settings', { input: settings })
      startTransition(() => {
        setDashboard(next)
        setSettings(next.settings)
        setMessage('Settings saved.')
      })
    } catch (error) {
      setMessage(String(error))
    } finally {
      setBusy('')
    }
  }

  async function handlePickFiles() {
    const picked = await open({
      directory: false,
      multiple: true,
      filters: [{ name: 'Course materials', extensions: ['pdf', 'docx', 'pptx', 'txt', 'md'] }],
    })

    if (!picked) {
      return
    }

    startTransition(() => {
      setSelectedPaths(Array.isArray(picked) ? picked : [picked])
    })
  }

  async function handleImport() {
    if (!courseName.trim() || selectedPaths.length === 0) {
      setMessage('Add a course name and select at least one document.')
      return
    }

    setBusy('import')
    setMessage('')
    try {
      await invoke('import_course_materials', {
        request: {
          courseName,
          paths: selectedPaths,
        },
      })
      setCourseName('')
      setSelectedPaths([])
      setMessage('Course material imported.')
      await refreshDashboard()
    } catch (error) {
      setMessage(String(error))
    } finally {
      setBusy('')
    }
  }

  async function handleCaptureNow() {
    setBusy('capture')
    setMessage('')
    try {
      await invoke('run_capture_and_solve')
      setMessage('已拿到快速答案，完整解析会在后台继续生成。')
    } catch (error) {
      setMessage(String(error))
    } finally {
      setBusy('')
    }
  }

  async function handleClipboardSolveNow() {
    setBusy('clipboard')
    setMessage('')
    try {
      await invoke('run_clipboard_text_and_solve')
      setMessage('已从剪贴板拿到快速答案，完整解析会在后台继续生成。')
    } catch (error) {
      setMessage(String(error))
    } finally {
      setBusy('')
    }
  }

  async function handleSetActiveCourse(courseId: string) {
    setBusy(courseId)
    try {
      const next = await invoke<DashboardData>('set_active_course', { courseId })
      startTransition(() => {
        setDashboard(next)
        setSettings(next.settings)
      })
    } catch (error) {
      setMessage(String(error))
    } finally {
      setBusy('')
    }
  }

  const courses = dashboard?.courses ?? []
  const results = dashboard?.recentResults ?? []

  return (
    <main className="shell">
      <section className="hero-panel">
        <div>
          <p className="eyebrow">macOS menu bar study assistant</p>
          <h1>一张截图支持多题识别，答案汇总后导出到 `.docx`</h1>
          <p className="lede">
            现在同一张截图里的多道题会按前后顺序一起求解，写入同一个文档；小型悬浮进度窗会在完成后直接显示形如 `1.A  2.C  3.B` 的答案列表。
          </p>
        </div>
        <div className="status-card">
          <span className="status-dot" />
          <div>
            <strong>{dashboard?.status ?? 'Loading…'}</strong>
            <p>{message || 'Ready for multi-question capture solving.'}</p>
          </div>
          <button className="primary" onClick={() => void handleCaptureNow()} disabled={busy !== ''}>
            {busy === 'capture' ? '处理中…' : '立即截图求解'}
          </button>
          <button className="secondary" onClick={() => void handleClipboardSolveNow()} disabled={busy !== ''}>
            {busy === 'clipboard' ? '读取中…' : '读取复制文字求解'}
          </button>
        </div>
      </section>

      <section className="grid">
        <article className="panel">
          <div className="panel-head">
            <h2>课程知识库</h2>
            <span>{courses.length} 门课程</span>
          </div>

          <label className="field">
            <span>课程名称</span>
            <input value={courseName} onChange={(event) => setCourseName(event.target.value)} placeholder="例如：高等数学" />
          </label>

          <div className="field">
            <span>课件文件</span>
            <button className="secondary" onClick={() => void handlePickFiles()}>
              选择 PDF / DOCX / PPTX / TXT / Markdown
            </button>
            <p className="hint">{selectedPaths.length > 0 ? `已选择 ${selectedPaths.length} 个文件` : '尚未选择文件'}</p>
          </div>

          <button className="primary" onClick={() => void handleImport()} disabled={busy !== ''}>
            {busy === 'import' ? '导入中…' : '导入并建立知识库'}
          </button>

          <div className="course-list">
            {courses.map((course) => (
              <button
                key={course.id}
                className={`course-card ${course.isActive ? 'active' : ''}`}
                onClick={() => void handleSetActiveCourse(course.id)}
                disabled={busy !== ''}
              >
                <strong>{course.name}</strong>
                <span>{course.documentCount} 份资料</span>
                <span>{course.chunkCount} 个知识片段</span>
                <em>{course.isActive ? '当前解题课程' : '设为当前课程'}</em>
              </button>
            ))}
          </div>
        </article>

        <article className="panel">
          <div className="panel-head">
            <h2>模型与输出</h2>
            <span>SiliconFlow / OpenAI 兼容</span>
          </div>

          <div className="provider-tip">
            <strong>推荐给 SiliconFlow 的组合</strong>
            <p>`Qwen/Qwen3-32B` 负责先快速给答案，`Pro/zai-org/GLM-5.1` 负责后台补全翻译、解析和文档。</p>
            <button className="secondary" onClick={applySiliconFlowPreset} disabled={busy !== ''}>
              一键套用 SiliconFlow 预设
            </button>
          </div>

          <div className="settings-grid">
            <label className="field">
              <span>API Base URL</span>
              <input value={settings.apiBase} onChange={(event) => setSettings({ ...settings, apiBase: event.target.value })} placeholder="https://api.siliconflow.cn/v1" />
            </label>
            <label className="field">
              <span>API Key</span>
              <input type="password" value={settings.apiKey} onChange={(event) => setSettings({ ...settings, apiKey: event.target.value })} placeholder="sk-..." />
            </label>
            <label className="field">
              <span>视觉模型</span>
              <input value={settings.visionModel} onChange={(event) => setSettings({ ...settings, visionModel: event.target.value })} placeholder="THUDM/GLM-4.1V-9B-Thinking" />
              <p className="hint">用于从一张截图中提取多道题。</p>
            </label>
            <label className="field">
              <span>快速答案模型</span>
              <input value={settings.fastAnswerModel} onChange={(event) => setSettings({ ...settings, fastAnswerModel: event.target.value })} placeholder="Qwen/Qwen3-32B" />
              <p className="hint">先尽快输出 `1.A  2.C  3.B` 这类摘要，并立即释放下一次截图。</p>
            </label>
            <label className="field">
              <span>完整解析模型</span>
              <input value={settings.answerModel} onChange={(event) => setSettings({ ...settings, answerModel: event.target.value })} placeholder="Pro/zai-org/GLM-5.1" />
              <p className="hint">在后台补全中文翻译、完整答案、解析和 `.docx`。</p>
            </label>
            <label className="field">
              <span>Embedding 模型</span>
              <input value={settings.embeddingModel} onChange={(event) => setSettings({ ...settings, embeddingModel: event.target.value })} placeholder="BAAI/bge-m3" />
              <p className="hint">用于课件切块向量化和语义检索。</p>
            </label>
            <label className="field">
              <span>截图快捷键</span>
              <input value={settings.shortcut} onChange={(event) => setSettings({ ...settings, shortcut: event.target.value })} />
            </label>
            <label className="field">
              <span>文字快捷键</span>
              <input value={settings.textShortcut} onChange={(event) => setSettings({ ...settings, textShortcut: event.target.value })} />
              <p className="hint">先复制题目文字，再按这里设置的快捷键直接求解。</p>
            </label>
            <label className="field field-wide">
              <span>`.docx` 输出目录</span>
              <input value={settings.outputDir} onChange={(event) => setSettings({ ...settings, outputDir: event.target.value })} placeholder="/Users/yourname/Documents/answers" />
              <p className="hint">单次截图会输出一个多题文档。</p>
            </label>
          </div>

          <button className="primary" onClick={() => void handleSaveSettings()} disabled={busy !== ''}>
            {busy === 'settings' ? '保存中…' : '保存设置并更新快捷键'}
          </button>
        </article>
      </section>

      <section className="panel jobs">
        <div className="panel-head">
          <h2>最近输出</h2>
          <span>{results.length} 条记录</span>
        </div>

        {results.length === 0 ? (
          <p className="empty">还没有求解记录。先导入课件，再运行一次截图求解。</p>
        ) : (
          <div className="job-list">
            {results.map((result) => (
              <article key={result.id} className="job-card">
                <div className="job-head">
                  <strong>{new Date(result.createdAt).toLocaleString()}</strong>
                  <span className={result.lowConfidence ? 'warn' : 'ok'}>
                    {result.lowConfidence ? '需要人工复核' : '置信度较高'}
                  </span>
                </div>
                <p className="job-summary">{result.answerPreview}</p>
                {result.items.map((item) => (
                  <div key={`${result.id}-${item.ordinal}`} className="job-item">
                    <p className="job-question">{item.ordinal}. {item.question}</p>
                    <p className="job-translation">{item.questionZh}</p>
                    <p className="job-answer">{item.answer}</p>
                  </div>
                ))}
                <p className="job-output">{result.outputPath}</p>
              </article>
            ))}
          </div>
        )}
      </section>
    </main>
  )
}

function ProgressView() {
  const [progress, setProgress] = useState<StatusEvent>(initialProgress)
  const activeTaskIdRef = useRef('')

  useEffect(() => {
    const unlistenPromise = listen<StatusEvent>('study://status', (event) => {
      const next = event.payload
      const activeTaskId = activeTaskIdRef.current
      if (next.takeoverProgress || !activeTaskId || next.taskId === activeTaskId) {
        startTransition(() => {
          setProgress(next)
          if (next.taskId) {
            activeTaskIdRef.current = next.taskId
          }
        })
      }
    })

    return () => {
      void unlistenPromise.then((unlisten) => unlisten())
    }
  }, [])

  const percent = progress.totalSteps > 0 ? Math.round((progress.currentStep / progress.totalSteps) * 100) : 0

  return (
    <main className={`progress-shell ${progress.isError ? 'error' : progress.isTerminal ? 'done' : ''}`}>
      <div className="progress-topline">
        <span className="progress-chip">{progress.status}</span>
        <span className="progress-percent">{percent}%</span>
      </div>
      <h2>{progress.detail}</h2>
      <div className="progress-track" aria-hidden="true">
        <div className="progress-fill" style={{ width: `${percent}%` }} />
      </div>
      <p className="progress-answer">{progress.answerPreview || '完成后会在这里显示 1.A  2.C  3.B 这类答案列表。'}</p>
      <p className="progress-note">{progress.isTerminal ? '任务已结束，窗口会保留直到你手动关闭。' : '保持这个小窗打开即可查看当前阶段进度。'}</p>
    </main>
  )
}

export default App
