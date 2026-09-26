# Privacy and data flows

This document describes the current developer-preview design as understood from the code and documentation review on 2026-09-26. It is not a security audit, legal advice, or a certification. Review it against the exact public release before publishing.

## Local data

- Meeting recordings, transcripts, notes, and chat history are stored locally by the app.
- The current app database is SQLite and is **not encrypted by the app**. Device disk encryption and account access controls are the user's responsibility.
- Current speech-recognition and Ollama workflows are designed to use local services and local model files.
- Local storage does not by itself guarantee that every feature, configured endpoint, operating-system service, or future version is offline.

## Network use

- Web search is user-triggered. Search terms are sent to the search provider used by the app. The current implementation can use Sogou or Bing China; an optional Tavily integration uses a user-configured API key.
- The app can use an `OLLAMA_URL` configuration. A non-loopback endpoint could be outside the user's device. Users should verify the configured endpoint before sending prompts, transcripts, or other content.
- The product proposal includes optional cloud-model fallback. That is not represented here as an existing capability. If implemented, it must be opt-in, identify the provider, disclose which content is sent, and never silently fall back from local to cloud.

## Telemetry and updates

The launch team has not completed an independent, release-specific audit of every network request, dependency, operating-system service, or bundled component. Do not interpret this draft as a verified "zero telemetry" claim.

Before each public release, test network behavior from a clean environment and document any update checks, error reporting, analytics, provider calls, or other external requests.

## Preview guidance

- Do not test with confidential, personal, regulated, or employer-owned data.
- Use synthetic or explicitly authorized recordings.
- Do not upload recordings, transcripts, database files, logs containing personal information, or API keys to public GitHub Issues.
- Use only model weights and runtimes whose licenses permit the intended download and redistribution method.

## Contact

Add a monitored privacy/security contact before public release. Until then, do not invite people to send sensitive reports through a public issue.
