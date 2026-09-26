# Public GitHub release checklist

## Repository and history

- [ ] Create a new public repository with a clean history; do not push the current PanNote repository.
- [ ] Review the exact staged tree, including hidden files and submodules.
- [ ] Ensure no `.temp`, databases, audio, transcripts, local backups, app bundles, DMGs, logs, credentials, keys, or private signing assets are included.
- [ ] Run the public-tree scanner and resolve every finding.
- [ ] Review Git history for accidentally committed secrets/data; scanning the current tree is not enough.
- [ ] Confirm the repository owner, canonical name, issue policy, and monitored security contact.

## Code, dependencies, and models

- [ ] Review source ownership and every dependency's license.
- [ ] Review model weights, tokenizers, datasets, fonts, icons, and runtime redistribution terms individually.
- [ ] Do not commit model weights or proprietary credentials. Document approved download source, version, checksum, and license.
- [ ] Remove developer-machine absolute paths, personal identifiers, private environment assumptions, and internal notes.
- [ ] Confirm debug logging does not expose transcript text, prompts, local paths, or credentials.

## Privacy and product claims

- [ ] Re-test outbound network behavior from a clean environment.
- [ ] Verify whether telemetry, update checks, analytics, crash reporting, search, and model-provider requests occur.
- [ ] Document data stored locally, its protection limits, and the effect of a custom/non-loopback endpoint.
- [ ] Verify cloud features are opt-in and cannot silently receive content on local failure.
- [ ] Do not claim regulatory compliance, employer endorsement, or guaranteed confidentiality without evidence and authorization.

## Build and release

- [ ] Build from a clean checkout on every claimed OS/architecture.
- [ ] Test fresh install, first launch, microphone permission, model acquisition, transcription, summary, upgrade, uninstall, and data retention.
- [ ] Test offline operation separately from online search/provider features.
- [ ] Publish only validated platforms and exact tested system requirements.
- [ ] For macOS general availability, sign and notarize. Unsigned builds, if any, are explicitly limited to opt-in technical testers.
- [ ] Generate checksums and attach concise release notes with model/runtime licenses and known issues.
- [ ] Confirm issue templates warn users not to upload private meeting data or secrets.

## Promotion

- [ ] Obtain written authorization for any identifiable employer or workplace case study.
- [ ] Use synthetic or authorized demo recordings and sanitized screenshots.
- [ ] Follow each community's self-promotion policy.
- [ ] Link to the privacy statement, release notes, and verified download—not a placeholder.
- [ ] Track successful installs, first successful task, repeat use, and support burden; do not optimize only for views/downloads.
