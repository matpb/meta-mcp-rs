# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Hardening of the download, file and send paths, and the setup wizard as built.

### Security

- Downloads never fetch the Graph API host. The host of `META_GRAPH_BASE_URL` is refused on every redirect
  hop, whether or not a token would be attached and even when `META_MCP_DOWNLOAD_HOSTS` names it.
  `*.facebook.com` and `*.messenger.com` leave the default download allowlist, which is now
  `*.fbcdn.net`, `*.fbsbx.com`, `*.whatsapp.net` and `lookaside.fbsbx.com`.
- Download tokens are tied to the exact host and port of the request, and a redirect to another host or
  port drops them. Messenger sends the Page token only after an HTTP 401 or 403, and only to
  `.fbcdn.net`, `.fbsbx.com` or an operator host; WhatsApp sends its token only to `lookaside.fbsbx.com`,
  `*.whatsapp.net` or an operator host.
- A picture is returned inline only after a structural check of its bytes (PNG `IHDR`, JPEG frame header,
  GIF screen descriptor, WEBP RIFF size) and only up to 2 MiB. The type a sender or server reports is never
  trusted, and a picture that is saved but not shown says why (`note` for Messenger, `inline_skipped` for
  WhatsApp).
- Downloads are saved under `inbound/messenger/` and `inbound/whatsapp/` in the files directory. The `path`
  of `messenger_send_attachment` and `whatsapp_send_media`, and `whatsapp_upload_media`, refuse files under
  `inbound/` unless `allow_inbound` is true.
- Download file names are reduced to one safe component of at most 200 bytes: control, bidirectional and
  zero-width characters are replaced, Windows device names are prefixed, and a Messenger name that already
  holds different content gets a short URL hash instead of replacing it. `Content-Disposition` is never
  read. A WhatsApp `dest_name` with a component over 200 bytes is refused before any request.
- `whatsapp_download_media` refuses `overwrite=true` in read-only mode.
- `META_MCP_FILES_DIR` may not contain `META_MCP_HOME`, `credentials.json` or the inbox database: startup
  fails and the error names the overlap.
- IDs are checked more strictly. A Graph ID must start with a letter, a digit or `_` and cannot be `.`, `..`
  or begin like a Graph version such as `v26.0`. WhatsApp message IDs must start with `wamid.` (the
  `whatsapp:` prefix of an inbox event ID is dropped) and media IDs are 1 to 32 digits.

### Changed

- `HUMAN_AGENT` is described as a tag for a person replying by hand, with Meta's note that automated
  messages are a disallowed use, in the tool descriptions, the server instructions and the outside-window
  hint.
- Messenger conversation reads are spaced 500 ms apart per Page, and a transient failure is repeated once
  through the same spacing; the Graph client's own retries are off for these reads. The reduced-fields
  fallback of `messenger_list_conversations` fires only on a code 100 whose message says `nonexisting field`.
- WhatsApp sends work out `window_warning` from the person's own newest inbound message under every ID the
  person is known by (number, `wa_id`, `user_id` and parent user ID). A send that fails with an open outcome
  (a transport error, an unreadable answer or an HTTP 5xx) says the message may still have been delivered,
  and the advice to resend with `media_id` after an upload appears only after a definite rejection.
- WhatsApp recipients lose a `(0)` trunk prefix after the country code and numbers that start with 0 are
  refused; reactions must be emoji; a `mime_type` on a path upload must restate the type of the file
  extension.
- Error hints: the outside-window hint is keyed on subcode 2018278 or 2534022 whatever the code, 200 with
  subcode 2018028 has its own "app not approved" hint, and the 131030 hint states no recipient count.
- `meta-mcp setup` as built: credentials are saved after the token, Pages and WhatsApp steps and around the
  webhook step; a verify token is created when none exists; `--webhook-override` points only the WhatsApp
  account at the webhook (a JSON body) and skips the app-level WhatsApp attempt; a different webhook
  callback, or a different app in `credentials.json`, is replaced only after a confirmation (`--yes`
  answers it, and `--replace`, which also drops the Pages a run did not select, covers the app switch);
  a run that cannot ask must give `--page-id` or `--all-pages` when it signs in, and `--phone-number-id`
  for WhatsApp unless stored numbers can be reused. `meta-mcp doctor --json` gains a `summary` key and
  carries information lines such as the gate-test hint. `setup`, `doctor` and `token` switch off the
  logs of the HTTP and TLS libraries.

### Added

- `meta-mcp setup --stored` reuses the tokens in `credentials.json` instead of signing in, which is how to
  subscribe or re-subscribe a webhook after downtime.
- `allow_inbound` on `messenger_send_attachment`, `whatsapp_send_media` and `whatsapp_upload_media`.

### Fixed

- Documentation: the tool reference now matches the code for the results of `whatsapp_mark_read` and
  `whatsapp_upload_media`, the static sticker limit (100 KiB), the reactions of `messenger_sender_action`,
  the file names of `messenger_download_attachment` and the event IDs of WhatsApp group statuses.

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
