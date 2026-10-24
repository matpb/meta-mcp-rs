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
	log.Error().Bytes("resp_body", []byte("SECRETBODY")).Str("op", "send_text").Msg("two")
	log.Error().Any("response_proto_err", map[string]any{"x": "SECRETPROTO"}).Msg("three")
	log.Info().Dict("a", zerolog.Dict().Dict("cookies", zerolog.Dict().Str("SID", "SECRETCOOKIE")).Str("ok", "fine")).Msg("four")
	log.Info().Array("list", zerolog.Arr().
		Dict(zerolog.Dict().Str("token", "SECRETTOKEN").Str("id", "1")).
		Dict(zerolog.Dict().Str("tachyon_token", "SECRETTACHYON"))).Msg("five")
	log.Info().Str("Headers", "SECRETHEADER").Str("text", "SECRETTEXT").Int("count", 7).Msg("six")

	out := dst.String()
	for _, secret := range []string{"SECRETDATA", "SECRETBODY", "SECRETPROTO", "SECRETCOOKIE", "SECRETTOKEN", "SECRETTACHYON", "SECRETHEADER", "SECRETTEXT", "fine"} {
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
	if !strings.Contains(out, `"op":"send_text"`) || !strings.Contains(out, `"count":7`) {
		t.Fatalf("allowlisted fields must survive:\n%s", out)
	}
}

// wantAllowKeys pins the list so that adding or removing a key fails the test.
var wantAllowKeys = []string{
	"level", "time", "message", "op", "code", "reason", "component",
	"count", "event", "platform", "thread", "id", "state",
}

func TestAllowKeysPinned(t *testing.T) {
	got := map[string]bool{}
	for _, k := range AllowKeys {
		got[k] = true
	}
	if len(got) != len(wantAllowKeys) || len(AllowKeys) != len(wantAllowKeys) {
		t.Fatalf("allowlist is %v, want exactly %v", AllowKeys, wantAllowKeys)
	}
	for _, k := range wantAllowKeys {
		if !got[k] {
			t.Fatalf("allowlist lost %q", k)
		}
	}
}

// dropped lists fields the old deny list named plus the library fields the review found on stderr.
var dropped = []string{
	"data", "resp_body", "response_body", "response_proto_err", "body", "payload", "cookies", "cookie", "token",
	"tachyon_token", "auth", "message_data", "decrypted_data", "header", "headers", "content", "text",
	"evt_data", "devices", "response", "parsed_data", "raw", "url", "qr", "pair_code", "phone", "jid",
	"passkey", "public_key", "caption", "step_data", "block_data", "input_data", "item", "table",
	"error", "err", "mod", "module", "sublogger", "caller", "stack", "event_type", "ok", "Level", "OP",
}

func TestEveryOtherFieldIsDropped(t *testing.T) {
	for _, key := range dropped {
		var dst bytes.Buffer
		log := zerolog.New(New(&dst))
		log.Info().Str(key, "SECRET").Str("op", "ping").Msg("top")
		log.Info().Dict("obj", zerolog.Dict().Str("n", "1").Str(key, "SECRET")).Msg("nested")
		log.Info().Array("arr", zerolog.Arr().
			Dict(zerolog.Dict().Str("n", "1")).
			Dict(zerolog.Dict().Dict("deep", zerolog.Dict().Str(key, "SECRET")))).Msg("array")
		out := dst.String()
		if strings.Contains(out, "SECRET") {
			t.Fatalf("field %q leaks:\n%s", key, out)
		}
		lines := strings.Split(strings.TrimSpace(out), "\n")
		if len(lines) != 3 {
			t.Fatalf("field %q: want 3 lines, got %d:\n%s", key, len(lines), out)
		}
		for i, msg := range []string{"top", "nested", "array"} {
			var m map[string]any
			if err := json.Unmarshal([]byte(lines[i]), &m); err != nil {
				t.Fatalf("field %q line %d not JSON: %v", key, i, err)
			}
			if m["level"] != "info" || m["message"] != msg {
				t.Fatalf("field %q line %d lost level or message: %s", key, i, lines[i])
			}
		}
		if !strings.Contains(strings.Split(out, "\n")[0], `"op":"ping"`) {
			t.Fatalf("field %q: the allowlisted op must survive: %s", key, out)
		}
	}
}

func TestNestedValuesUnderAllowedKeysAreDropped(t *testing.T) {
	var dst bytes.Buffer
	log := zerolog.New(New(&dst))
	log.Info().Dict("thread", zerolog.Dict().Str("id", "NESTEDSECRET")).Array("reason", zerolog.Arr().Str("NESTEDSECRET")).Str("id", "m1").Msg("m")
	out := strings.TrimSpace(dst.String())
	if strings.Contains(out, "NESTEDSECRET") || out != `{"level":"info","id":"m1","message":"m"}` {
		t.Fatalf("got %s", out)
	}
}

