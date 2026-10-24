package normalize

import (
	"testing"

	"go.mau.fi/mautrix-meta/pkg/messagix/table"
)

const (
	tk1  = int64(1000001)
	meID = int64(111)
	them = int64(222)
)

func ctx() Context {
	return Context{Me: meID, Name: func(id int64) string {
		if id == them {
			return "Alice"
		}
		return ""
	}}
}

func insertMsg(mid string, ts int64) *table.LSInsertMessage {
	return &table.LSInsertMessage{MessageId: mid, ThreadKey: tk1, TimestampMs: ts, SenderId: them, Text: "hello"}
}

func only(t *testing.T, b Batch) {
	t.Helper()
	if len(b.Messages) != 1 {
		t.Fatalf("want 1 message, got %d", len(b.Messages))
	}
}

func TestTablePlainText(t *testing.T) {
	tbl := &table.LSTable{LSInsertMessage: []*table.LSInsertMessage{insertMsg("mid.$x", 1700000000123)}}
	b := Table(tbl, ctx())
	only(t, b)
	m := b.Messages[0]
	if m.ID != "fb:mid.$x" || m.ThreadID != "1000001" || m.Sender.ID != "222" || m.Sender.Name != "Alice" || m.Sender.IsMe {
		t.Fatalf("unexpected message %+v", m)
	}
	if m.Text == nil || *m.Text != "hello" || m.Kind != "text" || m.Encrypted || m.Edited || m.Unsent {
		t.Fatalf("unexpected body %+v", m)
	}
	if m.TS != 1700000000 || b.TSMS["fb:mid.$x"] != 1700000000123 {
		t.Fatalf("ts %d / %d", m.TS, b.TSMS["fb:mid.$x"])
	}
	if m.Attachments == nil || m.Reactions == nil || m.ReplyTo != nil {
		t.Fatalf("slices must be non-nil and no reply: %+v", m)
	}
}

func TestTableOwnMessageAndEmptyText(t *testing.T) {
	msg := insertMsg("mid.$own", 1000)
	msg.SenderId = meID
	msg.Text = ""
	b := Table(&table.LSTable{LSInsertMessage: []*table.LSInsertMessage{msg}}, ctx())
	only(t, b)
	m := b.Messages[0]
	if !m.Sender.IsMe || m.Text != nil || m.Kind != "unsupported" {
		t.Fatalf("unexpected %+v", m)
	}
}

func TestTableReply(t *testing.T) {
	msg := insertMsg("mid.$r", 2000)
	msg.ReplySourceId = "mid.$src"
	msg.ReplySourceType = 1
	b := Table(&table.LSTable{LSInsertMessage: []*table.LSInsertMessage{msg}}, ctx())
	only(t, b)
	if b.Messages[0].ReplyTo == nil || *b.Messages[0].ReplyTo != "fb:mid.$src" {
		t.Fatalf("reply_to %v", b.Messages[0].ReplyTo)
	}
}

func TestTableReactions(t *testing.T) {
	tbl := &table.LSTable{
		LSInsertNewMessageRange: []*table.LSInsertNewMessageRange{{ThreadKey: tk1}},
		LSUpsertMessage:         []*table.LSUpsertMessage{{MessageId: "mid.$a", ThreadKey: tk1, TimestampMs: 5000, SenderId: them, Text: "yo"}},
		LSUpsertReaction: []*table.LSUpsertReaction{
			{ThreadKey: tk1, MessageId: "mid.$a", ActorId: meID, Reaction: "\U0001F44D", TimestampMs: 6000},
		},
		LSDeleteReaction: []*table.LSDeleteReaction{{ThreadKey: tk1, MessageId: "mid.$a", ActorId: them}},
	}
	b := Table(tbl, ctx())
	only(t, b)
	r := b.Messages[0].Reactions
	if len(r) != 1 || r[0].SenderID != "111" || r[0].Emoji != "\U0001F44D" {
		t.Fatalf("joined reactions %+v", r)
	}
	if len(b.Reactions) != 2 {
		t.Fatalf("want 2 events, got %d", len(b.Reactions))
	}
	up, del := b.Reactions[0], b.Reactions[1]
	if up.Emoji == nil || *up.Emoji != "\U0001F44D" || up.MessageID != "fb:mid.$a" || up.ThreadID != "1000001" || up.SenderID != "111" || up.TS != 6 {
		t.Fatalf("upsert %+v", up)
	}
	if del.Emoji != nil || del.SenderID != "222" || del.MessageID != "fb:mid.$a" {
		t.Fatalf("delete %+v", del)
	}
}

