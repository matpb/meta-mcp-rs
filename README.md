# meta-mcp

meta-mcp is a Model Context Protocol (MCP) server for the Meta Graph API: it lets an AI assistant read
and send **Facebook Messenger** messages on a Page inbox, and send and receive **WhatsApp** messages on
a WhatsApp Business number. It comes with a guided setup that handles Facebook Login, token exchange and
Page discovery for you, and with a step-by-step guide that takes you from no Meta developer account to a
working assistant.

A second, separate mode, **personal mode**, reaches your own personal Messenger and WhatsApp accounts through
unofficial clients. It is outside the platforms' terms and can get an account restricted or banned: see
[Personal mode](#personal-mode) before you decide to use it.

> **Scope, honestly.**
>
> - **Business mode: Facebook Page inboxes and WhatsApp Business numbers only.** It uses Meta's official Graph
>   API, which does not reach personal Messenger accounts or personal WhatsApp accounts.
> - **Personal mode: your own personal accounts, through unofficial clients.** It reaches what the Graph API cannot,
>   and it is **outside the terms of Facebook and WhatsApp for personal accounts**. It can get the account
>   restricted or banned (for WhatsApp, temporarily or permanently, with the chat history and backups out of reach
>   while banned). It is separate from business mode and you opt in to it by installing its daemon. Read
>   [Personal mode](#personal-mode) first.
> - **Business WhatsApp inbound needs a public HTTPS webhook.** The WhatsApp Cloud API has no message history
>   endpoint, so received messages exist only if the webhook receiver stored them. Messenger works by
>   polling, and a webhook is optional there. Personal mode needs no webhook.
> - **In Development mode the Page can only talk to people with a role on your Meta app.** Reaching real
>   customers may need Meta's App Review: the guide has a test that tells you.
> - **Everything Meta returns is untrusted text.** An assistant that can read messages and send replies
>   can be steered by them, in personal mode as you. Read-only mode removes the send tools.

## Contents

- [Features](#features)
- [Personal mode](#personal-mode)
- [Quick start](#quick-start)
- [Three ways to configure it](#three-ways-to-configure-it)
- [Tools](#tools)
- [Configuration](#configuration)
- [Security](#security)
- [FAQ](#faq)
- [Documentation](#documentation)
- [Development](#development)
- [License](#license)

## Features

- **Messenger.** List a Page's conversations, read messages with attachments, send text, images, audio,
  video and files (by URL or from a local file), download attachments, typing indicators, mark seen,
  reactions, and user profile lookup.
- **WhatsApp.** Send text, media (uploaded, by URL or from a local file), templates, reactions and
  locations; mark messages read with a typing indicator; upload and download media. Business-scoped user
  IDs are supported.
- **A local inbox.** A signature-verified webhook receiver stores Messenger and WhatsApp events in SQLite.
  The assistant reads them with `inbox_list` and `inbox_wait`.
- **Personal mode.** Your own Messenger and WhatsApp accounts through unofficial clients and a local daemon:
  list, read, wait, send text and files, react, mark read, typing and attachments, with a write-ahead audit log
  of sends and a read-only mode. Opt-in, outside the platforms' terms, with a real risk to the account.
- **Guided setup.** `meta-mcp setup` signs you in with Facebook Login (or takes a pasted token, or reuses the
  tokens you stored with `--stored`), exchanges it for a long-lived token, finds your Pages and WhatsApp
  numbers, subscribes the webhook and writes a private `credentials.json`. `meta-mcp doctor` re-checks
  everything live and says exactly what to fix.
- **Two transports.** stdio for a local client, and streamable HTTP with API keys for a server.
- **Safe by default.** Tokens only in the `Authorization` header, `appsecret_proof` on every call, 0600
  credentials, a confined files directory that keeps received files apart, a download host allowlist that
  refuses the Graph API host, a Host guard, and a read-only mode.
- **Graph API v26.0**, configurable.

## Personal mode

Personal mode lets an assistant read and send on **your own** Facebook Messenger and WhatsApp accounts, the ones on
your phone, the way you do. Meta has no API for that, so it uses two **unofficial clients**, written in Go and run
as child processes of a small local daemon: `meta-messenger-sidecar` (Messenger, logged in with your browser
cookies, built on the `messagix` library of the mautrix-meta bridge and on `whatsmeow` for encrypted chats) and
`meta-whatsapp-sidecar` (WhatsApp as a linked device, built on `whatsmeow`). It needs no Meta developer account,
app, token or webhook. The daemon, the sidecars and everything they store run on your machine; the message text your
assistant reads goes wherever the assistant runs.

> **Read this before you use it.** Unofficial clients are outside the terms of Facebook and WhatsApp for personal
> accounts. WhatsApp's own pages say that linking an account to an unofficial app "violates our Terms of Service"
> and that the account "might also be temporarily or permanently banned", that the wording reaches back ("now or in
> the past"), and that a banned account cannot reach its chat history or backups while the ban lasts. Facebook's
> terms forbid automated access, and users of the Messenger library report checkpoints and "suspect automated
> behaviour" locks. The design offers no way to create a group or send a broadcast (the kind of
> behaviour WhatsApp's own pages warn about), opens a new chat only for a send to one person that the platform confirms
> (first contact is the pattern both platforms flag most), keeps one connection set per account, and adds a read-only mode and an
> audit log of sends, but **the risk is not zero** and no source shows how much any of that helps.
> The account at risk is your phone number or your Facebook identity: use one you can afford to lose. Details:
> [SECURITY.md](SECURITY.md#unofficial-clients-the-risk-to-the-account) and the
> [guide, Part 10.0](docs/GUIDE.md#100-what-this-is-and-is-not).

- **What the assistant can do.** List and read conversations, wait for the next incoming message, send text and
  files, react, mark read, show typing, search Messenger for people to message, and save attachments. 19 tools in all
  ([docs/TOOLS.md](docs/TOOLS.md#personal-tools)); the messages also land in the local inbox as the platforms
  `messenger_personal` and `whatsapp_personal`.
- **What it needs.** Rust 1.89 or newer, Go 1.26 or newer, and Linux, macOS or Windows. On Windows, Rust needs the
  MSVC toolchain (Visual Studio Build Tools with the C++ workload). `meta-mcp personal install` sets the daemon up
  to start at login: systemd user units on Linux (with socket activation, which is Linux only), a launchd agent on
  macOS, a logon task in Task Scheduler on Windows. Anywhere else, run `meta-mcp personal daemon` under your own
  supervisor. The daemon listens on a Unix socket (Linux, macOS) or a named pipe (Windows), reachable by your user only.
- **What it does not do.** Create groups, send to groups it has not seen, broadcast, edit or unsend, or send polls.
  A first message to a WhatsApp number works once WhatsApp confirms the number is registered (see
  [SECURITY.md](SECURITY.md#unofficial-clients-the-risk-to-the-account): first contact is the pattern WhatsApp flags
  most). Encrypted Messenger history from before the device was registered is not available.
- **Pacing.** The daemon paces writes per platform with a token bucket: new messages one per 2 seconds with a burst
  of 3, reactions, read marks and typing one per second with a burst of 5. A call without a token is refused with
  `rate_limited` and a `retry_after_ms` value. It is a courtesy limiter, not a promise about what the platforms
  tolerate ([docs/TOOLS.md](docs/TOOLS.md#personal-tools) has the details and the two variables).
- **Over HTTP.** The personal tools are served over stdio. `meta-mcp http` leaves them out unless
  `META_MCP_PERSONAL_HTTP=1` is set.

From the repository root:

On Linux:

```bash
scripts/build-all.sh                       # meta-mcp and both sidecars, into dist/
install -m 0755 dist/meta-mcp dist/meta-messenger-sidecar dist/meta-whatsapp-sidecar ~/.local/bin/
mkdir -p ~/.local/share/meta-mcp/files     # where attachments are saved and sent from (optional)
META_MCP_FILES_DIR=~/.local/share/meta-mcp/files meta-mcp personal install   # daemon + socket as systemd user units
meta-mcp personal login whatsapp           # QR code in the terminal (or --pair-code YOUR_NUMBER)
meta-mcp personal login messenger --cookies-file cookies.json
meta-mcp personal status
claude mcp add meta-mcp --env META_MCP_FILES_DIR=$HOME/.local/share/meta-mcp/files -- meta-mcp
```

On macOS the build and the commands are the same, and `personal install` writes a launchd agent
(`~/Library/LaunchAgents/dev.meta-mcp.personal.plist`) instead of systemd units. It needs a logged-in graphical
session. Keep the files directory (for example `~/meta-mcp-files`) outside the home, which on macOS is
`~/Library/Application Support/meta-mcp`.

On Windows (PowerShell; Rust with the MSVC toolchain and Go installed):

```powershell
scripts\build-all.ps1                      # meta-mcp.exe and both sidecars, into dist\
$bin = "$env:USERPROFILE\bin"; New-Item -ItemType Directory -Force $bin | Out-Null
Copy-Item dist\meta-mcp.exe, dist\meta-messenger-sidecar.exe, dist\meta-whatsapp-sidecar.exe $bin   # add $bin to your PATH
$files = "$env:LOCALAPPDATA\meta-mcp\files"; New-Item -ItemType Directory -Force $files | Out-Null
setx META_MCP_FILES_DIR $files             # saved for the logon start; new shells see it
$env:META_MCP_FILES_DIR = $files           # this shell
meta-mcp personal install                  # logon task "meta-mcp personal", and the daemon starts now, with no window
meta-mcp personal login whatsapp
meta-mcp personal login messenger --cookies-file cookies.json
meta-mcp personal status
claude mcp add meta-mcp --env META_MCP_FILES_DIR=$files -- meta-mcp
```

`personal install` copies `META_MCP_FILES_DIR` into the unit (systemd) or the plist (launchd), so set it before
installing (or run `install` again with it); the MCP server needs the same value to read and send files. A Windows
logon task cannot carry variables: it starts the daemon with your saved user environment, which is what `setx` is
for (`install` prints a note when a variable is set only in the current shell).

The cookies, the QR flow, the passkey step some WhatsApp accounts need, operations and troubleshooting are in the
[guide, Part 10](docs/GUIDE.md#part-10-personal-mode-your-own-messenger-and-whatsapp). Session files live in
`META_MCP_HOME/personal/` (folders 0700, files 0600; on Windows an owner-only ACL instead) and are as sensitive as
passwords; the daemon listens on a Unix socket or, on Windows, a named pipe that only your user can open.

## Quick start

This is the quick start for business mode (Pages and business numbers). Personal mode has its own, above.

You need a Meta app first. The guide walks through it: [Part 1](docs/GUIDE.md#part-1-meta-developer-account-and-app)
(developer account and app), [Part 2](docs/GUIDE.md#part-2-pages-roles-and-access) (roles and access)
and [Part 3](docs/GUIDE.md#part-3-facebook-login-for-the-wizard) (the redirect address). In short:
register as a developer, create an app with the Messenger use case, copy the App ID and App Secret from
**Settings > Basic**, and register `http://localhost:8766/callback` as a redirect URI under **Facebook
Login for Business > Settings** (the guide explains why, and what to do if Meta refuses it).

1. **Install.** You need Rust 1.89 or newer and a C compiler (on Windows, the MSVC toolchain). Business mode and
   personal mode build on Linux, macOS and Windows.

   ```bash
   git clone https://github.com/matpb/meta-mcp-rs.git
   cd meta-mcp-rs
   cargo install --path .
   ```

   If a release binary is published for your platform you can download it instead and put it on your
   `PATH`.

2. **Set up.** The wizard asks for the App ID and App Secret, opens your browser for the Facebook
   sign-in, lets you pick your Pages and writes the configuration.

   ```bash
   meta-mcp setup
   ```

3. **Connect your assistant.** For Claude Code:

   ```bash
   claude mcp add meta-mcp -- meta-mcp
   ```

   Other clients (Claude Desktop, Cursor, any MCP client) are covered in the
   [guide](docs/GUIDE.md#48-client-snippets).

4. **First tool call.** Ask your assistant to "list my Messenger conversations": it calls
   `messenger_list_conversations`. To check the setup yourself, run `meta-mcp doctor`.

In Development mode the Page can only message people who have a role on your app, so add the
Facebook account you test with as a **Tester** ([guide, Part 2.2](docs/GUIDE.md#22-app-roles)).

## Three ways to configure it

| Route | Use it when | How |
|---|---|---|
| **The wizard** | You are setting up by hand | `meta-mcp setup` writes `credentials.json` (mode 0600). Every prompt also has a flag or environment variable, so it runs non-interactively with `--non-interactive` |
| **Environment variables only** | Containers, scripts, a first test | Set `META_PAGE_ACCESS_TOKEN` and `META_APP_SECRET` (the Page ID is read at startup). For WhatsApp add `META_WHATSAPP_ACCESS_TOKEN` and `META_WHATSAPP_PHONE_NUMBER_ID`. No file is needed, and variables win over `credentials.json` |
| **HTTP mode** | A shared or remote server | `META_MCP_API_KEYS="$(openssl rand -hex 32)" meta-mcp http`, behind TLS. Add `--with-webhook` to receive Meta's webhooks in the same process |

`env.example` lists every variable. meta-mcp never reads `.env` files: load them through your shell,
systemd or your process manager. Personal mode needs none of these routes: it has no Meta credentials.

## Tools

39 tools: 20 for business mode and 19 for personal mode. Full parameters, results and examples are in
[docs/TOOLS.md](docs/TOOLS.md). "Read-only mode" says whether the tool stays available when
`META_MCP_READ_ONLY=true`. The personal tools are served over stdio; `meta-mcp http`
lists only the 20 business tools unless `META_MCP_PERSONAL_HTTP=1` is set.

| Tool | What it does | Read-only mode |
|---|---|---|
| `meta_status` | Shows the app, Pages, WhatsApp numbers, token expiry, webhook and inbox health | kept |
| `inbox_list` | Lists stored events (webhook, and personal ones when `platform` names one or `include_personal` is true), newest first, with filters | kept |
| `inbox_wait` | Waits for new inbox events (personal ones as for `inbox_list`) | kept |
| `inbox_mark_processed` | Flags inbox events as handled | removed |
| `messenger_list_conversations` | Lists a Page's conversations and the customers' PSIDs | kept |
| `messenger_read_conversation` | Reads the messages of one conversation | kept |
| `messenger_get_message` | Reads one message | kept |
| `messenger_send_text` | Sends a text message as the Page | removed |
| `messenger_send_attachment` | Sends an image, audio, video or file | removed |
| `messenger_download_attachment` | Saves an attachment into `inbound/messenger` of the files directory | kept (read-only apart from the saved file) |
| `messenger_sender_action` | Typing indicator, mark seen, reactions | removed |
| `messenger_get_profile` | Looks up a person's name and picture | kept |
| `whatsapp_send_text` | Sends a WhatsApp text message | removed |
| `whatsapp_send_media` | Sends an image, video, audio, document or sticker | removed |
| `whatsapp_send_template` | Sends an approved template (the only way to start a conversation) | removed |
| `whatsapp_send_reaction` | Reacts to a message with an emoji | removed |
| `whatsapp_send_location` | Sends a location | removed |
| `whatsapp_mark_read` | Marks a message read, optionally showing typing | removed |
| `whatsapp_upload_media` | Uploads a file and returns a media ID | removed |
| `whatsapp_download_media` | Saves an inbound media file into `inbound/whatsapp` of the files directory | kept (read-only apart from the saved file) |
| `personal_status` | Shows both personal accounts: login, connection, sending state, versions | kept |
| `messenger_personal_list_threads` | Lists your own Messenger conversations | kept |
| `messenger_personal_read_thread` | Reads the messages of one of them | kept |
| `messenger_personal_search_users` | Searches for people your Messenger account can message, by name | kept |
| `messenger_personal_send_text` | Sends a text message from your Messenger account | removed |
| `messenger_personal_send_media` | Sends an image, video, audio clip or file from your Messenger account (encrypted chats: images only, no captions) | removed |
| `messenger_personal_react` | Reacts to a message, or removes your reaction with an empty emoji (removal is not verified in encrypted threads) | removed |
| `messenger_personal_mark_read` | Marks a conversation read (the other person sees it) | removed |
| `messenger_personal_typing` | Starts or stops the typing indicator | removed |
| `messenger_personal_download_attachment` | Saves an attachment into `inbound/messenger_personal` of the files directory | kept (read-only apart from the saved file) |
| `whatsapp_personal_list_chats` | Lists your own WhatsApp chats | kept |
| `whatsapp_personal_read_chat` | Reads the messages of one of them, asking the phone for older ones when needed | kept |
| `whatsapp_personal_send_text` | Sends a text message from your WhatsApp account | removed |
| `whatsapp_personal_send_media` | Sends an image, video, audio clip or file from your WhatsApp account | removed |
| `whatsapp_personal_react` | Reacts to a message, or removes your reaction with an empty emoji (removal is not verified on WhatsApp) | removed |
| `whatsapp_personal_mark_read` | Marks a chat read (blue ticks) | removed |
| `whatsapp_personal_typing` | Starts or stops the typing indicator | removed |
| `whatsapp_personal_download_media` | Saves media into `inbound/whatsapp_personal` of the files directory | kept (read-only apart from the saved file) |
| `personal_wait_for_message` | Waits for the next live incoming personal message | kept |

Tools that return content from Meta say so in their description: names, text and attachments are
third-party content, never instructions. The send tools say they reach real people and ask the client to
confirm first; the personal ones reach them as you.

## Configuration

Resolution order: **process environment, then `credentials.json`** in `META_MCP_HOME`. The wizard
writes the file; every variable below overrides it.

| Variable | Default | Meaning |
|---|---|---|
| `META_APP_ID` | none | Meta App ID |
| `META_APP_SECRET` | none | Meta App Secret: signs `appsecret_proof`, verifies webhook signatures |
| `META_PAGE_ACCESS_TOKEN` | none | Page access token. Replaces the stored Pages with this one Page |
| `META_PAGE_ID` | read with `GET /me` at startup | Page ID for that token |
| `META_WHATSAPP_ACCESS_TOKEN` | none | WhatsApp token. Replaces the stored WhatsApp block |
| `META_WHATSAPP_PHONE_NUMBER_ID` | none | Business phone number ID that sends |
| `META_WABA_ID` | none | WhatsApp Business Account (Messaging account) ID |
| `META_WEBHOOK_VERIFY_TOKEN` | none (generated by setup) | Verify token Meta echoes during the handshake |
| `META_WEBHOOK_PUBLIC_URL` | none | Public HTTPS URL of the webhook (also feeds the Host guard) |
| `META_GRAPH_VERSION` | `v26.0` | Graph API version; must look like `v26.0` |
| `META_GRAPH_BASE_URL` | `https://graph.facebook.com` | Graph base URL; `https` required unless the host is loopback |
| `META_MCP_HOME` | platform config dir (`~/.config/meta-mcp` on Linux) | Directory holding `credentials.json` and, for personal mode, `personal/` (session state, the sidecars' data and the audit log) |
| `META_MCP_INBOX_DB` | `inbox.db` in the platform data dir (`~/.local/share/meta-mcp` on Linux) | SQLite inbox file |
| `META_MCP_FILES_DIR` | unset (file tools disabled) | The only directory the file tools read from and write to; must exist. Downloads go to `inbound/messenger` and `inbound/whatsapp` inside it (`inbound/messenger_personal` and `inbound/whatsapp_personal` in personal mode). It may not contain `META_MCP_HOME`, `credentials.json` or the inbox database: startup fails and the error names the overlap |
| `META_MCP_MAX_FILE_BYTES` | `104857600` (100 MiB) | Largest file the file tools will read, upload or save |
| `META_MCP_READ_ONLY` | `false` | `true` removes every send and state-changing tool. The personal daemon reads it too and refuses writes with `read_only` |
| `META_MCP_API_KEYS` | none | HTTP mode only. Comma-separated keys, each at least 32 characters; required |
| `META_MCP_HOST` | `127.0.0.1` | HTTP mode bind address |
| `META_MCP_PORT` | `8788` | HTTP mode port |
| `META_MCP_ALLOWED_HOSTS` | none (loopback and the bind host are always allowed) | Extra `Host` values the HTTP Host guard accepts, comma-separated |
| `META_MCP_WEBHOOK_LISTEN` | `127.0.0.1:8787` | Address the standalone webhook receiver binds |
| `META_MCP_DOWNLOAD_HOSTS` | none | Extra exact host names (no wildcards, no ports) that downloads may fetch from and that may receive the token. The Graph API host is refused whatever this says. Leave unset in production |
| `META_MCP_LOG` | `meta_mcp=info` | Log filter (tracing syntax); logs go to stderr |
| `META_MCP_PERSONAL_SOCKET` | Linux: `$XDG_RUNTIME_DIR/meta-mcp/personal.sock`, else `META_MCP_HOME/personal/personal.sock` (macOS has no `XDG_RUNTIME_DIR`, so the second). Windows: `\\.\pipe\meta-mcp-personal-<USERNAME>` | Personal mode. The Unix socket (Windows: the named pipe, which must start with `\\.\pipe\`) of the personal daemon, used by the daemon, the MCP server and every `meta-mcp personal` command. Keep a socket path under about 100 bytes |
| `META_MCP_SIDECAR_DIR` | the directory of the `meta-mcp` executable | Personal mode. Where `meta-messenger-sidecar` and `meta-whatsapp-sidecar` are (`.exe` files on Windows) |
| `META_MCP_PERSONAL_HTTP` | unset | Personal mode. `1` serves the personal tools over `meta-mcp http` too; without it they are not registered there. Every API key holder then reaches your accounts |
| `META_MCP_PERSONAL_SEND_INTERVAL_MS` | `2000` | Personal mode, daemon. Milliseconds between the tokens of the new-message bucket (`send_text`, `send_media`), per platform |
| `META_MCP_PERSONAL_SEND_BURST` | `3` | Personal mode, daemon. Size of that bucket. Reactions, read marks and typing have a fixed bucket (one per second, burst 5) |

`env.example` also documents `META_MCP_PERSONAL_BACKOFF_MS`, `META_MCP_PERSONAL_STABLE_MS`, `META_MCP_PERSONAL_PING_MS`
and `META_MCP_PERSONAL_PING_TIMEOUT_MS`, which tune the daemon's restarts of a sidecar.

Booleans accept `true`, `false`, `1`, `0`, `yes`, `no`, `on` and `off`.

## Security

The access token is the Page, or the business number, so the defaults are conservative:

- Tokens travel only in the `Authorization: Bearer` header. `appsecret_proof` is added whenever the app
  secret is known. Secrets never print: debug output shows `***` and error text is scrubbed of Graph
  tokens.
- `credentials.json` is written atomically as 0600 in a 0700 directory.
- Webhooks are accepted only with a valid `X-Hub-Signature-256` (HMAC-SHA256 over the raw body, constant
  time), and the verify token is compared in constant time.
- Downloads are limited to Meta's media hosts (`*.fbcdn.net`, `*.fbsbx.com`, `*.whatsapp.net`), re-checked on
  every redirect hop and size-capped. The Graph API host is never fetched, and the token goes only to the
  exact host and port of a media URL on those hosts (a redirect to another host or port drops it). An
  image is shown inline only when its bytes pass a structural check and fit in 2 MiB.
- File tools are confined to `META_MCP_FILES_DIR`. It is a path check, not an operating-system sandbox,
  and everything inside the directory is readable by the assistant. Files other people sent are saved
  under `inbound/`, and the upload tools refuse to send them on unless the call says `allow_inbound=true`.
- HTTP mode requires API keys (32 characters or more, constant-time comparison), guards the `Host` and
  `Origin` headers, binds to loopback by default and sends no CORS headers. TLS is your reverse proxy's job.
- `META_MCP_READ_ONLY=true` removes the 21 tools that send or change state (11 business, 10 personal).
- Personal mode keeps a copy of your logged-in session, so its files (`META_MCP_HOME/personal/`) are 0600 in 0700
  folders (Windows: an owner-only ACL) and the daemon socket or pipe accepts only your own user. The sidecars start with an empty environment, the
  logs are scrubbed, and sends are written to an audit log (hashes and ids, never the text) before they are
  attempted. The daemon paces writes and refuses an excess with `rate_limited`. The HTTP transport does not register
  the personal tools unless you set `META_MCP_PERSONAL_HTTP=1`; with it, every holder of one of its API keys can
  reach your accounts. The risk that no control removes is the platforms': an unofficial client can get the
  account restricted or banned.

The full threat model, each control's limits and how to report a vulnerability are in
[SECURITY.md](SECURITY.md).

## FAQ

**Can it read my personal Messenger or WhatsApp?** Business mode cannot: the Graph API reaches Facebook
Page inboxes and WhatsApp Business numbers only. A personal WhatsApp number can become a Business API
number only by deleting the personal account first (Meta's rule), and there is no endpoint for personal
Messenger. **Personal mode can**, through unofficial clients, outside the platforms' terms and with a real risk to
the account: see [Personal mode](#personal-mode).

**Will personal mode get my account banned?** Nobody can promise it will not. WhatsApp states that linking an
account to an unofficial app can lead to a temporary or permanent ban, retroactively, and the Messenger
library's users report checkpoints and automated-behaviour locks. The design offers no way to create a group or
send a broadcast, the kind of behaviour WhatsApp's pages warn about, opens a new chat only for a send to one person
that the platform confirms, and adds a read-only mode and a
client that can ask you before each send, and none of that removes the risk or is shown to lower it. Use an account
you can afford to lose, and read
[SECURITY.md](SECURITY.md#unofficial-clients-the-risk-to-the-account).

**Does personal mode need a Meta app or a webhook?** No. It needs the two sidecars and the daemon on one machine
you control, and a login on each platform.

**What is the 24-hour window?** A Messenger Page can reply to a person for 24 hours after their last
message (up to 7 days after a click-to-Messenger ad); WhatsApp free-form messages work for 24 hours after
the person's last message. Outside it Messenger sends fail, and WhatsApp needs an approved template. The
three Messenger tags for updates were retired on 2026-04-27, and the remaining `HUMAN_AGENT` tag is for a
person replying by hand: Meta lists automated messages as a disallowed use, so the tool descriptions, the
server instructions and the error hint all tell the assistant never to use it for generated replies. See the
[guide, Part 5.2](docs/GUIDE.md#52-the-24-hour-window-and-the-dead-tags).

**Why is there no WhatsApp history?** The Cloud API has no endpoint that lists or fetches messages, and
Meta cannot replay webhooks. Run the webhook receiver (`meta-mcp webhook`, or `meta-mcp http
--with-webhook`) behind public HTTPS and the assistant reads from the local inbox
([guide, Part 6](docs/GUIDE.md#part-6-webhooks)).

**How far does Development mode reach?** Only to Facebook accounts with the Administrator, Developer or
Tester role on your app, and webhooks fire only for them. Whether your app needs Meta's App Review to
reach everyone else is a point where Meta's own pages disagree. The guide gives a gate test that settles
it for your app ([Part 2.5](docs/GUIDE.md#25-the-conflict-and-the-gate-test-that-settles-it)).

**What does it cost?** meta-mcp is free software. Meta publishes prices in USD, and the CAD figure is in
parentheses at a rate of 1.3993 CAD per USD. WhatsApp bills per delivered message since 2025-07-01. From
2026-10-01, on the North America rate card: marketing templates USD 0.0250 (CAD 0.0350), and utility,
authentication and service messages USD 0.0034 (CAD 0.00476) each, with the first 1,000 service messages
per business phone number per month free. A payment method is still required, and Meta does not bill in
CAD. The Messenger documents fetched for the guide list rate limits but no per-message price; check
Meta's current documentation. Full table in the [guide, Part 7.12](docs/GUIDE.md#712-pricing-as-of-2026-10-01).

**When do tokens expire?** A long-lived user token lasts about 60 days (`meta-mcp token refresh`). Page
tokens derived from it do not expire but can be invalidated (a password change, a logout, 90 days of app
inactivity). A WhatsApp user token lasts 60 days, and a System User token is non-expiring or 60-day
expiring. `meta-mcp doctor` warns when fewer than 10 days are left
([guide, Part 8.5](docs/GUIDE.md#85-the-token-refresh-schedule)).

**Does it support Instagram?** Not in 0.1.0. Instagram webhook payloads are stored as raw events, and
`meta-mcp setup --instagram` can request the Instagram scopes, but there are no Instagram tools.

**Can I stop the assistant from sending?** Yes: `META_MCP_READ_ONLY=true`. The read tools, the inbox and
the download tools remain, and the downloads only write inside the files directory. In this mode
the two business download tools refuse `overwrite=true`, so they only create new files
([SECURITY.md](SECURITY.md#read-only-mode) has the details). For personal mode set the variable for the
personal daemon as well as for the MCP server: they are two switches.

## Documentation

- [docs/GUIDE.md](docs/GUIDE.md): the implementation guide, from no Meta account to a working assistant,
  with the WhatsApp track, production advice and troubleshooting, and Part 10 for personal mode.
- [docs/TOOLS.md](docs/TOOLS.md): every tool with parameters, results, errors and examples.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): modules, data flow, the personal daemon and its sidecars, trust
  boundaries and sequence diagrams.
- [sidecars/messenger/docs/SIDECAR-MESSENGER.md](sidecars/messenger/docs/SIDECAR-MESSENGER.md) and
  [sidecars/whatsapp/docs/SIDECAR-WHATSAPP.md](sidecars/whatsapp/docs/SIDECAR-WHATSAPP.md): the two Go sidecars.
- [SECURITY.md](SECURITY.md): the threat model, the limits of each control and how to report a problem.
- [CHANGELOG.md](CHANGELOG.md) and [env.example](env.example).

## Development

`scripts/check.sh` is the local gate: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and
`cargo test`, stopping at the first failure. Tests use wiremock and never touch the network.

```bash
./scripts/check.sh
```

The Go sidecars have their own gate: each `sidecars/*/build.sh` runs `go vet`, `go test`, a static build and
`govulncheck`. `scripts/build-all.sh` builds `meta-mcp` and both sidecars into `dist/`; it needs Go 1.26 or newer,
`govulncheck` and `file`.

## License

MIT. See [LICENSE](LICENSE). What to know about the personal-mode sidecars, which are separate Go programs built from
`sidecars/`: the Messenger sidecar links mautrix-meta, which is AGPL-3.0, so its documentation states that the
combined `meta-messenger-sidecar` binary is AGPL-3.0 and that distributing it carries the AGPL source obligations. The
WhatsApp sidecar links `whatsmeow` (MPL-2.0), and both sidecars link `go.mau.fi/libsignal` (the Messenger sidecar
through mautrix-meta and `whatsmeow`), which is GPL-3.0. Check the terms before you distribute a sidecar binary. Not legal advice.