func TestKeyOrderAndEscapingKept(t *testing.T) {
	got := string(scrubLine([]byte(`{"level":"info","data":{"a":[1,{"b":2}]},"count":1.50,"message":"a<b>&\u00e9","id":null,"state":true,"time":"x"}`)))
	want := `{"level":"info","count":1.50,"message":"a<b>&é","id":null,"state":true,"time":"x"}`
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
	log.Info().Str("op", "send_text").Str("thread", "15551234567@s.whatsapp.net").Str("reason", "ring +15551234567").
		Str("id", "ABCD-1234").Str("state", "999999999999999@lid").Str("event", "120363012345678901@g.us").
		Int("count", 42).Msg("to 15551234567:7@s.whatsapp.net via https://wa.me/settings/linked_devices#SECRETQR")
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
	if m["op"] != "send_text" || m["thread"] != "[jid]@s.whatsapp.net" || m["reason"] != "ring [phone]" || m["count"] != float64(42) ||
		m["id"] != "[code]" || m["state"] != "[jid]@lid" || m["event"] != "120363012345678901@g.us" {
		t.Fatalf("unexpected values: %s", out)
	}
	if m["message"] != "to [jid]@s.whatsapp.net via https://wa.me/settings/linked_devices#[redacted]" {
		t.Fatalf("message not scrubbed: %v", m["message"])
	}
}

func TestParseErrorsAreReducedToTheirKind(t *testing.T) {
	cases := []struct{ name, in, want string }{
		{"strconv int", `bad id: strconv.ParseInt: parsing "4111111111": invalid syntax`, "bad id: strconv.ParseInt: invalid syntax"},
		{"strconv atoi range", `strconv.Atoi: parsing "99999999999999999999999": value out of range`, "strconv.Atoi: value out of range"},
		{"strconv quoted text", `strconv.ParseUint: parsing "say \"hi\" to Ann": invalid syntax`, "strconv.ParseUint: invalid syntax"},
		{"url", `Get failed: parse "https://x.test/a b?token=SECRET": invalid character " " in URL`, "Get failed: parse: invalid character \" \" in URL"},
		{"time", `parsing time "yesterday at 5" as "2006-01-02": cannot parse "yesterday at 5" as "2006"`, "parsing time: cannot parse"},
		{"plain text kept", "failed to parse the frame", "failed to parse the frame"},
	}
	for _, c := range cases {
		if got := scrubString(c.in); got != c.want {
			t.Errorf("%s: got %q want %q", c.name, got, c.want)
		}
		var dst bytes.Buffer
		l := zerolog.New(New(&dst))
		l.Warn().Str("error", c.in).Msg(c.in)
		var m map[string]any
		if err := json.Unmarshal(dst.Bytes(), &m); err != nil {
			t.Fatalf("%s: %v", c.name, err)
		}
		if m["message"] != c.want || m["error"] != nil {
			t.Errorf("%s: logged %v", c.name, m)
		}
	}
}

// Review sample set: identifiers inside free text must be redacted wherever they appear in the message.
func TestReviewWAScrubSamples(t *testing.T) {
	samples := []struct{ in, want string }{
		{"Failed to encrypt 3EB0ABCDEF for 14155550100.0:5@s.whatsapp.net: boom", "Failed to encrypt 3EB0ABCDEF for [phone].[jid]@s.whatsapp.net: boom"},
		{"from 14155550100:23@s.whatsapp.net/ok", "from [jid]@s.whatsapp.net/ok"},
		{"phone 4155550100 called", "phone [phone] called"},
		{"code ABCD-1234 shown", "code [code] shown"},
		{"qr https://wa.me/settings/linked_devices#2@REF,NOISE,IDENT,ADV,1 end", "qr https://wa.me/settings/linked_devices#[redacted] end"},
		{"lid 100000000000001:3@lid", "lid [jid]@lid"},
		{"group 120363012345678901@g.us", "group 120363012345678901@g.us"},
		{"jid 14155550100@c.us", "jid [jid]@c.us"},
		{"jid 14155550100@s.whatsapp.net.", "jid [jid]@s.whatsapp.net."},
	}
	for _, s := range samples {
		var buf bytes.Buffer
		log := zerolog.New(New(&buf))
		log.Warn().Str("error", s.in).Msg(s.in)
		var m map[string]any
		if err := json.Unmarshal(buf.Bytes(), &m); err != nil {
			t.Fatalf("%q: %v", s.in, err)
		}
		if m["error"] != nil {
			t.Errorf("%q: the error field must be dropped: %v", s.in, m)
		}
		if got, _ := m["message"].(string); got != s.want {
			t.Errorf("%q: message %q, want %q", s.in, got, s.want)
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
