package logscrub

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/rs/zerolog"
)

// wantAllowedKeys pins the list so that adding or removing a key fails the test.
var wantAllowedKeys = []string{
	"level", "time", "message", "op", "code", "reason", "component", "count", "event", "platform", "thread", "id", "state",
}

func TestAllowedKeysPinned(t *testing.T) {
	got := map[string]bool{}
	for _, k := range AllowedKeys {
		got[k] = true
	}
	if len(got) != len(wantAllowedKeys) || len(AllowedKeys) != len(wantAllowedKeys) {
		t.Fatalf("allowlist is %v, want %v", AllowedKeys, wantAllowedKeys)
	}
	for _, k := range wantAllowedKeys {
		if !got[k] {
			t.Fatalf("allowlist lost %q", k)
		}
	}
}

func newLog(dst *bytes.Buffer) *zerolog.Logger {
	l := zerolog.New(New(dst))
	return &l
}

func fields(t *testing.T, line string) map[string]any {
	t.Helper()
	var m map[string]any
	if err := json.Unmarshal([]byte(line), &m); err != nil {
		t.Fatalf("not a JSON object: %v: %s", err, line)
	}
	return m
}

func TestOnlyAllowedTopLevelFieldsSurvive(t *testing.T) {
	var dst bytes.Buffer
	log := zerolog.New(New(&dst)).With().Timestamp().Logger()
	log.Warn().
		Str("op", "list_threads").Str("code", "io").Str("reason", "transient").Str("component", "engine").
		Int("count", 3).Str("event", "message").Str("platform", "messenger").Str("thread", "222").Str("id", "wa:1@msgr:2@msgr:X").Str("state", "connected").
		Any("step_data", []any{"my private message text from Alice: the door code is 4711"}).
		Any("block_data", map[string]any{"k": "lunch at 12 with Bob"}).
		Str("input_data", "free text instead of a number").
		Any("item", "secret note about the surprise party").
		Str("table", "LSInsertMessage").
		Err(errors.New("some failure with message text")).
		Str("data", "SECRETDATA").Str("user_agent", "Mozilla/5.0").Str("fbid", "1000123456789012").
		Msg("request done")
	out := strings.TrimSpace(dst.String())
	m := fields(t, out)
	if len(strings.Split(out, "\n")) != 1 {
		t.Fatalf("want one line: %q", out)
	}
	for k := range m {
		ok := false
		for _, a := range AllowedKeys {
			ok = ok || a == k
		}
		if !ok {
			t.Fatalf("key %q reached the output: %s", k, out)
		}
	}
	for _, want := range []string{"level", "time", "message", "op", "code", "reason", "component", "count", "event", "platform", "thread", "id", "state"} {
		if _, ok := m[want]; !ok {
			t.Fatalf("lost %q: %s", want, out)
		}
	}
	if m["message"] != "request done" || m["count"] != float64(3) || m["thread"] != "222" || m["id"] != "wa:1@msgr:2@msgr:X" {
		t.Fatalf("allowed values changed: %s", out)
	}
	for _, leak := range []string{"door code", "lunch at 12", "free text", "surprise party", "LSInsertMessage", "some failure", "SECRETDATA", "Mozilla", "1000123456789012"} {
		if strings.Contains(out, leak) {
			t.Fatalf("output leaks %q: %s", leak, out)
		}
	}
}

func TestContainersUnderAllowedKeysAreDropped(t *testing.T) {
	var dst bytes.Buffer
	log := zerolog.New(New(&dst))
	log.Info().
		Dict("state", zerolog.Dict().Str("cookie", "SECRETCOOKIE").Str("op", "inner")).
		Array("id", zerolog.Arr().Str("SECRETID").Dict(zerolog.Dict().Str("x", "SECRETNESTED"))).
		Str("op", "kept").Msg("m")
	out := dst.String()
	if strings.Contains(out, "SECRET") || strings.Contains(out, "inner") {
		t.Fatalf("container content reached the output: %s", out)
	}
	m := fields(t, strings.TrimSpace(out))
	if m["op"] != "kept" || m["message"] != "m" || m["state"] != nil || m["id"] != nil {
		t.Fatalf("unexpected fields: %s", out)
	}
}

func TestKeysMatchExactly(t *testing.T) {
	var dst bytes.Buffer
	log := zerolog.New(New(&dst))
	for _, k := range []string{"OP", " op", "op ", "Message", "\tcode", "COUNT"} {
		log.Info().Str(k, "SECRET").Msg("m")
	}
	out := dst.String()
	if strings.Contains(out, "SECRET") {
		t.Fatalf("a near-miss key was kept: %s", out)
	}
	if n := len(strings.Split(strings.TrimSpace(out), "\n")); n != 6 {
		t.Fatalf("want 6 lines, got %d: %s", n, out)
	}
}

