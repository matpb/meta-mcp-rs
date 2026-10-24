package e2ee

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"sort"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

// entry is one cached message; json is kept so callers never share memory with the ring.
type entry struct {
	ts   int64
	id   string
	json []byte
}

func (e entry) before(ts int64, id string) bool {
	return e.ts < ts || (e.ts == ts && e.id < id)
}

// threadState holds the newest ringCap rows of a thread, ascending. When full is true the ring holds every row.
type threadState struct {
	ring  []entry
	full  bool
	count int
}

func (t *threadState) indexOf(id string) int {
	for i := range t.ring {
		if t.ring[i].id == id {
			return i
		}
	}
	return -1
}

func (t *threadState) remove(id string) {
	if i := t.indexOf(id); i >= 0 {
		t.ring = append(t.ring[:i], t.ring[i+1:]...)
	}
}

// upsert inserts e when it belongs among the newest rows; it reports false when the ring can no longer be trusted.
func (t *threadState) upsert(e entry) bool {
	t.remove(e.id)
	if len(t.ring) == 0 && !t.full {
		return false
	}
	if !t.full && e.before(t.ring[0].ts, t.ring[0].id) {
		return true
	}
	i := sort.Search(len(t.ring), func(i int) bool { return !t.ring[i].before(e.ts, e.id) })
	t.ring = append(t.ring, entry{})
	copy(t.ring[i+1:], t.ring[i:])
	t.ring[i] = e
	if len(t.ring) > ringCap {
		t.ring = append([]entry(nil), t.ring[len(t.ring)-ringCap:]...)
		t.full = false
	}
	return true
}

// state returns the loaded state of a thread. Callers hold s.mu.
func (s *Store) state(ctx context.Context, threadID string) (*threadState, error) {
	if st, ok := s.threads[threadID]; ok {
		return st, nil
	}
	rows, err := s.db.QueryContext(ctx,
		`SELECT ts_ms, id, json FROM sidecar_e2ee_messages WHERE thread_id = ? ORDER BY ts_ms DESC, id DESC LIMIT ?`,
		threadID, ringCap+1)
	if err != nil {
		return nil, ioErr("load thread")
	}
	defer rows.Close()
	var desc []entry
	for rows.Next() {
		var e entry
		if err := rows.Scan(&e.ts, &e.id, &e.json); err != nil {
			return nil, ioErr("load thread")
		}
		desc = append(desc, e)
	}
	if err := rows.Err(); err != nil {
		return nil, ioErr("load thread")
	}
	st := &threadState{full: len(desc) <= ringCap, count: len(desc)}
	if len(desc) > ringCap {
		desc = desc[:ringCap]
		if err := s.db.QueryRowContext(ctx, `SELECT COUNT(*) FROM sidecar_e2ee_messages WHERE thread_id = ?`, threadID).Scan(&st.count); err != nil {
			return nil, ioErr("load thread")
		}
	}
	for i := len(desc) - 1; i >= 0; i-- {
		st.ring = append(st.ring, desc[i])
	}
	s.threads[threadID] = st
	return st, nil
}

func encode(m api.Message) ([]byte, error) {
	b, err := json.Marshal(m)
	if err != nil {
		return nil, ioErr("encode")
	}
	return b, nil
}

func decode(b []byte) (api.Message, error) {
	var m api.Message
	if err := json.Unmarshal(b, &m); err != nil {
		return api.Message{}, ioErr("decode")
	}
	return m, nil
}

