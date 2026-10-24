package engine

import (
	"context"
	"encoding/json"
	"slices"
	"time"

	"go.mau.fi/mautrix-meta/pkg/messagix/table"
	"go.mau.fi/mautrix-meta/pkg/metaid"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
	"github.com/matpb/meta-mcp-rs/sidecars/messenger/e2ee"
	"github.com/matpb/meta-mcp-rs/sidecars/messenger/normalize"
)

// A history event carries at most this many messages and about this many bytes of JSON.
const (
	maxHistoryBatch      = 200
	maxHistoryBatchBytes = 4 << 20
)

type tableMode int

const (
	modeLive     tableMode = iota // pushed by the socket
	modeInitial                   // the page load table
	modeBackfill                  // thread or message fetch responses
)

func (s *session) normContext() normalize.Context {
	return normalize.Context{
		Me:       s.me.Load(),
		Name:     s.names.get,
		ThreadID: s.reg.threadID,
	}
}

func fbMsgID(mid string) string { return string(metaid.MakeFBMessageID(mid)) }

func previewOf(m api.Message) string {
	if m.Text != nil && *m.Text != "" {
		return *m.Text
	}
	if len(m.Attachments) > 0 {
		return "[" + m.Attachments[0].Kind + "]"
	}
	return ""
}

func (s *session) emitMessage(m api.Message) {
	dir := "in"
	if m.Sender.IsMe {
		dir = "out"
	}
	s.emit(api.EvMessage, api.MessageEvent{Message: m, Direction: dir})
}

func (s *session) emitThreads(ids []string) {
	for _, id := range ids {
		if t, ok := s.reg.snapshot(id); ok {
			s.emit(api.EvThread, api.ThreadEvent{Thread: t})
		}
	}
}

func applyReaction(m api.Message, r api.ReactionEvent) api.Message {
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
	return m
}

// processTable folds one Lightspeed table into the registry, caches and event stream.
func (s *session) processTable(tbl *table.LSTable, mode tableMode) {
	if tbl == nil {
		return
	}
	s.procMu.Lock()
	defer s.procMu.Unlock()

	for _, c := range tbl.LSDeleteThenInsertContact {
		s.names.set(c.GetFBID(), c.GetName())
	}
	for _, c := range tbl.LSVerifyContactRowExists {
		s.names.set(c.GetFBID(), c.GetName())
	}
	s.emitThreads(s.reg.ingest(tbl))

	b := normalize.Table(tbl, s.normContext())
	upserted := make(map[string]bool, len(tbl.LSUpsertMessage))
	for _, u := range tbl.LSUpsertMessage {
		upserted[fbMsgID(u.MessageId)] = true
	}
	history := map[string][]api.Message{}
	forCollector := map[string][]cachedMsg{}
	var attachments []e2ee.FBAttachments
	for _, m := range b.Messages {
		ts := b.TSMS[m.ID]
		isHistory := mode != modeLive || upserted[m.ID]
		if isHistory {
			forCollector[m.ThreadID] = append(forCollector[m.ThreadID], cachedMsg{m: m, ts: ts})
		}
		if !s.hist.put(m, ts, !isHistory) {
			continue
		}
		if row, ok := attachmentRow(m, ts); ok {
			attachments = append(attachments, row)
		}
		if isHistory {
			history[m.ThreadID] = append(history[m.ThreadID], m)
			continue
		}
		s.reg.touch(m.ThreadID, ts, previewOf(m), m.Sender.IsMe)
		s.emitMessage(m)
	}

	s.indexAttachments(attachments)

	complete := map[string]bool{}
	for _, r := range tbl.LSInsertNewMessageRange {
		tid := s.reg.threadID(r.ThreadKey)
		complete[tid] = !r.HasMoreBefore
		col := s.collectorFor(tid)
		adjacent := false
		if col != nil {
			if ref, _ := col.ref(); r.MaxTimestampMsTemplate <= ref {
				adjacent = s.hist.extendAdjacent(tid, ref, r.MinTimestampMs, r.HasMoreBefore)
			}
		}
		if !adjacent {
			s.hist.addRange(tid, r.MinTimestampMs, r.MaxTimestampMs, r.HasMoreBefore)
		}
		if col != nil {
			rng := &api.MessageRange{ThreadID: tid, FBKey: r.ThreadKey, MinTSMS: r.MinTimestampMs, HasMoreBefore: r.HasMoreBefore}
			if r.MinMessageId != "" {
				rng.MinMessageID = fbMsgID(r.MinMessageId)
			}
			col.feed(forCollector[tid], rng, r.MaxTimestampMsTemplate)
		}
	}
	for _, r := range tbl.LSUpdateExistingMessageRange {
		if col := s.collectorFor(s.reg.threadID(r.ThreadKey)); col != nil {
			col.existingRange(r.TimestampMS, r.UnknownBool2, r.UnknownBool3)
		}
	}

	tids := make([]string, 0, len(history))
	for tid := range history {
		tids = append(tids, tid)
	}
	slices.SortFunc(tids, cmpID)
	for _, tid := range tids {
		if s.collectorFor(tid) != nil {
			continue
		}
		batches := splitHistory(history[tid])
		for i, batch := range batches {
			s.emit(api.EvHistory, api.HistoryEvent{ThreadID: tid, Messages: batch, Complete: complete[tid] && i == len(batches)-1})
		}
	}

	for _, e := range b.Edits {
		s.applyEdit(e)
	}
	for _, r := range b.Removals {
		s.applyRemoval(r)
	}
	if mode == modeLive {
		for _, r := range b.Reactions {
			s.applyReactionEvent(r)
		}
		for _, r := range b.Receipts {
			s.emit(api.EvReceipt, r)
		}
	}
	if len(b.Sends) > 0 {
		s.onSendOutcomes(b.Sends)
	}
}

