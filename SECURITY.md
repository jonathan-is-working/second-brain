# Security

## Reporting a vulnerability

Please **do not open a public issue**. Use GitHub's private reporting instead:
*Security → Report a vulnerability* on this repository. You should get an answer
within a week. Please include steps to reproduce and the version (`sb --version`).

## What `sb` stores, and where

Everything lives in the brain directory (`~/second-brain` or `$SB_HOME`):

| Path | Content | Protection |
|---|---|---|
| `vault/` | your journals and generated notes (Markdown) | regular files: back them up, sync them as you like |
| `data/brain.db` | SQLite database (tasks, memories, audit log) | regular file |
| `data/backups/` | daily `VACUUM INTO` copies (7 kept) | regular files |
| `data/secrets/` | Google OAuth client and token | directory `0700`, files `0600`; git-ignored by `sb init` |

- The Anthropic API key is read from the environment (`ANTHROPIC_API_KEY` by
  default) and never written to disk. The Google client secret can also come from
  `SB_GOOGLE_CLIENT_SECRET`.
- Secrets are never logged.
- To disconnect Google, delete `data/secrets/google_token.json` and revoke access
  at <https://myaccount.google.com/permissions>.

## Model calls

- Only new journal blocks (plus a few known names relevant to them) are sent to
  the configured model, never the whole vault.
- Journal text is passed as data. The model has no tools, and its output is
  validated against a whitelist of item types before anything is stored.
- Model CLIs (`claude`, `codex`) run as subprocesses without a shell, with a
  timeout and no tools.

## Scope

In scope: anything that leaks secrets or journal content beyond the configured
model provider and Google, lets journal text (prompt injection) trigger actions
outside the documented item types, or writes outside the brain directory and the
dedicated calendar without being asked.
