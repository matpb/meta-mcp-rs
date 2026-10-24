package normalize

import (
	"testing"
	"time"

	"go.mau.fi/whatsmeow/proto/waCommon"
	"go.mau.fi/whatsmeow/proto/waConsumerApplication"
	"go.mau.fi/whatsmeow/proto/waMediaTransport"
	"go.mau.fi/whatsmeow/proto/waMsgApplication"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	"google.golang.org/protobuf/proto"
)

var (
	chatJID   = types.JID{User: "1234", Server: types.MessengerServer}
	senderJID = types.JID{User: "5678", Server: types.MessengerServer}
	evtTime   = time.Unix(1700000000, 0)
)

func e2eeEvt(id string, content *waConsumerApplication.ConsumerApplication_Content) *events.FBMessage {
	return &events.FBMessage{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{Chat: chatJID, Sender: senderJID},
			ID:            id,
			Timestamp:     evtTime,
		},
		Message: &waConsumerApplication.ConsumerApplication{
			Payload: &waConsumerApplication.ConsumerApplication_Payload{
				Payload: &waConsumerApplication.ConsumerApplication_Payload_Content{Content: content},
			},
		},
	}
}

func textContent(s string) *waConsumerApplication.ConsumerApplication_Content {
	return &waConsumerApplication.ConsumerApplication_Content{
		Content: &waConsumerApplication.ConsumerApplication_Content_MessageText{MessageText: &waCommon.MessageText{Text: proto.String(s)}},
	}
}

func e2eeCtxFor() E2EEContext {
	return E2EEContext{Me: 9999, Name: func(id int64) string {
		if id == 5678 {
			return "Bob"
		}
		return ""
	}}
}

func TestFBMessageTextWithQuote(t *testing.T) {
	evt := e2eeEvt("MSG1", textContent("hello e2ee"))
	evt.FBApplication = &waMsgApplication.MessageApplication{
		Metadata: &waMsgApplication.MessageApplication_Metadata{
			QuotedMessage: &waMsgApplication.MessageApplication_Metadata_QuotedMessage{
				StanzaID: proto.String("QUOTED"), Participant: proto.String("9999@msgr"),
			},
		},
	}
	res := FBMessage(evt, e2eeCtxFor())
	m := res.Message
	if m == nil {
		t.Fatal("no message")
	}
	if m.ID != "wa:1234@msgr:5678@msgr:MSG1" || m.ThreadID != "1234" || !m.Encrypted || m.Kind != "text" {
		t.Fatalf("unexpected %+v", m)
	}
	if m.ReplyTo == nil || *m.ReplyTo != "wa:1234@msgr:9999@msgr:QUOTED" {
		t.Fatalf("reply_to %v", m.ReplyTo)
	}
	if m.Text == nil || *m.Text != "hello e2ee" || m.Sender.ID != "5678" || m.Sender.Name != "Bob" || m.Sender.IsMe {
		t.Fatalf("body/sender %+v", m)
	}
	if m.TS != 1700000000 || res.TSMS != 1700000000000 || len(res.Media) != 0 || m.Attachments == nil {
		t.Fatalf("ts/media %+v", res)
	}
}

func TestFBMessageImage(t *testing.T) {
	integral := &waMediaTransport.WAMediaTransport_Integral{
		FileSHA256: []byte{1, 2}, MediaKey: []byte{3, 4}, FileEncSHA256: []byte{5, 6}, DirectPath: proto.String("/v/t62/abc"),
		MediaKeyTimestamp: proto.Int64(1700000000),
	}
	img := &waConsumerApplication.ConsumerApplication_ImageMessage{Caption: &waCommon.MessageText{Text: proto.String("look")}}
	err := img.Set(&waMediaTransport.ImageTransport{
		Integral: &waMediaTransport.ImageTransport_Integral{Transport: &waMediaTransport.WAMediaTransport{
			Integral:  integral,
			Ancillary: &waMediaTransport.WAMediaTransport_Ancillary{FileLength: proto.Uint64(4321), Mimetype: proto.String("image/jpeg")},
		}},
		Ancillary: &waMediaTransport.ImageTransport_Ancillary{Width: proto.Uint32(800), Height: proto.Uint32(600)},
	})
	if err != nil {
		t.Fatal(err)
	}
	evt := e2eeEvt("IMG1", &waConsumerApplication.ConsumerApplication_Content{
		Content: &waConsumerApplication.ConsumerApplication_Content_ImageMessage{ImageMessage: img},
	})
	res := FBMessage(evt, e2eeCtxFor())
	m := res.Message
	if m == nil || m.Kind != "media" || len(m.Attachments) != 1 {
		t.Fatalf("unexpected %+v", m)
	}
	a := m.Attachments[0]
	if a.ID != "0" || a.Kind != "image" || a.Mime == nil || *a.Mime != "image/jpeg" || a.Size == nil || *a.Size != 4321 || *a.Width != 800 || *a.Height != 600 || a.URL != nil {
		t.Fatalf("attachment %+v", a)
	}
	if m.Text == nil || *m.Text != "look" {
		t.Fatalf("caption %v", m.Text)
	}
	if len(res.Media) != 1 {
		t.Fatalf("media %+v", res.Media)
	}
	md := res.Media[0]
	if md.MessageID != m.ID || md.AttachmentID != "0" || md.MediaType != "WhatsApp Image Keys" || md.Mime != "image/jpeg" || md.Size != 4321 {
		t.Fatalf("media %+v", md)
	}
	var back waMediaTransport.WAMediaTransport_Integral
	if err := proto.Unmarshal(md.Integral, &back); err != nil {
		t.Fatal(err)
	}
	if !proto.Equal(&back, integral) {
		t.Fatalf("integral mismatch: %v vs %v", &back, integral)
	}
}

