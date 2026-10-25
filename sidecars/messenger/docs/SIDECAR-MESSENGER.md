# meta-messenger-sidecar

Reference for the Go sidecar that gives the meta-mcp daemon a personal Facebook Messenger account. Wire protocol: version 1 of the personal sidecar protocol (shared with the WhatsApp sidecar). The sidecar logs in with the owner's browser cookies, talks to Messenger through mautrix-meta's messagix library and reaches end-to-end encrypted threads through the whatsmeow client that messagix embeds.

## 1. What it is

The Rust daemon (`meta-mcp personal daemon`) spawns `meta-messenger-sidecar` as a child process, one per Messenger account. It speaks JSON lines on stdin and stdout (one object per line, at most 16 MiB), logs to stderr through a scrubber and listens on no port. Stdin closing ends it with status 0. The daemon sends `hello` first, then ops; the sidecar answers every request once and pushes events in between. One process is one account and one connection set: a Messenger socket plus, once registered, one encrypted-chat socket. A second sidecar on the same `data_dir` is refused in its first `hello` with `io` "another sidecar process holds this data_dir".

## 2. Module layout

Module `github.com/matpb/meta-mcp-rs/sidecars/messenger`.

| Path | Role |
|---|---|
| `main.go` | Entry point: Info-level zerolog through `logscrub`, runs `server.Run` with `engine.NewFactory`. Exit 0 clean, 2 frame violation, 1 I/O error. |
| `ipc/` | JSON-line framing and the reply writer. |
| `logscrub/` | Stderr scrubber: an allowlist of top-level log fields (`level`, `time`, `message`, `op`, `code`, `reason`, `component`, `count`, `event`, `platform`, `thread`, `id`, `state`), every other field is dropped, and URLs, cookies, tokens, long key-like runs, plus-prefixed phone numbers, XML tags with attributes (a protocol node carries push names) and the quoted input of strconv, time, URL and JSON parse errors are cut out of the strings that stay; a JSON line that is malformed, cut off or over 1 MiB becomes one fixed marker line and the rest of an oversized line is discarded. |
| `api/` | Wire shapes, error codes, `State`, and the `Factory` and `Session` contract between server and engine. |
| `server/` | Request loop, op validation, login state machine, event sink, error replies, the `data_dir` lock (`lock.go`, with `lock_unix.go` and `lock_windows.go`). |
| `engine/` | The session. `session.go` (state, validate, logout), `client.go` (the `metaClient` seam over messagix), `factory.go` (Open, Wipe), `connect.go`, `backoff.go`, `queue.go` (connect loop and event queue), `classify.go` (library errors to failure verdicts), `cookies.go` (cookie rotation to `state_updated`), `events.go` (tables to events), `threads.go` (thread registry, `list_threads`), `history.go` (message cache, `fetch_messages`), `e2ee.go` (encrypted client), `send.go`, `sendwait.go` (sends, reactions, read, typing, OTID waiter), `media.go` (uploads, downloads, file writer). |
| `normalize/` | Pure conversion of Lightspeed tables and E2EE events into protocol shapes and ids. |
| `e2ee/` | `e2ee.db`: whatsmeow device store, the encrypted message cache, the account binding and the attachment index of unencrypted messages. |
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

`data_dir` is given by the daemon in `hello`. It must be absolute and existing and should be mode 0700 (on Unix anything else logs a warning; on Windows the mode is not checked and access follows the folder ACL). Files inside are created 0600.