func withBlob(blob *table.LSInsertBlobAttachment) *table.LSTable {
	blob.MessageId = "mid.$img"
	blob.ThreadKey = tk1
	return &table.LSTable{
		LSInsertMessage:        []*table.LSInsertMessage{insertMsg("mid.$img", 3000)},
		LSInsertBlobAttachment: []*table.LSInsertBlobAttachment{blob},
	}
}

func TestTableImageExplicitExpiry(t *testing.T) {
	b := Table(withBlob(&table.LSInsertBlobAttachment{
		AttachmentFbid: "fbid1", AttachmentType: table.AttachmentTypeImage, Filename: "p.jpg", Filesize: 2048,
		PlayableUrl: "https://cdn.example/p.jpg?oe=65A1B2C3", PlayableUrlMimeType: "image/jpeg",
		PlayableUrlExpirationTimestampMs: 1700000000999,
	}), ctx())
	only(t, b)
	m := b.Messages[0]
	if m.Kind != "media" || len(m.Attachments) != 1 {
		t.Fatalf("unexpected %+v", m)
	}
	a := m.Attachments[0]
	if a.ID != "fbid1" || a.Kind != "image" || a.Mime == nil || *a.Mime != "image/jpeg" || a.URL == nil || *a.URL != "https://cdn.example/p.jpg?oe=65A1B2C3" {
		t.Fatalf("attachment %+v", a)
	}
	if a.ExpiresAt == nil || *a.ExpiresAt != 1700000000 || a.Size == nil || *a.Size != 2048 || a.Name == nil || *a.Name != "p.jpg" {
		t.Fatalf("expiry/size/name %+v", a)
	}
}

func TestTableImageExpiryFromURL(t *testing.T) {
	b := Table(withBlob(&table.LSInsertBlobAttachment{
		AttachmentType: table.AttachmentTypeImage, PreviewUrl: "https://cdn.example/p.jpg?oe=65A1B2C3",
		PreviewUrlMimeType: "image/jpeg", PreviewWidth: 640, PreviewHeight: 480,
	}), ctx())
	only(t, b)
	a := b.Messages[0].Attachments[0]
	if a.ID != "att:0" || a.ExpiresAt == nil || *a.ExpiresAt != 0x65A1B2C3 {
		t.Fatalf("attachment %+v", a)
	}
	if a.Width == nil || *a.Width != 640 || a.Height == nil || *a.Height != 480 {
		t.Fatalf("dims %+v", a)
	}
}

func TestTableSticker(t *testing.T) {
	tbl := &table.LSTable{
		LSInsertMessage: []*table.LSInsertMessage{insertMsg("mid.$s", 4000)},
		LSInsertStickerAttachment: []*table.LSInsertStickerAttachment{{
			MessageId: "mid.$s", AttachmentFbid: "st1", PlayableUrl: "https://cdn.example/s.png", PlayableUrlMimeType: "image/png",
		}},
	}
	b := Table(tbl, ctx())
	only(t, b)
	m := b.Messages[0]
	if m.Kind != "sticker" || len(m.Attachments) != 1 || m.Attachments[0].Kind != "sticker" || m.Attachments[0].ID != "st1" {
		t.Fatalf("unexpected %+v", m)
	}
}

func TestTableThumbsUp(t *testing.T) {
	tbl := &table.LSTable{
		LSInsertMessage: []*table.LSInsertMessage{func() *table.LSInsertMessage {
			m := insertMsg("mid.$t", 4000)
			m.Text, m.StickerId = "", 369239263222822
			return m
		}()},
		LSInsertStickerAttachment: []*table.LSInsertStickerAttachment{{MessageId: "mid.$t", PlayableUrl: "https://cdn.example/t.png"}},
	}
	b := Table(tbl, ctx())
	only(t, b)
	m := b.Messages[0]
	if m.Kind != "text" || m.Text == nil || *m.Text != "\U0001F44D" || len(m.Attachments) != 0 {
		t.Fatalf("unexpected %+v", m)
	}
}