func TestFBMessageReaction(t *testing.T) {
	evt := e2eeEvt("R1", &waConsumerApplication.ConsumerApplication_Content{
		Content: &waConsumerApplication.ConsumerApplication_Content_ReactionMessage{ReactionMessage: &waConsumerApplication.ConsumerApplication_ReactionMessage{
			Key:               &waCommon.MessageKey{ID: proto.String("TARGET"), Participant: proto.String("9999@msgr")},
			Text:              proto.String("❤"),
			SenderTimestampMS: proto.Int64(1700000005000),
		}},
	})
	res := FBMessage(evt, e2eeCtxFor())
	if res.Message != nil || res.Reaction == nil {
		t.Fatalf("unexpected %+v", res)
	}
	r := res.Reaction
	if r.MessageID != "wa:1234@msgr:9999@msgr:TARGET" || r.ThreadID != "1234" || r.SenderID != "5678" || r.Emoji == nil || *r.Emoji != "❤" || r.TS != 1700000005 {
		t.Fatalf("reaction %+v", r)
	}

	evt = e2eeEvt("R2", &waConsumerApplication.ConsumerApplication_Content{
		Content: &waConsumerApplication.ConsumerApplication_Content_ReactionMessage{ReactionMessage: &waConsumerApplication.ConsumerApplication_ReactionMessage{
			Key:  &waCommon.MessageKey{ID: proto.String("TARGET"), Participant: proto.String("9999@msgr")},
			Text: proto.String(""),
		}},
	})
	res = FBMessage(evt, e2eeCtxFor())
	if res.Reaction == nil || res.Reaction.Emoji != nil || res.Reaction.TS != 1700000000 {
		t.Fatalf("removal %+v", res.Reaction)
	}
}

func TestFBMessageEditAndRevoke(t *testing.T) {
	evt := e2eeEvt("E1", &waConsumerApplication.ConsumerApplication_Content{
		Content: &waConsumerApplication.ConsumerApplication_Content_EditMessage{EditMessage: &waConsumerApplication.ConsumerApplication_EditMessage{
			Key:         &waCommon.MessageKey{ID: proto.String("OLD"), FromMe: proto.Bool(true)},
			Message:     &waCommon.MessageText{Text: proto.String("new text")},
			TimestampMS: proto.Int64(1700000009000),
		}},
	})
	res := FBMessage(evt, e2eeCtxFor())
	if res.Message != nil || res.Edit == nil || res.Edit.MessageID != "wa:1234@msgr:5678@msgr:OLD" || res.Edit.Text != "new text" || res.Edit.TS != 1700000009 {
		t.Fatalf("edit %+v", res)
	}

	rev := &events.FBMessage{
		Info: evt.Info,
		Message: &waConsumerApplication.ConsumerApplication{
			Payload: &waConsumerApplication.ConsumerApplication_Payload{
				Payload: &waConsumerApplication.ConsumerApplication_Payload_ApplicationData{
					ApplicationData: &waConsumerApplication.ConsumerApplication_ApplicationData{
						ApplicationContent: &waConsumerApplication.ConsumerApplication_ApplicationData_Revoke{
							Revoke: &waConsumerApplication.ConsumerApplication_RevokeMessage{
								Key: &waCommon.MessageKey{ID: proto.String("GONE"), FromMe: proto.Bool(true)},
							},
						},
					},
				},
			},
		},
	}
	res = FBMessage(rev, e2eeCtxFor())
	if res.Message != nil || res.Removal == nil || !res.Removal.Unsent || res.Removal.MessageID != "wa:1234@msgr:5678@msgr:GONE" || res.Removal.ThreadID != "1234" {
		t.Fatalf("revoke %+v", res)
	}
}

