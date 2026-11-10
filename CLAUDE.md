# meta-mcp: MCP server that lets an AI assistant read and send Messenger and WhatsApp messages, through the Meta Graph API (Page and business number) or your own accounts (personal mode)

This codebase is yours: what it's for and how it's built are your call, and you'll keep building on it.

## Ground rules
1. Code and docs move together. A change to a tool, an error, a variable or a command updates each doc that describes it (README.md, docs/TOOLS.md, docs/ARCHITECTURE.md, SECURITY.md, docs/GUIDE.md) and CHANGELOG.md under Unreleased. Past fixes often needed a follow-up "docs in line" commit.
2. Never send or log in to a real account while developing. Sends reach real people as the Page, the business number or you, and personal logins are outside the platforms' terms. Test with wiremock (tests/) and the fake sidecar in tests/common/personal.rs.
3. No secret in the tree, a fixture, a log line or an argument list: tokens, the App Secret, cookies, `credentials.json`, session files. A `Secret` prints `***` in Debug and Display; read it with `expose()` only where it is sent.
4. The Rust daemon and both Go sidecars speak protocol version 1. A frame change touches `src/personal/protocol.rs`, the `ipc/` package of each sidecar and both sidecar docs, in one change.

## Commands
- Build: `cargo build`
- Test, full suite (about 45 s): `cargo test`
- Test, one Rust test: `cargo test --test graph bearer_header_and_appsecret_proof` (`--test` names a file in tests/)
- Test, Go sidecars (`cargo test` does not run them): `cd sidecars/whatsapp && GOTOOLCHAIN=local go test -count=1 ./...`, and the same in `sidecars/messenger`
- Test, one Go test: `cd sidecars/whatsapp && GOTOOLCHAIN=local go test -count=1 -run TestFileModeAndReopen ./store/`
- Vet, Go: `cd sidecars/whatsapp && GOTOOLCHAIN=local go vet ./...`, and the same in `sidecars/messenger`
- Format and lint, Rust: `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`
- Full local gate (format, clippy, tests): `./scripts/check.sh`
- Run without credentials: `cargo run --quiet -- tools` prints the stdio tool names, one per line
- Release binaries and sidecars: see the Development section of README.md

## Layout worth knowing
- Tool schemas, descriptions and read-only lists are in `src/mcp/*_tools.rs`; the code that does the work is in `src/messenger/`, `src/whatsapp/` and `src/personal/`.
- The inbox is one SQLite file written by the webhook receiver and the personal daemon; the MCP server reads it and flags rows as processed.
- `sidecars/messenger` and `sidecars/whatsapp` are separate Go modules, outside Cargo.
- `tests/personal.rs` is Unix only (`#![cfg(unix)]`) because its fake sidecar is a bash script.

## Conventions
- Tests never touch the network: Graph calls go to wiremock (`META_GRAPH_BASE_URL` points at it), and the binary runs as a child process over stdio or HTTP.
- Read-only mode removes write tools by name. A new write tool must be listed in its module's `WRITE_TOOLS` (or in `CORE_WRITE_TOOLS` in `src/mcp/mod.rs`), or it stays callable under `META_MCP_READ_ONLY=true`.
- Tool descriptions that return Meta or personal-account content say "untrusted third-party content", and send tools say they reach real people and ask the client to confirm first. The unit test `descriptions_are_short_and_mark_untrusted_content` in `src/mcp/mod.rs` enforces the first rule for the inbox and personal tools.
- Go builds use `GOTOOLCHAIN=local` and `CGO_ENABLED=0`; the Linux binaries are static.
- CHANGELOG.md follows Keep a Changelog: behaviour changes get an entry under Unreleased, and security fixes go under Security.

## Landmines
- Don't retry a send. The Graph client never retries POSTs to `/messages`, `/media` or `/message_attachments`: a retried send can reach the person twice.
- Don't raise the WhatsApp sidecar's session database above one connection (`SetMaxOpenConns(1)` in `sidecars/whatsapp/wa/session.go`). A second connection writing in WAL mode gets SQLITE_BUSY without honouring busy_timeout, which failed during the history burst on the first live link.
- Don't let a download reach the Graph API host, even when `META_MCP_DOWNLOAD_HOSTS` names it: the fetch would be an API call made with your credentials.
- Don't pass secrets as arguments: process lists show them. That is why `personal login messenger` takes `--cookies-file` or a hidden prompt, and why `--cookies-json` was removed.
- Don't enable request logs in setup, doctor or token: OAuth calls put the App Secret in the query string, so those commands cap logs at `meta_mcp=warn` and silence the HTTP libraries (`src/setup/mod.rs`).
- Don't pass the daemon's environment to a sidecar: it starts with HOME only, and that is part of its isolation (SECURITY.md, "Sidecars, logs and scrubbing").
- Don't register the personal tools on HTTP by default: every API key holder would reach the owner's accounts. Only `META_MCP_PERSONAL_HTTP=1` does that.
- `meta-mcp personal install` copies `META_MCP_FILES_DIR`, `META_MCP_SIDECAR_DIR`, `META_MCP_INBOX_DB`, `META_MCP_READ_ONLY` and `META_MCP_LOG` from the installing shell into the systemd unit or launchd plist, so set them first and re-run install after changing them. A Windows logon task cannot carry them, so they are saved with `setx`.
- Don't use the `HUMAN_AGENT` tag for a generated reply: Meta lists automated messages as a disallowed use. The server instructions and tool descriptions say so; keep them that way.
- Don't add a dependency to a sidecar without checking its licence: the Messenger sidecar links AGPL-3.0 code, so the combined binary is AGPL-3.0, and both sidecars link GPL-3.0 libsignal (README.md, License).

## Deeper context
- docs/ARCHITECTURE.md: modules, Graph client, webhook, inbox schema, personal daemon and its wire protocol.
- docs/TOOLS.md: parameters, results and errors of every tool.
- docs/GUIDE.md: Meta app and WhatsApp setup, operations and troubleshooting; personal mode is Part 10.
- SECURITY.md: threat model, file jail, read-only mode and the send audit log. Read it before touching downloads, files, sends or the daemon.
- sidecars/messenger/docs/SIDECAR-MESSENGER.md and sidecars/whatsapp/docs/SIDECAR-WHATSAPP.md: the sidecar side of the protocol.
- env.example: every environment variable, with its default.
