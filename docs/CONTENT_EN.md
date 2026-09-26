# English launch copy drafts

> Publish only after replacing the repository and feedback links. These drafts describe an early preview, not a security certification. Do not imply employer endorsement or use of confidential meetings without written authorization.

## Project-page tagline

**PanNote is a local-first desktop workspace for meeting transcription, notes, and AI-assisted summaries.**

The current developer preview is macOS-focused. Setup still requires local runtimes and models. Windows and Linux product releases are not ready.

## Hacker News / Reddit launch draft

### Title

Show HN: PanNote — a local-first desktop workflow for meeting notes

### Body

I have been exploring a practical question: do meeting transcription and note-taking workflows always need to send meeting content to a hosted AI service?

PanNote is my early attempt at a local-first desktop workflow for recording, transcription, notes, and AI-assisted summaries. The current preview is macOS-focused and still requires local runtime/model setup; this is not a polished one-click product yet.

I want to be precise about the privacy boundary. The app stores its database locally, but the current SQLite database is not encrypted by the app. User-triggered web search sends the query to a search provider. A custom Ollama endpoint may be remote if configured that way. Optional cloud-model fallback is a future product proposal, not a feature I am claiming here.

Please do not test this preview with confidential or regulated material. Synthetic or explicitly authorized audio is the right way to help. I am looking for feedback on setup friction, transcription quality, and whether the data-flow explanation is clear.

Project: **[public repository URL]**

## Short social post

I’m building PanNote, a local-first desktop workflow for meeting transcription and notes. It is an early macOS developer preview, not a security-certified product. I’m documenting exactly when data stays local and when a user action (such as web search) makes a network request. Feedback from technical testers using synthetic or authorized audio is welcome: **[public repository URL]**.

## Editorial notes

- Do not say "100% offline", "zero data collection", "GDPR/HIPAA compliant", or "secure for confidential meetings" without release-specific evidence and appropriate review.
- Do not ask users to bypass macOS Gatekeeper as a mainstream installation step.
- State tested OS versions, CPU architecture, model versions, setup steps, and known failures alongside each release.
- Check each community's self-promotion rules before posting.
