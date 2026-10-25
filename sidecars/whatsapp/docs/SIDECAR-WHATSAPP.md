# meta-whatsapp-sidecar

Operator and developer reference for the Go sidecar that gives the meta-mcp daemon a personal WhatsApp account. Wire protocol: version 1 of the personal sidecar protocol (shared with the Messenger sidecar) plus its Amendment 1 (WhatsApp pairing details). This document describes how the sidecar maps that protocol onto whatsmeow, how to pair it, what it refuses to do and what the owner is risking by running it.

## 1. What it is

`meta-whatsapp-sidecar` is a Go program that the Rust daemon of meta-mcp (`meta-mcp personal daemon`) spawns as a child process, one process per WhatsApp account. It speaks JSON lines on stdin and stdout, logs to stderr and listens on no network port. Through whatsmeow it registers itself as a linked companion device on the owner's own personal WhatsApp account (the same mechanism as WhatsApp Web), so the owner's phone stays the primary device. It is an unofficial client: see section 6 before pairing. Pinned dependencies:

| Dependency | Version | Licence |
|---|---|---|
| `go.mau.fi/whatsmeow` | `v0.0.0-20261017175727-b2b1e71d45e2` (no tags exist, so the pin is a pseudo-version) | MPL-2.0 |
| `modernc.org/sqlite` | `v1.61.0` (pure Go driver, name `sqlite`) | BSD-3-Clause |
| Go | `go 1.26.0` in go.mod, built with the local go1.26.10 | |

