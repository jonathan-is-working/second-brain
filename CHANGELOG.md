# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/) (0.x: breaking changes can happen in
minor versions).

## [Unreleased]

## [0.1.0] - 2026-11-09

First public version.

### Added

- Markdown journal ingestion: change detection per block, deterministic syntax
  (checkboxes, dates, estimates, priorities, `+project`), one batched model call
  for the rest, validated JSON, provenance for every item.
- Memory: profile, episodic, knowledge, decisions and ideas, hierarchical themes,
  reflection pass (`sb connect`), `sb unlink`, `sb reprocess`, audit log and undo.
- Tasks and planning: priority score, deterministic scheduler, incremental
  replanning, daily briefing, alerts.
- Google Calendar (dedicated calendar, opt-in access to your own calendars) and
  two-way Google Tasks sync; `sb event` from plain text.
- Daily, weekly and monthly syntheses; offline HTML knowledge graph.
- `sb dash`: terminal dashboard (board, activity, themes, commands, graph),
  English/French, 9 themes shared with nvim.
- Optional nvim plugin (`contrib/nvim`).
- Providers: Claude CLI, Codex CLI, Anthropic API, none.

[Unreleased]: https://github.com/jojorequiem/second-brain/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/jojorequiem/second-brain/releases/tag/v0.1.0