func TestFBMessageOddInputs(t *testing.T) {
	if res := FBMessage(nil, e2eeCtxFor()); res.Message != nil {
		t.Fatal("nil event")
	}
	if res := FBMessage(&events.FBMessage{}, e2eeCtxFor()); res.Message != nil {
		t.Fatal("empty event")
	}
	res := FBMessage(e2eeEvt("U1", &waConsumerApplication.ConsumerApplication_Content{}), e2eeCtxFor())
	if res.Message == nil || res.Message.Kind != "unsupported" || res.Message.Text != nil {
		t.Fatalf("unsupported %+v", res.Message)
	}
	own := e2eeEvt("O1", textContent("mine"))
	own.Info.IsFromMe = true
	if res := FBMessage(own, E2EEContext{}); res.Message == nil || !res.Message.Sender.IsMe {
		t.Fatal("is_me from IsFromMe")
	}
}

func TestExpiryFromURL(t *testing.T) {
	v := func(n int64) *int64 { return &n }
	cases := []struct {
		url  string
		want *int64
	}{
		{"https://cdn.example/a.jpg?oe=65A1B2C3&oh=x", v(0x65A1B2C3)},
		{"https://cdn.example/a.jpg?oe=0", nil},
		{"https://cdn.example/a.jpg?oe=zzzz", nil},
		{"https://cdn.example/a.jpg?oe=-5", nil},
		{"https://cdn.example/a.jpg", nil},
		{"", nil},
		{"://bad url", nil},
	}
	for _, tc := range cases {
		got := ExpiryFromURL(tc.url)
		if (got == nil) != (tc.want == nil) || (got != nil && *got != *tc.want) {
			t.Errorf("ExpiryFromURL(%q) = %v, want %v", tc.url, got, tc.want)
		}
	}
}

func TestThreadIDFromJID(t *testing.T) {
	if got := ThreadIDFromJID(types.JID{User: "42", Server: types.GroupServer}); got != "42" {
		t.Fatalf("got %q", got)
	}
}

func editEvt(chat, sender types.JID, key *waCommon.MessageKey) *events.FBMessage {
	return &events.FBMessage{
		Info: types.MessageInfo{MessageSource: types.MessageSource{Chat: chat, Sender: sender}, ID: "E", Timestamp: evtTime},
		Message: &waConsumerApplication.ConsumerApplication{
			Payload: &waConsumerApplication.ConsumerApplication_Payload{
				Payload: &waConsumerApplication.ConsumerApplication_Payload_Content{Content: &waConsumerApplication.ConsumerApplication_Content{
					Content: &waConsumerApplication.ConsumerApplication_Content_EditMessage{EditMessage: &waConsumerApplication.ConsumerApplication_EditMessage{
						Key: key, Message: &waCommon.MessageText{Text: proto.String("t")},
					}},
				}},
			},
		},
	}
}

func revokeEvt(chat, sender types.JID, key *waCommon.MessageKey) *events.FBMessage {
	return &events.FBMessage{
		Info: types.MessageInfo{MessageSource: types.MessageSource{Chat: chat, Sender: sender}, ID: "R", Timestamp: evtTime},
		Message: &waConsumerApplication.ConsumerApplication{
			Payload: &waConsumerApplication.ConsumerApplication_Payload{
				Payload: &waConsumerApplication.ConsumerApplication_Payload_ApplicationData{
					ApplicationData: &waConsumerApplication.ConsumerApplication_ApplicationData{
						ApplicationContent: &waConsumerApplication.ConsumerApplication_ApplicationData_Revoke{
							Revoke: &waConsumerApplication.ConsumerApplication_RevokeMessage{Key: key},
						},
					},
				},
			},
		},
	}
}

