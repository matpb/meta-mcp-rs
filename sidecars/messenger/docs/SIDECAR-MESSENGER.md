# meta-messenger-sidecar

Reference for the Go sidecar that gives the meta-mcp daemon a personal Facebook Messenger account. Wire protocol: version 1 of the personal sidecar protocol (shared with the WhatsApp sidecar). The sidecar logs in with the owner's browser cookies, talks to Messenger through mautrix-meta's messagix library and reaches end-to-end encrypted threads through the whatsmeow client that messagix embeds.

## 1. What it is

The Rust daemon (`meta-mcp personal daemon`) spawns `meta-messenger-sidecar` as a child process, one per Messenger account. It speaks JSON lines on stdin and stdout (one object per line, at most 16 MiB), logs to stderr through a scrubber and listens on no port. Stdin closing ends it with status 0. The daemon sends `hello` first, then ops; the sidecar answers every request once and pushes events in between. One process is one account and one connection set: a Messenger socket plus, once registered, one encrypted-chat socket.

## 2. Module layout

Module `github.com/matpb/meta-mcp-rs/sidecars/messenger`.

| Path | Role |
|---|---|
| `main.go` | Entry point: Info-level zerolog through `logscrub`, runs `server.Run` with `engine.NewFactory`. Exit 0 clean, 2 frame violation, 1 I/O error. |
| `ipc/` | JSON-line framing and the reply writer. |
| `logscrub/` | Stderr scrubber for cookies, tokens, keys and ids; the code does not log them in the first place. |
| `api/` | Wire shapes, error codes, `State`, and the `Factory` and `Session` contract between server and engine. |
| `server/` | Request loop, op validation, login state machine, event sink, error replies. |
| `engine/` | The session. `session.go` (state, validate, logout), `client.go` (the `metaClient` seam over messagix), `factory.go` (Open), `connect.go`, `backoff.go`, `queue.go` (connect loop and event queue), `classify.go` (library errors to failure verdicts), `cookies.go` (cookie rotation to `state_updated`), `events.go` (tables to events), `threads.go` (thread registry, `list_threads`), `history.go` (message cache, `fetch_messages`), `e2ee.go` (encrypted client), `send.go`, `sendwait.go` (sends, reactions, read, typing, OTID waiter), `media.go` (uploads, downloads, file writer). |
| `normalize/` | Pure conversion of Lightspeed tables and E2EE events into protocol shapes and ids. |
| `e2ee/` | `e2ee.db`: whatsmeow device store plus the encrypted message cache. |
| `build.sh` | The only supported build. |

## 3. Build

`./build.sh` runs `go vet ./...`, `go test -count=1 ./...`, `go build -trimpath` into `bin/meta-messenger-sidecar` with `CGO_ENABLED=0` (it fails unless `file` reports a static binary), then `govulncheck` on source and binary. Use `GOTOOLCHAIN=local`.

Pins in `go.mod`:

| Module | Version | Licence |
|---|---|---|
| `go.mau.fi/mautrix-meta` | `v0.2610.0` | AGPL-3.0 |
| `go.mau.fi/whatsmeow` | `v0.0.0-20261014094007-f16f9ca30d6a` (no tags, so a pseudo-version) | MPL-2.0 |
| `modernc.org/sqlite` | `v1.61.0` (pure Go, driver name `sqlite`) | BSD-3-Clause |
| go directive | `go 1.26.0` | |

`go.mod` must keep `replace github.com/imroc/req/v3 => github.com/beeper/req/v3 v3.0.0-20260925135712-b49593750e56`. messagix is written against Beeper's fork of req (extra TLS and HTTP/2 fingerprint controls); upstream req does not compile against it and a plain `go get -u` or `go mod tidy` run by hand can drop the line.

## 4. Data on disk and state

`data_dir` is given by the daemon in `hello`. It must be absolute and existing and should be mode 0700 (anything else logs a warning). Files inside are created 0600.

| File | Content |
|---|---|
| `e2ee.db` | SQLite (WAL, `synchronous=FULL`, so an acked message survives power loss). whatsmeow device keys, Signal sessions and sender keys in the clear, plus the E2EE message cache as plaintext JSON (newest 2000 messages per thread kept) and the encrypted media download records. Treat it like a password and like the messages themselves. Deleted by `logout`. |
| `threads.json` | Hybrid thread map: Messenger thread key to the shared thread id and thread type. No message content. |

