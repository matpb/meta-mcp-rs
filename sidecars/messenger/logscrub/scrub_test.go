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
	"xs", "c_user", "datr", "fr", "sb", "cookie_header", "set-cookie",
	"snippet", "caption", "preview", "otid_text", "key", "keys", "media_key",
	"identity_key", "noise_key", "signed_pre_key", "adv_secret", "cat",
	"crypto_auth_token", "access_token", "lsd", "dtsg", "fb_dtsg", "jazoest",
	"password", "pin", "reply_text", "quoted_text",
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
	big := strings.Repeat("x ", MaxPartial/2+1)
	n, err := w.Write([]byte(big))
	if err != nil || n != len(big) || dst.Len() != len(big) {
		t.Fatalf("n=%d err=%v forwarded=%d", n, err, dst.Len())
	}
	if len(w.buf) != 0 {
		t.Fatal("buffer must be empty after the flush")
	}
}

func TestUserAgentKeyIsNotDenied(t *testing.T) {
	for _, k := range DenyKeys {
		if strings.EqualFold(k, "user_agent") {
			t.Fatal("user_agent must stay loggable")
		}
	}
}

func TestCookiePatternsRedactedInStrings(t *testing.T) {
	cookies := []string{"xs", "c_user", "datr", "fr", "sb", "presence", "wd"}
	for _, name := range cookies {
		secret := "SECRETVAL9"
		for _, sep := range []string{";", "&", " ", "\"", "'", ""} {
			msg := "request failed " + name + "=" + secret + sep + "tail"
			var dst bytes.Buffer
			log := zerolog.New(New(&dst))
			log.Info().Str("error", msg).Dict("nested", zerolog.Dict().Str("detail", msg)).Msg(msg)
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

func TestCookieHeaderValueRedactedAtAnyDepth(t *testing.T) {
	var dst bytes.Buffer
	log := zerolog.New(New(&dst))
	log.Info().Array("arr", zerolog.Arr().Dict(zerolog.Dict().Str("e", "c_user=100012345678901; xs=47%3Aabc; datr=ZZtop; sb=q; fr=0zz.AAA; keep=1"))).Msg("m")
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
	log := zerolog.New(New(&dst))
	log.Info().Str("error", "bad "+b64+" and "+hex).Str("other", b64url).Msg("k " + b64)
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

func TestNonJSONLineScrubbed(t *testing.T) {
	var dst bytes.Buffer
	w := New(&dst)
	key := strings.Repeat("Zq9", 14)
	in := "panic: boom xs=SECRETXS; datr=SECRETDATR key " + key + "\n{broken c_user=123456 \n"
	if _, err := w.Write([]byte(in)); err != nil {
		t.Fatal(err)
	}
	out := dst.String()
	for _, s := range []string{"SECRETXS", "SECRETDATR", key, "c_user=123456"} {
		if strings.Contains(out, s) {
			t.Fatalf("leaks %s: %q", s, out)
		}
	}
	if !strings.Contains(out, "panic: boom") {
		t.Fatalf("lost text: %q", out)
	}
}

func TestPartialFlushScrubbed(t *testing.T) {
	var dst bytes.Buffer
	w := New(&dst)
	w.Write([]byte("tail xs=SECRETXS"))
	if err := w.Flush(); err != nil || strings.Contains(dst.String(), "SECRETXS") {
		t.Fatalf("flush: %v %q", err, dst.String())
	}
}

func TestNormalFieldsSurvive(t *testing.T) {
	var dst bytes.Buffer
	log := zerolog.New(New(&dst)).With().Timestamp().Logger()
	log.Info().Str("op", "list_threads").Str("code", "rate_limited").Int("threads", 42).Str("fbid", "1000123456789012").Str("user_agent", "Mozilla/5.0 (X11; Linux x86_64)").Msg("request done")
	out := dst.String()
	for _, want := range []string{`"op":"list_threads"`, `"code":"rate_limited"`, `"threads":42`, `"fbid":"1000123456789012"`, `"user_agent":"Mozilla/5.0 (X11; Linux x86_64)"`, `"message":"request done"`, `"time":"`} {
		if !strings.Contains(out, want) {
			t.Fatalf("lost %s: %s", want, out)
		}
	}
}

func TestTokenKeyValuePatternsRedacted(t *testing.T) {
	for _, name := range []string{"fb_dtsg", "lsd", "jazoest", "access_token", "__user"} {
		msg := "bad request " + name + "=SECRETTOK&other=1"
		var dst bytes.Buffer
		log := zerolog.New(New(&dst))
		log.Info().Str("error", msg).Dict("n", zerolog.Dict().Str("d", msg)).Msg(msg)
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
		log := zerolog.New(New(&dst))
		log.Info().Str("error", msg).Dict("n", zerolog.Dict().Str("d", msg)).Msg(msg)
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
