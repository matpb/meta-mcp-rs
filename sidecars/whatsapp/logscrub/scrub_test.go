package logscrub

import (
	"bytes"
	"encoding/json"
	"strings"
	"testing"

	"github.com/rs/zerolog"
)

func TestScrubsLoggerOutput(t *testing.T) {
	var dst bytes.Buffer
	log := zerolog.New(New(&dst)).With().Timestamp().Logger()

	log.Warn().Str("data", "SECRETDATA").Msg("one")
	log.Error().Bytes("resp_body", []byte("SECRETBODY")).Str("keep", "visible").Msg("two")
	log.Error().Any("response_proto_err", map[string]any{"x": "SECRETPROTO"}).Msg("three")
	log.Info().Dict("a", zerolog.Dict().Dict("cookies", zerolog.Dict().Str("SID", "SECRETCOOKIE")).Str("ok", "fine")).Msg("four")
	log.Info().Array("list", zerolog.Arr().
		Dict(zerolog.Dict().Str("token", "SECRETTOKEN").Str("id", "1")).
		Dict(zerolog.Dict().Str("tachyon_token", "SECRETTACHYON"))).Msg("five")
	log.Info().Str("Headers", "SECRETHEADER").Str("text", "SECRETTEXT").Int("n", 7).Msg("six")

	out := dst.String()
	for _, secret := range []string{"SECRETDATA", "SECRETBODY", "SECRETPROTO", "SECRETCOOKIE", "SECRETTOKEN", "SECRETTACHYON", "SECRETHEADER", "SECRETTEXT"} {
		if strings.Contains(out, secret) {
			t.Fatalf("output leaks %s:\n%s", secret, out)
		}
	}
	lines := strings.Split(strings.TrimSpace(out), "\n")
	if len(lines) != 6 {
		t.Fatalf("want 6 lines, got %d:\n%s", len(lines), out)
	}
	wantMsg := []string{"one", "two", "three", "four", "five", "six"}
	wantLevel := []string{"warn", "error", "error", "info", "info", "info"}
	for i, l := range lines {
		var m map[string]any
		if err := json.Unmarshal([]byte(l), &m); err != nil {
			t.Fatalf("line %d is not JSON: %v: %s", i, err, l)
		}
		if m["message"] != wantMsg[i] || m["level"] != wantLevel[i] || m["time"] == nil {
			t.Fatalf("line %d lost level, time or message: %s", i, l)
		}
	}
	if !strings.Contains(out, `"keep":"visible"`) || !strings.Contains(out, `"ok":"fine"`) || !strings.Contains(out, `"id":"1"`) || !strings.Contains(out, `"n":7`) {
		t.Fatalf("harmless keys must survive:\n%s", out)
	}
}

// wantDenyKeys pins the list so that removing a key from DenyKeys fails the test.
var wantDenyKeys = []string{
	"data", "resp_body", "response_body", "response_proto_err",
	"body", "payload", "cookies", "cookie", "token",
	"tachyon_token", "auth", "message_data", "decrypted_data",
	"header", "headers", "content", "text",
	"evt_data", "devices", "response", "parsed_data", "raw", "url",
	"qr", "code", "pair_code", "phone", "jid", "passkey", "public_key", "caption",
}

func TestDenyKeysPinned(t *testing.T) {
	got := map[string]bool{}
	for _, k := range DenyKeys {
		got[k] = true
	}
	for _, k := range wantDenyKeys {
		if !got[k] {
			t.Fatalf("deny list lost %q", k)
		}
	}
}