// Put upserts a message by ID; a non-empty media slice replaces its media rows, an empty one keeps them.
func (s *Store) Put(ctx context.Context, m api.Message, tsMS int64, media []api.E2EEMedia) error {
	b, err := encode(m)
	if err != nil {
		return err
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	var oldThread string
	var oldTS int64
	existing := true
	err = s.db.QueryRowContext(ctx, `SELECT thread_id, ts_ms FROM sidecar_e2ee_messages WHERE id = ?`, m.ID).Scan(&oldThread, &oldTS)
	if errors.Is(err, sql.ErrNoRows) {
		existing = false
	} else if err != nil {
		return ioErr("put")
	}

	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return ioErr("put")
	}
	defer func() { _ = tx.Rollback() }()
	if _, err := tx.ExecContext(ctx,
		`INSERT INTO sidecar_e2ee_messages (id, thread_id, ts_ms, json) VALUES (?, ?, ?, ?)
		 ON CONFLICT(id) DO UPDATE SET thread_id = excluded.thread_id, ts_ms = excluded.ts_ms, json = excluded.json`,
		m.ID, m.ThreadID, tsMS, b); err != nil {
		return ioErr("put")
	}
	if len(media) > 0 {
		if _, err := tx.ExecContext(ctx, `DELETE FROM sidecar_e2ee_media WHERE message_id = ?`, m.ID); err != nil {
			return ioErr("put media")
		}
		for _, md := range media {
			if _, err := tx.ExecContext(ctx,
				`INSERT OR REPLACE INTO sidecar_e2ee_media (message_id, attachment_id, media_type, integral, mime, size) VALUES (?, ?, ?, ?, ?, ?)`,
				m.ID, md.AttachmentID, md.MediaType, md.Integral, md.Mime, md.Size); err != nil {
				return ioErr("put media")
			}
		}
	}
	if err := tx.Commit(); err != nil {
		return ioErr("put")
	}

	if existing && oldThread != m.ThreadID {
		delete(s.threads, oldThread)
		delete(s.threads, m.ThreadID)
		return s.pruneLocked(ctx, m.ThreadID)
	}
	st, ok := s.threads[m.ThreadID]
	if !ok {
		return s.pruneLocked(ctx, m.ThreadID)
	}
	if !existing {
		st.count++
	}
	if !st.upsert(entry{ts: tsMS, id: m.ID, json: b}) {
		delete(s.threads, m.ThreadID)
	}
	return s.pruneLocked(ctx, m.ThreadID)
}

// pruneLocked drops rows beyond retainCap once a thread exceeds retainCap+pruneSlack.
func (s *Store) pruneLocked(ctx context.Context, threadID string) error {
	st, err := s.state(ctx, threadID)
	if err != nil {
		return err
	}
	if st.count <= retainCap+pruneSlack {
		return nil
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return ioErr("prune")
	}
	defer func() { _ = tx.Rollback() }()
	const stale = `SELECT id FROM sidecar_e2ee_messages WHERE thread_id = ? ORDER BY ts_ms DESC, id DESC LIMIT -1 OFFSET ?`
	if _, err := tx.ExecContext(ctx, `DELETE FROM sidecar_e2ee_media WHERE message_id IN (`+stale+`)`, threadID, retainCap); err != nil {
		return ioErr("prune")
	}
	if _, err := tx.ExecContext(ctx, `DELETE FROM sidecar_e2ee_messages WHERE id IN (`+stale+`)`, threadID, retainCap); err != nil {
		return ioErr("prune")
	}
	if err := tx.Commit(); err != nil {
		return ioErr("prune")
	}
	delete(s.threads, threadID)
	return nil
}

// load reads one row; found is false when the id is unknown.
func (s *Store) load(ctx context.Context, id string) (m api.Message, ts int64, found bool, err error) {
	var b []byte
	err = s.db.QueryRowContext(ctx, `SELECT ts_ms, json FROM sidecar_e2ee_messages WHERE id = ?`, id).Scan(&ts, &b)
	if errors.Is(err, sql.ErrNoRows) {
		return api.Message{}, 0, false, nil
	} else if err != nil {
		return api.Message{}, 0, false, ioErr("get")
	}
	m, err = decode(b)
	if err != nil {
		return api.Message{}, 0, false, err
	}
	return m, ts, true, nil
}

// save rewrites the json of an existing row and mirrors it in the ring.
func (s *Store) save(ctx context.Context, m api.Message, ts int64, dropMedia bool) error {
	b, err := encode(m)
	if err != nil {
		return err
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return ioErr("update")
	}
	defer func() { _ = tx.Rollback() }()
	if _, err := tx.ExecContext(ctx, `UPDATE sidecar_e2ee_messages SET json = ? WHERE id = ?`, b, m.ID); err != nil {
		return ioErr("update")
	}
	if dropMedia {
		if _, err := tx.ExecContext(ctx, `DELETE FROM sidecar_e2ee_media WHERE message_id = ?`, m.ID); err != nil {
			return ioErr("update")
		}
	}
	if err := tx.Commit(); err != nil {
		return ioErr("update")
	}
	if st, ok := s.threads[m.ThreadID]; ok {
		if i := st.indexOf(m.ID); i >= 0 {
			st.ring[i].json = b
		}
	}
	return nil
}

