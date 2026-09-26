# PanNote (笔尖)

PanNote is a macOS-focused desktop app for meeting recording, transcription, notes, and AI-assisted summaries. This repository is an **experimental source snapshot**, not a tested public installer.

## Important: this is not ready for general download

The source currently depends on local Python ASR services and model files that are not bundled here. Some service paths and startup assumptions still reflect the original developer environment. A successful source build alone does **not** mean a fresh user can install and use the complete product.

- No public, validated installer is currently provided.
- No model weights are included. Model/runtime licenses and download instructions still need a release-specific review.
- The existing ASR setup uses macOS-specific service management and is not a Windows or Linux release.
- The app database is local SQLite and is not encrypted by the app.
- User-triggered web search sends the query to a search provider. A custom Ollama URL may point outside the device.
- Do not use this preview for confidential, personal, regulated, or employer-owned information.

## Current capabilities

- Meeting recording, segmented transcription, retry/status reporting, and meeting summaries.
- Local notes with Markdown editing and export.
- AI chat and summary workflows through a configured Ollama-compatible endpoint.
- User-triggered web search integrations.

Feature availability depends on local model files, Python packages, services, and hardware. The repository does not bundle those prerequisites.

## Build from source (developer preview)

This build path is for developers who can install and troubleshoot the toolchain; it is not an end-user installation guide.

### Toolchain

- macOS with Xcode Command Line Tools
- Rust stable and the Tauri v2 CLI
- Node.js and npm
- Python 3 plus the project-specific ASR dependencies and model assets (not bundled)
- Ollama and a compatible local model for LLM features

### Commands

```bash
npm ci
npm run build
cargo test --all-targets
cargo tauri build
```

The Tauri bundle is produced under `target/release/bundle/`. A build on one developer Mac is not a substitute for validating a clean install on another machine.

## Download status

GitHub's **Download ZIP** provides source code only. It is not an installer and does not include Python dependencies, ASR model weights, Ollama, or an LLM model. Do not describe it as a ready-to-use download.

Release binaries will be posted under GitHub Releases only after clean-machine installation, model/license review, privacy review, and platform-specific tests are complete.

## Privacy and feedback

Read [Privacy and Data Flows](docs/PRIVACY.md) and the [GitHub launch plan](docs/LAUNCH_PLAN.md). Use synthetic or explicitly authorized audio for testing. Never upload recordings, transcripts, database files, logs containing personal information, or credentials to a public issue.

## License

The app source snapshot includes the repository's MIT license. Third-party models, runtimes, and other assets may have separate terms; the app license does not grant rights to redistribute them.