// M1: edits and unsends carry the authenticated sender of the event and the sender named by the key.
func TestFBMessageEditAndRevokeCarryBothSenders(t *testing.T) {
	grp := types.JID{User: "9000", Server: types.GroupServer}
	me := types.JID{User: "9999", Device: 4, Server: types.MessengerServer}
	cases := []struct {
		name       string
		chat, from types.JID
		key        *waCommon.MessageKey
		sender     string
		target     string
		id         string
	}{
		{"own message in a dm", chatJID, senderJID, &waCommon.MessageKey{ID: proto.String("X"), FromMe: proto.Bool(true)}, "5678", "5678", "wa:1234@msgr:5678@msgr:X"},
		{"counterparty names the owner's message", chatJID, senderJID, &waCommon.MessageKey{ID: proto.String("X")}, "5678", "9999", "wa:1234@msgr:9999@msgr:X"},
		{"owner on another device names the peer's message", chatJID, me, &waCommon.MessageKey{ID: proto.String("X")}, "9999", "1234", "wa:1234@msgr:1234@msgr:X"},
		{"owner on another device names an own message", chatJID, me, &waCommon.MessageKey{ID: proto.String("X"), FromMe: proto.Bool(true)}, "9999", "9999", "wa:1234@msgr:9999@msgr:X"},
		{"group member names another participant", grp, senderJID, &waCommon.MessageKey{ID: proto.String("X"), Participant: proto.String("222@msgr")}, "5678", "222", "wa:9000@g.us:222@msgr:X"},
		{"group member names an own message", grp, senderJID, &waCommon.MessageKey{ID: proto.String("X"), FromMe: proto.Bool(true), RemoteJID: proto.String("9000@g.us")}, "5678", "5678", "wa:9000@g.us:5678@msgr:X"},
	}
	for _, c := range cases {
		edit := FBMessage(editEvt(c.chat, c.from, c.key), e2eeCtxFor()).Edit
		rev := FBMessage(revokeEvt(c.chat, c.from, c.key), e2eeCtxFor()).Removal
		if edit == nil || edit.MessageID != c.id || edit.SenderID != c.sender || edit.TargetSenderID != c.target || edit.ThreadID != ThreadIDFromJID(c.chat) {
			t.Fatalf("%s: edit %+v", c.name, edit)
		}
		if rev == nil || !rev.Unsent || rev.MessageID != c.id || rev.SenderID != c.sender || rev.TargetSenderID != c.target || rev.ByAdmin {
			t.Fatalf("%s: revoke %+v", c.name, rev)
		}
	}
}

func TestFBMessageKeysThatCannotBeTrustedResolveToNothing(t *testing.T) {
	grp := types.JID{User: "9000", Server: types.GroupServer}
	other := types.JID{User: "9001", Server: types.GroupServer}
	cases := []struct {
		name       string
		chat, from types.JID
		key        *waCommon.MessageKey
	}{
		{"remote group differs from the event chat", grp, senderJID, &waCommon.MessageKey{ID: proto.String("X"), Participant: proto.String("222@msgr"), RemoteJID: proto.String(other.String())}},
		{"remote group in a dm", chatJID, senderJID, &waCommon.MessageKey{ID: proto.String("X"), FromMe: proto.Bool(true), RemoteJID: proto.String(grp.String())}},
		{"group key without a participant", grp, senderJID, &waCommon.MessageKey{ID: proto.String("X")}},
		{"empty id", chatJID, senderJID, &waCommon.MessageKey{FromMe: proto.Bool(true)}},
		{"unparsable participant", grp, senderJID, &waCommon.MessageKey{ID: proto.String("X"), Participant: proto.String("@@")}},
	}
	for _, c := range cases {
		if res := FBMessage(editEvt(c.chat, c.from, c.key), e2eeCtxFor()); res.Edit != nil {
			t.Fatalf("%s: edit %+v", c.name, res.Edit)
		}
		if res := FBMessage(revokeEvt(c.chat, c.from, c.key), e2eeCtxFor()); res.Removal != nil {
			t.Fatalf("%s: revoke %+v", c.name, res.Removal)
		}
		react := e2eeEvt("RX", &waConsumerApplication.ConsumerApplication_Content{
			Content: &waConsumerApplication.ConsumerApplication_Content_ReactionMessage{ReactionMessage: &waConsumerApplication.ConsumerApplication_ReactionMessage{Key: c.key, Text: proto.String("x")}},
		})
		react.Info.Chat = c.chat
		if res := FBMessage(react, e2eeCtxFor()); res.Reaction != nil {
			t.Fatalf("%s: reaction %+v", c.name, res.Reaction)
		}
	}
}

func TestFBMessageNonNumericSenderResolvesToNothing(t *testing.T) {
	odd := types.JID{User: "abc", Server: types.MessengerServer}
	res := FBMessage(editEvt(chatJID, odd, &waCommon.MessageKey{ID: proto.String("X"), FromMe: proto.Bool(true)}), e2eeCtxFor())
	if res.Edit != nil {
		t.Fatalf("edit %+v", res.Edit)
	}
}
