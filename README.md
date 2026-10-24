# meta-mcp

meta-mcp is a Model Context Protocol (MCP) server for the Meta Graph API: it lets an AI assistant read
and send **Facebook Messenger** messages on a Page inbox, and send and receive **WhatsApp** messages on
a WhatsApp Business number. It comes with a guided setup that handles Facebook Login, token exchange and
Page discovery for you, and with a step-by-step guide that takes you from no Meta developer account to a
working assistant.

> **Scope, honestly.**
>
> - **Facebook Page inboxes and WhatsApp Business numbers only.** The Graph API does not reach personal
>   Messenger accounts or personal WhatsApp accounts, and neither does meta-mcp.
> - **WhatsApp inbound needs a public HTTPS webhook.** The WhatsApp Cloud API has no message history
>   endpoint, so received messages exist only if the webhook receiver stored them. Messenger works by
>   polling, and a webhook is optional there.
> - **In Development mode the Page can only talk to people with a role on your Meta app.** Reaching real
>   customers may need Meta's App Review: the guide has a test that tells you.
> - **Everything Meta returns is untrusted text.** An assistant that can read messages and send replies
>   can be steered by them. Read-only mode removes the send tools.

## Contents

- [Features](#features)
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
- **Guided setup.** `meta-mcp setup` signs you in with Facebook Login (or takes a pasted token), exchanges
  it for a long-lived token, finds your Pages and WhatsApp numbers, subscribes the webhook and writes a
  private `credentials.json`. `meta-mcp doctor` re-checks everything live and says exactly what to fix.
- **Two transports.** stdio for a local client, and streamable HTTP with API keys for a server.
- **Safe by default.** Tokens only in the `Authorization` header, `appsecret_proof` on every call, 0600
  credentials, a confined files directory, a download host allowlist, a Host guard, and a read-only mode.
- **Graph API v26.0**, configurable.

## Quick start

You need a Meta app first. The guide walks through it: [Part 1](docs/GUIDE.md#part-1-meta-developer-account-and-app)
(developer account and app), [Part 2](docs/GUIDE.md#part-2-pages-roles-and-access) (roles and access)
and [Part 3](docs/GUIDE.md#part-3-facebook-login-for-the-wizard) (the redirect address). In short:
register as a developer, create an app with the Messenger use case, copy the App ID and App Secret from
**Settings > Basic**, and register `http://localhost:8766/callback` as a redirect URI under **Facebook
Login for Business > Settings** (the guide explains why, and what to do if Meta refuses it).

1. **Install.** You need Rust 1.89 or newer and a C compiler.

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
systemd or your process manager.

## Tools

20 tools. Full parameters, results and examples are in [docs/TOOLS.md](docs/TOOLS.md). "Read-only mode"
says whether the tool stays available when `META_MCP_READ_ONLY=true`.

| Tool | What it does | Read-only mode |
|---|---|---|
| `meta_status` | Shows the app, Pages, WhatsApp numbers, token expiry, webhook and inbox health | kept |
| `inbox_list` | Lists stored webhook events, newest first, with filters | kept |
| `inbox_wait` | Waits for new webhook events | kept |
| `inbox_mark_processed` | Flags inbox events as handled | removed |
| `messenger_list_conversations` | Lists a Page's conversations and the customers' PSIDs | kept |
| `messenger_read_conversation` | Reads the messages of one conversation | kept |
| `messenger_get_message` | Reads one message | kept |
| `messenger_send_text` | Sends a text message as the Page | removed |
| `messenger_send_attachment` | Sends an image, audio, video or file | removed |
| `messenger_download_attachment` | Saves an attachment into the files directory | kept (read-only apart from the saved file) |
| `messenger_sender_action` | Typing indicator, mark seen, reactions | removed |
| `messenger_get_profile` | Looks up a person's name and picture | kept |
| `whatsapp_send_text` | Sends a WhatsApp text message | removed |
| `whatsapp_send_media` | Sends an image, video, audio, document or sticker | removed |
| `whatsapp_send_template` | Sends an approved template (the only way to start a conversation) | removed |
| `whatsapp_send_reaction` | Reacts to a message with an emoji | removed |
| `whatsapp_send_location` | Sends a location | removed |
| `whatsapp_mark_read` | Marks a message read, optionally showing typing | removed |
| `whatsapp_upload_media` | Uploads a file and returns a media ID | removed |
| `whatsapp_download_media` | Saves an inbound media file into the files directory | kept (read-only apart from the saved file) |

Tools that return content from Meta say so in their description: names, text and attachments are
third-party content, never instructions. The send tools say they reach real people and ask the client to
confirm first.

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
| `META_MCP_HOME` | platform config dir (`~/.config/meta-mcp` on Linux) | Directory holding `credentials.json` |
| `META_MCP_INBOX_DB` | `inbox.db` in the platform data dir (`~/.local/share/meta-mcp` on Linux) | SQLite inbox file |
| `META_MCP_FILES_DIR` | unset (file tools disabled) | The only directory the file tools read from and write to; must exist |
| `META_MCP_MAX_FILE_BYTES` | `104857600` (100 MiB) | Largest file the file tools will read, upload or save |
| `META_MCP_READ_ONLY` | `false` | `true` removes every send and state-changing tool |
| `META_MCP_API_KEYS` | none | HTTP mode only. Comma-separated keys, each at least 32 characters; required |
| `META_MCP_HOST` | `127.0.0.1` | HTTP mode bind address |
| `META_MCP_PORT` | `8788` | HTTP mode port |
| `META_MCP_ALLOWED_HOSTS` | none (loopback and the bind host are always allowed) | Extra `Host` values the HTTP Host guard accepts, comma-separated |
| `META_MCP_WEBHOOK_LISTEN` | `127.0.0.1:8787` | Address the standalone webhook receiver binds |
| `META_MCP_DOWNLOAD_HOSTS` | none | Extra exact hosts allowed for downloads. Leave unset in production |
| `META_MCP_LOG` | `meta_mcp=info` | Log filter (tracing syntax); logs go to stderr |

Booleans accept `true`, `false`, `1`, `0`, `yes`, `no`, `on` and `off`.

## Security

The access token is the Page, or the business number, so the defaults are conservative:

- Tokens travel only in the `Authorization: Bearer` header. `appsecret_proof` is added whenever the app
  secret is known. Secrets never print: debug output shows `***` and error text is scrubbed of Graph
  tokens.
- `credentials.json` is written atomically as 0600 in a 0700 directory.
- Webhooks are accepted only with a valid `X-Hub-Signature-256` (HMAC-SHA256 over the raw body, constant
  time), and the verify token is compared in constant time.
- Downloads are limited to Meta's CDN hosts, re-checked on every redirect hop, size-capped, and the token is
  sent only to those hosts.
- File tools are confined to `META_MCP_FILES_DIR`. It is a path check, not an operating-system sandbox,
  and everything inside the directory is readable by the assistant.
- HTTP mode requires API keys (32 characters or more, constant-time comparison), guards the `Host` and
  `Origin` headers, binds to loopback by default and sends no CORS headers. TLS is your reverse proxy's job.
- `META_MCP_READ_ONLY=true` removes the 11 tools that send or change state.

The full threat model, each control's limits and how to report a vulnerability are in
[SECURITY.md](SECURITY.md).

## FAQ

**Can it read my personal Messenger or WhatsApp?** No. The Graph API reaches Facebook Page inboxes and
WhatsApp Business numbers only. A personal WhatsApp number can become a Business API number only by
deleting the personal account first (Meta's rule), and there is no endpoint for personal Messenger.

**What is the 24-hour window?** A Messenger Page can reply to a person for 24 hours after their last
message (up to 7 days after a click-to-Messenger ad); WhatsApp free-form messages work for 24 hours after
the person's last message. Outside it Messenger sends fail, and WhatsApp needs an approved template. The
three Messenger tags for updates were retired on 2026-04-27, and the remaining `HUMAN_AGENT` tag is for a
person typing, not for AI replies. See the [guide, Part 5.2](docs/GUIDE.md#52-the-24-hour-window-and-the-dead-tags).

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
the two download tools remain.

## Documentation

- [docs/GUIDE.md](docs/GUIDE.md): the implementation guide, from no Meta account to a working assistant,
  with the WhatsApp track, production advice and troubleshooting.
- [docs/TOOLS.md](docs/TOOLS.md): every tool with parameters, results, errors and examples.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): modules, data flow, trust boundaries and sequence diagrams.
- [SECURITY.md](SECURITY.md): the threat model, the limits of each control and how to report a problem.
- [CHANGELOG.md](CHANGELOG.md) and [env.example](env.example).

## Development

`scripts/check.sh` is the local gate: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and
`cargo test`, stopping at the first failure. Tests use wiremock and never touch the network.

```bash
./scripts/check.sh
```

## License

MIT. See [LICENSE](LICENSE).