// Get returns one cached message.
func (s *Store) Get(ctx context.Context, messageID string) (api.Message, bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	m, _, found, err := s.load(ctx, messageID)
	return m, found, err
}

// ErrDenied means an edit or unsend did not come from the stored sender of the message.
var ErrDenied = errors.New("e2ee store: sender may not change this message")

// ApplyEdit sets the text and Edited on a cached message sent by e.SenderID in e.ThreadID.
// A stored message from another sender or thread is left alone: found is true and the error ErrDenied.
func (s *Store) ApplyEdit(ctx context.Context, e api.Edit) (api.Message, bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	m, ts, found, err := s.load(ctx, e.MessageID)
	if err != nil || !found {
		return api.Message{}, false, err
	}
	if m.ThreadID != e.ThreadID || e.SenderID == "" || m.Sender.ID != e.SenderID {
		return api.Message{}, true, ErrDenied
	}
	text := e.Text
	m.Text = &text
	m.Edited = true
	if err := s.save(ctx, m, ts, false); err != nil {
		return api.Message{}, false, err
	}
	return m, true, nil
}

// ApplyRemoval blanks an unsent message and keeps its row, or deletes a delete-for-me message with its media.
// An unsend needs the stored sender (or ByAdmin) in its thread; a delete-for-me is local and not checked.
func (s *Store) ApplyRemoval(ctx context.Context, r api.Removal) (api.Message, bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	m, ts, found, err := s.load(ctx, r.MessageID)
	if err != nil || !found {
		return api.Message{}, false, err
	}
	if r.Unsent && (m.ThreadID != r.ThreadID || r.SenderID == "" || (m.Sender.ID != r.SenderID && !r.ByAdmin)) {
		return api.Message{}, true, ErrDenied
	}
	if r.Unsent {
		m.Text = nil
		m.Attachments = []api.Attachment{}
		m.Unsent = true
		if err := s.save(ctx, m, ts, true); err != nil {
			return api.Message{}, false, err
		}
		return m, true, nil
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return api.Message{}, false, ioErr("remove")
	}
	defer func() { _ = tx.Rollback() }()
	if _, err := tx.ExecContext(ctx, `DELETE FROM sidecar_e2ee_media WHERE message_id = ?`, m.ID); err != nil {
		return api.Message{}, false, ioErr("remove")
	}
	if _, err := tx.ExecContext(ctx, `DELETE FROM sidecar_e2ee_messages WHERE id = ?`, m.ID); err != nil {
		return api.Message{}, false, ioErr("remove")
	}
	if err := tx.Commit(); err != nil {
		return api.Message{}, false, ioErr("remove")
	}
	if st, ok := s.threads[m.ThreadID]; ok {
		st.remove(m.ID)
		st.count--
		if len(st.ring) == 0 && !st.full {
			delete(s.threads, m.ThreadID)
		}
	}
	return m, true, nil
}

// ApplyReaction keeps one reaction per sender: it replaces it, or removes it when Emoji is nil.
func (s *Store) ApplyReaction(ctx context.Context, r api.ReactionEvent) (api.Message, bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	m, ts, found, err := s.load(ctx, r.MessageID)
	if err != nil || !found {
		return api.Message{}, false, err
	}
	out := make([]api.Reaction, 0, len(m.Reactions)+1)
	replaced := false
	for _, re := range m.Reactions {
		if re.SenderID != r.SenderID {
			out = append(out, re)
			continue
		}
		if r.Emoji != nil && !replaced {
			out = append(out, api.Reaction{SenderID: r.SenderID, Emoji: *r.Emoji})
			replaced = true
		}
	}
	if r.Emoji != nil && !replaced {
		out = append(out, api.Reaction{SenderID: r.SenderID, Emoji: *r.Emoji})
	}
	m.Reactions = out
	if err := s.save(ctx, m, ts, false); err != nil {
		return api.Message{}, false, err
	}
	return m, true, nil
}

