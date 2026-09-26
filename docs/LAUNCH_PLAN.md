# PanNote GitHub launch plan

## Objective

Earn trust and validate recurring use before spending on broad distribution. Start with a narrow, honest developer preview; expand only after installation, privacy, and support workflows are repeatable.

## Current recommendation

**Do not push the existing PanNote repository or its current Git history to a public GitHub repository.** It has no configured remote, contains tracked `.temp` database backups and application artifacts, and has an untracked `dist/` directory with a DMG and internal reports. A clean working tree alone would not remove sensitive material from Git history.

Use a new public repository with a curated, reviewed source snapshot and clean history. Keep experiments, internal case notes, user data, signing material, and release candidates in separate private locations.

## Launch stages

### Stage 0 — Publication safety

- Decide the canonical repository name, owner, visibility, and support contact.
- Create a clean public repository; do not reuse or push the existing repository history.
- Review every included source file, dependency, model, font, icon, and bundled runtime for ownership and redistribution rights.
- Add a clear privacy statement, preview status, support path, and security-reporting route.
- Run `scripts/check-public-release.sh` against the exact candidate tree and resolve every finding.
- Confirm the Git history is new and contains no copied local history or data.

**Gate:** no public source or binary until the public snapshot and its history are reviewed.

### Stage 1 — Trust and demand validation

- Publish one practical, non-sensitive story about the problem: keeping control of meeting transcripts and understanding where data goes.
- Do not claim employer endorsement, regulatory compliance, or that confidential financial meetings were used unless written permission has been obtained.
- Invite a small group of technically confident testers. Describe the macOS setup limitations before they opt in.
- Measure installation completion, first successful transcription, time to first useful summary, repeat use after one week, and support burden.

**Gate:** proceed when independent users can complete the workflow and the privacy explanation matches observed behavior.

### Stage 2 — Public source preview

- Publish only the reviewed source and non-sensitive sample data.
- Use GitHub Releases for versioned source and, only when separately validated, signed/checksummed binaries.
- Include exact supported OS/hardware, known limitations, installation/uninstallation steps, model downloads and licenses, and checksums.
- Do not tell general users to bypass Gatekeeper with `xattr -cr`. Unsigned macOS builds are for opt-in technical testing only.

**Gate:** repeatable clean-machine installation and smoke tests on every platform listed as supported.

### Stage 3 — Broader distribution

- Improve macOS signing and notarization before mainstream promotion.
- Add Windows and Linux only after platform-specific service lifecycle, audio, model, installer, update, and uninstall tests pass.
- Consider App Store or Setapp only after user retention and support requirements justify the fees and review constraints.
- Treat cloud-model support as an explicit opt-in capability with a visible provider, data disclosure, and no silent fallback.

## Content sequence

1. Problem and workflow: what "local-first" means in practice.
2. Transparent data map: local processing, user-triggered search, and current limitations.
3. Installation and hardware notes from reproducible tests.
4. Product updates based on tester feedback, with metrics and known issues.

Do not publish one-off performance numbers as universal guarantees. The existing local benchmark is a developer measurement, not a cross-platform user promise.

## Launch metrics

- Qualified visits to the project page.
- Release download-to-successful-install ratio.
- First successful transcription and summary completion.
- Time to first useful result.
- Seven-day repeat use.
- Crash/error reports per successful session.
- Support time per tester and top recurring blockers.
- Privacy comprehension: testers can correctly explain when data leaves their device.

Downloads and social views are leading indicators only; successful repeat use is the stronger signal.