func TestTableXMALink(t *testing.T) {
	tbl := &table.LSTable{
		LSInsertMessage: []*table.LSInsertMessage{insertMsg("mid.$l", 5000)},
		LSInsertXmaAttachment: []*table.LSInsertXmaAttachment{{
			MessageId: "mid.$l", AttachmentFbid: "x1", TitleText: "Example",
		}},
		LSInsertAttachmentCta: []*table.LSInsertAttachmentCta{{
			AttachmentFbid: "x1", Type_: "xma_web_url",
			ActionUrl: "https://l.facebook.com/l.php?u=https%3A%2F%2Fexample.com%2Fa%3Fb%3D1&h=zzz",
		}},
	}
	b := Table(tbl, ctx())
	only(t, b)
	m := b.Messages[0]
	if m.Kind != "link" || len(m.Attachments) != 1 {
		t.Fatalf("unexpected %+v", m)
	}
	a := m.Attachments[0]
	if a.Kind != "link" || a.ID != "x1" || a.URL == nil || *a.URL != "https://example.com/a?b=1" || a.Mime != nil || a.ExpiresAt != nil {
		t.Fatalf("attachment %+v", a)
	}
	if a.Name == nil || *a.Name != "Example" {
		t.Fatalf("name %+v", a)
	}
}

func TestTableLiveLocation(t *testing.T) {
	tbl := &table.LSTable{
		LSInsertMessage:       []*table.LSInsertMessage{insertMsg("mid.$g", 5000)},
		LSInsertXmaAttachment: []*table.LSInsertXmaAttachment{{MessageId: "mid.$g", AttachmentFbid: "x2", TitleText: "Spot"}},
		LSInsertAttachmentCta: []*table.LSInsertAttachmentCta{{AttachmentFbid: "x2", Type_: "xma_live_location_sharing", NativeUrl: "45.4,-71.9"}},
	}
	b := Table(tbl, ctx())
	only(t, b)
	m := b.Messages[0]
	if m.Kind != "location" || m.Attachments[0].Kind != "location" || *m.Attachments[0].URL != "geo:45.4,-71.9" {
		t.Fatalf("unexpected %+v", m)
	}
}

func TestTableAdminMessage(t *testing.T) {
	msg := insertMsg("mid.$adm", 5000)
	msg.IsAdminMessage = true
	b := Table(&table.LSTable{LSInsertMessage: []*table.LSInsertMessage{msg}}, ctx())
	only(t, b)
	if b.Messages[0].Kind != "system" {
		t.Fatalf("kind %q", b.Messages[0].Kind)
	}
}

func TestTableUnsendAndDelete(t *testing.T) {
	tbl := &table.LSTable{
		LSDeleteThenInsertMessage: []*table.LSDeleteThenInsertMessage{
			{ThreadKey: tk1, MessageId: "mid.$u", IsUnsent: true},
			{ThreadKey: tk1, MessageId: "mid.$ignored"},
		},
		LSDeleteMessage: []*table.LSDeleteMessage{{ThreadKey: tk1, MessageId: "mid.$d"}},
	}
	b := Table(tbl, ctx())
	if len(b.Removals) != 2 {
		t.Fatalf("removals %+v", b.Removals)
	}
	del, uns := b.Removals[0], b.Removals[1]
	if del.MessageID != "fb:mid.$d" || del.Unsent || del.ThreadID != "1000001" {
		t.Fatalf("delete %+v", del)
	}
	if uns.MessageID != "fb:mid.$u" || !uns.Unsent {
		t.Fatalf("unsent %+v", uns)
	}
}

func TestTableUnsentTombstone(t *testing.T) {
	msg := insertMsg("mid.$tomb", 5000)
	msg.IsUnsent = true
	b := Table(&table.LSTable{LSInsertMessage: []*table.LSInsertMessage{msg}}, ctx())
	only(t, b)
	if !b.Messages[0].Unsent || b.Messages[0].Text != nil {
		t.Fatalf("unexpected %+v", b.Messages[0])
	}
}

func TestTableEdit(t *testing.T) {
	msg := insertMsg("mid.$e", 5000)
	msg.EditCount = 1
	tbl := &table.LSTable{
		LSInsertMessage: []*table.LSInsertMessage{msg},
		LSEditMessage:   []*table.LSEditMessage{{MessageID: "mid.$e", Text: "fixed", EditCount: 1}, {MessageID: "mid.$other", Text: "x"}},
	}
	b := Table(tbl, ctx())
	only(t, b)
	if !b.Messages[0].Edited {
		t.Fatal("edited flag")
	}
	if len(b.Edits) != 2 || b.Edits[0].MessageID != "fb:mid.$e" || b.Edits[0].Text != "fixed" || b.Edits[0].ThreadID != "1000001" || b.Edits[1].ThreadID != "" {
		t.Fatalf("edits %+v", b.Edits)
	}
}