| File | Content |
|---|---|
| `e2ee.db` | SQLite (WAL, `synchronous=FULL`, so an acked message survives power loss). whatsmeow device keys, Signal sessions and sender keys in the clear, plus the E2EE message cache as plaintext JSON (newest 2000 messages per thread kept) and the encrypted media download records. It also records the account (fbid) it belongs to and, for each unencrypted message with downloadable media, the thread, timestamp and attachment metadata including the signed CDN link (newest 10000 kept), so `download_attachment` works after a restart. Opening it for a different account wipes it first, while a file written before accounts were recorded is judged by the JID of its device and otherwise adopted. Treat it like a password and like the messages themselves. Deleted by `logout`. |
| `threads.json` | Hybrid thread map: Messenger thread key to the shared thread id and thread type, with the account it belongs to (a map of another account is ignored, one without an account is adopted). No message content. |
| `sidecar.lock` | Empty file, 0600, holding an exclusive lock for the life of the process: `flock` on Unix, `LockFileEx` on Windows. `logout` leaves it in place. |

`state` (opaque to the daemon, persisted from `state_updated`): `{"cookies": {"c_user": "...", "xs": "...", ...}, "user_agent": "<string>", "fbid": "<c_user>", "e2ee_registered": bool}`. Cookies rotate; the sidecar re-emits the blob when they change. `user_agent` is stored but not applied (section 9).

## 5. Ops and library calls

Thread ids are the decimal portal id. A thread counts as encrypted when it has a hybrid mapping to another key, sits in an `e2ee_cutover` folder, has `ThreadType` encrypted-over-WA, or has received an E2EE message; routing uses that flag. Message ids are `fb:<mid>` or `wa:<chat>:<sender>:<id>`; a wa id on an unencrypted thread, or the reverse, is `bad_params` "message id does not belong to this thread". Numeric ids are canonicalised before any lookup, so `0333` and `+333` are thread `333` and cannot dodge the encrypted flag. A thread id the registry does not list is `thread_not_found`, also when it is the id of a known contact, because it may be an encrypted chat that is not listed yet and a send must never fall back to plaintext. The one exception is `send_text` and `send_media` to a numeric id that is vetted as a messageable person: the sidecar then registers and creates an encrypted DM first (see "New chats" below). Encrypted edits and unsends are applied only when the authenticated sender of the event wrote the target message (an unsend may also come from a group admin the sidecar knows from the group info and from promote, demote and leave events); anything else is ignored and logged at info with ids only. Encrypted messages are stored under chat, sender and id, so a peer reusing an id lands on its own row and a redelivery never overwrites a stored message.