func TestEveryDenyKeyIsScrubbed(t *testing.T) {
	keys := append(append([]string{}, DenyKeys...), wantDenyKeys...)
	for _, k := range keys {
		variants := []string{k, strings.ToUpper(k), " " + k + "\t"}
		for _, key := range variants {
			var dst bytes.Buffer
			log := zerolog.New(New(&dst))
			log.Info().Str(key, "SECRET").Msg("top")
			log.Info().Dict("obj", zerolog.Dict().Str("n", "1").Str(key, "SECRET")).Msg("nested")
			log.Info().Array("arr", zerolog.Arr().
				Dict(zerolog.Dict().Str("n", "1")).
				Dict(zerolog.Dict().Dict("deep", zerolog.Dict().Str(key, "SECRET")))).Msg("array")
			out := dst.String()
			if strings.Contains(out, "SECRET") {
				t.Fatalf("key %q leaks:\n%s", key, out)
			}
			lines := strings.Split(strings.TrimSpace(out), "\n")
			if len(lines) != 3 {
				t.Fatalf("key %q: want 3 lines, got %d:\n%s", key, len(lines), out)
			}
			for i, msg := range []string{"top", "nested", "array"} {
				var m map[string]any
				if err := json.Unmarshal([]byte(lines[i]), &m); err != nil {
					t.Fatalf("key %q line %d not JSON: %v", key, i, err)
				}
				if m["level"] != "info" || m["message"] != msg {
					t.Fatalf("key %q line %d lost level or message: %s", key, i, lines[i])
				}
			}
		}
	}
}

func TestKeyOrderAndEscapingKept(t *testing.T) {
	got := string(scrubLine([]byte(`{"level":"info","data":{"a":[1,{"b":2}]},"z":1.50,"msg":"a<b>&\u00e9","n":null,"t":true,"time":"x"}`)))
	want := `{"level":"info","z":1.50,"msg":"a<b>&é","n":null,"t":true,"time":"x"}`
	if got != want {
		t.Fatalf("got  %s\nwant %s", got, want)
	}
}

func TestNonJSONLinesUnchanged(t *testing.T) {
	var dst bytes.Buffer
	w := New(&dst)
	in := "panic: plain text with data=SECRET\n{not json\n[1,2]\n{\"a\":1} trailing\n\n"
	if _, err := w.Write([]byte(in)); err != nil {
		t.Fatal(err)
	}
	if dst.String() != in {
		t.Fatalf("got %q want %q", dst.String(), in)
	}
}

func TestPartialWritesAreBuffered(t *testing.T) {
	var dst bytes.Buffer
	w := New(&dst)
	w.Write([]byte(`{"level":"info","da`))
	if dst.Len() != 0 {
		t.Fatal("a partial line must not be forwarded")
	}
	w.Write([]byte("ta\":\"SECRET\",\"message\":\"m\"}\n{\"level\":"))
	if got := dst.String(); got != "{\"level\":\"info\",\"message\":\"m\"}\n" {
		t.Fatalf("got %q", got)
	}
	if err := w.Flush(); err != nil || !strings.HasSuffix(dst.String(), `{"level":`) {
		t.Fatalf("flush: %v %q", err, dst.String())
	}
}

func TestOversizedPartialFlushedAsIs(t *testing.T) {
	var dst bytes.Buffer
	w := New(&dst)
	big := strings.Repeat("x", MaxPartial+1)
	n, err := w.Write([]byte(big))
	if err != nil || n != len(big) || dst.Len() != len(big) {
		t.Fatalf("n=%d err=%v forwarded=%d", n, err, dst.Len())
	}
	if len(w.buf) != 0 {
		t.Fatal("buffer must be empty after the flush")
	}
}

func TestScrubString(t *testing.T) {
	cases := []struct{ name, in, want string }{
		{"qr", "scan https://wa.me/settings/linked_devices#2@abc,DEF+/=,xyz now", "scan https://wa.me/settings/linked_devices#[redacted] now"},
		{"qr only", "https://wa.me/settings/linked_devices#secret", "https://wa.me/settings/linked_devices#[redacted]"},
		{"pn jid", "from 15551234567@s.whatsapp.net ok", "from [jid]@s.whatsapp.net ok"},
		{"pn jid device", "15551234567:12@s.whatsapp.net", "[jid]@s.whatsapp.net"},
		{"lid", "123456789012345@lid", "[jid]@lid"},
		{"lid device", "123456789012345:3@lid", "[jid]@lid"},
		{"c.us", "15551234567@c.us", "[jid]@c.us"},
		{"group kept", "in 120363012345678901@g.us now", "in 120363012345678901@g.us now"},
		{"old group kept", "15551234567-1600000000@g.us", "15551234567-1600000000@g.us"},
		{"phone plus", "call +15551234567 now", "call [phone] now"},
		{"phone bare", "call 15551234567.", "call [phone]."},
		{"phone 7 digits", "5551234", "[phone]"},
		{"pair code", "code ABCD-1234 sent", "code [code] sent"},
		{"pair code digits", "1234-5678", "[code]"},
		{"two in one", "a 15551234567@s.whatsapp.net b +15559876543", "a [jid]@s.whatsapp.net b [phone]"},
		{"words survive", "send_text failed: not_connected", "send_text failed: not_connected"},
		{"op names survive", "op=fetch_messages id=abc123 thread", "op=fetch_messages id=abc123 thread"},
		{"small numbers survive", "count 42 limit 100 port 9222 id 123456", "count 42 limit 100 port 9222 id 123456"},
		{"too long digit run kept", "1234567890123456", "1234567890123456"},
		{"glued to letters kept", "msg3EB0C767D26A1234567", "msg3EB0C767D26A1234567"},
		{"lowercase dashes survive", "abcd-efgh", "abcd-efgh"},
		{"server only survives", "s.whatsapp.net and @lid", "s.whatsapp.net and @lid"},
	}
	for _, c := range cases {
		if got := scrubString(c.in); got != c.want {
			t.Errorf("%s: got %q want %q", c.name, got, c.want)
		}
	}
}