func TestTableSendOutcomes(t *testing.T) {
	tbl := &table.LSTable{
		LSReplaceOptimsiticMessage:    []*table.LSReplaceOptimsiticMessage{{OfflineThreadingId: "otid1", MessageId: "mid.$sent"}},
		LSMarkOptimisticMessageFailed: []*table.LSMarkOptimisticMessageFailed{{OTID: "otid2", Message: "blocked"}},
		LSHandleFailedTask:            []*table.LSHandleFailedTask{{OTID: "otid3", Message: "boom"}, {Message: "no otid"}},
	}
	b := Table(tbl, ctx())
	if len(b.Sends) != 3 {
		t.Fatalf("sends %+v", b.Sends)
	}
	if s := b.Sends[0]; s.OTID != "otid1" || s.MessageID != "fb:mid.$sent" || s.Failed {
		t.Fatalf("ok %+v", s)
	}
	if s := b.Sends[1]; s.OTID != "otid2" || !s.Failed || s.Reason != "blocked" {
		t.Fatalf("failed %+v", s)
	}
	if s := b.Sends[2]; s.OTID != "otid3" || !s.Failed {
		t.Fatalf("task %+v", s)
	}
}

func TestTableRangeAndReceipts(t *testing.T) {
	tbl := &table.LSTable{
		LSInsertNewMessageRange: []*table.LSInsertNewMessageRange{{ThreadKey: tk1, MinTimestampMs: 900, MinMessageId: "mid.$min", HasMoreBefore: true}},
		LSUpdateReadReceipt:     []*table.LSUpdateReadReceipt{{ThreadKey: tk1, ContactId: them, ReadWatermarkTimestampMs: 8123}},
		LSMarkThreadReadV2:      []*table.LSMarkThreadReadV2{{ThreadKey: tk1, LastReadWatermarkTimestampMs: 9000}},
	}
	b := Table(tbl, ctx())
	if len(b.Ranges) != 1 {
		t.Fatalf("ranges %+v", b.Ranges)
	}
	r := b.Ranges[0]
	if r.ThreadID != "1000001" || r.FBKey != tk1 || r.MinTSMS != 900 || r.MinMessageID != "fb:mid.$min" || !r.HasMoreBefore {
		t.Fatalf("range %+v", r)
	}
	if len(b.Receipts) != 1 || b.Receipts[0].Kind != "read" || b.Receipts[0].SenderID != "222" || b.Receipts[0].UpToTS != 8 {
		t.Fatalf("receipts %+v", b.Receipts)
	}
}

func TestTableThreadIDMapping(t *testing.T) {
	c := ctx()
	c.ThreadID = func(k int64) string { return "T" + string(rune('0'+k%10)) }
	tbl := &table.LSTable{
		LSInsertMessage:         []*table.LSInsertMessage{insertMsg("mid.$m", 100)},
		LSInsertNewMessageRange: []*table.LSInsertNewMessageRange{{ThreadKey: tk1}},
		LSUpsertReaction:        []*table.LSUpsertReaction{{ThreadKey: tk1, MessageId: "mid.$m", ActorId: them, Reaction: "x"}},
		LSDeleteMessage:         []*table.LSDeleteMessage{{ThreadKey: tk1, MessageId: "mid.$m"}},
		LSUpdateReadReceipt:     []*table.LSUpdateReadReceipt{{ThreadKey: tk1, ContactId: them}},
	}
	b := Table(tbl, c)
	if b.Messages[0].ThreadID != "T1" || b.Ranges[0].ThreadID != "T1" || b.Reactions[0].ThreadID != "T1" || b.Removals[0].ThreadID != "T1" || b.Receipts[0].ThreadID != "T1" {
		t.Fatalf("mapping not applied: %+v", b)
	}
}

func TestTableOrderingAndNil(t *testing.T) {
	if b := Table(nil, ctx()); len(b.Messages) != 0 || b.TSMS == nil {
		t.Fatalf("nil table %+v", b)
	}
	tbl := &table.LSTable{
		LSInsertNewMessageRange: []*table.LSInsertNewMessageRange{{ThreadKey: 20}, {ThreadKey: 10}},
		LSUpsertMessage: []*table.LSUpsertMessage{
			{MessageId: "mid.$b", ThreadKey: 20, TimestampMs: 1},
			{MessageId: "mid.$d", ThreadKey: 10, TimestampMs: 9},
			{MessageId: "mid.$c", ThreadKey: 10, TimestampMs: 3},
		},
	}
	b := Table(tbl, ctx())
	var ids []string
	for _, m := range b.Messages {
		ids = append(ids, m.ID)
	}
	want := []string{"fb:mid.$c", "fb:mid.$d", "fb:mid.$b"}
	if len(ids) != 3 || ids[0] != want[0] || ids[1] != want[1] || ids[2] != want[2] {
		t.Fatalf("order %v", ids)
	}
}
