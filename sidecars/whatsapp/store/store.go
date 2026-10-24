// Package store keeps the sidecar's own message database (messages.db), separate from the whatsmeow session.
package store

import (
	"context"
	"database/sql"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"
	"unicode/utf8"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
	_ "modernc.org/sqlite"
)

// ErrBadCursor means a list_threads cursor could not be decoded.
var ErrBadCursor = errors.New("store: bad cursor")

const schemaVersion = 2

// schemaV1 stays verbatim: a new database starts here and migrates, so one code path serves both.
const schemaV1 = `
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE chats (
	jid TEXT PRIMARY KEY,
	alt_jid TEXT NOT NULL DEFAULT '',
	kind TEXT NOT NULL,
	name TEXT NOT NULL DEFAULT '',
	last_ts INTEGER NOT NULL DEFAULT 0,
	last_preview TEXT NOT NULL DEFAULT '',
	unread_count INTEGER NOT NULL DEFAULT 0,
	archived INTEGER NOT NULL DEFAULT 0,
	pinned INTEGER NOT NULL DEFAULT 0,
	muted_until INTEGER NOT NULL DEFAULT 0,
	read_up_to_ts INTEGER NOT NULL DEFAULT 0,
	end_of_history TEXT NOT NULL DEFAULT '',
	participants TEXT,
	group_info_at INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX chats_alt_jid ON chats(alt_jid);
CREATE INDEX chats_last ON chats(last_ts DESC, jid DESC);
CREATE TABLE messages (
	chat TEXT NOT NULL,
	id TEXT NOT NULL,
	chat_wire TEXT NOT NULL DEFAULT '',
	sender_wire TEXT NOT NULL DEFAULT '',
	sender TEXT NOT NULL DEFAULT '',
	sender_alt TEXT NOT NULL DEFAULT '',
	sender_name TEXT NOT NULL DEFAULT '',
	ts INTEGER NOT NULL,
	from_me INTEGER NOT NULL DEFAULT 0,
	kind TEXT NOT NULL,
	text TEXT,
	reply_to TEXT,
	attachments TEXT NOT NULL DEFAULT '[]',
	raw BLOB,
	edited INTEGER NOT NULL DEFAULT 0,
	unsent INTEGER NOT NULL DEFAULT 0,
	PRIMARY KEY (chat, id)
);
CREATE INDEX messages_chat_ts ON messages(chat, ts DESC, id DESC);
CREATE TABLE reactions (
	chat TEXT NOT NULL,
	message_id TEXT NOT NULL,
	sender TEXT NOT NULL,
	emoji TEXT NOT NULL,
	ts INTEGER NOT NULL DEFAULT 0,
	PRIMARY KEY (chat, message_id, sender)
);
CREATE TABLE receipts (
	chat TEXT NOT NULL,
	sender TEXT NOT NULL,
	kind TEXT NOT NULL,
	ts INTEGER NOT NULL,
	PRIMARY KEY (chat, sender, kind)
);
CREATE TABLE contacts (
	jid TEXT PRIMARY KEY,
	full_name TEXT NOT NULL DEFAULT '',
	first_name TEXT NOT NULL DEFAULT '',
	push_name TEXT NOT NULL DEFAULT ''
);
CREATE TABLE lid_map (lid TEXT PRIMARY KEY, pn TEXT NOT NULL UNIQUE);
`

// schemaV2 keys messages by (chat, sender, id) with (chat, id) unique, and adds the read mark id and the group admins.
const schemaV2 = `
CREATE TABLE messages_v2 (
	chat TEXT NOT NULL,
	sender TEXT NOT NULL DEFAULT '',
	id TEXT NOT NULL,
	chat_wire TEXT NOT NULL DEFAULT '',
	sender_wire TEXT NOT NULL DEFAULT '',
	sender_alt TEXT NOT NULL DEFAULT '',
	sender_name TEXT NOT NULL DEFAULT '',
	ts INTEGER NOT NULL,
	from_me INTEGER NOT NULL DEFAULT 0,
	kind TEXT NOT NULL,
	text TEXT,
	reply_to TEXT,
	attachments TEXT NOT NULL DEFAULT '[]',
	raw BLOB,
	edited INTEGER NOT NULL DEFAULT 0,
	unsent INTEGER NOT NULL DEFAULT 0,
	PRIMARY KEY (chat, sender, id)
);
INSERT INTO messages_v2 (chat, sender, id, chat_wire, sender_wire, sender_alt, sender_name, ts, from_me, kind, text, reply_to, attachments, raw, edited, unsent)
	SELECT chat, sender, id, chat_wire, sender_wire, sender_alt, sender_name, ts, from_me, kind, text, reply_to, attachments, raw, edited, unsent FROM messages;
DROP TABLE messages;
ALTER TABLE messages_v2 RENAME TO messages;
CREATE UNIQUE INDEX messages_chat_id ON messages(chat, id);
CREATE INDEX messages_chat_ts ON messages(chat, ts DESC, id DESC);
ALTER TABLE chats ADD COLUMN read_up_to_id TEXT NOT NULL DEFAULT '';
ALTER TABLE chats ADD COLUMN admins TEXT;
UPDATE chats SET read_up_to_id = COALESCE((SELECT MAX(m.id) FROM messages m WHERE m.chat = chats.jid AND m.from_me = 0 AND m.ts = chats.read_up_to_ts), '') WHERE read_up_to_ts > 0;
`

