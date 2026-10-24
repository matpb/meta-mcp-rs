// Package normalize turns whatsmeow message content into the protocol shapes.
// It also classifies messages, canonicalises JIDs and batches history.
package normalize

import (
	"go.mau.fi/whatsmeow/proto/waE2E"
	"google.golang.org/protobuf/reflect/protoreflect"
)

// Action says what the ingest path does with a message.
type Action int

const (
	ActIgnore Action = iota
	ActMessage
	ActReaction
	ActEdit
	ActRevoke
	ActEncryptedEdit
	ActEncryptedReaction
)

// Classified is the result of Classify.
type Classified struct {
	Action       Action
	TargetID     string         // reaction, edit or revoke target message id
	TargetFromMe bool           // key.FromMe of the target
	TargetSender string         // key.Participant of the target, "" when absent
	Emoji        string         // reaction text, "" = removed
	Content      *waE2E.Message // ActMessage: msg itself; ActEdit: the new content
}

// housekeeping lists top-level fields that carry no user-visible content.
var housekeeping = map[protoreflect.Name]bool{
	"messageContextInfo":                         true,
	"senderKeyDistributionMessage":               true,
	"fastRatchetKeySenderKeyDistributionMessage": true,
	"pollUpdateMessage":                          true,
	"keepInChatMessage":                          true,
}

// Classify decides what an already unwrapped message is.
func Classify(msg *waE2E.Message) Classified {
	if msg == nil {
		return Classified{Action: ActIgnore}
	}
	if pm := msg.GetProtocolMessage(); pm != nil {
		switch pm.GetType() {
		case waE2E.ProtocolMessage_REVOKE:
			return Classified{
				Action:       ActRevoke,
				TargetID:     pm.GetKey().GetID(),
				TargetFromMe: pm.GetKey().GetFromMe(),
				TargetSender: pm.GetKey().GetParticipant(),
			}
		case waE2E.ProtocolMessage_MESSAGE_EDIT:
			if pm.GetEditedMessage() == nil {
				return Classified{Action: ActIgnore}
			}
			return Classified{
				Action:       ActEdit,
				TargetID:     pm.GetKey().GetID(),
				TargetFromMe: pm.GetKey().GetFromMe(),
				TargetSender: pm.GetKey().GetParticipant(),
				Content:      pm.GetEditedMessage(),
			}
		}
		return Classified{Action: ActIgnore}
	}
	if r := msg.GetReactionMessage(); r != nil {
		return Classified{
			Action:       ActReaction,
			TargetID:     r.GetKey().GetID(),
			TargetFromMe: r.GetKey().GetFromMe(),
			TargetSender: r.GetKey().GetParticipant(),
			Emoji:        r.GetText(),
		}
	}
	if r := msg.GetEncReactionMessage(); r != nil {
		return encrypted(ActEncryptedReaction, r.GetTargetMessageKey().GetID(), r.GetTargetMessageKey().GetFromMe(), r.GetTargetMessageKey().GetParticipant())
	}
	if s := msg.GetSecretEncryptedMessage(); s != nil {
		return encrypted(ActEncryptedEdit, s.GetTargetMessageKey().GetID(), s.GetTargetMessageKey().GetFromMe(), s.GetTargetMessageKey().GetParticipant())
	}
	if onlyHousekeeping(msg) {
		return Classified{Action: ActIgnore}
	}
	return Classified{Action: ActMessage, Content: msg}
}

func encrypted(a Action, id string, fromMe bool, sender string) Classified {
	return Classified{Action: a, TargetID: id, TargetFromMe: fromMe, TargetSender: sender}
}

// onlyHousekeeping is true for an empty message or one with only housekeeping fields set.
func onlyHousekeeping(msg *waE2E.Message) bool {
	other := false
	msg.ProtoReflect().Range(func(fd protoreflect.FieldDescriptor, _ protoreflect.Value) bool {
		if !housekeeping[fd.Name()] {
			other = true
			return false
		}
		return true
	})
	return !other
}
