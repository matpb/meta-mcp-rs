// Package normalize turns messagix tables and E2EE events into the protocol shapes of package api.
package normalize

import (
	"net/url"
	"strings"

	"go.mau.fi/mautrix-meta/pkg/metaid"
	"go.mau.fi/mautrix-meta/pkg/msgconv/mediadl"
	"go.mau.fi/whatsmeow/types"
)

// ExpiryFromURL returns the unix-second expiry from the hex `oe` query parameter, nil if absent or invalid.
func ExpiryFromURL(u string) *int64 {
	t := mediadl.ParseExpiryFromURL(u)
	if t.IsZero() {
		return nil
	}
	v := t.Unix()
	return &v
}

// ThreadIDFromJID is the portal id of an E2EE chat, shared with its Messenger thread key.
func ThreadIDFromJID(jid types.JID) string {
	return string(metaid.MakeWAPortalID(jid))
}

func removeLPHP(addr string) string {
	parsed, _ := url.Parse(addr)
	if parsed != nil && parsed.Path == "/l.php" {
		if u := parsed.Query().Get("u"); u != "" {
			return u
		}
	}
	return addr
}

func strPtr(s string) *string {
	if s == "" {
		return nil
	}
	return &s
}

func posPtr(v int64) *int64 {
	if v <= 0 {
		return nil
	}
	return &v
}

func kindFromMime(mime string) string {
	switch strings.SplitN(mime, "/", 2)[0] {
	case "image":
		return "image"
	case "video":
		return "video"
	case "audio":
		return "audio"
	case "":
		return "other"
	}
	return "file"
}

func derefStr(s *string) string {
	if s == nil {
		return ""
	}
	return *s
}
