package normalize

import (
	"strconv"

	"go.mau.fi/whatsmeow/proto/waE2E"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
)

// Content is the protocol view of one message body.
type Content struct {
	Kind        string
	Text        *string
	ReplyTo     *string
	Attachments []api.Attachment
}

// Extract converts an unwrapped message into protocol fields.
func Extract(msg *waE2E.Message) Content {
	c := Content{Kind: api.MsgUnsupported, Attachments: []api.Attachment{}}
	if msg == nil {
		return c
	}
	c.ReplyTo = nonEmpty(contextInfo(msg).GetStanzaID())

	switch {
	case msg.ExtendedTextMessage != nil:
		et := msg.GetExtendedTextMessage()
		c.Kind = api.MsgText
		c.Text = nonEmpty(et.GetText())
		if et.GetMatchedText() != "" {
			c.Kind = api.MsgLink
			url := et.GetMatchedText()
			c.Attachments = append(c.Attachments, api.Attachment{ID: AttachmentID, Kind: api.AttLink, Name: nonEmpty(et.GetTitle()), URL: &url})
		}
	case msg.Conversation != nil:
		c.Kind = api.MsgText
		c.Text = nonEmpty(msg.GetConversation())
	case msg.ImageMessage != nil:
		m := msg.GetImageMessage()
		c.Kind = api.MsgMedia
		c.Text = nonEmpty(m.GetCaption())
		c.Attachments = append(c.Attachments, media(api.AttImage, m.GetMimetype(), m.GetFileLength(), m.GetWidth(), m.GetHeight(), 0, ""))
	case msg.GetVideoMessage() != nil || msg.GetPtvMessage() != nil:
		m := msg.GetVideoMessage()
		if m == nil {
			m = msg.GetPtvMessage()
		}
		kind := api.AttVideo
		if m.GetGifPlayback() {
			kind = api.AttGIF
		}
		c.Kind = api.MsgMedia
		c.Text = nonEmpty(m.GetCaption())
		c.Attachments = append(c.Attachments, media(kind, m.GetMimetype(), m.GetFileLength(), m.GetWidth(), m.GetHeight(), m.GetSeconds(), ""))
	case msg.AudioMessage != nil:
		m := msg.GetAudioMessage()
		c.Kind = api.MsgMedia
		c.Attachments = append(c.Attachments, media(api.AttAudio, m.GetMimetype(), m.GetFileLength(), 0, 0, m.GetSeconds(), ""))
	case msg.DocumentMessage != nil:
		m := msg.GetDocumentMessage()
		c.Kind = api.MsgMedia
		c.Text = nonEmpty(m.GetCaption())
		c.Attachments = append(c.Attachments, media(api.AttFile, m.GetMimetype(), m.GetFileLength(), 0, 0, 0, m.GetFileName()))
	case msg.StickerMessage != nil:
		m := msg.GetStickerMessage()
		c.Kind = api.MsgSticker
		c.Attachments = append(c.Attachments, media(api.AttSticker, m.GetMimetype(), m.GetFileLength(), m.GetWidth(), m.GetHeight(), 0, ""))
	case msg.LocationMessage != nil:
		m := msg.GetLocationMessage()
		c.Kind = api.MsgLocation
		name := m.GetName()
		if name == "" {
			name = m.GetAddress()
		}
		c.Attachments = append(c.Attachments, location(m.GetDegreesLatitude(), m.GetDegreesLongitude(), name))
	case msg.LiveLocationMessage != nil:
		m := msg.GetLiveLocationMessage()
		c.Kind = api.MsgLocation
		c.Attachments = append(c.Attachments, location(m.GetDegreesLatitude(), m.GetDegreesLongitude(), ""))
	case msg.CallLogMesssage != nil || msg.BcallMessage != nil:
		c.Kind = api.MsgCall
	case msg.ContactMessage != nil:
		c.Text = nonEmpty(msg.GetContactMessage().GetDisplayName())
	case msg.ContactsArrayMessage != nil:
		c.Text = nonEmpty(msg.GetContactsArrayMessage().GetDisplayName())
	case pollOf(msg) != nil:
		c.Text = nonEmpty(pollOf(msg).GetName())
	case msg.EventMessage != nil:
		c.Text = nonEmpty(msg.GetEventMessage().GetName())
	case msg.GroupInviteMessage != nil:
		c.Text = nonEmpty(msg.GetGroupInviteMessage().GetGroupName())
	}

	return c
}

func pollOf(msg *waE2E.Message) *waE2E.PollCreationMessage {
	for _, p := range []*waE2E.PollCreationMessage{
		msg.GetPollCreationMessage(), msg.GetPollCreationMessageV2(), msg.GetPollCreationMessageV3(),
		msg.GetPollCreationMessageV5(), msg.GetPollCreationMessageV6(),
	} {
		if p != nil {
			return p
		}
	}
	return nil
}

// contextInfo returns the ContextInfo of whichever sub-message carries one.
func contextInfo(msg *waE2E.Message) *waE2E.ContextInfo {
	for _, ci := range []*waE2E.ContextInfo{
		msg.GetExtendedTextMessage().GetContextInfo(),
		msg.GetImageMessage().GetContextInfo(),
		msg.GetVideoMessage().GetContextInfo(),
		msg.GetPtvMessage().GetContextInfo(),
		msg.GetAudioMessage().GetContextInfo(),
		msg.GetDocumentMessage().GetContextInfo(),
		msg.GetStickerMessage().GetContextInfo(),
		msg.GetLocationMessage().GetContextInfo(),
		msg.GetLiveLocationMessage().GetContextInfo(),
		msg.GetContactMessage().GetContextInfo(),
		msg.GetContactsArrayMessage().GetContextInfo(),
		msg.GetGroupInviteMessage().GetContextInfo(),
		msg.GetEventMessage().GetContextInfo(),
		pollOf(msg).GetContextInfo(),
	} {
		if ci != nil {
			return ci
		}
	}
	return nil
}

func media(kind, mime string, size uint64, w, h, seconds uint32, name string) api.Attachment {
	a := api.Attachment{ID: AttachmentID, Kind: kind, Mime: nonEmpty(mime), Name: nonEmpty(name)}
	if size > 0 {
		v := int64(size)
		a.Size = &v
	}
	if w > 0 {
		v := int64(w)
		a.Width = &v
	}
	if h > 0 {
		v := int64(h)
		a.Height = &v
	}
	if seconds > 0 {
		v := int64(seconds) * 1000
		a.DurationMS = &v
	}
	return a
}

func location(lat, lng float64, name string) api.Attachment {
	url := "geo:" + strconv.FormatFloat(lat, 'f', -1, 64) + "," + strconv.FormatFloat(lng, 'f', -1, 64)
	return api.Attachment{ID: AttachmentID, Kind: api.AttLocation, Name: nonEmpty(name), URL: &url}
}

func nonEmpty(s string) *string {
	if s == "" {
		return nil
	}
	return &s
}
