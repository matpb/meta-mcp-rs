// Package logscrub keeps an allowlist of log fields and redacts WhatsApp identifiers in zerolog JSON lines before they reach stderr.
package logscrub

import (
	"bytes"
	"encoding/json"
	"io"
	"regexp"
	"strings"
	"sync"
)

// MaxPartial is the longest unterminated line held back before it is flushed as is.
const MaxPartial = 1 << 20

// AllowKeys are the only top-level log fields that survive; every other field, nested ones included, is dropped.
var AllowKeys = []string{
	"level", "time", "message", "op", "code", "reason", "component",
	"count", "event", "platform", "thread", "id", "state",
}

// valueRe matches, leftmost and in this order: QR payload, group JID (kept), user JID, pair code, phone number.
var valueRe = regexp.MustCompile(
	`https://wa\.me/settings/linked_devices#\S*` +
		`|\b\d+(?:-\d+)?@g\.us\b` +
		`|\b\d+(?::\d+)?@(?:s\.whatsapp\.net|lid|c\.us)\b` +
		`|\b[A-Z0-9]{4}-[A-Z0-9]{4}\b` +
		`|\+?\b\d{7,15}\b`)

const qrPrefix = "https://wa.me/settings/linked_devices#"

// parseErrRes cut the input that strconv, net/url and time errors quote and keep the error kind.
var parseErrRes = []struct {
	re   *regexp.Regexp
	repl string
}{
	{regexp.MustCompile(`(strconv\.\w+): parsing "(?:[^"\\]|\\.)*": `), "$1: "},
	{regexp.MustCompile(`\bparse "(?:[^"\\]|\\.)*": `), "parse: "},
	{regexp.MustCompile(`parsing time "(?:[^"\\]|\\.)*"(?: as "(?:[^"\\]|\\.)*")?: cannot parse "(?:[^"\\]|\\.)*" as "(?:[^"\\]|\\.)*"`), "parsing time: cannot parse"},
}

// scrubString redacts QR payloads, user JIDs, pair codes and phone numbers in s and reduces parse errors to their kind.
func scrubString(s string) string {
	for _, p := range parseErrRes {
		s = p.re.ReplaceAllString(s, p.repl)
	}
	return valueRe.ReplaceAllStringFunc(s, func(m string) string {
		switch {
		case strings.HasPrefix(m, qrPrefix):
			return qrPrefix + "[redacted]"
		case strings.HasSuffix(m, "@g.us"):
			return m
		case strings.Contains(m, "@"):
			return "[jid]" + m[strings.IndexByte(m, '@'):]
		case strings.Contains(m, "-"):
			return "[code]"
		}
		return "[phone]"
	})
}

var allowed = func() map[string]struct{} {
	m := make(map[string]struct{}, len(AllowKeys))
	for _, k := range AllowKeys {
		m[k] = struct{}{}
	}
	return m
}()

// Writer scrubs each complete line written through it and forwards it to dst.
type Writer struct {
	mu  sync.Mutex
	dst io.Writer
	buf []byte
}

// New wraps dst.
func New(dst io.Writer) *Writer {
	return &Writer{dst: dst}
}

// Write buffers p until newline, then forwards each scrubbed line in one write.
func (w *Writer) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.buf = append(w.buf, p...)
	for {
		i := bytes.IndexByte(w.buf, '\n')
		if i < 0 {
			break
		}
		line := w.buf[:i]
		out := append(scrubLine(line), '\n')
		w.buf = w.buf[i+1:]
		if _, err := w.dst.Write(out); err != nil {
			return len(p), err
		}
	}
	if len(w.buf) > MaxPartial {
		_, err := w.dst.Write([]byte(scrubString(string(w.buf))))
		w.buf = nil
		return len(p), err
	}
	if len(w.buf) == 0 {
		w.buf = nil
	}
	return len(p), nil
}

// Flush forwards a pending unterminated line with value scrubbing only.
func (w *Writer) Flush() error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if len(w.buf) == 0 {
		return nil
	}
	_, err := w.dst.Write([]byte(scrubString(string(w.buf))))
	w.buf = nil
	return err
}

// scrubLine keeps allowlisted scalar fields of a JSON object line and scrubs their values; other lines get value scrubbing only.
func scrubLine(line []byte) []byte {
	trimmed := bytes.TrimSpace(line)
	if len(trimmed) == 0 || trimmed[0] != '{' {
		return []byte(scrubString(string(line)))
	}
	out, ok := filterObject(trimmed)
	if !ok {
		return []byte(scrubString(string(line)))
	}
	return out
}

// filterObject keeps the allowlisted scalar fields of one JSON object; ok is false when raw is not exactly one object.
func filterObject(raw []byte) ([]byte, bool) {
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.UseNumber()
	if tok, err := dec.Token(); err != nil || tok != json.Delim('{') {
		return nil, false
	}
	var out bytes.Buffer
	out.WriteByte('{')
	first := true
	for dec.More() {
		kt, err := dec.Token()
		if err != nil {
			return nil, false
		}
		key, _ := kt.(string)
		vt, err := dec.Token()
		if err != nil {
			return nil, false
		}
		if _, nested := vt.(json.Delim); nested {
			if err := skipNested(dec); err != nil {
				return nil, false
			}
			continue
		}
		if _, keep := allowed[key]; !keep {
			continue
		}
		if str, isStr := vt.(string); isStr {
			vt = scrubString(str)
		}
		if !first {
			out.WriteByte(',')
		}
		first = false
		if err := writeScalar(key, &out); err != nil {
			return nil, false
		}
		out.WriteByte(':')
		if err := writeScalar(vt, &out); err != nil {
			return nil, false
		}
	}
	if tok, err := dec.Token(); err != nil || tok != json.Delim('}') {
		return nil, false
	}
	if dec.InputOffset() != int64(len(raw)) {
		return nil, false
	}
	out.WriteByte('}')
	return out.Bytes(), true
}

// skipNested consumes the rest of an object or array whose opening delimiter was just read.
func skipNested(dec *json.Decoder) error {
	for depth := 1; depth > 0; {
		t, err := dec.Token()
		if err != nil {
			return err
		}
		if d, isDelim := t.(json.Delim); isDelim {
			if d == '{' || d == '[' {
				depth++
			} else {
				depth--
			}
		}
	}
	return nil
}

func writeScalar(v any, out *bytes.Buffer) error {
	var b bytes.Buffer
	enc := json.NewEncoder(&b)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(v); err != nil {
		return err
	}
	out.Write(bytes.TrimRight(b.Bytes(), "\n"))
	return nil
}
