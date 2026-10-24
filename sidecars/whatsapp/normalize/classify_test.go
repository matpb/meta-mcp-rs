package normalize

import (
	"testing"

	"go.mau.fi/whatsmeow/proto/waCommon"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"google.golang.org/protobuf/proto"
)

func key(id string, fromMe bool, participant string) *waCommon.MessageKey {
	k := &waCommon.MessageKey{ID: proto.String(id), FromMe: proto.Bool(fromMe)}
	if participant != "" {
		k.Participant = proto.String(participant)
	}
	return k
}

func TestClassify(t *testing.T) {
	edited := &waE2E.Message{Conversation: proto.String("new text")}
	tests := []struct {
		name         string
		msg          *waE2E.Message
		action       Action
		targetID     string
		targetFromMe bool
		targetSender string
		emoji        string
		content      *waE2E.Message
	}{
		{
			name:   "conversation",
			msg:    &waE2E.Message{Conversation: proto.String("hi")},
			action: ActMessage,
		},
		{
			name:   "image",
			msg:    &waE2E.Message{ImageMessage: &waE2E.ImageMessage{}},
			action: ActMessage,
		},
		{
			name:   "unsupported poll still a message",
			msg:    &waE2E.Message{PollCreationMessage: &waE2E.PollCreationMessage{Name: proto.String("q")}},
			action: ActMessage,
		},
		{
			name: "reaction",
			msg: &waE2E.Message{ReactionMessage: &waE2E.ReactionMessage{
				Key: key("T1", true, "123@s.whatsapp.net"), Text: proto.String("👍"),
			}},
			action: ActReaction, targetID: "T1", targetFromMe: true, targetSender: "123@s.whatsapp.net", emoji: "👍",
		},
		{
			name: "reaction removal has empty text",
			msg: &waE2E.Message{ReactionMessage: &waE2E.ReactionMessage{
				Key: key("T2", false, ""), Text: proto.String(""),
			}},
			action: ActReaction, targetID: "T2",
		},
		{
			name: "edit carries new content",
			msg: &waE2E.Message{ProtocolMessage: &waE2E.ProtocolMessage{
				Type: waE2E.ProtocolMessage_MESSAGE_EDIT.Enum(), Key: key("T3", true, ""), EditedMessage: edited,
			}},
			action: ActEdit, targetID: "T3", targetFromMe: true, content: edited,
		},
		{
			name: "edit without content is ignored",
			msg: &waE2E.Message{ProtocolMessage: &waE2E.ProtocolMessage{
				Type: waE2E.ProtocolMessage_MESSAGE_EDIT.Enum(), Key: key("T3", true, ""),
			}},
			action: ActIgnore,
		},
		{
			name: "revoke",
			msg: &waE2E.Message{ProtocolMessage: &waE2E.ProtocolMessage{
				Type: waE2E.ProtocolMessage_REVOKE.Enum(), Key: key("T4", false, "999@lid"),
			}},
			action: ActRevoke, targetID: "T4", targetSender: "999@lid",
		},
		{
			name: "encrypted reaction",
			msg: &waE2E.Message{EncReactionMessage: &waE2E.EncReactionMessage{
				TargetMessageKey: key("T5", false, "55@s.whatsapp.net"), EncPayload: []byte{1},
			}},
			action: ActEncryptedReaction, targetID: "T5", targetSender: "55@s.whatsapp.net",
		},
		{
			name: "secret encrypted edit",
			msg: &waE2E.Message{SecretEncryptedMessage: &waE2E.SecretEncryptedMessage{
				TargetMessageKey: key("T6", true, ""), EncPayload: []byte{1},
			}},
			action: ActEncryptedEdit, targetID: "T6", targetFromMe: true,
		},
		{
			name: "history sync notification is ignored",
			msg: &waE2E.Message{ProtocolMessage: &waE2E.ProtocolMessage{
				Type: waE2E.ProtocolMessage_HISTORY_SYNC_NOTIFICATION.Enum(),
			}},
			action: ActIgnore,
		},
		{
			name: "ephemeral setting is ignored",
			msg: &waE2E.Message{ProtocolMessage: &waE2E.ProtocolMessage{
				Type: waE2E.ProtocolMessage_EPHEMERAL_SETTING.Enum(),
			}},
			action: ActIgnore,
		},
		{
			name: "sender key distribution only",
			msg: &waE2E.Message{SenderKeyDistributionMessage: &waE2E.SenderKeyDistributionMessage{
				GroupID: proto.String("g"),
			}},
			action: ActIgnore,
		},
		{
			name:   "message context info only",
			msg:    &waE2E.Message{MessageContextInfo: &waE2E.MessageContextInfo{}},
			action: ActIgnore,
		},
		{
			name:   "poll update",
			msg:    &waE2E.Message{PollUpdateMessage: &waE2E.PollUpdateMessage{}},
			action: ActIgnore,
		},
		{
			name:   "keep in chat",
			msg:    &waE2E.Message{KeepInChatMessage: &waE2E.KeepInChatMessage{}},
			action: ActIgnore,
		},
		{
			name:   "empty message",
			msg:    &waE2E.Message{},
			action: ActIgnore,
		},
		{
			name:   "nil message",
			msg:    nil,
			action: ActIgnore,
		},
		{
			name: "content plus context info stays a message",
			msg: &waE2E.Message{
				Conversation:       proto.String("hi"),
				MessageContextInfo: &waE2E.MessageContextInfo{},
			},
			action: ActMessage,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := Classify(tc.msg)
			if got.Action != tc.action {
				t.Fatalf("action = %v, want %v", got.Action, tc.action)
			}
			if got.TargetID != tc.targetID || got.TargetFromMe != tc.targetFromMe || got.TargetSender != tc.targetSender {
				t.Errorf("target = (%q,%v,%q), want (%q,%v,%q)",
					got.TargetID, got.TargetFromMe, got.TargetSender, tc.targetID, tc.targetFromMe, tc.targetSender)
			}
			if got.Emoji != tc.emoji {
				t.Errorf("emoji = %q, want %q", got.Emoji, tc.emoji)
			}
			want := tc.content
			if tc.action == ActMessage {
				want = tc.msg
			}
			if got.Content != want {
				t.Errorf("content = %v, want %v", got.Content, want)
			}
		})
	}
}