`state` (opaque to the daemon, persisted from `state_updated`): `{"cookies": {"c_user": "...", "xs": "...", ...}, "user_agent": "<string>", "fbid": "<c_user>", "e2ee_registered": bool}`. Cookies rotate; the sidecar re-emits the blob when they change. `user_agent` is stored but not applied (section 9).

## 5. Ops and library calls

Thread ids are the decimal portal id. A thread counts as encrypted when it has a hybrid mapping to another key, sits in an `e2ee_cutover` folder, has `ThreadType` encrypted-over-WA, or has received an E2EE message; routing uses that flag. Message ids are `fb:<mid>` or `wa:<chat>:<sender>:<id>`; a wa id on an unencrypted thread, or the reverse, is `bad_params` "message id does not belong to this thread". An unknown thread id is `thread_not_found`, except a numeric id of a known contact, which is treated as a 1:1.

| Op | Unencrypted (messagix) | Encrypted (whatsmeow) |
|---|---|---|
| `hello`, `ping`, `shutdown` | No library call; `hello` builds the session without network access. | same |
| `login_start`, `login_finish`, `login_cancel` | Cookie login: `LoadMessagesPage` once to check the account. | none |
| `connect`, `disconnect` | `Connect` the realtime socket in a loop with backoff; after the first connect `RegisterE2EE` (once) and `PrepareE2EEClient` + `Connect`. | same loop |
| `logout` | Drops cookies, `threads.json`, `e2ee.db`, caches. | same |
| `list_threads` | `FetchMoreThreads` on sync groups 1 (and 95 when E2EE is active) when the registry is short. | threads join as messages arrive |
| `fetch_messages` | Cache, else `FetchMessagesTask` with a collector (30 s). | Answered from `e2ee.db`; `has_more` false past the stored range. |
| `download_attachment` | Cached message, then `mediadl.DownloadMedia` of the CDN URL; on 403, 410 or an `expires_at` within 60 s, one `FetchMessagesTask` page refreshes the URL. | `e2ee.db` media record, then `DownloadFBToFile` into a temp file. |
| `send_text` | `SendMessageTask` (TEXT) through `ExecuteTasks`, then wait for the OTID outcome. | `SendFBMessage` with a `ConsumerApplication` text, quoted reply key from the wa id. |
| `send_media` | `SendMercuryUploadRequest`, then `SendMessageTask` (MEDIA, `AttachmentFBIds`, caption as text). Caps: image 8 MB, other 25 MB. | Images only: `Upload`, image transport, `SendFBMessage`. Cap 100 MB. |
| `send_reaction` | `SendReactionTask` (empty emoji removes). | `SendFBMessage` with a reaction `ConsumerApplication`. |
| `mark_read` | `ThreadMarkReadTask` with `up_to_ts_ms` or now. | `MarkRead` for the newest 100 stored incoming messages up to the watermark, grouped by sender. |
| `set_typing` | `ExecuteStatelessTask(UpdatePresenceTask)`. | `SendChatPresence` composing or paused. Global presence is never changed. |

Send details:
- OTID: every unencrypted send gets `methods.GenerateEpochID()` and registers a waiter first. The outcome comes from the `ExecuteTasks` response table or the socket (`LSReplaceOptimsiticMessage` success, `LSMarkOptimisticMessageFailed` or `LSHandleFailedTask` failure), whichever is first, within one 30 s budget that starts when `ExecuteTasks` is called.
- Results: success gives `{message_id: "fb:<mid>", ts: now}`; a rejection is `send_failed` "facebook rejected the message" (the server text is never returned or logged); no answer in 30 s or an error after the write started is `send_unknown`. Nothing is retried by the sidecar. Text is capped at 20000 characters.
- The connector's trailing mark-read task after a send is not copied: sending does not mark the thread read. Use `mark_read`.
- Encrypted sends use `cli.GenerateMessageID()` (a Facebook-style numeric id), time out after 30 s and write the sent message into `e2ee.db` with `is_me` true, because the server does not echo it back to this device. No `message` event is emitted for the send.
- Encrypted images carry thumbnail dimensions only, as the connector does: the decoded size clamped to 400 by 400, or 400 by 400 when the size is unknown. Odd PNG color models are re-encoded as NRGBA. GIFs go out as GIF-playback videos. Captions are dropped because Messenger Web does not render captions in encrypted chats.
- Attachments of kind `link` and `location` are not downloadable. Downloads stream into a temp file in the destination directory (0600), cap at 100 MiB, hash while writing, fsync and rename. Errors never contain the CDN URL.

