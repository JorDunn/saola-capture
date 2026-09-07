# Changelog

This file lists every change to this project.

This file follows the Keep a Changelog format at keepachangelog.com.
This project follows Semantic Versioning at semver.org.

## [Unreleased]

## [0.1.0-dev](https://github.com/JorDunn/saola-capture/releases/tag/saola-capture-v0.1.0-dev) - 2026-09-07

### Added

- move to saola-theme 0.15.0 and use its new tokens
- add CI, package files, README, and release setup
- land stage 16 — history library, color picker, GIF/WebP export
- land stages 12-15 — tray, region/window recording, audio, editor
- land stage 11 — EncoderSink + ffmpeg CLI: recording end-to-end
- land stage 10 — ScreenCast session + PipeWire frames
- land stage 9 — the main app window process
- land stages 7-8 — region overlay, window capture, delayed capture
- land stage 6 — flash + toast surfaces, the PrintScr MVP
- land stage 5 — screencopy backend, WebP/PNG storage, clipboard
- land stages 1-4 — CLI, capture.toml config, D-Bus seam, surfaceless daemon

### Fixed

- optimize dependencies in dev builds to unblock the per-screenshot encode path
