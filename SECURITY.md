# Security

meta-mcp gives an AI assistant the ability to read and send messages as a Facebook Page
and as a WhatsApp business number. In personal mode it can also read and send as you, on your
own Messenger and WhatsApp accounts, through unofficial clients that are outside the platforms'
terms; that mode has its own section, [Personal mode](#personal-mode). This document states what
the server protects, how, and where the protection stops. Where a guarantee has limits, the limits
are written next to it.

## Reporting a vulnerability

Use GitHub private vulnerability reporting for this repository: open the repository's
**Security** tab and choose **Report a vulnerability**. Please do not open a public issue
for a security problem.

A useful report has the output of `meta-mcp --version`, the transport you run (stdio, http,
webhook), the steps that reproduce the problem and what you expected. Never include a real
access token, app secret, verify token or API key, and for personal mode never include session
files (`state.json`, `session.db`, `e2ee.db`, `messages.db`), cookies, QR codes, pair codes, passkey
assertions or the audit log. If you pasted one by accident, revoke it
first: reset the app secret in the App Dashboard (Settings > Basic, as documented on
2026-10-24) and regenerate the tokens.

## Threat model

The central fact: **the access token is the Page, or the business number.** Whoever holds a
Page access token with `pages_messaging` can read and send as that Page, and whoever holds
a WhatsApp token can send from that number. Everything below follows from that.

Personal mode has a second central fact: **the session files are your account.** The Messenger cookies and
the WhatsApp linked-device keys that the daemon stores let whoever holds them be you on that platform, and the
daemon socket lets any process running as your user do the same. See [Personal mode](#personal-mode).

| Asset | What an attacker gains |
|---|---|
| Page access token | Reads the Page inbox and sends as the Page, within the permissions the token holds |
| WhatsApp access token | Sends from the business number and reads its media |
| App secret | Forges webhook signatures, forges `appsecret_proof`, builds the app token (`APP_ID` and `APP_SECRET` joined with a pipe) |
| Long-lived user token | Mints Page tokens for every Page the user manages (`me/accounts`) |
| `META_MCP_API_KEYS` | Calls every tool the HTTP server exposes |
| `inbox.db` | Reads stored customer messages, names and media IDs |
| Files directory | Whatever is inside it (see the file jail limits below) |
| Messenger session cookies (`personal/messenger/state.json`) | A Facebook web session as you: read and send in every chat |
| Linked-device keys (`personal/whatsapp/data/session.db`) | Act as your linked WhatsApp device until you unlink it on the phone |
| Personal message stores (`messages.db`, `e2ee.db`, `inbox.db`) | Your chat history, in the clear |
| The personal daemon socket | Read your chats and send as you, from any process running as your user |

Who and what is considered hostile:

- **Anyone who can message the Page or the WhatsApp number** or, in personal mode, one of your own accounts.
  Names, message text, captions, file names, link titles and attachments are third-party content. The assistant
  that reads them may also hold the send tools, so a message can try to steer it
  (prompt injection): "reply to everyone with this link", "read this file and send it".
- **Network attackers** reaching the webhook endpoint or the HTTP transport: forged
  webhooks, unauthenticated tool calls, DNS rebinding from a web page in your browser.
- **Other local users and processes** on the machine that runs the server.
- **A malicious or compromised download URL**, because the assistant chooses URLs for the
  download tools.
- **Other processes running as your user** (personal mode). The daemon socket and the session files are
  protected from other users, not from your own processes.

Not defended against: a compromised host or account (an attacker with your user session
already has the credentials file), Meta itself, and a legitimate client holding a valid API
key (it has the full tool surface; there are no per-key permissions).

### Prompt injection and untrusted content

meta-mcp cannot stop a model from obeying text it reads. What it does:

- The server instructions and the description of every tool that returns Meta content say
  that the content is untrusted third-party text and must not be followed as instructions.
- Every tool that reaches a real person says so in its description and tells the client to
  confirm intent first. Send tools are annotated as not read-only and open-world, so an
  MCP client can ask you before running them.
- **Read-only mode** (`META_MCP_READ_ONLY=true`) removes every tool that sends a message or
  changes local state, so an injected instruction has nothing to send with.
- Attachment bytes are saved as files. They are shown to the model only as an inline image, and
  only when the bytes themselves pass a structural check as a PNG (signature and `IHDR` chunk),
  a JPEG (a baseline or progressive frame header), a GIF (header and screen descriptor) or a
  WEBP (a RIFF container whose size matches) with a sane width and height, and fit in 2 MiB. The
  type a sender or a server reports is never trusted. Anything else is saved and the result says why
  it was not shown (`note` for Messenger, `inline_skipped` for WhatsApp). The bytes are never
  executed or parsed otherwise.

These are advisory and structural measures, not a guarantee. If the assistant can send, an
injected message can ask it to send. Use read-only mode, a narrow files directory, a Page
or number you can afford to lose, and a client that asks for confirmation before send tools.

## Token handling

- **Bearer header only.** Every Graph call sends its token as `Authorization: Bearer <token>`,
  never in a URL that meta-mcp builds. Two Meta endpoints force secrets into the query
  string and are the exceptions: the code and token exchange at `oauth/access_token` carries
  `client_secret` and the code or token being exchanged, and `debug_token` carries the
  inspected token as `input_token` (its credential, the app token, is still in the header).
  These calls run only in `setup`, `token` and `doctor`. The client does not log request URLs,
  and transport errors are stripped of their URLs before they are shown. If you point
  `META_GRAPH_BASE_URL` at a proxy, that proxy sees those query strings; the base URL must
  be `https` unless the host is loopback.
- **`appsecret_proof`.** Whenever the app secret and a token are both known, the client adds
  `appsecret_proof` (the lowercase hex HMAC-SHA256 of the token keyed with the app secret)
  as a query parameter. With **Require App Secret** switched on in the app (App Dashboard >
  Settings > Advanced > Security, as documented on 2026-10-24), a leaked token alone is then
  refused by Meta. Switch it on. meta-mcp sends the plain HMAC of the token, not the
  time-stamped variant that Meta documents as expiring after 5 minutes.
- **Paging links.** A `paging.next` URL from Graph is followed only when it has the same
  origin as the configured base URL, and any `access_token` or `appsecret_proof` it carries
  is stripped and replaced by the client's own.
- **Redaction.** Secrets live in a `Secret` type whose debug and display output is `***`, and
  the configuration debug output hides tokens, the app secret, the verify token and the
  API keys. Error text (tool results and the log lines that repeat it) passes through a scrubber that
  replaces any run shaped like `EAA` followed by 20 or more letters and digits with `EAA***`. The limit: the scrubber
  is pattern based. A token with another shape is not caught. The app secret, the verify
  token and API keys are protected by never being formatted into messages, not by scrubbing.
- **`credentials.json`.** Written atomically (private temporary file, fsync, rename) as mode
  0600 inside a directory of mode 0700 on Unix. On load, a file readable by group or others
  logs a warning that names the `chmod 600` fix. On non-Unix systems the permission bits
  are not set, so rely on the ACLs of your profile directory. When meta-mcp creates the inbox
  database it is created 0600 as well; an existing file keeps its mode.
- **Environment variables** override the file and suit containers. They are visible to the
  same user and to root through the process table, and to anything that inherits the
  environment. Prefer a systemd `EnvironmentFile` or your platform's secret mechanism over
  command-line arguments.
- **Command line.** `meta-mcp setup --app-secret` works but leaves the secret in shell
  history, and the wizard says so. Use `--app-secret-env`, `--app-secret-file` or the
  hidden prompt.
- **`meta-mcp token show`** reports kinds, expiry, scopes and names, never a token value.
- meta-mcp never reads `.env` files. `env.example` is a template you load yourself.

## Webhook receiver

- **Signature.** Every `POST /webhook` needs `X-Hub-Signature-256: sha256=<hex>`. The server
  computes the HMAC-SHA256 of the raw request bytes, keyed with the app secret, and compares
  in constant time. A missing or wrong signature is answered 401 and nothing is stored. The
  body is parsed only after the signature passes. The limit is 1 MiB (413 above it).
- **Raw bytes.** The signature is checked over the bytes as received, never over re-encoded
  JSON. Meta documents that its Messenger signature is computed over an escaped-unicode
  form of the payload (the text `äöå` hashed as `\u00e4\u00f6\u00e5`) and warns that hashing
  decoded text gives a different result. UNVERIFIED for WhatsApp, whose pages do not
  repeat the note. If valid events containing non-ASCII text are rejected with 401, report it.
- **Verify token.** The `GET` handshake succeeds only when `hub.mode` is `subscribe` and
  `hub.verify_token` matches in constant time; the answer is the challenge as plain text,
  otherwise 403. The verify token is a shared secret between you and Meta. Keep it private.
- **No API key on the webhook.** Meta cannot send one, so the signature is the only gate.
  With `meta-mcp http --with-webhook` the `/webhook` routes sit outside the API-key layer
  but inside the Host guard. The standalone `meta-mcp webhook` has neither a Host guard nor
  API keys and binds `127.0.0.1:8787`; expose it only through a TLS tunnel or reverse proxy.
- **What the signature does not prove.** It proves the sender holds the app secret. It does
  not make the content safe: message text is still untrusted. There is no replay protection
  beyond deduplication by event ID, because Meta signs no timestamp. A captured valid request
  replayed later is accepted if its event IDs were never stored.
- **No TLS in the process.** meta-mcp serves plain HTTP. Meta needs public HTTPS with a valid
  certificate, which a tunnel or reverse proxy provides. mTLS (optional on Meta's side) is
  not implemented here; the signature is the gate.

## Downloads

`messenger_download_attachment` and `whatsapp_download_media` fetch URLs that the assistant
(and ultimately the message sender) influences, so they are the SSRF-shaped part of the
server. The rules, enforced in the Graph client:

- https only (plain http is accepted for loopback hosts, which exists for tests), no user
  name or password in the URL.
- The host must match the allowlist on **every hop**: `*.fbcdn.net`, `*.fbsbx.com`,
  `*.whatsapp.net` and `lookaside.fbsbx.com`, plus any exact host listed in
  `META_MCP_DOWNLOAD_HOSTS` (bare host names: no wildcard and no port). A wildcard needs at least
  one label before the suffix. `*.facebook.com` and `*.messenger.com` are not on the list.
- **The Graph API host is never fetched.** A URL on the host of `META_GRAPH_BASE_URL`
  (`graph.facebook.com` by default) is refused on every hop, whether or not a token would be
  attached, whatever its port or path, and even when `META_MCP_DOWNLOAD_HOSTS` names it. A fetch
  there is an API call made with your credentials, not a media download. Because redirects are
  followed by hand, a redirect that lands on it is refused too.
- Redirects are followed by hand, at most 5, each re-checked against the allowlist and the
  Graph host rule.
- The body is streamed and aborted past `META_MCP_MAX_FILE_BYTES`.
- **Token rules.** The first Messenger attempt carries no token. After an HTTP 401 or 403 the
  download is retried once with the Page token, and only when the host ends in `.fbcdn.net` or
  `.fbsbx.com` (or is listed in `META_MCP_DOWNLOAD_HOSTS`). The WhatsApp media URL requires the
  token and is fetched with it only when its host is `lookaside.fbsbx.com`, ends in `.whatsapp.net`
  or is listed in `META_MCP_DOWNLOAD_HOSTS`. The token is tied to the exact host and port of that
  request, so a redirect to another host, or to the same host on another port, drops it.
- Limits: the allowlist is by host name. DNS answers are not pinned or checked against
  private ranges, and the three wildcards cover every subdomain under them.
  `META_MCP_DOWNLOAD_HOSTS` widens both the allowlist and the set of hosts that may receive a
  token: leave it unset in production.
- URLs given to the send tools are fetched by Meta's servers, not by meta-mcp.

## File jail

`META_MCP_FILES_DIR` names the only directory the file tools read from and write into. With
it unset, path arguments fail with an error naming the variable. What the jail guarantees:

- The directory is canonicalised once at startup. Paths containing `..` that leave it, and
  absolute paths outside it, are refused.
- A read resolves symlinks first and then checks that the real path is inside the directory
  and is a regular file. A write destination that is a symlink, or that passes through one
  leaving the directory, is refused. Missing parent directories are created one level at a
  time with the containment check repeated.
- Destination components that start with a dot are refused. Writes go to a temporary file
  and are then hard-linked or renamed into place, so a failed write leaves no partial file
  and an existing file is never replaced unless `overwrite` is true.
- Reads and writes are capped by `META_MCP_MAX_FILE_BYTES` (default 100 MiB).
- **Received files are kept apart.** Downloads are saved under `inbound/messenger/` and
  `inbound/whatsapp/` inside the directory. The upload paths (`path` of `messenger_send_attachment`
  and `whatsapp_send_media`, and `whatsapp_upload_media`) refuse any file under `inbound/`, unless
  the call says `allow_inbound=true`, so passing what one person sent to someone else is a step the
  assistant has to take on purpose. The flag is an argument of the tool call: an injected
  instruction can set it too. It makes the step visible to a client that shows tool arguments
  before a send tool runs, and it is not a permission boundary.
- **Download names are safe and short.** Messenger reduces any name that comes from the sender or
  from a URL (and `dest_name` too) to one path component: control, bidirectional and zero-width
  characters and `: * ? " < > |` are replaced, Windows device names such as `CON` are prefixed with
  `_`, and the name is cut to 200 bytes on a character boundary. A `Content-Disposition` header is
  never read. A derived Messenger name that already holds different content gets a short hash of
  the URL instead of replacing it, and an explicit `dest_name` or an identical file fails with
  "already exists" unless `overwrite` is true. WhatsApp names the file
  `whatsapp_<media id>.<extension>`; a `dest_name` may add sub-folders but cannot leave
  `inbound/whatsapp/`, and a component over 200 bytes is refused before any request is made.
- **The directory cannot contain the configuration.** meta-mcp refuses to start when
  `META_MCP_FILES_DIR` contains `META_MCP_HOME`, `credentials.json` or the inbox database
  (`META_MCP_INBOX_DB`), after resolving symlinks. The error names the overlap, because the file
  tools could read or replace those files.

What it does not guarantee:

- **It is a path check, not an operating-system sandbox.** The check and the later open are
  separate steps. A local process that can modify the directory tree while the server runs
  (for example, swapping a directory for a symlink after the check) can defeat it. Do not
  share the directory with untrusted local users.
- A hard link inside the directory that points at a file outside it passes the check,
  because the link's path is inside.
- **Everything inside the directory is readable by the assistant** and can be sent to a real
  person through the send tools. Keep the directory dedicated and free of secrets. Reads of
  dot files inside it work; the dot rule applies to write destinations only.
- Saved files are created with the process default permissions (the umask), not 0600.
- The jail covers tool path arguments only. `META_MCP_HOME` and `META_MCP_INBOX_DB` are
  operator choices and are not jailed. The startup check above refuses a files directory that
  contains them; it runs once, at startup.

## HTTP mode

`meta-mcp http` serves MCP at `/mcp` and `GET /health`; `--with-webhook` adds `/webhook`.

- **API keys are mandatory.** `META_MCP_API_KEYS` is a comma-separated list; the server
  refuses to start without one, and every key must be at least 32 characters (generate one
  with `openssl rand -hex 32`). Keys are sent as `Authorization: Bearer <key>` or
  `X-API-Key`, compared in constant time against every configured key (the key length is not
  hidden), and never logged. A bad key gets 401 with `WWW-Authenticate: Bearer`.
- **Host and Origin guard** (DNS rebinding). The `Host` header must be `localhost`,
  `127.0.0.1` or `::1` (any port), the bind host, an entry of `META_MCP_ALLOWED_HOSTS`, or
  the host of `META_WEBHOOK_PUBLIC_URL` when `--with-webhook` is set. An `Origin` header, when
  present, must also name an allowed host. Anything else gets 403. Requests without an
  `Origin` header (non-browser clients) pass. Put your proxy's public host name in
  `META_MCP_ALLOWED_HOSTS`.
- **Loopback by default.** The bind address is `127.0.0.1:8788`. Binding anything else logs
  a warning. The process speaks plain HTTP, so terminate TLS in front of it.
- **No CORS.** No cross-origin headers are sent, so a web page cannot call the server from a
  browser.
- **Limits.** 2 MiB request body on `/mcp`, 1 MiB on `/webhook`. Sessions are stateless.
- **All keys are equal.** There are no per-key permissions, rate limits or audit log beyond
  the request trace. A key holder can call every exposed tool. Run a second instance with
  `META_MCP_READ_ONLY=true` for clients that should only read.
- `/health` is open and answers `{"ok":true}` only. In `meta-mcp webhook` mode `/health` also
  reports event counts and the time of the last event.

## Read-only mode

`META_MCP_READ_ONLY=true` removes these tools from the server (they are not listed and cannot
be called): `inbox_mark_processed`, `messenger_send_text`, `messenger_send_attachment`,
`messenger_sender_action`, `whatsapp_send_text`, `whatsapp_send_media`,
`whatsapp_send_template`, `whatsapp_send_reaction`, `whatsapp_send_location`,
`whatsapp_mark_read` and `whatsapp_upload_media`, and the 10 personal write tools:
`messenger_personal_send_text`, `whatsapp_personal_send_text`, `messenger_personal_send_media`,
`whatsapp_personal_send_media`, `messenger_personal_react`, `whatsapp_personal_react`,
`messenger_personal_mark_read`, `whatsapp_personal_mark_read`, `messenger_personal_typing` and
`whatsapp_personal_typing` (21 tools in all; 17 remain). It does not hide data: the read tools still
return conversations and inbox events, and the download tools remain because they only
write inside the files directory. In read-only mode the two business download tools refuse `overwrite=true`, so they
can only create new files, and the personal download tools always save under a name that ends in a random suffix.
The webhook receiver keeps storing events, and the `setup`, `token` and `doctor` commands still write the
credentials file; they are commands, not tools. The personal daemon is a separate process with a switch of its
own: see [Read-only mode in personal mode](#read-only-mode-in-personal-mode).

## Personal mode

Personal mode lets the assistant read and send on **your own** Messenger and WhatsApp accounts. It rests on two
unofficial clients that the platforms do not offer to third parties, it keeps a copy of your logged-in session
on disk, and everything it sends carries your name. The mechanics are in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#personal-mode) and the setup in the
[guide, Part 10](docs/GUIDE.md#part-10-personal-mode-your-own-messenger-and-whatsapp).

### Unofficial clients: the risk to the account

**Personal mode is outside the terms of Facebook and WhatsApp for personal accounts.** The Messenger sidecar
re-implements Messenger's web protocol (the `messagix` library of the mautrix-meta bridge, with `whatsmeow` for
end-to-end encrypted chats), and the WhatsApp sidecar re-implements a linked device (`whatsmeow`). The platforms
support neither. What follows is what the platforms and the projects behind the libraries document, quoted as the
research read it on 2026-10-24. It is not a prediction about your account and not legal advice.

| Platform | What is documented | Where it was stated |
|---|---|---|
| Facebook | Terms section 3.2: "You may not access or collect data from our Products using automated means (without our prior permission) ... regardless of whether such automated access or collection is undertaken while logged-in to a Facebook account." Section 4.2 lets Meta "suspend or permanently disable your access". The Automated Data Collection Terms (effective 2024-10-07): "You will not engage in Automated Data Collection without first obtaining Meta's express written permission" | Facebook Terms; Automated Data Collection Terms |
| Facebook | "In some cases, Meta may decide your account has suspicious activity and block you until you do some tasks like completing a captcha, adding a phone number or resetting your password. It is recommended to have two-factor authentication enabled to reduce the risk of such blocks." | mautrix-meta authentication documentation |
| Facebook | Users of the library report the message "We suspect automated behaviour on your account", soft locks, a bridge token blocked within 24 hours of re-adding it, and "Your account is disabled" (an issue open since 2024-03-22). Results are mixed: one user ran 7 months before a flag, another is flagged every one to two months. These are user reports, not statements from Meta | mautrix-meta issue tracker |
| Facebook | A contributor reports that about half of new logins were revoked within minutes after the secure-history PIN step shipped (a 418 on the encrypted socket, then the `xs` cookie deleted, sometimes a checkpoint). The root cause is **UNVERIFIED**. The PIN restore is deliberately not implemented | mautrix-meta pull request 425 |
| WhatsApp | "Using these apps or linking your WhatsApp account to unofficial versions of WhatsApp violates our Terms of Service." "Your WhatsApp account might also be temporarily or permanently banned, or it could lead to restrictions on your account, including the ability to link devices." | WhatsApp Help Center, unofficial apps |
| WhatsApp | "Linking your account to an unofficial app or website, now or in the past, may result in a temporary or permanent account ban": the exposure is retroactive | WhatsApp Help Center, linked devices |
| WhatsApp | A banned number shows "This phone number can't use WhatsApp", and "You can't access your chat history or backups while your account is banned." A review can be requested ("Most reviews are completed within 24 hours"); "If you don't see an option to request a review, it means the ban is final". The ban lands on the account on your phone, not only on the linked device | WhatsApp Help Center, account bans |
| WhatsApp | A restricted account "can't start chats or calls with new contacts, create new groups, add new members to groups, or use linked devices. You may also be logged out of some companion devices." | WhatsApp Help Center, restricted accounts |
| WhatsApp | "Don't bulk message, auto-message, or auto-dial using WhatsApp." The research's reading, not a WhatsApp statement: an assistant that drafts and sends replies sits near the "auto-message" wording. Anecdotal reports (none from WhatsApp) include bans of accounts that only replied to incoming messages at low volume | WhatsApp Help Center, using WhatsApp responsibly; the library issue trackers |

The Messenger sidecar's own documentation says to expect checkpoints and to use an account you can afford to
lose. Only you can decide whether the account, which is your phone number or your Facebook identity, is one you
can.

**What the design does.** Some of it follows what the platforms or the library maintainers say; some of it is a
precaution whose effect is unproven. None of it brings the risk to zero, and no source shows how much it lowers it.

- **One persistent connection set per account.** One sidecar process per account, which never opens a second
  connection set (Messenger: its main socket plus, once the encrypted-chat device is registered, one more for
  encrypted chats; WhatsApp: one). Each sidecar holds a lock on its data folder and refuses a second process on it. A
  second process on the same WhatsApp keys would make WhatsApp end the first (`stream_replaced`), which can look like
  abuse.
- **WhatsApp presence stays "unavailable".** The sidecar never marks the device available and never subscribes to
  anyone's presence. It never sends a read receipt on its own: blue ticks go out only for an explicit `mark_read`.
  The reason is notification routing (marking a companion available may change when your phone buzzes), not a known
  ban trigger, and whether staying unavailable makes the Linked devices screen show the device as stale for
  WhatsApp's 30-day inactivity rule is **UNVERIFIED**.
- **No new chats, no bulk operations.** No operation creates a group or a broadcast, or edits or unsends a message,
  and each sidecar sends only into a conversation it already holds (next section). Nothing stops a client from
  calling a send tool many times, which is why approval and read-only mode matter.
- **A human can approve each send, if your client is set to ask.** The personal send tools are annotated as not read-only and
  open-world, and their descriptions say they reach real people as you and to confirm intent first. meta-mcp has no
  approval step of its own: whether you are asked depends on your MCP client's settings.
- **Nothing paces sends.** Neither the daemon nor the sidecars limit the rate of sends, reactions, read marks or
  typing. The figures that circulate (at least 500 ms between messages, about 20 per minute per account) are
  folklore: neither is a threshold WhatsApp published.
- **A fatal signal stops sending.** A temporary ban, a reach-out time-lock, an outdated client, a replaced stream, a
  removed device or a ban sets `sending_blocked` and stops reconnecting. `meta-mcp personal restart` clears the
  block, not the cause: do not restart in a loop.
- **Read-only mode**, in two layers (below).

### The no-new-chats rule

The rule is enforced by the sidecars, not by the daemon, which checks only the shape of an id.

- **WhatsApp** sends only into a chat that is already in the sidecar's store, and answers `thread_not_found` otherwise
  ("the owner must open this chat on the phone first"). WhatsApp's own pages list "you can't start chats or calls
  with new contacts" among the restrictions and name contacts who "have contacted you first" as the best practice.
- **Messenger** sends, reacts, marks read and shows typing only in a thread that is in the sidecar's thread list. Any
  other id, the id of a contact you have no conversation with included, is `thread_not_found`: it might be an
  encrypted chat that is not listed yet, and a send must never fall back to plaintext. Reading is looser: for an
  unlisted numeric id the sidecar still asks Messenger for the messages, and the answer depends on Messenger.

### Read-only mode in personal mode

`META_MCP_READ_ONLY=true` is honoured twice, and the layers are independent. In the MCP server it removes the 10
personal write tools. In the personal daemon it makes `send_text`, `send_media`, `send_reaction`, `mark_read` and
`set_typing` answer `read_only` (a refused send, media send or reaction is recorded in the audit log as a `refused`
line; mark read and typing are not recorded). `meta-mcp personal install` copies the variable
into the service when the installing shell has it; if you run the daemon yourself, set it there too. Read-only
does not stop logging in or out, reading, waiting, downloading attachments or storing incoming messages. See
[Read-only mode](#read-only-mode).

### Session files

The daemon keeps your logged-in sessions in `META_MCP_HOME/personal/` (folders 0700, files 0600):

| File | What it holds | What a copy lets someone do |
|---|---|---|
| `messenger/state.json` | The Facebook session cookies (`c_user`, `xs`, `datr` and others), the user agent, your Facebook ID | Be logged in to Facebook as you, read and send in every chat, until Meta ends the session |
| `messenger/data/e2ee.db` | The encrypted-chat device keys and Signal sessions, the decrypted cache of encrypted messages as plain JSON (about the newest 2000 per thread) and the media keys and download paths of encrypted attachments, the Facebook account it belongs to, and for unencrypted messages that carry media the attachment details with the signed download link (about the newest 10000) | Read the cached encrypted messages and the attachment details; act as the registered encrypted-chat device |
| `messenger/data/threads.json` | The thread mapping and the account it belongs to, no message content | Learn your thread IDs |
| `whatsapp/state.json` | Account metadata: your JID (which holds your phone number), LID, display name, pairing time, history choice | Learn who you are; it holds no keys |
| `whatsapp/data/session.db` | The linked device's identity, Signal sessions, sender keys, pre-keys, app state keys, contacts and LID map, with **private keys in the clear** | Act as the linked device until you unlink it on the phone |
| `whatsapp/data/messages.db` | Your chats, messages (text in the clear and the raw protobuf of each), reactions, receipts and contact names | Read your WhatsApp history |
| `audit.jsonl` | See below | Learn the thread IDs, message IDs and times of your sends |
| `inbox.db` (`META_MCP_INBOX_DB`) | Every personal event the daemon stored, with the whole event in `raw` | Read stored messages, names and attachment descriptions |

- **Who can read them.** Your user and root. Under `personal/`, folders are reset to 0700 whenever the daemon prepares
  them and files are created 0600 (the unit sets `UMask=0077`), so another local user cannot read them. The exception
  is `inbox.db`: it lives outside `personal/`, the daemon resets neither its mode nor the mode of its folder, and
  meta-mcp creates a new one 0600 while an existing file keeps its mode (see
  [Token handling](#token-handling)). Check both modes yourself if you created the file earlier. A process running as
  you can read all of them, and so can anyone who holds a copy of your disk.
- **Nothing in them is encrypted by meta-mcp.** There is no passphrase and no keyring. An encrypted disk is the
  control. Back them up only to encrypted storage, never commit them, and never copy `session.db` to another machine.
- **Keep the files directory away from them.** Startup refuses a `META_MCP_FILES_DIR` that contains `META_MCP_HOME`,
  but accepts one that sits inside it, and the file tools can read everything in the files directory. Keep
  `META_MCP_FILES_DIR` outside `META_MCP_HOME`, and never point it at `personal/`.
- **Deleting a session.** With the daemon running, `meta-mcp personal logout PLATFORM` always tries to delete
  `state.json`; if that fails the command still says the session is cleared, and `meta-mcp personal status` shows `last error: state_delete_failed`.
  If the sidecar is running, it also deletes its own copies (Messenger: the cookies, `threads.json`, `e2ee.db`;
  WhatsApp: the device, `messages.db` and the leftovers in `session.db`), and on WhatsApp it removes the linked device
  on WhatsApp's side, which unlinks it on the phone, but only while it is connected: offline, the device stays linked,
  with no warning, so check the Linked devices list. If no sidecar is running, only `state.json` is deleted and the
  data folder stays as it is. A `logged_out` from the service deletes the state file as well (on WhatsApp the sidecar
  also tries to wipe the leftovers in `session.db`, as a best effort). `meta-mcp personal uninstall` keeps everything.
- **The cookies you paste.** The login command reads a cURL command or a JSON object and parses it in the process;
  nothing is executed. Its errors never carry cookie values. `--cookies-json` is visible in the process list and in
  shell history: prefer `--cookies-file` (a file only you can read, which you delete yourself afterwards) or the hidden prompt.

### The daemon socket

The socket is mode 0600. A folder that the daemon, or a socket unit written by `personal install`, has to create for
it is mode 0700, and so is `META_MCP_HOME/personal/`, which the daemon always resets to 0700; any other existing
folder keeps its mode, so check it if you chose the path with `--socket` or `META_MCP_PERSONAL_SOCKET`. The
daemon serves only connections whose peer uid is its own (other uids are logged and dropped). There are no tokens and
no per-client permissions: **any process running as your user can read your chats and send as you through it**, as it
could read the session files. The daemon limits a connection to 32 requests in flight and 16 waits, and the
daemon to 64 connections. The unit `personal install` writes sets `UMask=0077` and `NoNewPrivileges=yes` and no
further sandboxing.

**The HTTP transport reaches it too.** `meta-mcp http` builds the same tool router as `meta-mcp stdio`, personal tools
included. When a daemon runs under the same user and the HTTP server can open its socket, every holder of one of the
server's API keys can read your chats and send as you, and the keys carry no per-key permissions. Keep personal mode
on stdio for the assistant that should have it, or start the HTTP server read-only (`META_MCP_READ_ONLY=true`, and the
daemon read-only as well), and give HTTP API keys only to people you would give the session files.

### Sidecars, logs and scrubbing

- **Isolation.** A sidecar starts with an empty environment (only `HOME` is passed), listens on no port, speaks only
  on stdin and stdout, and by contract reads no file outside the `data_dir` it is given except the media copy it is told
  to send (the daemon hands it one in `data/outbox/`), and writes none outside it except the destination of a
  download (inside the files directory). A malformed or oversized
  line from it kills it. The error codes it sends are reduced to plain lower-case words before a client sees them.
- **Logs.** The daemon never logs message text, cookies, session state or phone numbers: its lines carry the op name,
  the request id and an error code. The sidecars log at info level (hard-coded) through an allowlist scrubber: in
  every well-formed JSON log line only a fixed set of top-level fields (`level`, `time`, `message`, `op`, `code`,
  `reason`, `component`, `count`, `event`, `platform`, `thread`, `id`, `state`) is written, and every other field,
  library fields included, is dropped (the Messenger scrubber turns a malformed or oversized line into one fixed marker); the
  `thread` and `id` fields can carry thread and message ids, which are metadata you may not want shared. In the
  strings that stay, the Messenger scrubber cuts URLs, cookies, tokens, long key-like runs, plus-prefixed phone
  numbers, XML tags with attributes (a protocol node carries push names) and the quoted input of strconv, time, URL and
  JSON parse errors; the WhatsApp scrubber cuts user JIDs (group JIDs stay), phone numbers, QR payloads and pair
  codes, and the quoted input of strconv, URL and time parse errors. The sidecars' code does not log those in the
  first place, and the scrubber is the second line of defence. Sidecar log lines reach the daemon's stderr.
- **Events.** `state_updated` (which holds the session) and `ready` are never forwarded to socket clients. QR codes,
  passkey options and message events go only to connections that sent `subscribe`, which are the login commands.
  Treat a QR payload, a pair code and a passkey assertion as secrets: they are never logged, and the login command
  prints the first two on your terminal.
- **What the commands print.** Errors from `meta-mcp personal` are scrubbed: a known secret and any run of 20 or more
  token characters become `[redacted]`, control characters become `?`, and the line is cut at 300 characters.
- **The limit.** Scrubbers are pattern based, like the Graph token scrubber above. A secret of another shape, or a
  value that was never meant to be secret, is not caught.

### The send audit log

`META_MCP_HOME/personal/audit.jsonl` (mode 0600, append-only) records every text, media and reaction send, and
every such write that one of the first pipeline steps refused before it reached the sidecar (read-only mode, a blocked
account, invalid input, a platform that is not online, a missing files directory, a file the jail refuses). A request that the connection limits
turn away, or one that names no valid platform, writes no line.

- **Write-ahead.** An `attempt` line is appended and fsynced before the sidecar is asked. If it cannot be written,
  nothing is sent (`audit_failed`). A completion line follows: `sent`, `failed` or `unknown` (a timeout or a broken link, where
  the message may have gone out). A completion line that cannot be written is reported as `audit_warning` and does
  not undo the send. `invalid` and `refused` lines record writes stopped before the sidecar.
- **What a line holds.** An increasing `id`, the time, the platform, the thread ID, `kind` (`text`, `media`,
  `reaction`), the SHA-256 and character count of the text, caption or emoji, for media the SHA-256 of the file, the
  outcome, `attempt_id`, `message_id` and `error_code`. **Never the text.**
- **Limits.** The SHA-256 of a short or guessable text can be confirmed by anyone who guesses it. Thread and message
  IDs and times are metadata that you may not want shared. `mark_read` and `set_typing` are not recorded. The file is
  a plain file you own, not tamper-evident, and meta-mcp never rotates or purges it: to archive it, stop the daemon
  first.

### Prompt injection in your own chats

Everything the assistant reads from your chats was written by someone else: names, previews, message text, captions
and files. The tool descriptions say so, as in business mode. The difference is the reach: an assistant that can read
and send here can send to every chat you have, as you. An injected "reply to everyone with this link" costs you a
conversation, a friend or the account. Use read-only mode when you only need to read, a client that asks before
each send and shows you the text, and a narrow files directory. The personal download tools take message and attachment
IDs, never URLs, so an injected message cannot point them at another host. A downloaded file is saved under
`inbound/messenger_personal/` or `inbound/whatsapp_personal/` and is subject to the same `allow_inbound` rule and
the same inline-image check as the business downloads.

### Supply chain and licences

Both sidecars are Go programs pinned in their `go.mod`. Their `build.sh` runs `go vet`, `go test`, a static build check
(`CGO_ENABLED=0`) and `govulncheck` on the source and the binary, and fails on any of them; for the
sidecars' dependencies that is the only check this repository performs. The Messenger sidecar builds against a fork of an HTTP client library
selected by a `replace` line, because `messagix` needs its TLS and HTTP/2 fingerprint controls. Because Meta changes
its protocols often, the pins need a bump and a rebuild after a Meta-side break. The Messenger sidecar links
mautrix-meta, which is AGPL-3.0, so the sidecar documentation states that the combined `meta-messenger-sidecar` binary
is AGPL-3.0 and that distributing it carries the AGPL source obligations; the WhatsApp sidecar links
`whatsmeow`, which is MPL-2.0. Both sidecars also link `go.mau.fi/libsignal` (the WhatsApp sidecar through `whatsmeow`, the Messenger sidecar through
mautrix-meta and `whatsmeow`), which is GPL-3.0, so
check the terms of every sidecar binary you distribute. The Rust `meta-mcp` binary is MIT. This is not legal advice.

### What personal mode does not defend against

A process running as your user, a compromised host or disk, and the platforms themselves: a restriction, a
checkpoint or a ban is a risk that no local control removes. A message that the platform rejects or delays after
meta-mcp handed it over is not detected beyond the outcome the sidecar reports (`send_unknown` exists for that gap).

## Data you keep

`inbox.db` holds message text, sender IDs, display names, media IDs and the raw webhook
payload of everything delivered to the webhook: personal data of your customers. meta-mcp has
no retention policy and never purges it. Delete rows or the file on your own schedule, and
back it up like any customer database (see the guide, Part 8). Webhook bodies are not logged;
the receiver logs batch counts only.

In personal mode the same file also holds **your own conversations**: every message, reaction and receipt that
the personal daemon sees (including the history batches WhatsApp sends after pairing), with the whole event in
`raw`, which `inbox_list` returns when asked for it. Next to it are the sidecars' own stores, `messages.db` and
`e2ee.db`, which hold message text in the clear. Nothing expires on a schedule. Sidecar data goes on a
logout; when you link a different WhatsApp account (which clears `messages.db`) or open the Messenger store with a
different account (it is wiped first); when the service ends the linked device (whatsmeow deletes the device); when
the Messenger sidecar trims a thread past about 2000 cached encrypted messages or keeps only about the newest 10000
attachment records; and when a deletion is mirrored (a WhatsApp chat you delete on your phone is deleted from
`messages.db`, a message unsent for everyone loses its text there, an encrypted Messenger message unsent for everyone
is blanked in `e2ee.db`, and the attachment record of an unencrypted Messenger message that is unsent or deleted is
dropped). `inbox.db` is never purged. Apply your own retention, and see
[Session files](#session-files).

## Hardening checklist

1. Switch on **Require App Secret** in the Meta app and keep the app secret out of shell
   history, repositories and container images.
2. Start with `META_MCP_READ_ONLY=true`; allow sends only where you need them.
3. Point `META_MCP_FILES_DIR` at an empty dedicated directory, or leave it unset.
4. Keep HTTP mode on loopback behind TLS, with long random API keys and an exact
   `META_MCP_ALLOWED_HOSTS`.
5. Run the service as an unprivileged user (the guide has a hardened systemd unit).
6. Run `meta-mcp doctor` on a schedule and refresh tokens before they expire.
7. If you enable Meta's **Server IP Allow List** (Settings > Advanced > Security), run
   meta-mcp from an allowed address: Meta documents that it blocks app-secret-signed calls
   from other addresses.
8. The crate denies `unsafe` code in its own source (`unsafe_code = "deny"` in `Cargo.toml`), with one
   exception. `take_fd3` in `src/personal/daemon/listen.rs` carries `#[allow(unsafe_code)]` and calls
   `from_raw_fd(3)`: when systemd starts the personal daemon through socket activation it passes the
   listening socket as file descriptor 3, and the standard library has no safe function that adopts a descriptor
   by its number. The call is made only when `LISTEN_FDS` is `1` and `LISTEN_PID` is the daemon's own pid, and the
   descriptor is then duplicated close-on-exec so that no sidecar inherits it. It is the only such
   function in the crate. The Go sidecars are separate programs (see
   [Supply chain and licences](#supply-chain-and-licences)). The dependencies of the Rust crate are not
   audited by this repository, so keep them updated.
9. Personal mode only: keep `META_MCP_HOME`, the inbox database and the files directory (which holds downloads)
   on an encrypted disk, with `META_MCP_FILES_DIR` outside `META_MCP_HOME`; use an account you can afford to lose;
   run read-only (`META_MCP_READ_ONLY=true` in the daemon and in the MCP server) until you trust what the assistant
   reads; use a client that asks before each send; and read
   [Unofficial clients: the risk to the account](#unofficial-clients-the-risk-to-the-account) before the first
   login.