| Op | Unencrypted (messagix) | Encrypted (whatsmeow) |
|---|---|---|
| `hello`, `ping`, `shutdown` | No library call; `hello` builds the session without network access. | same |
| `login_start`, `login_finish`, `login_cancel` | Cookie login: `LoadMessagesPage` once to check the account. | none |
| `connect`, `disconnect` | `Connect` the realtime socket in a loop with backoff; after the first connect `RegisterE2EE` (once) and `PrepareE2EEClient` + `Connect`. | same loop |
| `logout` | Drops cookies, then deletes `threads.json`, `e2ee.db` with its journal files and the caches through the factory, with or without a live session; a failed deletion answers `io` after the daemon state was cleared, and `login_start` answers `bad_params` "logout in progress" until it is done; it first waits (at most 20 s) for a session dropped after a fail-closed verdict to finish closing. The daemon calls it with a 60 s limit, stops the sidecar afterwards and empties `data_dir` itself (the files above and `sidecar.lock` included), so a deletion that failed here is retried by the daemon's wipe. | same |
| `list_threads` | `FetchMoreThreads` on sync groups 1 (and 95 when E2EE is active) when the registry is short. | threads join as messages arrive |
| `search_users` | One `SearchUserTask` (`SurfaceType` 5, the request the connector's `SearchUsers` sends) through `ExecuteTasks`, 20 s limit. Params: `query` (1 to 200 characters, required), `limit` (1 to 50, default 10). Answers `{"users":[{"id","name","has_thread"}]}`: only Facebook people (search types contact and non-contact) whose one-to-one result says the account can message them (`CanViewerMessage`, or a user id with no block), never the account itself, deduplicated. `has_thread` is true when the id is already in the thread list. Refused like a write after a fatal signal. | none |
| `fetch_messages` | Cache, else `FetchMessagesTask` with a collector (30 s, at most 10 requests per call; at the cap it returns what it has with `has_more` true, or `io` when nothing arrived). | Answered from `e2ee.db`; `has_more` false past the stored range. |
| `download_attachment` | Cached message or the persistent attachment index, then `mediadl.DownloadMedia` of the CDN URL; on 403, 410 or an `expires_at` within 60 s, one `FetchMessagesTask` page refreshes the URL. | `e2ee.db` media record, then `DownloadFBToFile` into a temp file. |
| `send_text` | `SendMessageTask` (TEXT) through `ExecuteTasks`, then wait for the OTID outcome. To a numeric id absent from the thread list: see "New chats"; the send then goes the encrypted way. | `SendFBMessage` with a `ConsumerApplication` text, quoted reply key from the wa id. |
| `send_media` | `SendMercuryUploadRequest`, then `SendMessageTask` (MEDIA, `AttachmentFBIds`, caption as text). Caps: image 8 MB, other 25 MB. | Images only: `Upload`, image transport, `SendFBMessage`. Cap 100 MB. |
| `send_reaction` | `SendReactionTask` (empty emoji removes). | `SendFBMessage` with a reaction `ConsumerApplication`. |
| `mark_read` | `ThreadMarkReadTask` with `up_to_ts_ms` or now. | `MarkRead` for the newest 100 stored incoming messages up to the watermark, grouped by sender. |
| `set_typing` | `ExecuteStatelessTask(UpdatePresenceTask)`. | `SendChatPresence` composing or paused. Global presence is never changed. |

Send details:
- OTID: every unencrypted send gets `methods.GenerateEpochID()` and registers a waiter first. The outcome comes from the `ExecuteTasks` response table or the socket (`LSReplaceOptimsiticMessage` success, `LSMarkOptimisticMessageFailed` or `LSHandleFailedTask` failure), whichever is first, within one 30 s budget that starts when `ExecuteTasks` is called.
- Results: success gives `{message_id: "fb:<mid>", ts: now}`; a rejection is `send_failed` "facebook rejected the message" (the server text is never returned or logged); no answer in 30 s or an error after the write started is `send_unknown`. Nothing is retried by the sidecar. Text is capped at 20000 characters.
- The connector's trailing mark-read task after a send is not copied: sending does not mark the thread read. Use `mark_read`.
- Encrypted sends use `cli.GenerateMessageID()` (a Facebook-style numeric id), time out after 30 s and write the sent message into `e2ee.db` with `is_me` true, because the server does not echo it back to this device. No `message` event is emitted for the send.

New chats:
- `send_text` and `send_media` to a numeric id that is not in the thread list vet the id first. It passes when the id is a person that a `search_users` call returned in the last hour of this session, or else when a fresh `SearchUserTask` with the id as the query returns that same id as messageable (same filter as `search_users`).
- A vetted id is registered as an encrypted DM (`ENCRYPTED_OVER_WA_ONE_TO_ONE`, chat `<id>@msgr`) before anything goes out, and then created the way the mautrix-meta connector's `ResolveIdentifier` does it: `CreateThreadTask`, then `CreateWhatsAppThreadTask` and the follow-up tasks it returns. Like the connector, only an account failure stops the send; other errors in these steps are logged. The send then goes through the encrypted path, so a new DM is never sent as plaintext.
- The encrypted client must be connected, otherwise the answer is `not_connected` and nothing is created.
- An id that is not a messageable person is `thread_not_found` "thread not found; this id is not a person you can message"; nothing is created or sent.
- Still `thread_not_found` with no network call: a non-numeric id, a `reply_to` on an unknown thread, and `send_reaction`, `mark_read` and `set_typing` on an unknown thread. An unknown group id is numeric, so it is searched and refused like any id that is not a person.
- Encrypted images carry thumbnail dimensions only, as the connector does: the decoded size clamped to 400 by 400, or 400 by 400 when the size is unknown. Odd PNG color models are re-encoded as NRGBA. GIFs go out as GIF-playback videos. Captions are dropped because Messenger Web does not render captions in encrypted chats.
- Attachments of kind `link` and `location` are not downloadable. Downloads stream into a temp file in the destination directory (0600), cap at 100 MiB while streaming (also on the encrypted path, where the capped file hides `os.File.ReadFrom` so `io.Copy` cannot skip the check), refuse up front when the advertised size is over the cap, hash while writing, fsync and rename. A `dest_path` that is a symlink, or sits directly in a symlinked directory, is refused with `bad_params` before the download starts and again before the rename. After a restart the thread need not be listed yet when a stored message of that thread carries the attachment. Errors never contain the CDN URL.

## 6. Events

| Event | When |
|---|---|
| `connection` | connecting, connected, disconnected (transient or requested), fatal (rate limit, permanent error, `repeated_failures` with action `relogin` after six page loads in a row that failed without a verdict, and the account signals `temporary_ban`, `stream_replaced`, `client_outdated` and `device_removed` from the encrypted client, which also block sending). |
| `logged_out`, `state_updated` | A fail-closed verdict; `state` null when the stored session must be forgotten. Also when cookies rotate or E2EE registers. |
| `message` | Live `LSInsertMessage` rows and live E2EE messages, `direction` out for our own account's other devices and for echoes of our unencrypted sends. |
| `history` | Initial-page, backfill and upserted messages (anything not a live insert), unless a `fetch_messages` collector is consuming them; one thread arrives in events of at most 200 messages and about 4 MiB of JSON, with `complete` only on the last. |
| `reaction`, `receipt` | Live reaction and read-receipt rows, E2EE reactions and receipts. |
| `thread` | A thread appears or its name, members, preview or read state changes. |
| `error` | Non-fatal: `e2ee_unavailable`, `e2ee_logged_out`, `e2ee_cat_refresh`, and `internal` when a panic in an event handler or connect cycle was contained (the encrypted handler then does not ack, so the message is redelivered). |

`logged_in` is emitted by the server after `login_finish`; `qr` and the pairing events of the shared protocol are not used by this sidecar. A `FetchMessagesTask` without a cursor uses the current time with an empty reference id; this is not verified against the live server.

## 7. Errors

Codes are the shared protocol codes. Messages are fixed text: never a cookie, token, key, number, url or message body.
- Request rules, identical in the WhatsApp sidecar: `ping` and `shutdown` are answered before `hello`; any other op before `hello` is `not_initialised`; a second `hello` is `bad_params` "already initialised"; an unknown op is `unsupported` "no such op"; a panic inside an op is `internal` "internal error".
- Gates: `not_logged_in` when the session was closed or failed closed; `not_connected` when the Messenger socket or the encrypted client is down or cannot send within 15 s; `thread_not_found`, `message_not_found`, `bad_params` (including "unknown attachment" and "file too large for messenger"); `unsupported`; `attachment_expired` when a refreshed link still answers 403 or 410; `io` for local file errors, "attachment exceeds 100 MiB", failed downloads, a failed deletion on `logout` and "another sidecar process holds this data_dir".
- Fail-closed (the `logged_out` event carries the reason, `url` when Meta gave one, and the op returns the matching code): `token_invalidated` (also for Meta error 1357001, "please log in"), `user_id_is_zero`, `dgw_unauthorized`, `account_disabled` (login_failed, state cleared); `checkpoint_required`, `challenge_required`, `consent_required`, `account_suspended` (state kept, the user acts on facebook.com); `account_mismatch` when the cookies belong to another account. The same verdicts apply when E2EE registration or connect fails. After any of these the session refuses ops with `not_logged_in` until a new login. `rate_limited` is a `connection` fatal with action `wait`, not a logout.
- `search_users`: `bad_params` (`query` or `limit` out of range), `not_connected`, `not_logged_in`, and `io` "user search failed" when the search request fails. The same `io` and gates apply to the vetting search of a send to a new chat.
- Sends: `send_failed` (rejected, or the upload failed), `send_unknown` (outcome unclear after the write), `rate_limited` and the fail-closed codes when the library says so. Encrypted: `ErrServerReturnedError` is `send_failed`, a timeout or any unclassified error is `send_unknown`.
- A library `ErrPleaseReloadPage` requests a full reconnect, honouring the one hour minimum between full reconnects.
- A fatal signal from the encrypted client (`TemporaryBan`, `StreamReplaced`, `ClientOutdated`, or a removed device) emits one `connection` fatal with the reason, stops that client for good and blocks sending until the process is restarted: `connect`, `search_users`, `send_text`, `send_media`, `send_reaction`, `mark_read` and `set_typing` answer `not_connected` "sending is blocked (reason)". Reads, and downloads of attachments of unencrypted messages, keep working; a download of an attachment of an encrypted thread needs the encrypted client, which is stopped, so it answers `not_connected` "not connected" until the process is restarted. No further connecting or connected event is sent, so the daemon keeps the fatal state it was given.

## 8. Not in v1

Encrypted `send_media` other than images; encrypted captions; encrypted history from before this device existed (the secure-history PIN restore is deliberately never called, see section 9); the pre-E2EE history of hybrid threads is not merged into the encrypted thread; sending edits or unsends; starting a conversation with anything but a person that Facebook's user search returns as messageable (groups, pages and Instagram accounts are out); message requests (accepting or listing); group admin (rename, add, remove, leave); polls; contacts; view-once sends; mentions; disappearing-message timers on encrypted sends; community subthreads.

## 9. Risks

- Meta's terms forbid automated clients. Accounts using unofficial clients get flagged and sometimes locked (mautrix-meta issue #44). Expect checkpoints; use an account you can afford to lose.
- Protocol churn: Meta changes Lightspeed and the web app roughly monthly. After a Meta-side break the `mautrix-meta` and `whatsmeow` pins need a bump and a rebuild; sends and history are the first to fail.
- Fingerprint: the library always presents a Chrome on Linux user agent and matching client hints. Take the cookies from Chrome on Linux. `user_agent` in the login params is stored but not applied; a cookie set from another browser raises the odds of a checkpoint.
- The PIN restore step for secure history is skipped on purpose: reports link it to session revocations (mautrix-meta PR #425).
- First contact with someone who has not messaged the account is the pattern Meta's anti-spam watches most. The sidecar now lets `send_text` and `send_media` open that kind of chat, so keep it to people who expect the message. The daemon's send audit log and the "confirm intent first" tool descriptions are unchanged. The id check relies on Facebook's search answer: an id it does not return as messageable is refused, and one it does return is trusted.
- No outbound rate limiter in the sidecar. The daemon paces writes per account with a token bucket (new messages: one token per 2 s, burst 3; reactions, read marks and typing: one per second, burst 5; `META_MCP_PERSONAL_SEND_INTERVAL_MS` and `META_MCP_PERSONAL_SEND_BURST` tune the first) and refuses a call without a token with `rate_limited` before it reaches the sidecar. The sidecar itself keeps no policy beyond its gates. No source shows what rate Meta tolerates.
- Licensing: mautrix-meta is AGPL-3.0, so the combined binary is AGPL-3.0 and distributing it carries the AGPL source obligations. whatsmeow is MPL-2.0.
- `e2ee.db` and the cookies in `state` are credentials. Keep `data_dir` 0700 on an encrypted disk.
- The library calls `os.Exit(1)` on a malformed Lightspeed step (`lightspeed/decode.go`), which no `recover` can catch, so a Meta format change can still end the process with status 1, the same as the sidecar's own I/O error; a panic in a library goroutine ends it with status 2, the same as a frame violation. The daemon restarts the sidecar in both cases.