func TestValuesScrubbedInJSONIncludingMessage(t *testing.T) {
	var dst bytes.Buffer
	log := zerolog.New(New(&dst))
	log.Info().Str("op", "send_text").Str("thread", "15551234567@s.whatsapp.net").Str("note", "ring +15551234567").
		Dict("nested", zerolog.Dict().Str("v", "ABCD-1234")).
		Array("arr", zerolog.Arr().Str("999999999999999@lid").Str("120363012345678901@g.us")).
		Int("n", 42).Msg("to 15551234567:7@s.whatsapp.net via https://wa.me/settings/linked_devices#SECRETQR")
	out := dst.String()
	for _, leak := range []string{"15551234567", "ABCD-1234", "999999999999999", "SECRETQR"} {
		if strings.Contains(out, leak) {
			t.Fatalf("leaks %s:\n%s", leak, out)
		}
	}
	var m map[string]any
	if err := json.Unmarshal([]byte(strings.TrimSpace(out)), &m); err != nil {
		t.Fatalf("not JSON: %v\n%s", err, out)
	}
	if m["op"] != "send_text" || m["thread"] != "[jid]@s.whatsapp.net" || m["note"] != "ring [phone]" || m["n"] != float64(42) {
		t.Fatalf("unexpected values: %s", out)
	}
	if m["message"] != "to [jid]@s.whatsapp.net via https://wa.me/settings/linked_devices#[redacted]" {
		t.Fatalf("message not scrubbed: %v", m["message"])
	}
	if !strings.Contains(out, "120363012345678901@g.us") || !strings.Contains(out, "[code]") || !strings.Contains(out, "[jid]@lid") {
		t.Fatalf("group JID must stay, code and lid must be scrubbed:\n%s", out)
	}
}

func TestNewDenyKeysDropped(t *testing.T) {
	for _, k := range []string{"qr", "code", "pair_code", "phone", "jid", "passkey", "public_key", "caption"} {
		var dst bytes.Buffer
		l := zerolog.New(New(&dst))
		l.Info().Str(k, "SECRET").Str("keep", "yes").Msg("m")
		if strings.Contains(dst.String(), "SECRET") || !strings.Contains(dst.String(), `"keep":"yes"`) {
			t.Fatalf("key %s: %s", k, dst.String())
		}
	}
}

func TestNonJSONLinesGetValuePatterns(t *testing.T) {
	var dst bytes.Buffer
	w := New(&dst)
	in := "panic: peer 15551234567@s.whatsapp.net code ABCD-1234 phone +15551234567\n{not json 15551234567\n"
	if _, err := w.Write([]byte(in)); err != nil {
		t.Fatal(err)
	}
	want := "panic: peer [jid]@s.whatsapp.net code [code] phone [phone]\n{not json [phone]\n"
	if dst.String() != want {
		t.Fatalf("got %q want %q", dst.String(), want)
	}
}

func TestFlushScrubsPendingLine(t *testing.T) {
	var dst bytes.Buffer
	w := New(&dst)
	w.Write([]byte("tail 15551234567"))
	if err := w.Flush(); err != nil || dst.String() != "tail [phone]" {
		t.Fatalf("flush: %v %q", err, dst.String())
	}
}
