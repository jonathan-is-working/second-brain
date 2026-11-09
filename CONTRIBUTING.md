# Contributing

Thanks for your interest! Issues and pull requests are welcome.

## Before opening a pull request

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs the same checks on Linux and macOS, plus a build with the minimum
supported Rust version (see `rust-version` in `Cargo.toml`).

## Guidelines

- **No network and no paid inference in tests.** Use the mock provider and the
  fake calendar backend (see `tests/pipeline.rs`).
- **Deterministic first.** Anything Rust can do reliably (dates, checkboxes,
  priorities, scheduling) should not need a model call. The model returns JSON
  that Rust validates; it never gets tools.
- **Never call the model on its own** from the dashboard or the nvim plugin: AI
  commands run only when the user starts them.
- **Never write the user's journals.** Generated notes only change inside their
  `sb:begin` / `sb:end` blocks.
- Interface text goes through `tr("français", "English")` in the dashboard so both
  languages stay complete.
- Keep the docs in sync: the command table in `README.md` and `CHANGELOG.md`
  (under *Unreleased*).

## Reporting bugs

Please include `sb --version`, your OS and terminal, the command you ran, and the
output of `sb doctor`. Do not paste private journal content or tokens. Security
issues: see [SECURITY.md](SECURITY.md).

By contributing, you agree that your contributions are licensed under the
project's terms (MIT OR Apache-2.0).