The build is `CGO_ENABLED=0`, so the binary is static and needs no C compiler. `build.sh` is the only supported build and runs, in order: `go vet ./...`, `go test -count=1 ./...`, `go build -trimpath` into `bin/meta-whatsapp-sidecar` with symbols kept, because `govulncheck -mode=binary` on a stripped binary flags whole modules (it fails when `file` does not report "statically linked"), `govulncheck ./...` on the source and `govulncheck -mode=binary` on the binary. Any step that fails fails the build. The sidecar starts with an empty environment (nothing from the daemon's environment reaches it) and reads no file outside `data_dir` except the `path` of a `send_media` request, and writes none outside it except the `dest_path` of a `download_attachment` request.

## 2. Module layout

Module `github.com/matpb/meta-mcp-rs/sidecars/whatsapp`.

| Path | Role |
|---|---|
| `main.go` | Entry point. Sets `syscall.Umask(0o077)`, hard-codes the zerolog Info level, wires `wa.Open` into `server.Run`. `main_test.go` spawns the real binary over pipes for hello, ping and shutdown, for the op rules before and after hello, and to check that stderr carries only allowlisted fields. |
| `ipc/` | JSON-line framing, copied from the sms-mcp-rs sidecar: one object per line, at most 16 MiB (16777216 bytes) per line including the newline, blank lines ignored, a malformed or oversized line exits 2. |
| `logscrub/` | Scrubber on the stderr log path. An allowlist of top-level fields (`level`, `time`, `message`, `op`, `code`, `reason`, `component`, `count`, `event`, `platform`, `thread`, `id`, `state`) that drops every other field, nested values and library fields included, plus WhatsApp value redaction (JIDs, phone numbers, QR payloads, pair codes) and strconv, URL and time parse errors reduced to their kind. The code never logs secrets in the first place; the scrubber is the second line of defence. |
| `api/` | Protocol shapes (Thread, Message, Attachment, events, `State`) and the error codes, including `sending_blocked`. |
| `store/` | `messages.db`: the sidecar's own chats, messages (keyed by chat, sender and id), reactions, receipts, contact names and LID cache. whatsmeow keeps no messages, so this is the only message history there is. |
| `normalize/` | Converts `waE2E.Message` into protocol shapes: `Classify` (message, edit, revoke, reaction, encrypted variants, ignore), `Extract` (kind, text, reply, attachments, preview), `Filtered` (status, broadcast, newsletters, bots), `Canonical` (phone JID vs LID), `Batches` and `BatchesSized` (history batches of at most 200 messages and about 4 MiB serialised). |
| `wa/` | The `Client` interface, the `Real` adapter (thin pass-throughs to `*whatsmeow.Client`; its event trampoline turns a handler panic into a failed handler), `wa.Open` (the Factory: lock file, `session.db`, sqlstore upgrade, `Scrub`) and the web version refresh. |
| `server/` | Request loop, one handler per op, the login state machine, event ingest, history sync, sends, media, and `download.go` (the capped temp file and the checked destination of downloads). |
| `docs/` | This file. |
| `build.sh` | The build and verification script described above. |

Files under `data_dir` (given by the daemon in `hello`; it must be an absolute path to an existing directory, mode 0700 expected, a laxer mode only logs a warning). The process umask is 077 and every file is chmod 0600:

| File | Content | Handling |
|---|---|---|
| `session.db` | whatsmeow's SQL store: device identity, Signal sessions and sender keys, pre-keys, app state keys, contacts, LID map. Private keys are stored in the clear, and the file runs with `secure_delete` on so deleted rows are zeroed. | Treat it like a password. Whoever copies it can act as the linked device until it is unlinked on the phone. Never commit it, back it up only to encrypted storage, keep the disk encrypted. |
| `messages.db` | The sidecar's own store (schema version 2 by `PRAGMA user_version`; a version 1 file is migrated in place on open, keeping every row): chats, messages (with the raw protobuf of each), reactions, receipts, contact names, meta keys `paired_at`, `full_history`, `owner_jid`. Message text is in the clear. | Same care as the message content itself. Cleared by `logout` and when a different account is paired; the clearing runs with `secure_delete`, then a WAL checkpoint and a `VACUUM`, so no deleted text stays in the file. |
| `sidecar.lock` | Empty file holding an exclusive non-blocking `flock` for the life of the process. | Not data. A second process on the same `data_dir` fails to start with `io` ("another sidecar process holds this data_dir"). |

SQLite runs in WAL mode, so `-wal` and `-shm` files appear beside each database and share its protection. Both databases use `foreign_keys(1)`, `busy_timeout(5000)` and a single connection.

## 3. Op to library mapping

Replies are `{"id","ok":true,"result"}` or `{"id","ok":false,"error":{"code","message"}}`. `hello` must be first: only `ping` and `shutdown` are served before it, any other op fails with `not_initialised`, and a second `hello` fails with `bad_params` ("already initialised"). An op that is not in the table below fails with `unsupported` ("no such op"). Error messages never contain keys, numbers or message bodies. Sends are gated in this order: `sending_blocked`, `not_logged_in`, `not_connected`, `thread_not_found`, `bad_params`.

### 3.1 Ops

| Op | Params | Behaviour and whatsmeow calls |
|---|---|---|
| `hello` | `protocol` 1, `platform` "whatsapp", `data_dir`, `state` | `protocol_mismatch` for another protocol or platform, `bad_params` for a bad `data_dir` or a second hello. The first hello opens `session.db` (`sqlstore.NewWithDB` then `Upgrade`, `GetFirstDevice`), `messages.db` and builds the client with `whatsmeow.NewClient`. No network access. Result carries `library {name "whatsmeow", version from build info}`, `go`, `logged_in` (`HasDevice()`), `account {id, name}` or null. A non-null `state` with no device on disk makes the sidecar emit `state_updated {"state": null}` after the reply. |
| `ping` | null | `{"pong": true}`, also before `hello`. No library call. |
| `shutdown` | null | Replies `{}`, drains in-flight ops, closes both databases, releases the lock, exits 0. It never calls `Logout`, so the device stays linked. Closing stdin does the same with status 0. |
| `login_start` `qr` | `method` "qr", optional `full_history`, `device_name` | `GetQRChannel(loginCtx)` then `Connect()`; replies `{}` right after `Connect`, codes follow as `qr` events (section 4). `already_logged_in` when a device exists. |
| `login_start` `pair_code` | `method` "pair_code", `phone` (7 to 15 digits, no leading 0), optional `full_history`, `device_name` | `SetPrePairCallback`, `GetQRChannel`, `Connect`, then on the first channel code `PairPhone(ctx, phone, true, whatsmeow.PairClientChrome, "Chrome (Linux)")`. Replies `{"code": "XXXX-XXXX"}`. `login_failed` when `PairPhone` errors or no first code arrives within 30 s (`rate_limited` for `ErrIQRateOverLimit`). |
| `login_finish` `passkey_response` | `{"passkey_response": <WebAuthn assertion JSON>}` | Unmarshals into `types.WebAuthnResponse` (bad JSON is `bad_params`), then `SendPasskeyResponse`. |
| `login_finish` `confirm` | `{"confirm": true}` | `SendPasskeyConfirmation`. Either form without a matching pending step is `bad_params`. |
| `login_cancel` | null | Cancels the login context, `Disconnect()`, swaps in a fresh unpaired client. `{}`. Once the phone has accepted the pairing it is ignored and setup finishes. |
| `logout` | null | When connected `Logout(ctx)` (sends `remove-companion-device`). When not connected or `Logout` fails: `Disconnect()` then `Store.Delete` (the Real adapter's `DeleteDevice`). Then `store.Reset` and the factory `Scrub` of `session.db` (it deletes `whatsmeow_lid_map` and `whatsmeow_privacy_tokens`, which no device row owns, then checkpoints the WAL and runs `VACUUM`), `state_updated {"state": null}`, fresh client. `{}`, or `io` ("could not wipe the session database") when the wipe of `session.db` failed after the unlink went through. This unlinks the device; it is never called on a normal stop. Only a connected client sends `remove-companion-device`: otherwise the device is deleted from the local store only, the reply is still `{}` and WhatsApp is not told (the device then stays in the phone's Linked devices list until you remove it there). When no sidecar runs, `meta-mcp personal logout` makes the daemon start this one without connecting it, so that logout is local only; when the sidecar already runs and is connected, the unlink goes out. The daemon stops the sidecar afterwards and empties `data_dir` itself. |
| `connect` | null | `not_logged_in` without a device; `not_connected` naming the reason while a fatal signal blocks the account (section 3.3); `{}` when already connected. Otherwise one `whatsmeow.GetLatestVersion` (5 s timeout, applied with `store.SetWAVersion`, failure is not an error), `connection {connecting}`, `Connect()`. A `Connect` error replies `io` and emits `connection {disconnected, reason "connect_error", action "wait"}`. Auto reconnect is on (`EnableAutoReconnect`, `InitialAutoReconnect`). |
| `disconnect` | null | `Disconnect()`, `connection {disconnected, reason "requested", action "none"}`, `{}`. |
| `list_threads` | `limit` 1..200, `cursor` | Served from `messages.db` (keyset cursor, newest activity first). No library call. Bad cursor is `bad_params`. |
| `fetch_messages` | `thread_id`, `limit` 1..100, `before_ts_ms`, `before_id` | Store first. When fewer than `limit` rows came back, the client is connected, the chat has an oldest stored message and its end-of-history marker is not `COMPLETE_AND_NO_MORE_MESSAGE_REMAIN_ON_PRIMARY`, it sends one on-demand request: `GenerateMessageID`, `BuildHistorySyncRequest(anchor, 50)` where the anchor is the oldest stored message, then `SendMessage(ctx, ownJID.ToNonAD(), req, SendRequestExtra{ID, Peer: true})`. One request in flight per chat, wait up to 30 s, match the answer to the request by `Notification.GetPeerDataRequestSessionID()` on the `ON_DEMAND` `HistorySync` (an answer that names another session never finishes the wait, only one without a session id matches by chat), then re-query the store. On timeout it answers with what the store holds. Unknown thread is `thread_not_found`. |
| `download_attachment` | `thread_id`, `message_id`, `attachment_id` ("0"), `dest_path` | Re-reads the stored protobuf and calls `DownloadToFile` on the Image, Video, Audio, Document or Sticker sub-message, streaming into an exclusive 0600 temp file next to `dest_path` that a counting wrapper stops at 100 MiB (plus 32 bytes of slack for the padding and MAC the encrypted stream carries). An advertised `FileLength` over 100 MiB is refused up front; both give `io` ("attachment exceeds 100 MiB"). The sha256 is taken over the decrypted temp file, which is then renamed over `dest_path`, so a failed download leaves no file, an existing file is replaced and never written through, and a hard link keeps its other name. A `dest_path` that is a symlink, a directory, or sits in a symlinked directory is `bad_params`; writes go through the open parent directory. On `ErrMediaDownloadFailedWith404`, `410` or `403`: `SendMediaRetryReceipt`, wait up to 20 s for `events.MediaRetry`, `DecryptMediaRetry` (`whatsmeow.DecryptMediaRetryNotification`), retry the download once; otherwise `attachment_expired`. Returns `{path, mime, bytes, sha256}`. `bad_params` for a relative path or missing parent, `not_connected` when offline. |
| `send_text` | `thread_id`, `text` (1..65536 bytes), `reply_to` | `GenerateMessageID`, then `SendMessage(ctx, chat, msg, SendRequestExtra{ID})`. Plain text is `Conversation`; a reply is `ExtendedTextMessage` with a `ContextInfo` (StanzaID, Participant, QuotedMessage) built from the stored target (`message_not_found` when absent). Records the message in the store and emits `message {direction "out"}`. Result `{message_id, ts}`. |
| `send_media` | `thread_id`, `path`, `mime`, `kind`, `caption`, `filename` | Absolute regular file of at most 100 MiB. `Upload(ctx, bytes, MediaImage / MediaVideo / MediaAudio / MediaDocument)`, then an `ImageMessage`, `VideoMessage`, `AudioMessage` or `DocumentMessage` from the upload response, then the same send path as text. Images get width, height and a JPEG thumbnail (longest side 100 px). Audio is a voice note (`PTT`) when the mime is `audio/ogg` with `codecs=opus`. Documents carry `FileName` and `Title`. |
| `send_reaction` | `thread_id`, `message_id`, `emoji` ("" removes) | `BuildReaction(chat, sender, id, emoji)` then `SendMessage`; `sender` is the target's author (own JID for own messages). `{}`. |
| `mark_read` | `thread_id`, `up_to_ts_ms` | `MarkRead(ids, now, chat, sender)` for the stored unread incoming messages, grouped by sender (sender is empty in DMs) and sent 100 ids at a time. Unread means after the chat's read mark in (timestamp, id) order, so messages that share one second are all reached and a message in the same second as the mark with a larger id is still unread; the walk pages with a (timestamp, id) cursor, 100 rows at a time, until it runs out. Then the mark moves to the newest message sent, the unread count is zeroed and `thread` is emitted. Nothing unread is `{}`. This is the only place a read receipt (blue ticks) is ever sent. |
| `set_typing` | `thread_id`, `on` | `SendChatPresence(chat, composing or paused, "")`. `{}`. |
| anything else | | `unsupported` ("no such op"). |

Send error mapping: `ErrServerReturnedError` with a message ending in " 463" turns into the fatal `reachout_timelock` below; `ErrMessageTimedOut`, a context deadline, a `DisconnectedError` while waiting for the ack, or `ErrNotConnected` from a message send (it only comes after the frame was written) is `send_unknown` (the message carries the message id, the outcome is uncertain, do not blindly resend); `ErrNotConnected` from receipts and typing is `not_connected`; `ErrNotLoggedIn` is `not_logged_in`; anything else is `send_failed`.

### 3.2 Events

| Event | Produced by |
|---|---|
| `ready` | Process start, before `hello`: `{"pid", "protocol": 1}`. |
| `connection` | `events.Connected` (also sets presence unavailable first), `events.Disconnected` (reason "network", action "wait"), `events.KeepAliveTimeout` (reason "keepalive_timeout"), `events.KeepAliveRestored` (back to connected), non-fatal `events.ConnectFailure` (any reason but 409; reason "connect_failure_<code>", action "retry" because whatsmeow stops reconnecting; the daemon calls `connect`), the ops `connect`, `disconnect`, and the login channel (`timeout` gives reason "qr_timeout" with action "retry", other pairing errors give "pair_error" with action "retry"). Fatal states: see 3.3. |
| `qr` | QR channel item `code`: `{"code", "expires_in_s"}` (the item's `Timeout`). |
| `passkey_request` | QR channel item `passkey-request` (`events.PairPasskeyRequest`): `{"public_key", "instructions"}`, with the WebAuthn options exactly as the library gives them. |
| `passkey_confirmation` | QR channel item `passkey-confirmation` (`events.PairPasskeyConfirmation`): `{"code"}`, always set. When whatsmeow can skip the hand-off to the phone it confirms by itself and the channel carries no item, so no event is emitted in that case. |
| `logged_in` | QR channel item `success` (`events.PairSuccess`), then `events.Connected`, then `Client.PreKeysUploaded` done. Waits at most 120 s. |
| `logged_out` | `events.LoggedOut`: `{"reason": "banned" or "device_removed", ...}`. whatsmeow has already deleted the device; the sidecar also emits `state_updated {"state": null}`, wipes the leftovers of `session.db` once the device record is gone (the same `Scrub` as `logout`) and installs a fresh unpaired client. |
| `state_updated` | After a successful login (`State {jid, lid, push_name, paired_at, full_history}`, opaque to the daemon, which stores it and hands it back in `hello`), after `logout`, after `LoggedOut`, and after a `hello` whose state has no device behind it. `"state": null` means forget the session. |
| `message` | `events.Message` (unwrapped by whatsmeow): new message, decrypted secret-encrypted edit, edit (`ProtocolMessage` MESSAGE_EDIT), revoke (REVOKE, sets `unsent`); `direction` is "out" when `Info.IsFromMe` (sent from the phone or another device); also the echo of our own sends. An edit or revoke is applied and reported only when its sender wrote the target message, a revoke also when the sender is a known admin of the group (section 3.4); any other one is acknowledged, ignored and logged at info with ids only. A message whose id another sender already holds in the chat is ignored the same way. History messages are not emitted here, they go to `history`. |
| `reaction` | `ReactionMessage` or a decrypted `EncReactionMessage` inside `events.Message`; `emoji` is null when the reaction was removed. |
| `receipt` | `events.Receipt` of type delivered (empty type), read, played (reported as "read"). `read-self` (read on another device) is not emitted as a receipt; it zeroes the unread count and emits `thread` instead. Other receipt types are ignored. |
| `thread` | A chat created by an incoming message or history, a LID chat merged into its phone-number chat (the thread now carries the phone-number id and the LID as `alt_id`), a group info refresh (`GetGroupInfo`, at most daily per chat), `read-self` receipts, app state events (`events.Pin`, `Archive`, `Mute`, `MarkChatAsRead`, `DeleteChat`, `JoinedGroup`, `GroupInfo`), and `mark_read`. Contact and push name events update names without an event. |
| `history` | `events.HistorySync` (initial bootstrap, recent, full and `ON_DEMAND` types): `{thread_id, messages newest first, complete}` in batches of at most 200 messages and about 4 MiB serialised; `complete` is true on the last batch of a thread within one sync event. A batch that still cannot be written (one message over the 16 MiB frame limit) is skipped with an `error` event and the rest of the sync goes on; the messages stay in `messages.db`, and a thread whose last batch was skipped is closed with an empty `complete` batch. |
| `error` | Non-fatal problems: `login_failed` ("paired but setup did not finish; call connect", or a fixed text naming the pairing event that failed), `internal` for a panic that was recovered in an op, an event handler or a background goroutine (section 3.5), `io` for a skipped history batch. |

Ingest commits before it acknowledges. The client runs with `SynchronousAck`, `EnableDecryptedEventBuffer` and `AutomaticMessageRerequestFromPhone`; the handler returns success only after the store commit and the stdout write both succeeded, otherwise WhatsApp redelivers the message. Decryption failures are logged (no content) and skipped.

### 3.3 Fatal signals (Amendment 1)

A fatal signal emits `connection {"state": "fatal", "reason": R, "action": A}` and sets the sidecar's blocked reason. From then on `send_text`, `send_media`, `send_reaction`, `mark_read` and `set_typing` fail with `sending_blocked` naming the reason, and on-demand history requests and media retry receipts are no longer sent (fetch_messages answers from the store, download_attachment of expired media gives `attachment_expired`). Only a process restart clears it, a re-link included; the daemon must surface the reason to the owner and not restart blindly. While the block holds, `connect` is refused with `not_connected` naming the reason, and every fatal signal except the reach-out time-lock (which leaves the connection up) also ends the wish to be connected, so the socket closing afterwards is not reported as a retryable drop.

| whatsmeow signal | reason | action |
|---|---|---|
| `events.TemporaryBan` (codes 101 to 106) | `temporary_ban` | `wait` |
| `events.NotifyAccountReachoutTimelock` with `IsActive` | `reachout_timelock` | `wait` |
| Send ack error 463 (`ErrServerReturnedError`, message ends in " 463") | `reachout_timelock` | `wait` |
| `events.ClientOutdated` (connect failure 405), or `err-client-outdated` on the QR channel | `client_outdated` | `none` |
| `events.StreamReplaced`, or `events.ConnectFailure` with reason 409 (whatsmeow's `ConnectFailureBadUserAgent`, "client user agent was rejected"; whatsmeow stops reconnecting after it) | `stream_replaced` | `none` |
| `events.LoggedOut` with `Reason == events.ConnectFailureUnknownLogout` (406) | `banned` | `relogin` |
| `events.LoggedOut` with any other reason (401 `device_removed`, 403 main device gone) | `device_removed` | `relogin` |

For both `LoggedOut` rows the sidecar also emits `logged_out {reason}` and `state_updated {"state": null}`. Note that 403 is documented in the library as "seems to happen for both bans and switching phones", so `device_removed` is not proof of a non-ban.

### 3.4 Stored messages and who may change them

- Messages are keyed by (chat, sender, id) and (chat, id) is unique, because every op addresses a message by thread and id alone: the first sender to hold an id in a chat keeps it, and a later message with that id from another sender is ignored, with a log line carrying ids only.
- A redelivery from the same sender, in either of its id forms (phone number or LID), never changes the stored content, never counts as unread twice and never reverts an edit or a revoke.
- An edit changes a stored message only when its sender wrote it. A revoke needs the same, or a sender recorded as an admin of the group. Admins come from group info (`GetGroupInfo`, `JoinedGroup`) and from promote and demote events; a sender that is not known to be an admin is not one. This applies to live events and secret-encrypted edits, and to the revokes found in history sync.
- The chat's read mark is a (timestamp, id) pair; incoming messages after it are unread.
- A `messages.db` of schema version 1 (keyed by chat and id) is migrated to version 2 on open, in one transaction, keeping every row; a read mark gets the largest incoming id of its second as its id.

### 3.5 Logging and failures

- Log lines keep only the allowlisted top-level fields (section 2); the library's own fields (the whatsmeow adapter's `sublogger`, for one) never reach stderr, and parse errors lose the input they quote.
- A panic in an op, an event handler or a background goroutine is recovered and logged by its Go type only. An op answers `internal`, an event handler returns "not handled" so WhatsApp redelivers the event, and an `error` event with code `internal` tells the daemon; a panic that repeats on every redelivery of the same event keeps that event unacknowledged.

## 4. Pairing flows

Pairing happens once. Nothing here touches the network until `login_start`, and the sidecar refuses `login_start` when a device exists (`already_logged_in`; run `logout` first). One login runs at a time; a new `login_start` cancels the previous one.

`login_start` options (Amendment 1): `full_history` (default false) asks the phone for a full history sync, and `device_name` (default "meta-mcp") is the name shown in the phone's Linked devices list. Both are set on the device properties before the first connect of an unpaired device and travel in the pairing handshake, so they are honoured on a first pairing only and cannot be changed for an already linked device (to change them: `logout`, then pair again). With `full_history` the sidecar sets `RequireFullSync` and raises the history limits (3650 days, 10240 MB). How much history arrives is the phone's decision, and a full sync can take hours; keep the sidecar connected through the first session.

### 4.1 What the owner does on the phone

Summarised from WhatsApp's help pages (https://faq.whatsapp.com/1317564962315842 and https://faq.whatsapp.com/1324084875126592), as written in the research brief (section 2.4):

- QR: open WhatsApp on the phone, Linked devices, Link a device, verify identity if prompted (biometric or phone PIN), scan the QR. Android: tap the menu then Linked devices; iPhone: Settings tab then Linked devices.
- Code: tap the "Enter code to link new device" notification, check the displayed location and device name, tap Confirm, verify identity, type the 8-character code. Or Linked devices, Link a device, "Link with phone number instead", enter the code.
- If WhatsApp does not recognise the device it asks "Link device / Don't link".

The QR must be rendered by the daemon or its front end (the `code` is the whole string to encode; today it is a `https://wa.me/settings/linked_devices#...` URL). Treat the QR payload and the pair code as secrets: they are never logged.

### 4.2 QR flow

1. `login_start {"method": "qr"}` answers `{}` right after `Connect()`.
2. `qr` events follow, one per code: about six per connection, the first valid for about 60 s and the others about 20 s each (about 160 s in all). Each carries `expires_in_s`. Show the newest one.
3. The owner scans. whatsmeow gets `PairSuccess`, the server ends the stream with 515 and the library reconnects by itself. The sidecar then waits for `Connected` and for the pre-key upload to finish (up to 120 s), because stopping the process before the pre-keys are uploaded can lose the login.
4. `state_updated` then `logged_in`. `logged_in` is emitted only after `Connected` and the pre-key upload, never on `PairSuccess` alone.
5. If the codes run out, the sidecar emits `connection {"state":"disconnected","reason":"qr_timeout","action":"retry"}`, returns to idle and installs a fresh client. Call `login_start` again for new codes.

```
< {"event":"ready","data":{"pid":4242,"protocol":1}}
> {"id":1,"op":"hello","params":{"protocol":1,"platform":"whatsapp","data_dir":"/srv/meta-mcp/whatsapp","state":null}}
< {"id":1,"ok":true,"result":{"protocol":1,"sidecar_version":"0.1.0","library":{"name":"whatsmeow","version":"v0.0.0-20261017175727-b2b1e71d45e2"},"go":"go1.26.10","logged_in":false,"account":null}}
> {"id":2,"op":"login_start","params":{"method":"qr","full_history":false,"device_name":"meta-mcp"}}
< {"id":2,"ok":true,"result":{}}
< {"event":"qr","data":{"code":"https://wa.me/settings/linked_devices#FAKE1,FAKE2,FAKE3,FAKE4,1","expires_in_s":60}}
< {"event":"qr","data":{"code":"https://wa.me/settings/linked_devices#FAKE5,FAKE6,FAKE7,FAKE8,1","expires_in_s":20}}
< {"event":"state_updated","data":{"state":{"jid":"15555550100@s.whatsapp.net","lid":"100000000000001@lid","push_name":"Owner","paired_at":1793000000,"full_history":false}}}
< {"event":"logged_in","data":{"account":{"id":"15555550100@s.whatsapp.net","name":"Owner"}}}
> {"id":3,"op":"connect","params":null}
< {"event":"connection","data":{"state":"connecting"}}
< {"event":"connection","data":{"state":"connected"}}
< {"id":3,"ok":true,"result":{}}
```

QR timeout variant (nobody scanned):

```
< {"event":"qr","data":{"code":"https://wa.me/settings/linked_devices#FAKE9,FAKE10,FAKE11,FAKE12,1","expires_in_s":20}}
< {"event":"connection","data":{"state":"disconnected","reason":"qr_timeout","action":"retry"}}
> {"id":4,"op":"login_start","params":{"method":"qr"}}
< {"id":4,"ok":true,"result":{}}
```

### 4.3 Pair code flow

Use it when the owner cannot scan a QR on screen (phone and sidecar host are apart, for example).

1. `login_start {"method": "pair_code", "phone": "15555550100"}`. The phone is the owner's own number in international digits, 7 to 15 of them, without a leading 0 or a plus sign (`bad_params` otherwise).
2. The sidecar installs `PrePairCallback`: a pairing whose JID user part differs from the digits given is refused locally, so a code entered on the wrong account never completes.
3. It connects, waits for the first QR channel item (later codes are ignored), then calls `PairPhone` with `showPushNotification` true, client type Chrome and the display name "Chrome (Linux)". The phone receives the "Enter code to link new device" push notification.
4. `login_start` answers `{"code": "XXXX-XXXX"}` (8 characters). The owner types it on the phone. WhatsApp states no exact expiry, and the login websocket closes when the QR codes run out, so treat it as valid for about 160 s.
5. Success is the same as the QR flow: `state_updated`, then `logged_in` after `Connected` and the pre-key upload. A pair code login needs no `login_finish` unless WhatsApp asks for a passkey (4.4).
6. `PairPhone` failing, or no first code within 30 s, is `login_failed`; WhatsApp rate limiting code requests is `rate_limited`.

```
> {"id":2,"op":"login_start","params":{"method":"pair_code","phone":"15555550100","device_name":"meta-mcp"}}
< {"id":2,"ok":true,"result":{"code":"ABCD-1234"}}
< {"event":"state_updated","data":{"state":{"jid":"15555550100@s.whatsapp.net","lid":"100000000000001@lid","push_name":"Owner","paired_at":1793000000,"full_history":false}}}
< {"event":"logged_in","data":{"account":{"id":"15555550100@s.whatsapp.net","name":"Owner"}}}
```

Wrong number variant: the owner's phone completes the link but the JID does not match, the callback refuses, and the sidecar emits `error {"code":"login_failed", ...}` and `connection {"state":"disconnected","reason":"pair_error","action":"retry"}`.

### 4.4 Passkey step

WhatsApp states: "As an enhanced security measure, WhatsApp may require you to use a passkey when you link your primary phone to a device using WhatsApp Web." (https://faq.whatsapp.com/1011243695265795). It applies to some accounts only, for QR and pair code linking alike, so whether the owner's account is gated is only known at the first pairing. A headless process cannot produce the assertion: WebAuthn needs a user gesture on the `web.whatsapp.com` origin. The sidecar therefore hands the request to the owner and takes the assertion back.

1. The sidecar emits `passkey_request {"public_key": <options JSON>, "instructions": "..."}`. The phone typically shows "Continue on your other device".
2. The owner opens https://web.whatsapp.com in a Chrome-based browser (on Linux only Chrome-based browsers can generate the cross-device passkey QR; Firefox supports only local passkeys), opens the developer console and runs the snippet below with `<public_key>` replaced by the `public_key` JSON from the event (paste it as an object literal):

```js
console.log(JSON.stringify((await navigator.credentials.get({publicKey: PublicKeyCredential.parseRequestOptionsFromJSON(<public_key>)})).toJSON()))
```

3. The browser walks the owner through the passkey prompt (a QR to scan with the phone, or the local passkey). The console prints one line of JSON.
4. The daemon sends that line verbatim: `login_finish {"passkey_response": <that JSON>}`. The sidecar unmarshals it into `types.WebAuthnResponse` and calls `SendPasskeyResponse`. Invalid JSON is `bad_params` and the step stays pending.
5. The sidecar then emits `passkey_confirmation {"code": "..."}`. The `code` must be compared with the one on the phone, and the daemon continues with `login_finish {"confirm": true}`, which calls `SendPasskeyConfirmation`. When whatsmeow can skip the hand-off to the phone it confirms by itself and no `passkey_confirmation` event is emitted; a `confirm` then has nothing pending and is `bad_params`.
6. Pairing then completes as before: `state_updated`, then `logged_in` after `Connected` and the pre-key upload.

```
> {"id":2,"op":"login_start","params":{"method":"qr"}}
< {"id":2,"ok":true,"result":{}}
< {"event":"qr","data":{"code":"https://wa.me/settings/linked_devices#FAKE1,FAKE2,FAKE3,FAKE4,1","expires_in_s":60}}
< {"event":"passkey_request","data":{"public_key":{"challenge":"FAKECHALLENGE","rpId":"example.invalid","timeout":60000,"userVerification":"preferred"},"instructions":"Open https://web.whatsapp.com in a Chrome-based browser, open the developer console, run the snippet and pass the printed JSON as login_finish passkey_response."}}
> {"id":3,"op":"login_finish","params":{"passkey_response":{"id":"FAKEID","rawId":"FAKEID","type":"public-key","response":{"authenticatorData":"FAKE","clientDataJSON":"FAKE","signature":"FAKE","userHandle":"FAKE"},"clientExtensionResults":{}}}}
< {"id":3,"ok":true,"result":{}}
< {"event":"passkey_confirmation","data":{"code":"123456"}}
> {"id":4,"op":"login_finish","params":{"confirm":true}}
< {"id":4,"ok":true,"result":{}}
< {"event":"state_updated","data":{"state":{"jid":"15555550100@s.whatsapp.net","lid":"100000000000001@lid","push_name":"Owner","paired_at":1793000000,"full_history":false}}}
< {"event":"logged_in","data":{"account":{"id":"15555550100@s.whatsapp.net","name":"Owner"}}}
```

Pairing failures: `err-client-outdated` is fatal `client_outdated` (update the pin, section 7); any other `err-*` or `error` channel item emits `error {"code":"login_failed", ...}` and `connection {"state":"disconnected","reason":"pair_error","action":"retry"}`. If `PairSuccess` arrives but `Connected` and the pre-key upload do not finish in 120 s, the sidecar emits `error {"code":"login_failed","message":"paired but setup did not finish; call connect"}`; call `connect`.

## 5. Unsupported in v1

Everything below is out of scope. An op that is not in the table of section 3.1 returns `unsupported`.

- Starting chats with ids that are not phone numbers. `send_text` and `send_media` to a chat absent from the store accept a phone-number id (`<digits>@s.whatsapp.net`) after asking WhatsApp whether the number is registered (`IsOnWhatsApp`): a registered number gets its DM created and the message sent, an unregistered one is `thread_not_found` ("this number is not registered on WhatsApp"). The owner's own number skips the check. Group ids and LIDs absent from the store stay `thread_not_found`, and so do reactions, read marks and typing on any unknown chat. Section 6 explains the risk of first contact.
- Group management (create, add, remove, rename, admin changes). Group chats are readable and repliable only when they already exist.
- Polls (creation and votes), status updates, newsletters and channels, calls (call log entries are read as kind `call`, nothing is placed or answered).
- Edits and revokes from our side. Incoming edits and revokes are applied to the store (`edited`, `unsent`) only when their sender wrote the message, a revoke also when the sender is a known group admin (section 3.4).
- Presence subscription. No `SubscribePresence`, no typing indicators of others, no online status.
- Contact sync beyond what app state delivers. Names come from app state contacts, push names and history sync; there is no address book import and no number lookup (`IsOnWhatsApp` is not used).
- Read receipts sent automatically. Never. Blue ticks are sent only by an explicit `mark_read`.
- Video thumbnails and audio duration on send (images get a thumbnail, videos and audio do not get a thumbnail, duration or waveform).
- Multi-account. One process, one account, one `data_dir`.
- Contact cards, location sharing, stickers and link previews on send. Received ones are read (kinds `unsupported`, `location`, `sticker`, `link`).

## 6. Risks

Read this before the first pairing. Quotes are copied from WhatsApp's own help pages as rendered on 2026-10-24; the URL follows each quote.

**Unofficial clients violate the terms.** https://faq.whatsapp.com/1217634902127718 (unofficial apps):

> "Using these apps or linking your WhatsApp account to unofficial versions of WhatsApp violates our Terms of Service."

> "Your WhatsApp account might also be temporarily or permanently banned, or it could lead to restrictions on your account, including the ability to link devices."

whatsmeow is a re-implemented client, so this applies to the sidecar in full. The linked devices page (https://faq.whatsapp.com/378279804439436) makes the exposure retroactive:

> "Linking your account to an unofficial app or website, now or in the past, may result in a temporary or permanent account ban."

The ban page (https://faq.whatsapp.com/1064395290901991): "Using an unauthorized application and/or unsupported device violates our Terms of Service and can result in your account being banned." and "We can reinstate an account ban if the account activity continues to violate our Terms of Service."

**Bans hit the owner's whole account, not only the companion.** https://faq.whatsapp.com/465883178708358 (account bans): a banned number shows "This phone number can't use WhatsApp", and:

> "You can't access your chat history or backups while your account is banned."

A review can be requested in the app ("Most reviews are completed within 24 hours"); "If you don't see an option to request a review, it means the ban is final". The temporary ban page says the in-app message "Temporarily banned" can mean "you're using an unofficial version of WhatsApp or harvesting information, known as scraping", and that if you keep going "your account might be permanently banned" (https://faq.whatsapp.com/1848531392146538).

**Restrictions short of a ban.** https://faq.whatsapp.com/717472490411581 (restricted accounts):

> "you can't start chats or calls with new contacts, create new groups, add new members to groups, or use linked devices. You may also be logged out of some companion devices."

That is why v1 never starts chats (section 5) and why a restriction shows up first as a `logged_out` or a fatal `connection` event.

**Automated messaging.** https://faq.whatsapp.com/361005896189245 (use WhatsApp responsibly): "Don't bulk message, auto-message, or auto-dial using WhatsApp. WhatsApp uses both machine learning technology and reports from users to detect and ban accounts that send unwanted automated messages." Its best practice: "Communicate with known contacts: Only send messages to those who have contacted you first or have requested you contact them on WhatsApp." An assistant that drafts and sends replies sits near the "auto-message" wording, which is why every outbound message needs a human approval (section 7). Anecdotal reports (none from WhatsApp) include bans of accounts that only replied to incoming messages at low volume, so the risk is not zero even when the rules are followed.

**Linked device expiry.** https://faq.whatsapp.com/378279804439436:

> "Linked devices work without your phone online, but will log out if your phone is unused for over 14 days."

> "We'll also automatically disconnect linked devices after 30 days of inactivity."

What counts as inactivity for the 30-day rule is not defined. The sidecar keeps presence unavailable (section 7), and it is unverified whether that makes the Linked devices screen show the device as stale. A phone left unused for more than 14 days logs the sidecar out (`LoggedOut`, then `device_removed`); the owner must open WhatsApp on the phone at least every 14 days. Up to four linked devices can be attached to one phone; the sidecar uses one slot.

**Library-side signals.** Treat each as a stop sign:

- `events.TemporaryBan` codes: 101 (too many messages to people who do not have you in their address books), 102 (too many people blocked you), 103 (too many groups created with such people), 104 (the same message sent to too many people), 106 (too many messages to a broadcast list). The event carries an expiry.
- Reach-out time-lock: `events.NotifyAccountReachoutTimelock` and send acknowledgement error 463. It blocks messages to chats the account has not talked to, reportedly for 24 hours to 7 days (unverified). Reports tie it to a companion logout shortly after.
- Connect failure 406 is called BANNED in the WhatsApp Web code and deletes the session; 403 can be a ban or a phone switch; 405 is a client that is too old.

## 7. Operational rules

1. One process per `data_dir`. The `sidecar.lock` file enforces it; never defeat it. A second process with the same keys would make WhatsApp end the first with `StreamReplaced`, which disables auto reconnect and can look like abuse. If `stream_replaced` shows up, find and stop the other process (an old sidecar, a copy of `session.db`).
2. Presence stays unavailable. The sidecar sends `PresenceUnavailable` on every `Connected` and never `Available`. Marking the companion available may change when the owner's phone notifies, and it makes delivery receipts render differently. Do not add a default `Available` call.
3. New chats only to a phone number WhatsApp confirms as registered, and only from `send_text` and `send_media`; everything else needs the chat in the local store (`thread_not_found`). First contact with a number that never messaged the owner is the pattern WhatsApp flags most, so keep it to people who expect the message. No broadcast, no group creation, no identical text to many chats.
4. Stop on any fatal signal and tell the owner. `temporary_ban`, `reachout_timelock`, `client_outdated`, `stream_replaced`, `device_removed` and `banned` all block sends until restart. The daemon does this: it keeps the reason in `status`, refuses writes with `sending_blocked` itself, stops reconnecting and does not send `connect` (the sidecar refuses both while blocked). Do not restart in a loop: restarting clears the block but does not clear the cause.
5. Keep the pin current. whatsmeow has no releases, so bump the pseudo-version at least monthly (`go get go.mau.fi/whatsmeow@latest`, then `go mod tidy`, then `build.sh`), and re-read the library's API changes: the API still moves. Also update after any `ClientOutdated`, and treat a mass `LoggedOut` right after pairing as "the pin is stale". The sidecar refreshes the WhatsApp Web version once per process at `connect` and `login_start`, but a version bump alone may not be enough when the protobufs change.
6. Never call `logout` on a normal stop. `logout` removes the companion device on WhatsApp's side when the sidecar is connected, and wipes `messages.db` and the leftovers in `session.db`; re-pairing needs the phone again. A normal stop is `shutdown` or closing stdin. Re-linking right after an unlink may be refused by WhatsApp for a while.
7. Approval and pacing are not the sidecar's job. The sidecar enforces no policy beyond the gates in section 3 (blocked, logged in, connected, existing chat) and does not rate limit. The daemon has no approval step either: whether a human confirms each send depends on the MCP client's settings. What the daemon does is pace writes per account with a token bucket and refuse a call without a token with `rate_limited` (new messages: one token per 2 s, burst 3, tunable with `META_MCP_PERSONAL_SEND_INTERVAL_MS` and `META_MCP_PERSONAL_SEND_BURST`; reactions, read marks and typing: one per second, burst 5). The circulating figures (at least 500 ms apart, no more than about 20 messages per minute per account) are folklore; neither is a threshold WhatsApp published.
8. Protect the data directory: mode 0700, encrypted disk, no backup to an unencrypted place, no copying `session.db` to another machine. Do not log frames, params or results on the daemon side either.
9. Keep the phone healthy: open WhatsApp on it at least every 14 days, keep it on the same account, and avoid emulators or VMs for the primary phone and unusual networks for the sidecar (a home connection is the profile this was designed for).
