package logscrub

import (
	"bytes"
	"strings"
	"testing"

	"github.com/rs/zerolog"
)

// M2: secrets and message text in library fields never reach the output; secret-shaped strings in the fixed message are redacted.
func TestReviewMScrubSamples(t *testing.T) {
	type sample struct{ in, leak string }
	samples := []sample{
		{"Cookie: datr=AbCdEfGhIjKlMnOpQrStUvWx; xs=47%3Ax8GnBwi2d4hY2gA%3A2%3A1739912345%3A-1%3A-1%3A%3AAcWaBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789ab; c_user=1000123456789012", "AbCdEfGhIjKlMnOpQrStUvWx"},
		{"bad value 47%3Ax8GnBwi2d4hY2gA%3A2%3A1739912345%3A-1%3A-1%3A%3AAcWaBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789ab", "AcWaBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789ab"},
		{"xs: 47%3Ax8GnBwi2d4hY2gA%3A2%3A1739912345%3A-1%3A-1", ""},
		{"https://www.facebook.com/checkpoint/?next=https%3A%2F%2Fwww.facebook.com&token=SECRET", "token=SECRET"},
		{"phone +1 415 555 0100 call", "415 555 0100"},
		{"identity key Zm9vYmFyYmF6cXV4Zm9vYmFyYmF6cXV4Zm9vYmFyYmF6cXV4", "Zm9vYmFyYmF6cXV4Zm9vYmFyYmF6cXV4"},
	}
	for _, c := range samples {
		var buf bytes.Buffer
		log := zerolog.New(New(&buf))
		log.Warn().Str("error", c.in).Str("text", c.in).Msg(c.in)
		out := buf.String()
		if c.leak != "" && strings.Contains(out, c.leak) {
			t.Errorf("leaks %q:\nIN : %s\nOUT: %s", c.leak, c.in, out)
		}
		if strings.Count(out, "REDACTED") == 0 && c.leak != "" {
			t.Errorf("no redaction marker:\nIN : %s\nOUT: %s", c.in, out)
		}
	}
	// free text under any other key is dropped whole
	var buf bytes.Buffer
	log := zerolog.New(New(&buf))
	log.Warn().Str("error", "message text: the door code is 4711").Str("text", "message text: the door code is 4711").Msg("fixed")
	if strings.Contains(buf.String(), "4711") || !strings.Contains(buf.String(), `"message":"fixed"`) {
		t.Errorf("unexpected output: %s", buf.String())
	}
}