func (s *session) applyEdit(e api.Edit) {
	text := e.Text
	if m, _, ok := s.hist.get(e.MessageID); ok {
		if m.Edited && m.Text != nil && *m.Text == text {
			return
		}
		m.Text = &text
		m.Edited = true
		if m.Kind == api.MsgUnsupported && text != "" {
			m.Kind = api.MsgText
		}
		s.hist.replace(m)
		s.emitMessage(m)
		return
	}
	s.emitMessage(api.Message{
		ID: e.MessageID, ThreadID: e.ThreadID, TS: e.TS, Text: &text, Attachments: []api.Attachment{},
		Reactions: []api.Reaction{}, Edited: true, Kind: api.MsgText,
	})
}

func (s *session) applyRemoval(r api.Removal) {
	s.dropAttachments(r.MessageID)
	m, _, ok := s.hist.get(r.MessageID)
	if !r.Unsent {
		s.hist.remove(r.MessageID)
		return
	}
	if !ok {
		s.emitMessage(api.Message{
			ID: r.MessageID, ThreadID: r.ThreadID, Attachments: []api.Attachment{}, Reactions: []api.Reaction{},
			Unsent: true, Kind: api.MsgUnsupported,
		})
		return
	}
	m.Text = nil
	m.Attachments = []api.Attachment{}
	m.Unsent = true
	m.Kind = api.MsgUnsupported
	s.hist.replace(m)
	s.emitMessage(m)
}

func (s *session) applyReactionEvent(r api.ReactionEvent) {
	if m, _, ok := s.hist.get(r.MessageID); ok {
		s.hist.replace(applyReaction(m, r))
	}
	if r.TS == 0 {
		r.TS = time.Now().Unix()
	}
	s.emit(api.EvReaction, r)
}

// splitHistory cuts one thread's messages into batches within the count and size caps of a history event.
func splitHistory(msgs []api.Message) [][]api.Message {
	var out [][]api.Message
	var cur []api.Message
	size := 0
	for _, m := range msgs {
		n := messageSize(m)
		if len(cur) > 0 && (len(cur) >= maxHistoryBatch || size+n > maxHistoryBatchBytes) {
			out = append(out, cur)
			cur, size = nil, 0
		}
		cur = append(cur, m)
		size += n
	}
	if len(cur) > 0 {
		out = append(out, cur)
	}
	return out
}

// messageSize is the serialised size of a message; an unencodable one counts as a full batch.
func messageSize(m api.Message) int {
	b, err := json.Marshal(m)
	if err != nil {
		return maxHistoryBatchBytes
	}
	return len(b)
}

// attachmentRow is the persistent download record of a message, if it carries downloadable media.
func attachmentRow(m api.Message, ts int64) (e2ee.FBAttachments, bool) {
	var atts []api.Attachment
	for _, a := range m.Attachments {
		if downloadable(a) == nil {
			atts = append(atts, a)
		}
	}
	if len(atts) == 0 {
		return e2ee.FBAttachments{}, false
	}
	return e2ee.FBAttachments{MessageID: m.ID, ThreadID: m.ThreadID, TSMS: ts, Attachments: atts}, true
}

// indexAttachments persists download records so download_attachment survives a restart; failures only cost that.
func (s *session) indexAttachments(rows []e2ee.FBAttachments) {
	if len(rows) == 0 {
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), e2eeOpTimeout)
	defer cancel()
	st, err := s.ensureStore(ctx)
	if err != nil {
		return
	}
	if err := st.PutFBAttachments(ctx, rows); err != nil {
		s.log.Warn().Msg("indexing attachments failed")
	}
}

// dropAttachments forgets the download record of a message that was unsent or deleted.
func (s *session) dropAttachments(messageID string) {
	ctx, cancel := context.WithTimeout(context.Background(), e2eeOpTimeout)
	defer cancel()
	st, err := s.ensureStore(ctx)
	if err != nil {
		return
	}
	if err := st.DropFBAttachments(ctx, messageID); err != nil {
		s.log.Warn().Msg("dropping an attachment record failed")
	}
}
