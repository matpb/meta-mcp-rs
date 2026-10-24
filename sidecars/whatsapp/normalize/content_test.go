package normalize

import (
	"testing"

	"go.mau.fi/whatsmeow/proto/waE2E"
	"google.golang.org/protobuf/proto"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
)

func sp(s string) *string { return &s }
func ip(i int64) *int64   { return &i }

func eqStr(a, b *string) bool {
	if a == nil || b == nil {
		return a == b
	}
	return *a == *b
}

func eqInt(a, b *int64) bool {
	if a == nil || b == nil {
		return a == b
	}
	return *a == *b
}

func eqAtt(a, b api.Attachment) bool {
	return a.ID == b.ID && a.Kind == b.Kind && eqStr(a.Mime, b.Mime) && eqStr(a.Name, b.Name) &&
		eqInt(a.Size, b.Size) && eqInt(a.Width, b.Width) && eqInt(a.Height, b.Height) &&
		eqInt(a.DurationMS, b.DurationMS) && eqStr(a.URL, b.URL) && eqInt(a.ExpiresAt, b.ExpiresAt)
}

func TestExtract(t *testing.T) {
	tests := []struct {
		name    string
		msg     *waE2E.Message
		kind    string
		text    *string
		replyTo *string
		atts    []api.Attachment
	}{
		{
			name: "conversation",
			msg:  &waE2E.Message{Conversation: proto.String("hello")},
			kind: api.MsgText, text: sp("hello"),
		},
		{
			name: "extended text with reply",
			msg: &waE2E.Message{ExtendedTextMessage: &waE2E.ExtendedTextMessage{
				Text:        proto.String("sure"),
				ContextInfo: &waE2E.ContextInfo{StanzaID: proto.String("QUOTED1")},
			}},
			kind: api.MsgText, text: sp("sure"), replyTo: sp("QUOTED1"),
		},
		{
			name: "extended text with link preview",
			msg: &waE2E.Message{ExtendedTextMessage: &waE2E.ExtendedTextMessage{
				Text:        proto.String("look https://example.com/a"),
				MatchedText: proto.String("https://example.com/a"),
				Title:       proto.String("Example"),
			}},
			kind: api.MsgLink, text: sp("look https://example.com/a"),
			atts: []api.Attachment{{ID: "0", Kind: api.AttLink, Name: sp("Example"), URL: sp("https://example.com/a")}},
		},
		{
			name: "image with caption and dimensions",
			msg: &waE2E.Message{ImageMessage: &waE2E.ImageMessage{
				Caption: proto.String("sunset"), Mimetype: proto.String("image/jpeg"),
				FileLength: proto.Uint64(2048), Width: proto.Uint32(640), Height: proto.Uint32(480),
				ContextInfo: &waE2E.ContextInfo{StanzaID: proto.String("Q2")},
			}},
			kind: api.MsgMedia, text: sp("sunset"), replyTo: sp("Q2"),
			atts: []api.Attachment{{ID: "0", Kind: api.AttImage, Mime: sp("image/jpeg"), Size: ip(2048), Width: ip(640), Height: ip(480)}},
		},
		{
			name: "image without caption",
			msg:  &waE2E.Message{ImageMessage: &waE2E.ImageMessage{Mimetype: proto.String("image/png")}},
			kind: api.MsgMedia,
			atts: []api.Attachment{{ID: "0", Kind: api.AttImage, Mime: sp("image/png")}},
		},
		{
			name: "video with duration",
			msg: &waE2E.Message{VideoMessage: &waE2E.VideoMessage{
				Mimetype: proto.String("video/mp4"), Seconds: proto.Uint32(12), FileLength: proto.Uint64(9000),
			}},
			kind: api.MsgMedia,
			atts: []api.Attachment{{ID: "0", Kind: api.AttVideo, Mime: sp("video/mp4"), Size: ip(9000), DurationMS: ip(12000)}},
		},
		{
			name: "video gif playback",
			msg: &waE2E.Message{VideoMessage: &waE2E.VideoMessage{
				Mimetype: proto.String("video/mp4"), GifPlayback: proto.Bool(true), Caption: proto.String("lol"),
			}},
			kind: api.MsgMedia, text: sp("lol"),
			atts: []api.Attachment{{ID: "0", Kind: api.AttGIF, Mime: sp("video/mp4")}},
		},
		{
			name: "audio push to talk",
			msg: &waE2E.Message{AudioMessage: &waE2E.AudioMessage{
				Mimetype: proto.String("audio/ogg; codecs=opus"), Seconds: proto.Uint32(7), PTT: proto.Bool(true),
				FileLength: proto.Uint64(5000),
			}},
			kind: api.MsgMedia,
			atts: []api.Attachment{{ID: "0", Kind: api.AttAudio, Mime: sp("audio/ogg; codecs=opus"), Size: ip(5000), DurationMS: ip(7000)}},
		},
		{
			name: "audio plain",
			msg:  &waE2E.Message{AudioMessage: &waE2E.AudioMessage{Mimetype: proto.String("audio/mpeg")}},
			kind: api.MsgMedia,
			atts: []api.Attachment{{ID: "0", Kind: api.AttAudio, Mime: sp("audio/mpeg")}},
		},
		{
			name: "document",
			msg: &waE2E.Message{DocumentMessage: &waE2E.DocumentMessage{
				FileName: proto.String("report.pdf"), Mimetype: proto.String("application/pdf"), FileLength: proto.Uint64(123456),
			}},
			kind: api.MsgMedia,
			atts: []api.Attachment{{ID: "0", Kind: api.AttFile, Mime: sp("application/pdf"), Name: sp("report.pdf"), Size: ip(123456)}},
		},
		{
			name: "document with caption",
			msg: &waE2E.Message{DocumentMessage: &waE2E.DocumentMessage{
				FileName: proto.String("a.txt"), Caption: proto.String("notes"),
			}},
			kind: api.MsgMedia, text: sp("notes"),
			atts: []api.Attachment{{ID: "0", Kind: api.AttFile, Name: sp("a.txt")}},
		},
		{
			name: "sticker",
			msg: &waE2E.Message{StickerMessage: &waE2E.StickerMessage{
				Mimetype: proto.String("image/webp"), Width: proto.Uint32(512), Height: proto.Uint32(512), FileLength: proto.Uint64(30000),
			}},
			kind: api.MsgSticker,
			atts: []api.Attachment{{ID: "0", Kind: api.AttSticker, Mime: sp("image/webp"), Size: ip(30000), Width: ip(512), Height: ip(512)}},
		},
		{
			name: "location with name",
			msg: &waE2E.Message{LocationMessage: &waE2E.LocationMessage{
				DegreesLatitude: proto.Float64(45.4), DegreesLongitude: proto.Float64(-71.9),
				Name: proto.String("Cafe"), Address: proto.String("1 Main St"),
			}},
			kind: api.MsgLocation,
			atts: []api.Attachment{{ID: "0", Kind: api.AttLocation, Name: sp("Cafe"), URL: sp("geo:45.4,-71.9")}},
		},
		{
			name: "location falls back to address",
			msg: &waE2E.Message{LocationMessage: &waE2E.LocationMessage{
				DegreesLatitude: proto.Float64(1), DegreesLongitude: proto.Float64(2), Address: proto.String("1 Main St"),
			}},
			kind: api.MsgLocation,
			atts: []api.Attachment{{ID: "0", Kind: api.AttLocation, Name: sp("1 Main St"), URL: sp("geo:1,2")}},
		},
		{
			name: "live location",
			msg: &waE2E.Message{LiveLocationMessage: &waE2E.LiveLocationMessage{
				DegreesLatitude: proto.Float64(10.5), DegreesLongitude: proto.Float64(20.25),
			}},
			kind: api.MsgLocation,
			atts: []api.Attachment{{ID: "0", Kind: api.AttLocation, URL: sp("geo:10.5,20.25")}},
		},
		{
			name: "call log",
			msg:  &waE2E.Message{CallLogMesssage: &waE2E.CallLogMessage{IsVideo: proto.Bool(true)}},
			kind: api.MsgCall,
		},
		{
			name: "contact is unsupported with display name",
			msg:  &waE2E.Message{ContactMessage: &waE2E.ContactMessage{DisplayName: proto.String("Ada")}},
			kind: api.MsgUnsupported, text: sp("Ada"),
		},
		{
			name: "poll is unsupported with question",
			msg: &waE2E.Message{PollCreationMessage: &waE2E.PollCreationMessage{
				Name:    proto.String("Lunch?"),
				Options: []*waE2E.PollCreationMessage_Option{{OptionName: proto.String("yes")}},
			}},
			kind: api.MsgUnsupported, text: sp("Lunch?"),
		},
		{
			name: "group invite",
			msg:  &waE2E.Message{GroupInviteMessage: &waE2E.GroupInviteMessage{GroupName: proto.String("Team")}},
			kind: api.MsgUnsupported, text: sp("Team"),
		},
		{
			name: "unknown content",
			msg:  &waE2E.Message{ListMessage: &waE2E.ListMessage{}},
			kind: api.MsgUnsupported,
		},
		{
			name: "nil message",
			msg:  nil,
			kind: api.MsgUnsupported,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := Extract(tc.msg)
			if got.Kind != tc.kind {
				t.Errorf("kind = %q, want %q", got.Kind, tc.kind)
			}
			if !eqStr(got.Text, tc.text) {
				t.Errorf("text = %v, want %v", deref(got.Text), deref(tc.text))
			}
			if !eqStr(got.ReplyTo, tc.replyTo) {
				t.Errorf("reply_to = %v, want %v", deref(got.ReplyTo), deref(tc.replyTo))
			}
			if got.Attachments == nil {
				t.Errorf("attachments is nil, want non-nil")
			}
			if len(got.Attachments) != len(tc.atts) {
				t.Fatalf("attachments = %d, want %d", len(got.Attachments), len(tc.atts))
			}
			for i := range tc.atts {
				if !eqAtt(got.Attachments[i], tc.atts[i]) {
					t.Errorf("attachment %d = %+v, want %+v", i, got.Attachments[i], tc.atts[i])
				}
			}
		})
	}
}

func deref(s *string) string {
	if s == nil {
		return "<nil>"
	}
	return *s
}