// Chat is one conversation row.
type Chat struct {
	JID, AltJID  string
	Kind         string
	Name         string
	LastTS       int64
	LastPreview  string
	UnreadCount  int
	Archived     bool
	Pinned       bool
	MutedUntil   int64
	ReadUpToTS   int64
	ReadUpToID   string // with ReadUpToTS, the newest message marked read in (ts, id) order
	EndOfHistory string
	Participants []api.Participant
	GroupInfoAt  int64
	Admins       []string // ids of the known group admins, both id forms
}

// Msg is one stored message; Reactions is filled on read.
type Msg struct {
	Chat, ID             string
	ChatWire, SenderWire string
	Sender, SenderAlt    string
	SenderName           string
	TS                   int64
	FromMe               bool
	Kind                 string
	Text, ReplyTo        *string
	Attachments          []api.Attachment
	Raw                  []byte
	Edited, Unsent       bool
	Reactions            []api.Reaction
}

// WrittenBy reports whether the sender of m is the owner (fromMe) or goes by any of ids.
// m names its sender in up to three forms (sender, sender_alt, sender_wire); one shared form is enough.
func (m Msg) WrittenBy(fromMe bool, ids ...string) bool {
	if m.FromMe != fromMe {
		return false
	}
	if fromMe {
		return true
	}
	for _, id := range ids {
		if id != "" && (id == m.Sender || id == m.SenderAlt || id == m.SenderWire) {
			return true
		}
	}
	return false
}

// Mark is a position in a chat's (ts, id) message order.
type Mark struct {
	TS int64
	ID string
}

// After reports whether a sorts after b.
func (a Mark) After(b Mark) bool { return a.TS > b.TS || (a.TS == b.TS && a.ID > b.ID) }

// Store is the messages.db handle.
type Store struct {
	db *sql.DB
}

type querier interface {
	ExecContext(ctx context.Context, query string, args ...any) (sql.Result, error)
	QueryContext(ctx context.Context, query string, args ...any) (*sql.Rows, error)
	QueryRowContext(ctx context.Context, query string, args ...any) *sql.Row
}

type scanner interface{ Scan(dest ...any) error }

// Open opens or creates messages.db at path with mode 0600 and migrates it.
func Open(path string) (*Store, error) {
	f, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return nil, fmt.Errorf("store: create db file: %w", err)
	}
	f.Close()
	esc := strings.NewReplacer("%", "%25", "?", "%3f", "#", "%23").Replace(path)
	dsn := "file:" + esc + "?_pragma=foreign_keys(1)&_pragma=busy_timeout(5000)&_pragma=journal_mode(WAL)&_pragma=secure_delete(1)&_pragma=temp_store(2)"
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		return nil, fmt.Errorf("store: open: %w", err)
	}
	db.SetMaxOpenConns(1)
	s := &Store{db: db}
	if err := s.migrate(context.Background()); err != nil {
		db.Close()
		return nil, err
	}
	for _, suffix := range []string{"", "-wal", "-shm"} {
		if err := os.Chmod(path+suffix, 0o600); err != nil && !errors.Is(err, os.ErrNotExist) {
			db.Close()
			return nil, fmt.Errorf("store: chmod: %w", err)
		}
	}
	return s, nil
}

func (s *Store) migrate(ctx context.Context) error {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("store: migrate: %w", err)
	}
	defer tx.Rollback()
	var v int
	if err := tx.QueryRowContext(ctx, "PRAGMA user_version").Scan(&v); err != nil {
		return fmt.Errorf("store: read version: %w", err)
	}
	if v > schemaVersion {
		return fmt.Errorf("store: database version %d is newer than supported %d", v, schemaVersion)
	}
	for _, step := range []struct {
		to  int
		ddl string
	}{{1, schemaV1}, {2, schemaV2}} {
		if v >= step.to {
			continue
		}
		if _, err := tx.ExecContext(ctx, step.ddl); err != nil {
			return fmt.Errorf("store: migrate to version %d: %w", step.to, err)
		}
		if _, err := tx.ExecContext(ctx, fmt.Sprintf("PRAGMA user_version = %d", step.to)); err != nil {
			return fmt.Errorf("store: set version: %w", err)
		}
	}
	return tx.Commit()
}

// Close closes the database.
func (s *Store) Close() error { return s.db.Close() }

func (s *Store) inTx(ctx context.Context, fn func(tx *sql.Tx) error) error {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if err := fn(tx); err != nil {
		return err
	}
	return tx.Commit()
}