## 6. Events

| Event | When |
|---|---|
| `connection` | connecting, connected, disconnected (transient or requested), fatal (rate limit, permanent error). |
| `logged_out`, `state_updated` | A fail-closed verdict; `state` null when the stored session must be forgotten. Also when cookies rotate or E2EE registers. |
| `message` | Live `LSInsertMessage` rows and live E2EE messages, `direction` out for our own account's other devices and for echoes of our unencrypted sends. |
| `history` | Initial-page, backfill and upserted messages (anything not a live insert), unless a `fetch_messages` collector is consuming them. |
| `reaction`, `receipt` | Live reaction and read-receipt rows, E2EE reactions and receipts. |
| `thread` | A thread appears or its name, members, preview or read state changes. |
| `error` | Non-fatal: `e2ee_unavailable`, `e2ee_logged_out`, `e2ee_cat_refresh`. |

`logged_in` is emitted by the server after `login_finish`; `qr` and the pairing events of the shared protocol are not used by this sidecar. A `FetchMessagesTask` without a cursor uses the current time with an empty reference id; this is not verified against the live server.

## 7. Errors

Codes are the shared protocol codes. Messages are fixed text: never a cookie, token, key, number, url or message body.
- Gates: `not_logged_in` when the session was closed or failed closed; `not_connected` when the Messenger socket or the encrypted client is down or cannot send within 15 s; `thread_not_found`, `message_not_found`, `bad_params` (including "unknown attachment" and "file too large for messenger"); `unsupported`; `attachment_expired` when a refreshed link still answers 403 or 410; `io` for local file errors, "attachment exceeds 100 MiB" and failed downloads.
- Fail-closed (the `logged_out` event carries the reason, `url` when Meta gave one, and the op returns the matching code): `token_invalidated`, `user_id_is_zero`, `dgw_unauthorized`, `account_disabled` (login_failed, state cleared); `checkpoint_required`, `challenge_required`, `consent_required`, `account_suspended` (state kept, the user acts on facebook.com); `account_mismatch` when the cookies belong to another account. The same verdicts apply when E2EE registration or connect fails. After any of these the session refuses ops with `not_logged_in` until a new login. `rate_limited` is a `connection` fatal with action `wait`, not a logout.
- Sends: `send_failed` (rejected, or the upload failed), `send_unknown` (outcome unclear after the write), `rate_limited` and the fail-closed codes when the library says so. Encrypted: `ErrServerReturnedError` is `send_failed`, a timeout or any unclassified error is `send_unknown`.
- A library `ErrPleaseReloadPage` requests a full reconnect, honouring the one hour minimum between full reconnects.

## 8. Not in v1

Encrypted `send_media` other than images; encrypted captions; encrypted history from before this device existed (the secure-history PIN restore is deliberately never called, see section 9); the pre-E2EE history of hybrid threads is not merged into the encrypted thread; sending edits or unsends; message requests (accepting or listing); group admin (rename, add, remove, leave); polls; contacts; view-once sends; mentions; disappearing-message timers on encrypted sends; community subthreads.

## 9. Risks

- Meta's terms forbid automated clients. Accounts using unofficial clients get flagged and sometimes locked (mautrix-meta issue #44). Expect checkpoints; use an account you can afford to lose.
- Protocol churn: Meta changes Lightspeed and the web app roughly monthly. After a Meta-side break the `mautrix-meta` and `whatsmeow` pins need a bump and a rebuild; sends and history are the first to fail.
- Fingerprint: the library always presents a Chrome on Linux user agent and matching client hints. Take the cookies from Chrome on Linux. `user_agent` in the login params is stored but not applied; a cookie set from another browser raises the odds of a checkpoint.
- The PIN restore step for secure history is skipped on purpose: reports link it to session revocations (mautrix-meta PR #425).
- No outbound rate limiter in the sidecar. The daemon must pace sends, reactions and read marks per account.
- Licensing: mautrix-meta is AGPL-3.0, so the combined binary is AGPL-3.0 and distributing it carries the AGPL source obligations. whatsmeow is MPL-2.0.
- `e2ee.db` and the cookies in `state` are credentials. Keep `data_dir` 0700 on an encrypted disk.