// List returns up to limit messages of a thread, newest first; the bool reports older rows past the page.
func (s *Store) List(ctx context.Context, threadID string, beforeTSMS *int64, beforeID string, limit int) ([]api.Message, bool, error) {
	if limit < 1 {
		return nil, false, api.Errorf(api.CodeBadParams, "limit must be positive")
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	var idTS int64
	if beforeID != "" {
		err := s.db.QueryRowContext(ctx, `SELECT ts_ms FROM sidecar_e2ee_messages WHERE id = ? AND thread_id = ?`, beforeID, threadID).Scan(&idTS)
		if errors.Is(err, sql.ErrNoRows) {
			return nil, false, api.Errorf(api.CodeMessageNotFound, "message not found")
		} else if err != nil {
			return nil, false, ioErr("list")
		}
	}
	keep := func(ts int64, id string) bool {
		if beforeTSMS != nil && ts >= *beforeTSMS {
			return false
		}
		if beforeID != "" && !(ts < idTS || (ts == idTS && id < beforeID)) {
			return false
		}
		return true
	}

	st, err := s.state(ctx, threadID)
	if err != nil {
		return nil, false, err
	}
	var picked [][]byte
	for i := len(st.ring) - 1; i >= 0 && len(picked) <= limit; i-- {
		if keep(st.ring[i].ts, st.ring[i].id) {
			picked = append(picked, st.ring[i].json)
		}
	}
	if !st.full && len(picked) <= limit {
		var err error
		picked, err = s.listDisk(ctx, threadID, beforeTSMS, beforeID, idTS, limit+1)
		if err != nil {
			return nil, false, err
		}
	}

	hasMore := len(picked) > limit
	if hasMore {
		picked = picked[:limit]
	}
	out := make([]api.Message, 0, len(picked))
	for _, b := range picked {
		m, err := decode(b)
		if err != nil {
			return nil, false, err
		}
		out = append(out, m)
	}
	return out, hasMore, nil
}

func (s *Store) listDisk(ctx context.Context, threadID string, beforeTSMS *int64, beforeID string, idTS int64, n int) ([][]byte, error) {
	q := `SELECT json FROM sidecar_e2ee_messages WHERE thread_id = ?`
	args := []any{threadID}
	if beforeTSMS != nil {
		q += ` AND ts_ms < ?`
		args = append(args, *beforeTSMS)
	}
	if beforeID != "" {
		q += ` AND (ts_ms < ? OR (ts_ms = ? AND id < ?))`
		args = append(args, idTS, idTS, beforeID)
	}
	q += ` ORDER BY ts_ms DESC, id DESC LIMIT ?`
	args = append(args, n)
	rows, err := s.db.QueryContext(ctx, q, args...)
	if err != nil {
		return nil, ioErr("list")
	}
	defer rows.Close()
	var out [][]byte
	for rows.Next() {
		var b []byte
		if err := rows.Scan(&b); err != nil {
			return nil, ioErr("list")
		}
		out = append(out, b)
	}
	if err := rows.Err(); err != nil {
		return nil, ioErr("list")
	}
	return out, nil
}

// Media returns the stored download info of one attachment.
func (s *Store) Media(ctx context.Context, messageID, attachmentID string) (api.E2EEMedia, bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	md := api.E2EEMedia{MessageID: messageID, AttachmentID: attachmentID}
	err := s.db.QueryRowContext(ctx,
		`SELECT media_type, integral, mime, size FROM sidecar_e2ee_media WHERE message_id = ? AND attachment_id = ?`,
		messageID, attachmentID).Scan(&md.MediaType, &md.Integral, &md.Mime, &md.Size)
	if errors.Is(err, sql.ErrNoRows) {
		return api.E2EEMedia{}, false, nil
	} else if err != nil {
		return api.E2EEMedia{}, false, ioErr("media")
	}
	return md, true, nil
}