func TestEveryNonAllowedKeyIsDropped(t *testing.T) {
	denied := []string{
		"data", "resp_body", "body", "payload", "cookies", "cookie", "token", "text", "content", "snippet", "caption", "preview",
		"key", "keys", "media_key", "xs", "c_user", "datr", "fr", "sb", "url", "error", "err", "step_data", "block_data",
		"input_data", "item", "table", "val", "struct_name", "field_name", "user_agent", "fbid", "mode", "blob_base64",
	}
	for _, k := range denied {
		var dst bytes.Buffer
		newLog(&dst).Info().Str(k, "SECRET").Msg("m")
		if strings.Contains(dst.String(), "SECRET") {
			t.Fatalf("key %q leaks: %s", k, dst.String())
		}
	}
}

func TestKeyOrderAndEscapingKept(t *testing.T) {
	got := string(scrubLine([]byte(`{"level":"info","data":{"a":[1,{"b":2}]},"count":1.50,"message":"a<b>&é","n":null,"state":true,"time":"x","thread":null}`)))
	want := `{"level":"info","count":1.50,"message":"a<b>&é","state":true,"time":"x","thread":null}`
	if got != want {
		t.Fatalf("got  %s\nwant %s", got, want)
	}
}

func TestMalformedObjectLinesAreReplacedByMarker(t *testing.T) {
	lines := []string{
		`{not json`,
		`{"level":"info","message":"x","step_data":[1,2`,
		`{"level":"info","message":"SECRET"} trailing`,
		`{"level":"info","message":"SECRET"}{"level":"info"}`,
		`{"level":"info" "message":"SECRET"}`,
	}
	for _, in := range lines {
		got := string(scrubLine([]byte(in)))
		if got != droppedJSON {
			t.Fatalf("%q -> %q, want the marker", in, got)
		}
	}
}

