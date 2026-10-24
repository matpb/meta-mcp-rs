package normalize

import (
	"cmp"
	"maps"
	"slices"
	"strconv"
	"strings"

	"go.mau.fi/mautrix-meta/pkg/messagix/table"
	"go.mau.fi/mautrix-meta/pkg/metaid"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

// Context carries the account and lookups the normalisers need.
type Context struct {
	Me       int64                    // own fbid
	Name     func(fbid int64) string  // may be nil; "" when unknown
	ThreadID func(fbKey int64) string // may be nil; nil means the decimal thread key
}

// Batch is everything one LSTable frame says, in protocol shapes.
type Batch struct {
	Messages  []api.Message    // inserts and upserts; upserts first by thread key then time, inserts by time
	TSMS      map[string]int64 // message id -> timestamp in ms, for every entry of Messages
	Edits     []api.Edit
	Removals  []api.Removal
	Reactions []api.ReactionEvent
	Receipts  []api.ReceiptEvent
	Ranges    []api.MessageRange
	Sends     []api.SendOutcome
}

const thumbsUpEmoji = "\U0001F44D"

var thumbsUpStickers = map[int64]bool{
	369239263222822: true,
	369239343222814: true,
	369239383222810: true,
}

func (c Context) thread(key int64) string {
	if c.ThreadID != nil {
		return c.ThreadID(key)
	}
	return strconv.FormatInt(key, 10)
}

func (c Context) name(fbid int64) string {
	if c.Name == nil {
		return ""
	}
	return c.Name(fbid)
}

func fbID(mid string) string {
	return string(metaid.MakeFBMessageID(mid))
}

// Table normalises one LSTable frame. It never mutates tbl beyond what WrapMessages does.
func Table(tbl *table.LSTable, c Context) Batch {
	b := Batch{TSMS: map[string]int64{}}
	if tbl == nil {
		return b
	}
	upsert, insert := tbl.WrapMessages()
	threadOf := map[string]int64{}
	add := func(w *table.WrappedMessage) {
		if w == nil || w.LSInsertMessage == nil {
			return
		}
		m := fbMessage(w, c)
		b.Messages = append(b.Messages, m)
		b.TSMS[m.ID] = w.TimestampMs
		threadOf[w.MessageId] = w.ThreadKey
	}
	for _, key := range slices.Sorted(maps.Keys(upsert)) {
		um := upsert[key]
		msgs := slices.Clone(um.Messages)
		slices.SortStableFunc(msgs, func(a, b *table.WrappedMessage) int {
			return cmp.Compare(a.TimestampMs, b.TimestampMs)
		})
		for _, w := range msgs {
			add(w)
		}
	}
	ins := slices.Clone(insert)
	slices.SortStableFunc(ins, func(a, b *table.WrappedMessage) int {
		return cmp.Compare(a.TimestampMs, b.TimestampMs)
	})
	for _, w := range ins {
		add(w)
	}

	for _, r := range tbl.LSInsertNewMessageRange {
		rng := api.MessageRange{
			ThreadID:      c.thread(r.ThreadKey),
			FBKey:         r.ThreadKey,
			MinTSMS:       r.MinTimestampMs,
			HasMoreBefore: r.HasMoreBefore,
		}
		if r.MinMessageId != "" {
			rng.MinMessageID = fbID(r.MinMessageId)
		}
		b.Ranges = append(b.Ranges, rng)
	}

	for _, e := range tbl.LSEditMessage {
		edit := api.Edit{MessageID: fbID(e.MessageID), Text: e.Text}
		if key, ok := threadOf[e.MessageID]; ok {
			edit.ThreadID = c.thread(key)
		}
		b.Edits = append(b.Edits, edit)
	}
	for _, d := range tbl.LSDeleteMessage {
		b.Removals = append(b.Removals, api.Removal{ThreadID: c.thread(d.ThreadKey), MessageID: fbID(d.MessageId)})
	}
	for _, d := range tbl.LSDeleteThenInsertMessage {
		if !d.IsUnsent {
			continue
		}
		b.Removals = append(b.Removals, api.Removal{ThreadID: c.thread(d.ThreadKey), MessageID: fbID(d.MessageId), Unsent: true})
	}
	for _, r := range tbl.LSUpsertReaction {
		emoji := r.Reaction
		b.Reactions = append(b.Reactions, api.ReactionEvent{
			ThreadID:  c.thread(r.ThreadKey),
			MessageID: fbID(r.MessageId),
			SenderID:  strconv.FormatInt(r.ActorId, 10),
			Emoji:     &emoji,
			TS:        r.TimestampMs / 1000,
		})
	}
	for _, r := range tbl.LSDeleteReaction {
		b.Reactions = append(b.Reactions, api.ReactionEvent{
			ThreadID:  c.thread(r.ThreadKey),
			MessageID: fbID(r.MessageId),
			SenderID:  strconv.FormatInt(r.ActorId, 10),
		})
	}
	for _, r := range tbl.LSUpdateReadReceipt {
		b.Receipts = append(b.Receipts, api.ReceiptEvent{
			ThreadID: c.thread(r.ThreadKey),
			Kind:     "read",
			SenderID: strconv.FormatInt(r.ContactId, 10),
			UpToTS:   r.ReadWatermarkTimestampMs / 1000,
		})
	}
	for _, r := range tbl.LSReplaceOptimsiticMessage {
		b.Sends = append(b.Sends, api.SendOutcome{OTID: r.OfflineThreadingId, MessageID: fbID(r.MessageId)})
	}
	for _, f := range tbl.LSMarkOptimisticMessageFailed {
		b.Sends = append(b.Sends, api.SendOutcome{OTID: f.OTID, Failed: true, Reason: f.Message})
	}
	for _, f := range tbl.LSHandleFailedTask {
		if f.OTID == "" {
			continue
		}
		b.Sends = append(b.Sends, api.SendOutcome{OTID: f.OTID, Failed: true, Reason: f.Message})
	}
	return b
}

func fbMessage(w *table.WrappedMessage, c Context) api.Message {
	m := api.Message{
		ID:       fbID(w.MessageId),
		ThreadID: c.thread(w.ThreadKey),
		TS:       w.TimestampMs / 1000,
		Sender: api.Sender{
			ID:   strconv.FormatInt(w.SenderId, 10),
			Name: c.name(w.SenderId),
			IsMe: c.Me != 0 && w.SenderId == c.Me,
		},
		Attachments: []api.Attachment{},
		Reactions:   []api.Reaction{},
		Edited:      w.EditCount > 0,
		Unsent:      w.IsUnsent,
	}
	for _, r := range w.Reactions {
		m.Reactions = append(m.Reactions, api.Reaction{SenderID: strconv.FormatInt(r.ActorId, 10), Emoji: r.Reaction})
	}
	if w.IsUnsent {
		m.Kind = api.MsgUnsupported
		return m
	}
	if w.ReplySourceId != "" && (w.ReplySourceTypeV2 == table.ReplySourceTypeNone || w.ReplySourceTypeV2 == table.ReplySourceTypeMessage) {
		reply := fbID(w.ReplySourceId)
		m.ReplyTo = &reply
	}

	text := w.Text
	stickers := w.Stickers
	if thumbsUpStickers[w.StickerId] && len(stickers) == 1 {
		text = thumbsUpEmoji
		stickers = nil
	}
	m.Text = strPtr(text)

	seen := map[string]bool{}
	hasMedia, hasSticker, hasLocation, hasLink := false, false, false, false
	for _, blob := range w.BlobAttachments {
		if blob.AttachmentFbid != "" {
			if seen[blob.AttachmentFbid] {
				continue
			}
			seen[blob.AttachmentFbid] = true
		}
		m.Attachments = append(m.Attachments, mediaAttachment(len(m.Attachments), mediaFields{
			fbid: blob.AttachmentFbid, kind: blob.AttachmentType, name: blob.Filename, size: blob.Filesize,
			playableURL: blob.PlayableUrl, playableMime: blob.PlayableUrlMimeType, playableExp: blob.PlayableUrlExpirationTimestampMs,
			previewURL: blob.PreviewUrl, previewMime: blob.PreviewUrlMimeType, previewExp: blob.PreviewUrlExpirationTimestampMs,
			attMime: blob.AttachmentMimeType, w: blob.PreviewWidth, h: blob.PreviewHeight, durationMS: blob.PlayableDurationMs,
		}))
		hasMedia = true
	}
	for _, att := range w.Attachments {
		m.Attachments = append(m.Attachments, mediaAttachment(len(m.Attachments), mediaFields{
			fbid: att.AttachmentFbid, kind: att.AttachmentType, name: att.Filename, size: att.Filesize,
			playableURL: att.PlayableUrl, playableMime: att.PlayableUrlMimeType, playableExp: att.PlayableUrlExpirationTimestampMs,
			previewURL: att.PreviewUrl, previewMime: att.PreviewUrlMimeType, previewExp: att.PreviewUrlExpirationTimestampMs,
			attMime: att.AttachmentMimeType, w: att.PreviewWidth, h: att.PreviewHeight, durationMS: att.PlayableDurationMs,
		}))
		hasMedia = true
	}
	for i, x := range w.XMAAttachments {
		if x.CTA != nil && strings.HasPrefix(x.CTA.Type_, "xma_poll_") {
			continue
		}
		a := xmaAttachment(i, x)
		m.Attachments = append(m.Attachments, a)
		if a.Kind == api.AttLocation {
			hasLocation = true
		} else {
			hasLink = true
		}
	}
	for i, s := range stickers {
		m.Attachments = append(m.Attachments, stickerAttachment(i, s))
		hasSticker = true
	}

	switch {
	case w.IsAdminMessage:
		m.Kind = api.MsgSystem
	case hasSticker:
		m.Kind = api.MsgSticker
	case hasMedia:
		m.Kind = api.MsgMedia
	case hasLocation:
		m.Kind = api.MsgLocation
	case hasLink:
		m.Kind = api.MsgLink
	case m.Text != nil:
		m.Kind = api.MsgText
	default:
		m.Kind = api.MsgUnsupported
	}
	return m
}

type mediaFields struct {
	fbid                      string
	kind                      table.AttachmentType
	name                      string
	size                      int64
	playableURL, playableMime string
	playableExp               int64
	previewURL, previewMime   string
	previewExp                int64
	attMime                   string
	w, h                      int64
	durationMS                int64
}

func expiry(explicitMS int64, u string) *int64 {
	if explicitMS > 0 {
		v := explicitMS / 1000
		return &v
	}
	return ExpiryFromURL(u)
}

func mediaAttachment(idx int, f mediaFields) api.Attachment {
	u, mime, exp := f.playableURL, f.playableMime, f.playableExp
	if mime == "" {
		mime = f.attMime
	}
	var width, height int64
	if u == "" {
		u, mime, exp = f.previewURL, f.previewMime, f.previewExp
		width, height = f.w, f.h
	}
	a := api.Attachment{
		ID:         f.fbid,
		Kind:       attachmentKind(f.kind, mime),
		Mime:       strPtr(mime),
		Name:       strPtr(f.name),
		Size:       posPtr(f.size),
		Width:      posPtr(width),
		Height:     posPtr(height),
		DurationMS: posPtr(f.durationMS),
		URL:        strPtr(u),
	}
	if a.ID == "" {
		a.ID = "att:" + strconv.Itoa(idx)
	}
	if u != "" {
		a.ExpiresAt = expiry(exp, u)
	}
	return a
}

func attachmentKind(t table.AttachmentType, mime string) string {
	switch t {
	case table.AttachmentTypeImage, table.AttachmentTypeEphemeralImage:
		return api.AttImage
	case table.AttachmentTypeAnimatedImage:
		return api.AttGIF
	case table.AttachmentTypeVideo, table.AttachmentTypeEphemeralVideo:
		return api.AttVideo
	case table.AttachmentTypeAudio:
		return api.AttAudio
	case table.AttachmentTypeFile:
		return api.AttFile
	case table.AttachmentTypeSticker:
		return api.AttSticker
	}
	return kindFromMime(mime)
}

func stickerAttachment(idx int, s *table.LSInsertStickerAttachment) api.Attachment {
	u, mime, exp := s.PlayableUrl, s.PlayableUrlMimeType, s.PlayableUrlExpirationTimestampMs
	if u == "" {
		u, mime, exp = s.PreviewUrl, s.PreviewUrlMimeType, s.PreviewUrlExpirationTimestampMs
	}
	a := api.Attachment{
		ID:     s.AttachmentFbid,
		Kind:   api.AttSticker,
		Mime:   strPtr(mime),
		Name:   strPtr(s.AccessibilitySummaryText),
		Width:  posPtr(s.PreviewWidth),
		Height: posPtr(s.PreviewHeight),
		URL:    strPtr(u),
	}
	if a.ID == "" {
		a.ID = "sticker:" + strconv.Itoa(idx)
	}
	if u != "" {
		a.ExpiresAt = expiry(exp, u)
	}
	return a
}

func xmaAttachment(idx int, x *table.WrappedXMA) api.Attachment {
	a := api.Attachment{ID: x.AttachmentFbid, Kind: api.AttLink, Name: strPtr(x.TitleText)}
	if a.ID == "" {
		a.ID = "xma:" + strconv.Itoa(idx)
	}
	action := x.ActionUrl
	if x.CTA != nil && x.CTA.ActionUrl != "" {
		action = x.CTA.ActionUrl
	}
	a.URL = strPtr(removeLPHP(action))
	if x.CTA != nil && x.CTA.Type_ == "xma_live_location_sharing" {
		a.Kind = api.AttLocation
		if x.CTA.NativeUrl != "" {
			geo := "geo:" + x.CTA.NativeUrl
			a.URL = &geo
		}
	}
	return a
}
