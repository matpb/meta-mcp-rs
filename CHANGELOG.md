# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - unreleased

First version: an MCP server for the Meta Graph API that reads and sends Facebook
Messenger (Page inbox) messages and WhatsApp Business Cloud API messages. Graph API
version `v26.0` is the default.

### Added

- Command line `meta-mcp` with the subcommands `stdio` (the default), `http`
  (`--with-webhook` mounts the webhook receiver on the same listener), `webhook`,
  `setup`, `doctor`, `token` (`refresh`, `import`, `show`) and `tools`. Exit codes:
  0 success, 1 runtime or configuration error, 2 usage error.
- Guided setup, `meta-mcp setup`: verifies the App ID and App Secret, signs in through
  Facebook Login (a loopback listener or a pasted redirect URL), exchanges the code for a
  long-lived user token, discovers Pages and stores their Page tokens, optionally
  discovers WhatsApp business accounts and phone numbers, optionally subscribes the
  webhook at app and Page level, writes `credentials.json`, prints client snippets and
  runs the doctor. Every prompt has a flag or environment equivalent, so it also runs
  with `--non-interactive`. Existing tokens can be pasted instead of signing in
  (`--page-token`, `--user-token`, `--whatsapp-token`).
- `meta-mcp doctor`: live checks of the app, token validity and remaining days, each Page
  and WhatsApp number, webhook subscriptions, the inbox database and the files directory,
  with a `Fix:` line for every failure; `--json` for machines, `--webhook` to include the
  public callback reachability check. Exits 1 when any check fails.
- `meta-mcp token refresh`, `token import` and `token show` (never prints a token value).
- Configuration from the process environment first and `credentials.json` second, with
  `credentials.json` written atomically as a 0600 file in a 0700 directory. Secrets print as
  `***` in debug output, and anything shaped like a Graph access token is scrubbed from error text.
- Graph client: `Authorization: Bearer` for every token, `appsecret_proof` whenever the app
  secret is known, bounded retries for transient failures and rate limits (never for
  sends), typed Graph errors with actionable hints, same-origin paging, and a download
  helper with a host allowlist and a size cap.
- Webhook receiver: GET verification handshake, HMAC-SHA256 `X-Hub-Signature-256`
  verification over the raw body with a constant-time comparison, 1 MiB body limit,
  normalisation of Messenger and WhatsApp payloads (including business-scoped user IDs,
  statuses, reactions, edits and unsupported types) and deduplication into a local SQLite
  inbox in WAL mode.
- Twenty MCP tools: `meta_status`; the inbox tools `inbox_list`, `inbox_wait` and
  `inbox_mark_processed`; the Messenger tools `messenger_list_conversations`,
  `messenger_read_conversation`, `messenger_get_message`, `messenger_send_text`,
  `messenger_send_attachment`, `messenger_download_attachment`,
  `messenger_sender_action` and `messenger_get_profile`; and the WhatsApp tools
  `whatsapp_send_text`, `whatsapp_send_media`, `whatsapp_send_template`,
  `whatsapp_send_reaction`, `whatsapp_send_location`, `whatsapp_mark_read`,
  `whatsapp_upload_media` and `whatsapp_download_media`.
- MCP over stdio and over streamable HTTP at `/mcp`. HTTP mode requires API keys of at
  least 32 characters, compares them in constant time, guards the `Host` and `Origin`
  headers, binds to loopback by default and sends no CORS headers.
- Read-only mode (`META_MCP_READ_ONLY`) removes every tool that sends a message or
  changes local state.
- A confined files directory (`META_MCP_FILES_DIR`) for uploads and downloads, with a size
  cap, no symlink escapes and no silent overwrites.
- Documentation: a step-by-step implementation guide from no Meta account to a working
  assistant (`docs/GUIDE.md`), the tool reference (`docs/TOOLS.md`), the architecture
  (`docs/ARCHITECTURE.md`), the security policy (`SECURITY.md`) and `env.example`.
- `scripts/check.sh`: formatting, lints and tests in one local gate.