// Reset deletes every row of every table, then rewrites the file so the deleted rows leave no residue.
func (s *Store) Reset(ctx context.Context) error {
	err := s.inTx(ctx, func(tx *sql.Tx) error {
		for _, t := range []string{"meta", "chats", "messages", "reactions", "receipts", "contacts", "lid_map"} {
			if _, err := tx.ExecContext(ctx, "DELETE FROM "+t); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		return err
	}
	for _, q := range []string{"PRAGMA wal_checkpoint(TRUNCATE)", "VACUUM", "PRAGMA wal_checkpoint(TRUNCATE)"} {
		if _, err := s.db.ExecContext(ctx, q); err != nil {
			return err
		}
	}
	return nil
}

// GetMeta reads a meta value.
func (s *Store) GetMeta(ctx context.Context, key string) (string, bool, error) {
	var v string
	err := s.db.QueryRowContext(ctx, "SELECT value FROM meta WHERE key = ?", key).Scan(&v)
	if errors.Is(err, sql.ErrNoRows) {
		return "", false, nil
	}
	return v, err == nil, err
}

// SetMeta writes a meta value.
func (s *Store) SetMeta(ctx context.Context, key, value string) error {
	_, err := s.db.ExecContext(ctx, "INSERT INTO meta(key, value) VALUES(?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value", key, value)
	return err
}

// ---- chats ----

const chatCols = "jid, alt_jid, kind, name, last_ts, last_preview, unread_count, archived, pinned, muted_until, read_up_to_ts, read_up_to_id, end_of_history, participants, group_info_at, admins"

func scanChat(r scanner) (Chat, error) {
	var c Chat
	var parts, admins sql.NullString
	if err := r.Scan(&c.JID, &c.AltJID, &c.Kind, &c.Name, &c.LastTS, &c.LastPreview, &c.UnreadCount,
		&c.Archived, &c.Pinned, &c.MutedUntil, &c.ReadUpToTS, &c.ReadUpToID, &c.EndOfHistory, &parts, &c.GroupInfoAt, &admins); err != nil {
		return Chat{}, err
	}
	if parts.Valid {
		if err := json.Unmarshal([]byte(parts.String), &c.Participants); err != nil {
			return Chat{}, fmt.Errorf("store: participants: %w", err)
		}
	}
	if admins.Valid {
		if err := json.Unmarshal([]byte(admins.String), &c.Admins); err != nil {
			return Chat{}, fmt.Errorf("store: admins: %w", err)
		}
	}
	return c, nil
}

// HasAdmin reports whether any of ids is a known admin of the group.
func (c Chat) HasAdmin(ids ...string) bool {
	for _, id := range ids {
		if id == "" {
			continue
		}
		for _, a := range c.Admins {
			if a == id {
				return true
			}
		}
	}
	return false
}

func adminsArg(a []string) (any, error) {
	if len(a) == 0 {
		return nil, nil
	}
	b, err := json.Marshal(a)
	return string(b), err
}

func getChat(ctx context.Context, q querier, id string) (Chat, bool, error) {
	row := q.QueryRowContext(ctx, "SELECT "+chatCols+" FROM chats WHERE jid = ?1 OR (alt_jid <> '' AND alt_jid = ?1) ORDER BY jid = ?1 DESC LIMIT 1", id)
	c, err := scanChat(row)
	if errors.Is(err, sql.ErrNoRows) {
		return Chat{}, false, nil
	}
	return c, err == nil, err
}

func participantsArg(p []api.Participant) (any, error) {
	if p == nil {
		return nil, nil
	}
	b, err := json.Marshal(p)
	return string(b), err
}

func writeChat(ctx context.Context, q querier, origJID string, c Chat) error {
	parts, err := participantsArg(c.Participants)
	if err != nil {
		return err
	}
	admins, err := adminsArg(c.Admins)
	if err != nil {
		return err
	}
	_, err = q.ExecContext(ctx, `UPDATE chats SET alt_jid=?, kind=?, name=?, last_ts=?, last_preview=?, unread_count=?,
		archived=?, pinned=?, muted_until=?, read_up_to_ts=?, read_up_to_id=?, end_of_history=?, participants=?, group_info_at=?, admins=? WHERE jid=?`,
		c.AltJID, c.Kind, c.Name, c.LastTS, c.LastPreview, c.UnreadCount, c.Archived, c.Pinned, c.MutedUntil,
		c.ReadUpToTS, c.ReadUpToID, c.EndOfHistory, parts, c.GroupInfoAt, admins, origJID)
	return err
}

func insertChat(ctx context.Context, q querier, c Chat) error {
	parts, err := participantsArg(c.Participants)
	if err != nil {
		return err
	}
	admins, err := adminsArg(c.Admins)
	if err != nil {
		return err
	}
	_, err = q.ExecContext(ctx, "INSERT INTO chats("+chatCols+") VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
		c.JID, c.AltJID, c.Kind, c.Name, c.LastTS, c.LastPreview, c.UnreadCount, c.Archived, c.Pinned,
		c.MutedUntil, c.ReadUpToTS, c.ReadUpToID, c.EndOfHistory, parts, c.GroupInfoAt, admins)
	return err
}

// GetChat finds a chat by its canonical or alternate id.
func (s *Store) GetChat(ctx context.Context, id string) (Chat, bool, error) {
	return getChat(ctx, s.db, id)
}

// EnsureChat inserts the chat when neither form exists and fills a missing alt on an existing one.
func (s *Store) EnsureChat(ctx context.Context, jid, alt, kind string) (Chat, bool, error) {
	var out Chat
	var created bool
	err := s.inTx(ctx, func(tx *sql.Tx) error {
		c, ok, err := getChat(ctx, tx, jid)
		if err != nil {
			return err
		}
		if !ok && alt != "" {
			if c, ok, err = getChat(ctx, tx, alt); err != nil {
				return err
			}
		}
		if !ok {
			out = Chat{JID: jid, AltJID: alt, Kind: kind}
			created = true
			return insertChat(ctx, tx, out)
		}
		out = c
		if c.AltJID == "" {
			other := alt
			if other == c.JID || other == "" {
				other = jid
			}
			if other != c.JID {
				out.AltJID = other
				return writeChat(ctx, tx, c.JID, out)
			}
		}
		return nil
	})
	return out, created, err
}

// UpdateChat applies fn to the chat in one transaction; the JID cannot be changed.
func (s *Store) UpdateChat(ctx context.Context, id string, fn func(*Chat)) (Chat, bool, error) {
	var out Chat
	var found bool
	err := s.inTx(ctx, func(tx *sql.Tx) error {
		c, ok, err := getChat(ctx, tx, id)
		if err != nil || !ok {
			return err
		}
		found = true
		orig := c.JID
		fn(&c)
		c.JID = orig
		out = c
		return writeChat(ctx, tx, orig, c)
	})
	if err != nil {
		return Chat{}, false, err
	}
	return out, found, nil
}

// MergeChat moves every row keyed on from to to and deletes the from chat, keeping the newer metadata.
func (s *Store) MergeChat(ctx context.Context, from, to string) error {
	if from == to {
		return nil
	}
	return s.inTx(ctx, func(tx *sql.Tx) error {
		src, srcOK, err := getChat(ctx, tx, from)
		if err != nil {
			return err
		}
		if srcOK {
			from = src.JID
		}
		dst, dstOK, err := getChat(ctx, tx, to)
		if err != nil {
			return err
		}
		if dstOK {
			to = dst.JID
		}
		if from == to {
			return nil
		}
		stmts := []string{
			"UPDATE OR IGNORE messages SET chat = ?2 WHERE chat = ?1",
			"DELETE FROM messages WHERE chat = ?1",
			"UPDATE OR IGNORE reactions SET chat = ?2 WHERE chat = ?1",
			"DELETE FROM reactions WHERE chat = ?1",
			"INSERT INTO receipts(chat, sender, kind, ts) SELECT ?2, sender, kind, ts FROM receipts WHERE chat = ?1 ON CONFLICT(chat, sender, kind) DO UPDATE SET ts = MAX(ts, excluded.ts)",
			"DELETE FROM receipts WHERE chat = ?1",
		}
		for _, q := range stmts {
			if _, err := tx.ExecContext(ctx, q, from, to); err != nil {
				return err
			}
		}
		if !srcOK {
			return nil
		}
		if _, err := tx.ExecContext(ctx, "DELETE FROM chats WHERE jid = ?", from); err != nil {
			return err
		}
		merged := mergeMeta(src, dst, dstOK, to)
		if dstOK {
			return writeChat(ctx, tx, to, merged)
		}
		return insertChat(ctx, tx, merged)
	})
}

// mergeMeta keeps the metadata of the chat with the newer last message, fills blanks from the other, ORs pinned.
func mergeMeta(src, dst Chat, dstOK bool, to string) Chat {
	if !dstOK {
		src.AltJID = src.JID
		src.JID = to
		return src
	}
	newer, older := dst, src
	if src.LastTS > dst.LastTS {
		newer, older = src, dst
	}
	m := newer
	m.JID = to
	if m.Name == "" {
		m.Name = older.Name
	}
	if m.Kind == "" {
		m.Kind = older.Kind
	}
	if m.EndOfHistory == "" {
		m.EndOfHistory = older.EndOfHistory
	}
	if m.Participants == nil {
		m.Participants = older.Participants
	}
	if m.GroupInfoAt == 0 {
		m.GroupInfoAt = older.GroupInfoAt
	}
	m.Pinned = m.Pinned || older.Pinned
	if older.MutedUntil > m.MutedUntil {
		m.MutedUntil = older.MutedUntil
	}
	if (Mark{older.ReadUpToTS, older.ReadUpToID}).After(Mark{m.ReadUpToTS, m.ReadUpToID}) {
		m.ReadUpToTS, m.ReadUpToID = older.ReadUpToTS, older.ReadUpToID
	}
	if m.Admins == nil {
		m.Admins = older.Admins
	}
	m.AltJID = dst.AltJID
	if m.AltJID == "" && src.JID != to {
		m.AltJID = src.JID
	}
	if m.AltJID == "" {
		m.AltJID = src.AltJID
	}
	return m
}

// DeleteChat removes the chat and every row keyed on it.
func (s *Store) DeleteChat(ctx context.Context, id string) error {
	return s.inTx(ctx, func(tx *sql.Tx) error {
		jid := id
		if c, ok, err := getChat(ctx, tx, id); err != nil {
			return err
		} else if ok {
			jid = c.JID
		}
		for _, q := range []string{
			"DELETE FROM messages WHERE chat = ?",
			"DELETE FROM reactions WHERE chat = ?",
			"DELETE FROM receipts WHERE chat = ?",
			"DELETE FROM chats WHERE jid = ?",
		} {
			if _, err := tx.ExecContext(ctx, q, jid); err != nil {
				return err
			}
		}
		return nil
	})
}

type chatCursor struct {
	TS  int64  `json:"t"`
	JID string `json:"j"`
}

// ListChats returns chats newest last_ts first, then jid descending, with an opaque keyset cursor.
func (s *Store) ListChats(ctx context.Context, limit int, cursor string) ([]Chat, string, error) {
	if limit < 1 {
		limit = 50
	}
	q := "SELECT " + chatCols + " FROM chats"
	var args []any
	if cursor != "" {
		raw, err := base64.RawURLEncoding.DecodeString(cursor)
		var cur chatCursor
		if err != nil || json.Unmarshal(raw, &cur) != nil || cur.JID == "" {
			return nil, "", ErrBadCursor
		}
		q += " WHERE last_ts < ?1 OR (last_ts = ?1 AND jid < ?2)"
		args = append(args, cur.TS, cur.JID)
	}
	q += " ORDER BY last_ts DESC, jid DESC LIMIT " + fmt.Sprint(limit+1)
	rows, err := s.db.QueryContext(ctx, q, args...)
	if err != nil {
		return nil, "", err
	}
	defer rows.Close()
	var out []Chat
	for rows.Next() {
		c, err := scanChat(rows)
		if err != nil {
			return nil, "", err
		}
		out = append(out, c)
	}
	if err := rows.Err(); err != nil {
		return nil, "", err
	}
	next := ""
	if len(out) > limit {
		out = out[:limit]
		last := out[limit-1]
		b, _ := json.Marshal(chatCursor{TS: last.LastTS, JID: last.JID})
		next = base64.RawURLEncoding.EncodeToString(b)
	}
	return out, next, nil
}

// ---- messages ----

const msgCols = "chat, id, chat_wire, sender_wire, sender, sender_alt, sender_name, ts, from_me, kind, text, reply_to, attachments, raw, edited, unsent"

func scanMsg(r scanner) (Msg, error) {
	var m Msg
	var text, reply sql.NullString
	var atts string
	if err := r.Scan(&m.Chat, &m.ID, &m.ChatWire, &m.SenderWire, &m.Sender, &m.SenderAlt, &m.SenderName,
		&m.TS, &m.FromMe, &m.Kind, &text, &reply, &atts, &m.Raw, &m.Edited, &m.Unsent); err != nil {
		return Msg{}, err
	}
	if text.Valid {
		m.Text = &text.String
	}
	if reply.Valid {
		m.ReplyTo = &reply.String
	}
	if err := json.Unmarshal([]byte(atts), &m.Attachments); err != nil {
		return Msg{}, fmt.Errorf("store: attachments: %w", err)
	}
	if len(m.Attachments) == 0 {
		m.Attachments = nil
	}
	return m, nil
}

func strArg(p *string) any {
	if p == nil {
		return nil
	}
	return *p
}

func attsArg(a []api.Attachment) (string, error) {
	if len(a) == 0 {
		return "[]", nil
	}
	b, err := json.Marshal(a)
	return string(b), err
}

func msgArgs(m Msg, atts string) []any {
	return []any{m.Chat, m.ID, m.ChatWire, m.SenderWire, m.Sender, m.SenderAlt, m.SenderName, m.TS, m.FromMe,
		m.Kind, strArg(m.Text), strArg(m.ReplyTo), atts, m.Raw, m.Edited, m.Unsent}
}

func writeMsg(ctx context.Context, q querier, m Msg) error {
	atts, err := attsArg(m.Attachments)
	if err != nil {
		return err
	}
	_, err = q.ExecContext(ctx, `UPDATE messages SET chat_wire=?3, sender_wire=?4, sender=?5, sender_alt=?6, sender_name=?7, ts=?8,
		from_me=?9, kind=?10, text=?11, reply_to=?12, attachments=?13, raw=?14, edited=?15, unsent=?16 WHERE chat=?1 AND id=?2`,
		msgArgs(m, atts)...)
	return err
}

// fillReactions loads the reactions of msgs in place.
func fillReactions(ctx context.Context, q querier, msgs []Msg) error {
	for i := range msgs {
		rows, err := q.QueryContext(ctx, "SELECT sender, emoji FROM reactions WHERE chat = ? AND message_id = ? ORDER BY ts, sender", msgs[i].Chat, msgs[i].ID)
		if err != nil {
			return err
		}
		msgs[i].Reactions = nil
		for rows.Next() {
			var r api.Reaction
			if err := rows.Scan(&r.SenderID, &r.Emoji); err != nil {
				rows.Close()
				return err
			}
			msgs[i].Reactions = append(msgs[i].Reactions, r)
		}
		err = rows.Err()
		rows.Close()
		if err != nil {
			return err
		}
	}
	return nil
}

func queryMsgs(ctx context.Context, q querier, query string, args ...any) ([]Msg, error) {
	rows, err := q.QueryContext(ctx, query, args...)
	if err != nil {
		return nil, err
	}
	var out []Msg
	for rows.Next() {
		m, err := scanMsg(rows)
		if err != nil {
			rows.Close()
			return nil, err
		}
		out = append(out, m)
	}
	err = rows.Err()
	rows.Close()
	if err != nil {
		return nil, err
	}
	return out, fillReactions(ctx, q, out)
}

func getMessage(ctx context.Context, q querier, chat, id string) (Msg, bool, error) {
	ms, err := queryMsgs(ctx, q, "SELECT "+msgCols+" FROM messages WHERE chat = ? AND id = ?", chat, id)
	if err != nil || len(ms) == 0 {
		return Msg{}, false, err
	}
	return ms[0], true, nil
}

// Preview is the chat-list preview of a message: its text cut to 100 runes, else a bracketed kind label.
func Preview(m Msg) string {
	if m.Unsent {
		return ""
	}
	if m.Text != nil && *m.Text != "" {
		t := *m.Text
		if utf8.RuneCountInString(t) > 100 {
			t = string([]rune(t)[:100])
		}
		return t
	}
	switch m.Kind {
	case api.MsgSticker:
		return "[sticker]"
	case api.MsgLocation:
		return "[location]"
	case api.MsgCall:
		return "[call]"
	}
	if len(m.Attachments) > 0 {
		a := m.Attachments[0]
		switch a.Kind {
		case api.AttImage:
			return "[image]"
		case api.AttVideo, api.AttGIF:
			return "[video]"
		case api.AttAudio:
			if a.Mime != nil && strings.Contains(*a.Mime, "codecs=opus") {
				return "[voice note]"
			}
			return "[audio]"
		case api.AttFile:
			return "[file]"
		case api.AttSticker:
			return "[sticker]"
		case api.AttLocation:
			return "[location]"
		}
	}
	return "[unsupported]"
}

// PutResult says what PutMessage did.
type PutResult int

const (
	// PutInserted means a new row was written.
	PutInserted PutResult = iota
	// PutExisting means the same sender already owns this id; the row is left alone unless m arrived as an edit or revoke.
	PutExisting
	// PutConflict means another sender owns this id in the chat; nothing was written.
	PutConflict
)

// PutMessage stores a message keyed by (chat, sender, id); the first sender to hold an id in a chat keeps it.
// A redelivery never double counts unread or changes content, unless it arrives as an edit or revoke from the same sender.
func (s *Store) PutMessage(ctx context.Context, m Msg, countUnread bool) (PutResult, error) {
	res := PutInserted
	err := s.inTx(ctx, func(tx *sql.Tx) error {
		old, found, err := getMessage(ctx, tx, m.Chat, m.ID)
		if err != nil {
			return err
		}
		if found {
			if !old.WrittenBy(m.FromMe, m.Sender, m.SenderAlt, m.SenderWire) {
				res = PutConflict
				return nil
			}
			res = PutExisting
			if !m.Edited && !m.Unsent {
				return nil
			}
			old.Kind, old.Text, old.ReplyTo, old.Attachments, old.Raw = m.Kind, m.Text, m.ReplyTo, m.Attachments, m.Raw
			old.Edited, old.Unsent = old.Edited || m.Edited, old.Unsent || m.Unsent
			return writeMsg(ctx, tx, old)
		}
		atts, err := attsArg(m.Attachments)
		if err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx, "INSERT INTO messages("+msgCols+") VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)", msgArgs(m, atts)...); err != nil {
			return err
		}
		count := countUnread && !m.FromMe
		_, err = tx.ExecContext(ctx, `UPDATE chats SET
			last_preview = CASE WHEN ?2 >= last_ts THEN ?3 ELSE last_preview END,
			last_ts = CASE WHEN ?2 >= last_ts THEN ?2 ELSE last_ts END,
			unread_count = unread_count + CASE WHEN ?4 AND (?2 > read_up_to_ts OR (?2 = read_up_to_ts AND ?5 > read_up_to_id)) THEN 1 ELSE 0 END
			WHERE jid = ?1`, m.Chat, m.TS, Preview(m), count, m.ID)
		return err
	})
	return res, err
}

// GetMessage reads one message with its reactions.
func (s *Store) GetMessage(ctx context.Context, chat, id string) (Msg, bool, error) {
	return getMessage(ctx, s.db, chat, id)
}

// UpdateResult says what UpdateMessageIf did.
type UpdateResult int

const (
	// UpdateMissing means no such message is stored.
	UpdateMissing UpdateResult = iota
	// UpdateDenied means the message exists and allow refused it; nothing was written.
	UpdateDenied
	// UpdateDone means fn ran and the row was written.
	UpdateDone
)

// UpdateMessage applies fn to a stored message in one transaction and refreshes the chat preview when it is the newest.
func (s *Store) UpdateMessage(ctx context.Context, chat, id string, fn func(*Msg)) (Msg, bool, error) {
	m, res, err := s.UpdateMessageIf(ctx, chat, id, nil, fn)
	return m, res == UpdateDone, err
}

// UpdateMessageIf is UpdateMessage that first asks allow (when not nil) about the stored row, in the same transaction.
func (s *Store) UpdateMessageIf(ctx context.Context, chat, id string, allow func(Msg) bool, fn func(*Msg)) (Msg, UpdateResult, error) {
	var out Msg
	res := UpdateMissing
	err := s.inTx(ctx, func(tx *sql.Tx) error {
		m, ok, err := getMessage(ctx, tx, chat, id)
		if err != nil || !ok {
			return err
		}
		if allow != nil && !allow(m) {
			res = UpdateDenied
			return nil
		}
		fn(&m)
		m.Chat, m.ID = chat, id
		if err := writeMsg(ctx, tx, m); err != nil {
			return err
		}
		out, res = m, UpdateDone
		_, err = tx.ExecContext(ctx, `UPDATE chats SET last_preview = ?3 WHERE jid = ?1 AND ?2 = (
			SELECT id FROM messages WHERE chat = ?1 ORDER BY ts DESC, id DESC LIMIT 1)`, chat, id, Preview(m))
		return err
	})
	if err != nil {
		return Msg{}, UpdateMissing, err
	}
	return out, res, nil
}

// FetchMessages returns up to limit messages newest first, older than the (beforeMS, beforeID) anchor when beforeMS is set.
func (s *Store) FetchMessages(ctx context.Context, chat string, limit int, beforeMS int64, beforeID string) ([]Msg, bool, error) {
	if limit < 1 {
		limit = 50
	}
	q := "SELECT " + msgCols + " FROM messages WHERE chat = ?1"
	args := []any{chat}
	if beforeMS != 0 {
		q += " AND (ts * 1000 < ?2 OR (ts * 1000 = ?2 AND id < ?3))"
		args = append(args, beforeMS, beforeID)
	}
	q += " ORDER BY ts DESC, id DESC LIMIT " + fmt.Sprint(limit+1)
	ms, err := queryMsgs(ctx, s.db, q, args...)
	if err != nil {
		return nil, false, err
	}
	more := len(ms) > limit
	if more {
		ms = ms[:limit]
	}
	return ms, more, nil
}

// OldestMessage returns the oldest stored message of a chat.
func (s *Store) OldestMessage(ctx context.Context, chat string) (Msg, bool, error) {
	ms, err := queryMsgs(ctx, s.db, "SELECT "+msgCols+" FROM messages WHERE chat = ? ORDER BY ts ASC, id ASC LIMIT 1", chat)
	if err != nil || len(ms) == 0 {
		return Msg{}, false, err
	}
	return ms[0], true, nil
}

// UnreadIncoming returns up to max incoming messages after the after mark, at or below upToTS and older than before, newest first.
// Only Chat, ID, TS, ChatWire and SenderWire are filled.
func (s *Store) UnreadIncoming(ctx context.Context, chat string, after Mark, upToTS int64, before *Mark, max int) ([]Msg, error) {
	if max < 1 {
		max = 100
	}
	q := `SELECT chat, id, ts, chat_wire, sender_wire FROM messages WHERE chat = ?1 AND from_me = 0
		AND (ts > ?2 OR (ts = ?2 AND id > ?3)) AND ts <= ?4`
	args := []any{chat, after.TS, after.ID, upToTS}
	if before != nil {
		q += " AND (ts < ?5 OR (ts = ?5 AND id < ?6))"
		args = append(args, before.TS, before.ID)
	}
	rows, err := s.db.QueryContext(ctx, q+" ORDER BY ts DESC, id DESC LIMIT "+fmt.Sprint(max), args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []Msg
	for rows.Next() {
		var m Msg
		if err := rows.Scan(&m.Chat, &m.ID, &m.TS, &m.ChatWire, &m.SenderWire); err != nil {
			return nil, err
		}
		out = append(out, m)
	}
	return out, rows.Err()
}

// ---- reactions and receipts ----

// PutReaction sets a sender's reaction on a message; an empty emoji removes it.
func (s *Store) PutReaction(ctx context.Context, chat, msgID, sender, emoji string, ts int64) error {
	if emoji == "" {
		_, err := s.db.ExecContext(ctx, "DELETE FROM reactions WHERE chat = ? AND message_id = ? AND sender = ?", chat, msgID, sender)
		return err
	}
	_, err := s.db.ExecContext(ctx, `INSERT INTO reactions(chat, message_id, sender, emoji, ts) VALUES(?,?,?,?,?)
		ON CONFLICT(chat, message_id, sender) DO UPDATE SET emoji = excluded.emoji, ts = excluded.ts`, chat, msgID, sender, emoji, ts)
	return err
}

// PutReceipt records a receipt, keeping the highest ts per (chat, sender, kind).
func (s *Store) PutReceipt(ctx context.Context, chat, sender, kind string, ts int64) error {
	_, err := s.db.ExecContext(ctx, `INSERT INTO receipts(chat, sender, kind, ts) VALUES(?,?,?,?)
		ON CONFLICT(chat, sender, kind) DO UPDATE SET ts = MAX(ts, excluded.ts)`, chat, sender, kind, ts)
	return err
}

// ---- contacts and LID cache ----

// PutContactName stores the address-book names of a contact, leaving its push name alone.
func (s *Store) PutContactName(ctx context.Context, jid, fullName, firstName string) error {
	_, err := s.db.ExecContext(ctx, `INSERT INTO contacts(jid, full_name, first_name) VALUES(?,?,?)
		ON CONFLICT(jid) DO UPDATE SET full_name = excluded.full_name, first_name = excluded.first_name`, jid, fullName, firstName)
	return err
}

// PutPushName stores the self-chosen name of a contact; an empty name is ignored.
func (s *Store) PutPushName(ctx context.Context, jid, name string) error {
	if name == "" {
		return nil
	}
	_, err := s.db.ExecContext(ctx, `INSERT INTO contacts(jid, push_name) VALUES(?,?)
		ON CONFLICT(jid) DO UPDATE SET push_name = excluded.push_name`, jid, name)
	return err
}

// ContactName returns the best known name: full name, then first name, then push name.
func (s *Store) ContactName(ctx context.Context, jid string) (string, bool, error) {
	var full, first, push string
	err := s.db.QueryRowContext(ctx, "SELECT full_name, first_name, push_name FROM contacts WHERE jid = ?", jid).Scan(&full, &first, &push)
	if errors.Is(err, sql.ErrNoRows) {
		return "", false, nil
	}
	if err != nil {
		return "", false, err
	}
	for _, n := range []string{full, first, push} {
		if n != "" {
			return n, true, nil
		}
	}
	return "", false, nil
}

// PutLIDMapping caches a LID to phone-number JID pair, replacing any older pairing of either side.
func (s *Store) PutLIDMapping(ctx context.Context, lid, pn string) error {
	if lid == "" || pn == "" {
		return nil
	}
	return s.inTx(ctx, func(tx *sql.Tx) error {
		if _, err := tx.ExecContext(ctx, "DELETE FROM lid_map WHERE pn = ? AND lid <> ?", pn, lid); err != nil {
			return err
		}
		_, err := tx.ExecContext(ctx, "INSERT INTO lid_map(lid, pn) VALUES(?,?) ON CONFLICT(lid) DO UPDATE SET pn = excluded.pn", lid, pn)
		return err
	})
}

// PNForLID looks up the phone-number JID of a LID.
func (s *Store) PNForLID(ctx context.Context, lid string) (string, bool, error) {
	return s.lookup(ctx, "SELECT pn FROM lid_map WHERE lid = ?", lid)
}

// LIDForPN looks up the LID of a phone-number JID.
func (s *Store) LIDForPN(ctx context.Context, pn string) (string, bool, error) {
	return s.lookup(ctx, "SELECT lid FROM lid_map WHERE pn = ?", pn)
}

func (s *Store) lookup(ctx context.Context, q, arg string) (string, bool, error) {
	var v string
	err := s.db.QueryRowContext(ctx, q, arg).Scan(&v)
	if errors.Is(err, sql.ErrNoRows) {
		return "", false, nil
	}
	return v, err == nil, err
}

// ---- conversions ----

// API converts a chat to its protocol thread.
func (c Chat) API() api.Thread {
	t := api.Thread{ID: c.JID, Kind: c.Kind, Participants: c.Participants, Encrypted: true}
	if t.Participants == nil {
		t.Participants = []api.Participant{}
	}
	if c.AltJID != "" {
		t.AltID = &c.AltJID
	}
	if c.Name != "" {
		t.Name = &c.Name
	}
	if c.LastTS != 0 {
		ts := c.LastTS
		t.LastMessageTS = &ts
	}
	if c.LastPreview != "" {
		t.LastMessagePreview = &c.LastPreview
	}
	unread := c.UnreadCount > 0
	t.Unread = &unread
	return t
}

// API converts a message to its protocol shape.
func (m Msg) API() api.Message {
	out := api.Message{
		ID: m.ID, ThreadID: m.Chat, TS: m.TS,
		Sender:      api.Sender{ID: m.Sender, Name: m.SenderName, IsMe: m.FromMe},
		Text:        m.Text,
		Attachments: m.Attachments,
		ReplyTo:     m.ReplyTo,
		Reactions:   m.Reactions,
		Edited:      m.Edited,
		Unsent:      m.Unsent,
		Encrypted:   true,
		Kind:        m.Kind,
	}
	if out.Attachments == nil {
		out.Attachments = []api.Attachment{}
	}
	if out.Reactions == nil {
		out.Reactions = []api.Reaction{}
	}
	return out
}