func TestNonObjectLinesAreScrubbedAsText(t *testing.T) {
	var dst bytes.Buffer
	w := New(&dst)
	key := strings.Repeat("Zq9", 14)
	in := "panic: boom xs=SECRETXS; datr=SECRETDATR key " + key + "\n[1,2] c_user=123456\n\n"
	if _, err := w.Write([]byte(in)); err != nil {
		t.Fatal(err)
	}
	out := dst.String()
	for _, s := range []string{"SECRETXS", "SECRETDATR", key, "c_user=123456"} {
		if strings.Contains(out, s) {
			t.Fatalf("leaks %s: %q", s, out)
		}
	}
	if !strings.Contains(out, "panic: boom") || !strings.Contains(out, "[1,2]") {
		t.Fatalf("lost text: %q", out)
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
	if err := w.Flush(); err != nil || !strings.HasSuffix(dst.String(), droppedJSON+"\n") {
		t.Fatalf("a cut-off object must flush as the marker: %v %q", err, dst.String())
	}
}

func TestOversizedPartialObjectIsDroppedAndTextIsScrubbed(t *testing.T) {
	var dst bytes.Buffer
	w := New(&dst)
	big := `{"message":"` + strings.Repeat("door code is 4711 ", MaxPartial/16)
	n, err := w.Write([]byte(big))
	if err != nil || n != len(big) {
		t.Fatalf("n=%d err=%v", n, err)
	}
	if dst.String() != droppedJSON+"\n" {
		t.Fatalf("an unfilterable object must become the marker, got %d bytes", dst.Len())
	}
	if len(w.buf) != 0 {
		t.Fatal("buffer must be empty after the drop")
	}

	var plain bytes.Buffer
	pw := New(&plain)
	text := strings.Repeat("x ", MaxPartial/2+1)
	if _, err := pw.Write([]byte(text)); err != nil || plain.Len() != len(text) {
		t.Fatalf("plain text is forwarded: err=%v forwarded=%d", err, plain.Len())
	}
}

// What is left of a dropped object arrives in later writes; none of it may be treated as a fresh line.
func TestTailOfADroppedOversizedObjectIsDiscarded(t *testing.T) {
	var dst bytes.Buffer
	w := New(&dst)
	if _, err := w.Write([]byte(`{"message":"` + strings.Repeat("a", MaxPartial))); err != nil {
		t.Fatal(err)
	}
	if dst.String() != droppedJSON+"\n" {
		t.Fatalf("want the marker, got %d bytes", dst.Len())
	}
	for _, part := range []string{`more text, the pin is 4711`, `","step_data":"secret note"}`} {
		n, err := w.Write([]byte(part))
		if err != nil || n != len(part) {
			t.Fatalf("n=%d err=%v", n, err)
		}
	}
	tail := "\"}\n" + `{"level":"info","op":"after","message":"next line"}` + "\n"
	if n, err := w.Write([]byte(tail)); err != nil || n != len(tail) {
		t.Fatalf("n=%d err=%v", n, err)
	}
	out := dst.String()
	if strings.Contains(out, "4711") || strings.Contains(out, "secret note") {
		t.Fatalf("the tail of the dropped object reached the output: %s", out)
	}
	if want := droppedJSON + "\n" + `{"level":"info","op":"after","message":"next line"}` + "\n"; out != want {
		t.Fatalf("got %q\nwant %q", out, want)
	}
	if _, err := w.Write([]byte(`{"level":"info","message":"again"}` + "\n")); err != nil || !strings.HasSuffix(dst.String(), `"message":"again"}`+"\n") {
		t.Fatalf("filtering must resume: %v %q", err, dst.String())
	}
}

func TestProtocolNodesAreRedactedFromMessages(t *testing.T) {
	var dst bytes.Buffer
	newLog(&dst).Warn().Msg(`Node handling took 12s for <message from="1000123@msgr" id="X" notify="Alice Example"><enc v="2"/></message>`)
	out := dst.String()
	if strings.Contains(out, "Alice Example") || strings.Contains(out, "1000123") || !strings.Contains(out, "Node handling took 12s for") {
		t.Fatalf("got %s", out)
	}
}

func TestPartialTextFlushScrubbed(t *testing.T) {
	var dst bytes.Buffer
	w := New(&dst)
	w.Write([]byte("tail xs=SECRETXS"))
	if err := w.Flush(); err != nil || strings.Contains(dst.String(), "SECRETXS") {
		t.Fatalf("flush: %v %q", err, dst.String())
	}
}

func TestCookiePatternsRedactedInAllowedStrings(t *testing.T) {
	for _, name := range []string{"xs", "c_user", "datr", "fr", "sb", "presence", "wd"} {
		secret := "SECRETVAL9"
		for _, sep := range []string{";", "&", " ", "\"", "'", ""} {
			msg := "request failed " + name + "=" + secret + sep + "tail"
			var dst bytes.Buffer
			log := zerolog.New(New(&dst))
			log.Info().Str("reason", msg).Msg(msg)
			out := dst.String()
			if strings.Contains(out, secret) {
				t.Fatalf("%s sep %q leaks:\n%s", name, sep, out)
			}
			if !strings.Contains(out, name+"=REDACTED") {
				t.Fatalf("%s sep %q lost marker:\n%s", name, sep, out)
			}
			if sep != "" && sep != "\"" && !strings.Contains(out, "tail") {
				t.Fatalf("%s sep %q ate the tail:\n%s", name, sep, out)
			}
		}
	}
}

func TestCookieHeaderValueRedacted(t *testing.T) {
	var dst bytes.Buffer
	newLog(&dst).Info().Str("reason", "c_user=100012345678901; xs=47%3Aabc; datr=ZZtop; sb=q; fr=0zz.AAA; keep=1").Msg("m")
	out := dst.String()
	for _, secret := range []string{"100012345678901", "47%3Aabc", "ZZtop", "0zz.AAA"} {
		if strings.Contains(out, secret) {
			t.Fatalf("leaks %s: %s", secret, out)
		}
	}
	if !strings.Contains(out, "keep=1") {
		t.Fatalf("lost unrelated pair: %s", out)
	}
}

func TestLongKeyRunsRedacted(t *testing.T) {
	b64 := "dGhpc2lzYW5leGFtcGxlb2Zrcnlwb2dyYXBoaWNrZXk9"
	b64url := "dGhpc2lzYW5leGFtcGxl-b2Zrcnlwb2dy_YXBoaWNrZXk"
	hex := strings.Repeat("ab12", 16)
	var dst bytes.Buffer
	newLog(&dst).Info().Str("reason", "bad "+b64+" and "+hex).Str("op", b64url).Msg("k " + b64)
	out := dst.String()
	for _, s := range []string{b64, b64url, hex} {
		if strings.Contains(out, s) {
			t.Fatalf("leaks %s: %s", s, out)
		}
	}
	if !strings.Contains(out, "REDACTED") {
		t.Fatalf("no marker: %s", out)
	}
}

func TestTokenKeyValuePatternsRedacted(t *testing.T) {
	for _, name := range []string{"fb_dtsg", "lsd", "jazoest", "access_token", "__user"} {
		msg := "bad request " + name + "=SECRETTOK&other=1"
		var dst bytes.Buffer
		newLog(&dst).Info().Str("reason", msg).Msg(msg)
		out := dst.String()
		if strings.Contains(out, "SECRETTOK") || !strings.Contains(out, name+"=REDACTED") || !strings.Contains(out, "other=1") {
			t.Fatalf("%s: %s", name, out)
		}
	}
}

func TestURLsRedactedInStrings(t *testing.T) {
	msgs := []string{
		"GET https://www.facebook.com/checkpoint/?next=SECRETPATH failed",
		`got "http://scontent.xx.fbcdn.net/v/t1/SECRETPATH.jpg" back`,
		"HTTPS://EXAMPLE.com/SECRETPATH",
	}
	for _, msg := range msgs {
		var dst bytes.Buffer
		newLog(&dst).Info().Str("reason", msg).Msg(msg)
		out := dst.String()
		if strings.Contains(out, "SECRETPATH") || strings.Contains(out, "facebook.com") || strings.Contains(out, "fbcdn") || !strings.Contains(out, "REDACTED") {
			t.Fatalf("%q: %s", msg, out)
		}
	}
	out := string(scrubLine([]byte(`{"message":"GET https://a.example/x y"}`)))
	if out != `{"message":"GET REDACTED y"}` {
		t.Fatalf("whitespace must end the URL: %s", out)
	}
}

func TestPhoneNumbersRedactedButIDsAndTimesSurvive(t *testing.T) {
	var dst bytes.Buffer
	newLog(&dst).Info().
		Str("reason", "call +1 415 555 0100 or +15551234567 or +1 (415) 555-0100 now").
		Str("id", "wa:100012345678901@msgr:100012345678902@msgr:ABC").
		Str("thread", "100012345678901").
		Str("time", "2026-10-24T10:00:00+02:00").Msg("m")
	out := dst.String()
	for _, secret := range []string{"415 555 0100", "5551234567", "(415) 555-0100"} {
		if strings.Contains(out, secret) {
			t.Fatalf("phone number %q leaks: %s", secret, out)
		}
	}
	for _, keep := range []string{"100012345678901@msgr", `"thread":"100012345678901"`, "10:00:00+02:00"} {
		if !strings.Contains(out, keep) {
			t.Fatalf("lost %q: %s", keep, out)
		}
	}
}

func TestParseErrorsAreReducedToTheirKind(t *testing.T) {
	_, perr := strconv.ParseInt("free text instead of a number", 10, 64)
	_, ferr := strconv.ParseFloat("call me at 555", 64)
	_, aerr := strconv.Atoi("door code is 4711")
	_, oerr := strconv.ParseInt("99999999999999999999", 10, 64)
	_, terr := time.Parse(time.RFC3339, "lunch at 12 with Bob")
	var j struct{}
	jerr := json.Unmarshal([]byte("{secret note}"), &j)
	cases := []struct {
		err  error
		want string
		leak string
	}{
		{perr, "strconv.ParseInt: invalid syntax", "free text"},
		{ferr, "strconv.ParseFloat: invalid syntax", "call me"},
		{aerr, "strconv.Atoi: invalid syntax", "door code"},
		{oerr, "strconv.ParseInt: value out of range", "9999999999"},
		{terr, "time parse error", "lunch"},
		{jerr, "json syntax error", "secret"},
		{fmt.Errorf("wrapped: %w", perr), "wrapped: strconv.ParseInt: invalid syntax", "free text"},
	}
	for _, c := range cases {
		var dst bytes.Buffer
		newLog(&dst).Warn().Msg("failed: " + c.err.Error())
		out := dst.String()
		if strings.Contains(out, c.leak) || !strings.Contains(out, c.want) {
			t.Fatalf("%v\n -> %s\nwant %q without %q", c.err, out, c.want, c.leak)
		}
	}
	got := scrubText(`url parse failed: parse "http://x/%zz secret": invalid URL escape "%zz"`)
	if strings.Contains(got, "secret") || !strings.Contains(got, "url parse error") {
		t.Fatalf("url parse error: %s", got)
	}
}

func TestNormalFieldsSurvive(t *testing.T) {
	var dst bytes.Buffer
	log := zerolog.New(New(&dst)).With().Timestamp().Logger()
	log.Info().Str("op", "list_threads").Str("code", "rate_limited").Int("count", 42).Msg("request done")
	out := dst.String()
	for _, want := range []string{`"op":"list_threads"`, `"code":"rate_limited"`, `"count":42`, `"message":"request done"`, `"time":"`, `"level":"info"`} {
		if !strings.Contains(out, want) {
			t.Fatalf("lost %s: %s", want, out)
		}
	}
}
