# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Personal mode (your own Messenger and WhatsApp accounts through unofficial clients), hardening of the
download, file and send paths, and the setup wizard as built.

### Security

- Personal mode keeps a copy of your logged-in sessions, so its files live in `META_MCP_HOME/personal/` (folders
  0700, files 0600), its socket is 0600 (a folder that the daemon or a unit from `personal install` creates for it is 0700) and
  serves only the daemon's own uid, its sidecars start with an empty environment (only `HOME`), each holds a lock on its
  data folder, their logs pass an allowlist scrubber, and its send audit log holds hashes and ids, never the text. `meta-mcp http` serves the personal tools too. `META_MCP_FILES_DIR`
  may not contain `META_MCP_HOME`; keep it outside the home so that the session files stay out of the file tools' reach.
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

- `inbox_list` and `inbox_wait` accept `messenger_personal` and `whatsapp_personal` as `platform`, and the
  unknown-platform error names them. Read-only mode now removes 21 tools instead of 11 (the 10 personal write tools
  join the 11 business ones) and keeps 17.
- The personal module needs Linux: the daemon reads its own user id from `/proc`, and it and the CLI use Unix-only APIs
  (`std::os::unix`) without a platform guard, so the crate is not expected to build on Windows (not tried; the older modules keep fallbacks for non-Unix systems,
  this one does not).
- Documentation: the README scope box and FAQ and the guide's Part 0.1 no longer say that meta-mcp cannot reach
  personal accounts. They separate business mode (the official Graph API: Page inboxes and business numbers only)
  from personal mode (unofficial clients, outside the platforms' terms).
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
- **Personal mode**, for your own personal Messenger and WhatsApp accounts. It uses unofficial clients that are
  outside the terms of Facebook and WhatsApp for personal accounts and can get an account restricted or banned:
  read the guide's Part 10.0 and the Personal mode section of `SECURITY.md` before use. A long-lived daemon,
  `meta-mcp personal daemon`, owns two Go sidecars and serves a Unix socket (mode 0600, same-uid peers only, systemd
  socket activation supported, a single instance per `META_MCP_HOME`). The daemon starts a sidecar at its first login or,
  when a session is stored, at daemon start; restarts one that stops with a 10, 30, 60 and 300 second backoff; stops
  sending after a fatal signal from the platform until the platform is restarted (`meta-mcp personal restart`); and
  stores incoming messages in the inbox.
- The Messenger sidecar, `meta-messenger-sidecar` (`sidecars/messenger`): the `messagix` library of mautrix-meta with a
  cookie login, and an embedded `whatsmeow` client for end-to-end encrypted chats (new messages only; the secure-history
  PIN restore is deliberately not implemented). The WhatsApp sidecar, `meta-whatsapp-sidecar` (`sidecars/whatsapp`):
  `whatsmeow` as a linked device with its own message store, QR and pair-code linking, the optional full-history request,
  the passkey step, and no new chats (each sidecar sends only into a conversation it already holds). Both speak version 1 of one JSON-lines sidecar protocol. Licences: mautrix-meta is
  AGPL-3.0 (so the combined Messenger sidecar binary is AGPL-3.0), `whatsmeow` is MPL-2.0, and both sidecars link
  `go.mau.fi/libsignal` (GPL-3.0).
- The `meta-mcp personal` command group: `daemon [--socket PATH]`, `login messenger [--cookies-file PATH |
  --cookies-json JSON]`, `login whatsapp [--pair-code PHONE] [--full-history] [--device-name NAME]`, `status [--json]`,
  `logout PLATFORM`, `restart PLATFORM`, `install [--dry-run]` and `uninstall [--dry-run]`. `install` writes the
  systemd user units `meta-mcp-personal.socket` and `meta-mcp-personal.service`.
- Eighteen MCP tools, 38 in all: `personal_status`, `personal_wait_for_message`, and for each platform the list tool
  (`messenger_personal_list_threads`, `whatsapp_personal_list_chats`), the read tool (`messenger_personal_read_thread`,
  `whatsapp_personal_read_chat`), `*_personal_send_text`, `*_personal_send_media`, `*_personal_react`,
  `*_personal_mark_read`, `*_personal_typing`, and the download tool (`messenger_personal_download_attachment`,
  `whatsapp_personal_download_media`).
- A write-ahead send audit log, `META_MCP_HOME/personal/audit.jsonl`: an `attempt` line is fsynced before the sidecar is
  asked (a send fails closed with `audit_failed` if it cannot be written), followed by a `sent`, `failed` or `unknown` line.
  It records ids, hashes and counts, never the text.
- Inbox platforms `messenger_personal` and `whatsapp_personal`. Message and echo IDs are
  `<platform>:<thread_id>:<message_id>`; reactions and receipts have their own shapes.
- Session state and sidecar data under `META_MCP_HOME/personal/` (`<platform>/state.json` 0600, `<platform>/data/` 0700).
- Environment variables `META_MCP_PERSONAL_SOCKET`, `META_MCP_SIDECAR_DIR` and `META_MCP_PERSONAL_BACKOFF_MS`.
- `scripts/build-all.sh`: builds `meta-mcp` and both sidecars into `dist/`.
- Documentation: the guide's Part 10 (install, Messenger and WhatsApp login, use, operations, troubleshooting), the personal
  tools in `docs/TOOLS.md`, the personal mode and its sequence diagrams in `docs/ARCHITECTURE.md`, a Personal mode section
  in `SECURITY.md`, and the two sidecar references.

### Fixed

- Documentation: `SECURITY.md` said the crate denies `unsafe` code without saying there is one exception. It now names it:
  `take_fd3` in `src/personal/daemon/listen.rs`, which adopts the socket that systemd passes as file descriptor 3.
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
