package normalize

import (
	"fmt"
	"net/url"
	"strconv"
	"strings"
	"time"

	"go.mau.fi/mautrix-meta/pkg/metaid"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waArmadilloApplication"
	"go.mau.fi/whatsmeow/proto/waArmadilloXMA"
	"go.mau.fi/whatsmeow/proto/waCommon"
	"go.mau.fi/whatsmeow/proto/waConsumerApplication"
	"go.mau.fi/whatsmeow/proto/waMediaTransport"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	"google.golang.org/protobuf/proto"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

// E2EEContext carries the account and name lookup for encrypted events.
type E2EEContext struct {
	Me   int64
	Name func(fbid int64) string
}

// FBResult is the outcome of one E2EE event; the zero value means ignore it.
type FBResult struct {
	Message  *api.Message
	TSMS     int64
	Media    []api.E2EEMedia // one per downloadable attachment of Message
	Reaction *api.ReactionEvent
	Edit     *api.Edit
	Removal  *api.Removal
}

type e2eeCtx struct {
	c     E2EEContext
	chat  types.JID
	from  types.JID
	msgID string
}

// FBMessage normalises one E2EE event into a message, reaction, edit or removal.
func FBMessage(evt *events.FBMessage, c E2EEContext) FBResult {
	if evt == nil || evt.Message == nil {
		return FBResult{}
	}
	x := e2eeCtx{
		c:     c,
		chat:  evt.Info.Chat,
		from:  evt.Info.Sender,
		msgID: string(metaid.MakeWAMessageID(evt.Info.Chat, evt.Info.Sender, evt.Info.ID)),
	}
	tsMS := int64(0)
	if !evt.Info.Timestamp.IsZero() {
		tsMS = evt.Info.Timestamp.UnixMilli()
	}

	var body e2eeBody
	switch msg := evt.Message.(type) {
	case *waConsumerApplication.ConsumerApplication:
		switch p := msg.GetPayload().GetPayload().(type) {
		case *waConsumerApplication.ConsumerApplication_Payload_ApplicationData:
			if rev := p.ApplicationData.GetRevoke(); rev != nil {
				id := x.keyToID(rev.GetKey())
				if id == "" {
					return FBResult{}
				}
				return FBResult{Removal: &api.Removal{ThreadID: ThreadIDFromJID(x.chat), MessageID: id, Unsent: true}}
			}
			return FBResult{}
		case *waConsumerApplication.ConsumerApplication_Payload_Content:
			content := p.Content
			if r := content.GetReactionMessage(); r != nil {
				return x.reaction(r, evt.Info.Timestamp)
			}
			if e := content.GetEditMessage(); e != nil {
				return x.edit(e, evt.Info.Timestamp)
			}
			body = x.consumerContent(content)
		default:
			body = e2eeBody{kind: api.MsgUnsupported}
		}
	case *waArmadilloApplication.Armadillo:
		body = x.armadillo(msg)
	default:
		body = e2eeBody{kind: api.MsgUnsupported}
	}

	sender := int64(evt.Info.Sender.UserInt())
	m := &api.Message{
		ID:       x.msgID,
		ThreadID: ThreadIDFromJID(x.chat),
		TS:       tsMS / 1000,
		Sender: api.Sender{
			ID:   strconv.FormatInt(sender, 10),
			Name: x.name(sender),
			IsMe: evt.Info.IsFromMe || (c.Me != 0 && sender == c.Me),
		},
		Text:        strPtr(body.text),
		Attachments: body.atts,
		Reactions:   []api.Reaction{},
		Encrypted:   true,
		Kind:        body.kind,
	}
	if m.Attachments == nil {
		m.Attachments = []api.Attachment{}
	}
	if qm := evt.FBApplication.GetMetadata().GetQuotedMessage(); qm != nil {
		pcp, _ := types.ParseJID(qm.GetParticipant())
		reply := string(metaid.MakeWAMessageID(x.chat, pcp, qm.GetStanzaID()))
		m.ReplyTo = &reply
	}
	return FBResult{Message: m, TSMS: tsMS, Media: body.media}
}

type e2eeBody struct {
	kind  string
	text  string
	atts  []api.Attachment
	media []api.E2EEMedia
}

func (x e2eeCtx) name(fbid int64) string {
	if x.c.Name == nil {
		return ""
	}
	return x.c.Name(fbid)
}

func (x e2eeCtx) reaction(r *waConsumerApplication.ConsumerApplication_ReactionMessage, fallback time.Time) FBResult {
	target := x.keyToID(r.GetKey())
	if target == "" {
		return FBResult{}
	}
	ev := &api.ReactionEvent{
		ThreadID:  ThreadIDFromJID(x.chat),
		MessageID: target,
		SenderID:  strconv.FormatUint(x.from.UserInt(), 10),
		TS:        tsOr(r.GetSenderTimestampMS(), fallback),
	}
	if emoji := r.GetText(); emoji != "" {
		ev.Emoji = &emoji
	}
	return FBResult{Reaction: ev}
}

func (x e2eeCtx) edit(e *waConsumerApplication.ConsumerApplication_EditMessage, fallback time.Time) FBResult {
	target := x.keyToID(e.GetKey())
	if target == "" {
		return FBResult{}
	}
	return FBResult{Edit: &api.Edit{
		ThreadID:  ThreadIDFromJID(x.chat),
		MessageID: target,
		Text:      e.GetMessage().GetText(),
		TS:        tsOr(e.GetTimestampMS(), fallback),
	}}
}

func tsOr(ms int64, fallback time.Time) int64 {
	if ms > 0 {
		return ms / 1000
	}
	if fallback.IsZero() {
		return 0
	}
	return fallback.Unix()
}

// keyToID resolves a message key to a wa: id the way the connector's waKeyToMessageID does.
func (x e2eeCtx) keyToID(key *waCommon.MessageKey) string {
	if key == nil || key.GetID() == "" {
		return ""
	}
	chat := x.chat
	sender := x.from.ToNonAD()
	if !key.GetFromMe() {
		if key.GetParticipant() != "" {
			parsed, err := types.ParseJID(key.GetParticipant())
			if err != nil {
				return ""
			}
			sender = parsed
			if sender.Server == types.LegacyUserServer {
				sender.Server = types.DefaultUserServer
			}
		} else if chat.Server == types.DefaultUserServer || chat.Server == types.MessengerServer {
			own := types.JID{User: strconv.FormatInt(x.c.Me, 10), Server: types.MessengerServer}
			if sender.User == own.User {
				sender = chat
			} else {
				sender = own
			}
		} else {
			return ""
		}
	}
	if remote, err := types.ParseJID(key.GetRemoteJID()); err == nil && !remote.IsEmpty() && remote.Server == types.GroupServer {
		chat = remote
	}
	return string(metaid.MakeWAMessageID(chat, sender, key.GetID()))
}

func (x e2eeCtx) consumerContent(content *waConsumerApplication.ConsumerApplication_Content) e2eeBody {
	switch c := content.GetContent().(type) {
	case *waConsumerApplication.ConsumerApplication_Content_MessageText:
		return e2eeBody{kind: api.MsgText, text: c.MessageText.GetText()}
	case *waConsumerApplication.ConsumerApplication_Content_ExtendedTextMessage:
		ext := c.ExtendedTextMessage
		b := e2eeBody{kind: api.MsgText, text: ext.GetText().GetText()}
		link := ext.GetCanonicalURL()
		if link == "" {
			link = ext.GetMatchedText()
		}
		if link != "" {
			b.kind = api.MsgLink
			b.atts = []api.Attachment{{ID: "0", Kind: api.AttLink, Name: strPtr(ext.GetTitle()), URL: &link}}
		}
		return b
	case *waConsumerApplication.ConsumerApplication_Content_ImageMessage:
		return x.image(c.ImageMessage)
	case *waConsumerApplication.ConsumerApplication_Content_VideoMessage:
		return x.video(c.VideoMessage)
	case *waConsumerApplication.ConsumerApplication_Content_AudioMessage:
		return x.audio(c.AudioMessage)
	case *waConsumerApplication.ConsumerApplication_Content_DocumentMessage:
		return x.document(c.DocumentMessage)
	case *waConsumerApplication.ConsumerApplication_Content_StickerMessage:
		return x.sticker(c.StickerMessage)
	case *waConsumerApplication.ConsumerApplication_Content_ViewOnceMessage:
		switch v := c.ViewOnceMessage.GetViewOnceContent().(type) {
		case *waConsumerApplication.ConsumerApplication_ViewOnceMessage_ImageMessage:
			return x.image(v.ImageMessage)
		case *waConsumerApplication.ConsumerApplication_ViewOnceMessage_VideoMessage:
			return x.video(v.VideoMessage)
		}
	case *waConsumerApplication.ConsumerApplication_Content_LocationMessage:
		loc := c.LocationMessage
		return locationBody(loc.GetLocation().GetDegreesLatitude(), loc.GetLocation().GetDegreesLongitude(), firstNonEmpty(loc.GetLocation().GetName(), loc.GetAddress()))
	case *waConsumerApplication.ConsumerApplication_Content_LiveLocationMessage:
		loc := c.LiveLocationMessage.GetLocation()
		return locationBody(loc.GetDegreesLatitude(), loc.GetDegreesLongitude(), loc.GetName())
	}
	return e2eeBody{kind: api.MsgUnsupported}
}

func firstNonEmpty(vals ...string) string {
	for _, v := range vals {
		if v != "" {
			return v
		}
	}
	return ""
}

func locationBody(lat, lon float64, name string) e2eeBody {
	geo := fmt.Sprintf("geo:%f,%f", lat, lon)
	return e2eeBody{kind: api.MsgLocation, atts: []api.Attachment{{ID: "0", Kind: api.AttLocation, Name: strPtr(name), URL: &geo}}}
}

// attachment builds the attachment and download record for one decoded transport at index 0.
func (x e2eeCtx) attachment(idx int, kind string, wt *waMediaTransport.WAMediaTransport, mt whatsmeow.MediaType) (api.Attachment, *api.E2EEMedia) {
	id := strconv.Itoa(idx)
	mime := wt.GetAncillary().GetMimetype()
	size := int64(wt.GetAncillary().GetFileLength())
	att := api.Attachment{ID: id, Kind: kind, Mime: strPtr(mime), Size: posPtr(size)}
	if wt.GetIntegral() == nil {
		return att, nil
	}
	integral, err := proto.Marshal(wt.GetIntegral())
	if err != nil {
		return att, nil
	}
	return att, &api.E2EEMedia{MessageID: x.msgID, AttachmentID: id, MediaType: string(mt), Integral: integral, Mime: mime, Size: size}
}

func finish(kind string, caption string, att api.Attachment, media *api.E2EEMedia) e2eeBody {
	b := e2eeBody{kind: kind, text: caption, atts: []api.Attachment{att}}
	if media != nil {
		b.media = []api.E2EEMedia{*media}
	}
	return b
}

func (x e2eeCtx) image(m *waConsumerApplication.ConsumerApplication_ImageMessage) e2eeBody {
	tr, err := m.Decode()
	if err != nil {
		return e2eeBody{kind: api.MsgUnsupported}
	}
	att, media := x.attachment(0, api.AttImage, tr.GetIntegral().GetTransport(), whatsmeow.MediaImage)
	att.Width, att.Height = posPtr(int64(tr.GetAncillary().GetWidth())), posPtr(int64(tr.GetAncillary().GetHeight()))
	return finish(api.MsgMedia, m.GetCaption().GetText(), att, media)
}

func (x e2eeCtx) video(m *waConsumerApplication.ConsumerApplication_VideoMessage) e2eeBody {
	tr, err := m.Decode()
	if err != nil {
		return e2eeBody{kind: api.MsgUnsupported}
	}
	anc := tr.GetAncillary()
	att, media := x.attachment(0, api.AttVideo, tr.GetIntegral().GetTransport(), whatsmeow.MediaVideo)
	if strings.HasPrefix(derefStr(att.Mime), "image/") {
		att.Kind = api.AttImage
	} else if anc.GetGifPlayback() {
		att.Kind = api.AttGIF
	}
	att.Width, att.Height = posPtr(int64(anc.GetWidth())), posPtr(int64(anc.GetHeight()))
	att.DurationMS = posPtr(int64(anc.GetSeconds()) * 1000)
	return finish(api.MsgMedia, m.GetCaption().GetText(), att, media)
}

func (x e2eeCtx) audio(m *waConsumerApplication.ConsumerApplication_AudioMessage) e2eeBody {
	tr, err := m.Decode()
	if err != nil {
		return e2eeBody{kind: api.MsgUnsupported}
	}
	att, media := x.attachment(0, api.AttAudio, tr.GetIntegral().GetTransport(), whatsmeow.MediaAudio)
	att.DurationMS = posPtr(int64(tr.GetAncillary().GetSeconds()) * 1000)
	return finish(api.MsgMedia, "", att, media)
}

func (x e2eeCtx) document(m *waConsumerApplication.ConsumerApplication_DocumentMessage) e2eeBody {
	tr, err := m.Decode()
	if err != nil {
		return e2eeBody{kind: api.MsgUnsupported}
	}
	att, media := x.attachment(0, api.AttFile, tr.GetIntegral().GetTransport(), whatsmeow.MediaDocument)
	att.Name = strPtr(m.GetFileName())
	return finish(api.MsgMedia, "", att, media)
}

func (x e2eeCtx) sticker(m *waConsumerApplication.ConsumerApplication_StickerMessage) e2eeBody {
	tr, err := m.Decode()
	if err != nil {
		return e2eeBody{kind: api.MsgUnsupported}
	}
	att, media := x.attachment(0, api.AttSticker, tr.GetIntegral().GetTransport(), whatsmeow.MediaImage)
	att.Width, att.Height = posPtr(int64(tr.GetAncillary().GetWidth())), posPtr(int64(tr.GetAncillary().GetHeight()))
	return finish(api.MsgSticker, "", att, media)
}

func (x e2eeCtx) armadillo(msg *waArmadilloApplication.Armadillo) e2eeBody {
	content := msg.GetPayload().GetContent()
	if ext := content.GetExtendedContentMessage(); ext != nil {
		return armadilloExtended(ext)
	}
	if gallery := content.GetImageGalleryMessage(); gallery != nil {
		images, err := gallery.Decode()
		if err != nil || len(images) == 0 {
			return e2eeBody{kind: api.MsgUnsupported}
		}
		b := e2eeBody{kind: api.MsgMedia}
		for i, img := range images {
			att, media := x.attachment(i, api.AttImage, img.GetIntegral().GetTransport(), whatsmeow.MediaImage)
			att.Width, att.Height = posPtr(int64(img.GetAncillary().GetWidth())), posPtr(int64(img.GetAncillary().GetHeight()))
			b.atts = append(b.atts, att)
			if media != nil {
				b.media = append(b.media, *media)
			}
		}
		return b
	}
	return e2eeBody{kind: api.MsgUnsupported}
}

func armadilloExtended(ext *waArmadilloXMA.ExtendedContentMessage) e2eeBody {
	b := e2eeBody{kind: api.MsgText, text: ext.GetMessageText()}
	link := ""
	for _, cta := range ext.GetCtas() {
		if parsed, err := url.Parse(cta.GetNativeURL()); err == nil && parsed.Scheme == "messenger" && parsed.Host == "location_share" {
			geo := "geo:" + parsed.Query().Get("lat") + "," + parsed.Query().Get("long")
			title := strings.TrimSpace(ext.GetTitleText() + "\n" + ext.GetSubtitleText())
			b.kind = api.MsgLocation
			b.atts = []api.Attachment{{ID: "0", Kind: api.AttLocation, Name: strPtr(title), URL: &geo}}
			return b
		}
		if link != "" {
			continue
		}
		for _, cand := range []string{cta.GetNativeURL(), cta.GetActionURL()} {
			if parsed, err := url.Parse(cand); err == nil && (parsed.Scheme == "https" || parsed.Scheme == "http") && parsed.Host != "" {
				link = cand
				break
			}
		}
	}
	if ext.GetTargetType() == waArmadilloXMA.ExtendedContentMessage_MSG_EXTERNAL_LINK_SHARE && link != "" {
		b.kind = api.MsgLink
		b.atts = []api.Attachment{{ID: "0", Kind: api.AttLink, Name: strPtr(ext.GetTitleText()), URL: &link}}
		return b
	}
	if b.text == "" {
		b.kind = api.MsgUnsupported
	}
	return b
}
