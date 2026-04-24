# Flnder

Flnder is a Tauri desktop study assistant for macOS and Windows. It can import course materials into a local knowledge base, solve questions from screenshots or copied text, show quick answers in a small floating window, and continue generating full explanations and `.docx` output in the background.

## Local development

Install dependencies and run the desktop app:

```bash
npm ci
npm run tauri:dev
```

To build a local release bundle:

```bash
npm run tauri:build
```

## GitHub Actions builds

This repository includes a workflow at `.github/workflows/build.yml`.

It runs automatically when code is pushed to `main`, and it can also be started manually from the GitHub Actions page.

The workflow builds:

- macOS bundles on `macos-latest`
- Windows bundles on `windows-latest`

Build outputs are uploaded as GitHub Actions artifacts:

- `flnder-macos-bundles`
- `flnder-windows-bundles`

To download them:

1. Open the repository Actions tab
2. Open a successful `Build Desktop Apps` run
3. Download the artifact for the platform you want
