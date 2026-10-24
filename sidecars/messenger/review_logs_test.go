package main

import (
	"bytes"
	"strings"
	"testing"

	"github.com/rs/zerolog"
	zlog "github.com/rs/zerolog/log"
	"go.mau.fi/mautrix-meta/pkg/messagix/lightspeed"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
)

// M2: the real library logs LS decode anomalies through the global logger the sidecar installed;
// the allowlist scrubber drops every field but the fixed message, so decoded text never reaches stderr.
func TestReviewLibraryWarnLeaksText(t *testing.T) {
	var buf bytes.Buffer
	_, scrub := newLogger(&buf)
	_ = zerolog.GlobalLevel()
	d := lightspeed.NewLightSpeedDecoder(lightspeed.DependencyMap{}, &table.LSTable{})
	// "Failed to decode block data": a BLOCK whose child is not an array
	d.Decode([]any{float64(lightspeed.BLOCK), "my private message text from Alice: the door code is 4711"})
	// "Unexpected step data in CALL_STORED_PROCEDURE (expected string)"
	d.Decode([]any{float64(lightspeed.CALL_STORED_PROCEDURE), []any{"lunch at 12 with Bob, call +1 555 010 0199"}})
	// "Got unknown step type"
	d.Decode([]any{float64(9999), "secret note about the surprise party"})
	// "[I64_FROM_STRING] failed to convert string to int64"
	d.Decode([]any{float64(lightspeed.I64_FROM_STRING), "free text instead of a number"})
	_ = scrub.Flush()
	out := buf.String()
	t.Logf("stderr after scrubber:\n%s", out)
	for _, s := range []string{"door code is 4711", "lunch at 12 with Bob", "surprise party", "free text instead of a number", "+1 555 010 0199"} {
		if strings.Contains(out, s) {
			t.Errorf("CONFIRMED: %q reached stderr", s)
		}
	}
}

func TestReviewGlobalLoggerLevelIsInfo(t *testing.T) {
	var buf bytes.Buffer
	_, _ = newLogger(&buf)
	if got := zlogLevel(); got != zerolog.InfoLevel {
		t.Fatalf("global library logger level is %v, want info (Trace would disable the library's redaction gates)", got)
	}
	t.Logf("global logger level: %v, GlobalLevel: %v", zlogLevel(), zerolog.GlobalLevel())
}

func zlogLevel() zerolog.Level { return zlog.Logger.GetLevel() }
