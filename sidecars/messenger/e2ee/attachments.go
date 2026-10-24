package e2ee

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

// Tunables; tests lower them.
var (
	attachRetain = 10000
	attachSlack  = 500
)

// FBAttachments is the download metadata of one unencrypted message, kept so that
// download_attachment still finds it after a restart.
type FBAttachments struct {
	MessageID   string
	ThreadID    string
	TSMS        int64
	Attachments []api.Attachment
}

// PutFBAttachments upserts the rows in one transaction and prunes the oldest beyond the retention cap.
func (s *Store) PutFBAttachments(ctx context.Context, rows []FBAttachments) error {
	if len(rows) == 0 {
		return nil
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return ioErr("attachments")
	}
	defer func() { _ = tx.Rollback() }()
	for _, r := range rows {
		b, err := json.Marshal(r.Attachments)
		if err != nil {
			return ioErr("attachments")
		}
		if _, err := tx.ExecContext(ctx,
			`INSERT INTO sidecar_fb_attachments (message_id, thread_id, ts_ms, attachments) VALUES (?, ?, ?, ?)
			 ON CONFLICT(message_id) DO UPDATE SET thread_id = excluded.thread_id, ts_ms = excluded.ts_ms, attachments = excluded.attachments`,
			r.MessageID, r.ThreadID, r.TSMS, b); err != nil {
			return ioErr("attachments")
		}
	}
	var n int
	if err := tx.QueryRowContext(ctx, `SELECT COUNT(*) FROM sidecar_fb_attachments`).Scan(&n); err != nil {
		return ioErr("attachments")
	}
	if n > attachRetain+attachSlack {
		if _, err := tx.ExecContext(ctx,
			`DELETE FROM sidecar_fb_attachments WHERE message_id IN (
				SELECT message_id FROM sidecar_fb_attachments ORDER BY ts_ms DESC, message_id DESC LIMIT -1 OFFSET ?)`,
			attachRetain); err != nil {
			return ioErr("attachments")
		}
	}
	if err := tx.Commit(); err != nil {
		return ioErr("attachments")
	}
	return nil
}

// FBAttachments returns the stored download metadata of one unencrypted message.
func (s *Store) FBAttachments(ctx context.Context, messageID string) (FBAttachments, bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := FBAttachments{MessageID: messageID}
	var b []byte
	err := s.db.QueryRowContext(ctx,
		`SELECT thread_id, ts_ms, attachments FROM sidecar_fb_attachments WHERE message_id = ?`, messageID).
		Scan(&out.ThreadID, &out.TSMS, &b)
	if errors.Is(err, sql.ErrNoRows) {
		return FBAttachments{}, false, nil
	} else if err != nil {
		return FBAttachments{}, false, ioErr("attachments")
	}
	if err := json.Unmarshal(b, &out.Attachments); err != nil {
		return FBAttachments{}, false, ioErr("attachments")
	}
	return out, true, nil
}

// DropFBAttachments forgets the stored metadata of a message that was unsent or deleted.
func (s *Store) DropFBAttachments(ctx context.Context, messageID string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, err := s.db.ExecContext(ctx, `DELETE FROM sidecar_fb_attachments WHERE message_id = ?`, messageID); err != nil {
		return ioErr("attachments")
	}
	return nil
}
