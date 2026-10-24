// Package logscrub keeps an allowlist of top-level fields of zerolog JSON lines and redacts secrets in their strings.
package logscrub

import (
	"bytes"
	"encoding/json"
	"io"
	"regexp"
	"sync"
)

// MaxPartial is the longest unterminated line held back before it is dropped or flushed.
const MaxPartial = 1 << 20

// AllowedKeys are the only top-level log keys that reach stderr; every other field is dropped.
// Values must be scalars, so an object or array under an allowed key is dropped too.
var AllowedKeys = []string{
	"level", "time", "message", "op", "code", "reason", "component", "count", "event", "platform", "thread", "id", "state",
}

const redacted = "REDACTED"

// droppedJSON replaces a JSON object line that cannot be filtered (too long, cut off or malformed).
const droppedJSON = `{"level":"warn","message":"log line dropped: not filterable"}`

var (
	// cookieValue matches name=value for the Facebook session cookies; the value ends at ; & space or quote.
	cookieValue = regexp.MustCompile(`(?i)\b(xs|c_user|datr|fr|sb|presence|wd|fb_dtsg|lsd|jazoest|access_token|__user)=[^;&\s"']*`)
	// urlRun matches an http or https URL up to whitespace or a quote.
	urlRun = regexp.MustCompile(`(?i)https?://[^\s"']+`)
	// keyRun matches 32 or more base64, base64url or hex characters in a row, which is key material.
	keyRun = regexp.MustCompile(`[A-Za-z0-9+/_=-]{32,}`)
	// phoneRun matches an international phone number written with a leading plus.
	phoneRun = regexp.MustCompile(`\+\d[\d\s().-]{6,}\d`)
	// xmlTag matches a tag with attributes, as whatsmeow prints a protocol node with its push name.
	xmlTag = regexp.MustCompile(`<[A-Za-z][^<>=]*=[^<>]*>`)

	// The four below match Go parse errors that print the input they could not parse.
	strconvErr = regexp.MustCompile(`strconv\.(\w+): parsing "(?:[^"\\]|\\.)*": (invalid syntax|value out of range|invalid base \d+|invalid bit size \d+)`)
	timeErr    = regexp.MustCompile(`parsing time "(?:[^"\\]|\\.)*"(?: as "(?:[^"\\]|\\.)*")?: [^\n]*`)
	urlErr     = regexp.MustCompile(`\bparse "(?:[^"\\]|\\.)*": [^\n]*`)
	jsonErr    = regexp.MustCompile(`invalid character '(?:[^'\\]|\\.)+' [a-z ]+`)
)

// reduceParseErrors cuts the quoted input out of Go parse errors and keeps only their kind.
func reduceParseErrors(s string) string {
	s = strconvErr.ReplaceAllString(s, "strconv.$1: $2")
	s = timeErr.ReplaceAllString(s, "time parse error")
	s = urlErr.ReplaceAllString(s, "url parse error")
	return jsonErr.ReplaceAllString(s, "json syntax error")
}

// scrubText reduces parse errors and redacts URLs, cookie and token values, phone numbers, XML tags and long key-like runs.
func scrubText(s string) string {
	s = reduceParseErrors(s)
	s = urlRun.ReplaceAllString(s, redacted)
	s = cookieValue.ReplaceAllString(s, "${1}="+redacted)
	s = phoneRun.ReplaceAllString(s, redacted)
	s = xmlTag.ReplaceAllString(s, redacted)
	return keyRun.ReplaceAllString(s, redacted)
}

var allowed = func() map[string]struct{} {
	m := make(map[string]struct{}, len(AllowedKeys))
	for _, k := range AllowedKeys {
		m[k] = struct{}{}
	}
	return m
}()

// Writer scrubs each complete line written through it and forwards it to dst.
// After an oversized JSON object was dropped, skip discards the rest of that line.
type Writer struct {
	mu   sync.Mutex
	dst  io.Writer
	buf  []byte
	skip bool
}

// New wraps dst.
func New(dst io.Writer) *Writer {
	return &Writer{dst: dst}
}

// Write buffers p until newline, then forwards each scrubbed line in one write.
func (w *Writer) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	n := len(p)
	if w.skip {
		i := bytes.IndexByte(p, '\n')
		if i < 0 {
			return n, nil
		}
		p, w.skip = p[i+1:], false
	}
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
			return n, err
		}
	}
	if len(w.buf) > MaxPartial {
		out, dropped := partialOut(w.buf)
		w.buf, w.skip = nil, dropped
		_, err := w.dst.Write(out)
		return n, err
	}
	if len(w.buf) == 0 {
		w.buf = nil
	}
	return n, nil
}

// Flush forwards a pending unterminated line; a cut-off JSON object cannot be filtered and is dropped.
func (w *Writer) Flush() error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if len(w.buf) == 0 {
		return nil
	}
	out, _ := partialOut(w.buf)
	w.buf = nil
	_, err := w.dst.Write(out)
	return err
}

// partialOut is what an unterminated buffer becomes: a marker for a JSON object (dropped is true), scrubbed text otherwise.
func partialOut(buf []byte) (out []byte, dropped bool) {
	if t := bytes.TrimSpace(buf); len(t) > 0 && t[0] == '{' {
		return []byte(droppedJSON + "\n"), true
	}
	return []byte(scrubText(string(buf))), false
}

// plainLine scrubs a line that is not one JSON object as text.
func plainLine(line []byte) []byte {
	return []byte(scrubText(string(line)))
}

// scrubLine returns the allowed scalar fields of a JSON object line with secrets redacted in their strings.
// A line that starts like an object but does not parse is replaced by a marker, never forwarded.
func scrubLine(line []byte) []byte {
	trimmed := bytes.TrimSpace(line)
	if len(trimmed) == 0 || trimmed[0] != '{' {
		return plainLine(line)
	}
	if out, ok := filterObject(trimmed); ok {
		return out
	}
	return []byte(droppedJSON)
}

// filterObject keeps the allowed scalar fields of one JSON object; ok is false when it does not parse.
func filterObject(obj []byte) (out []byte, ok bool) {
	dec := json.NewDecoder(bytes.NewReader(obj))
	dec.UseNumber()
	if tok, err := dec.Token(); err != nil || tok != json.Delim('{') {
		return nil, false
	}
	var buf bytes.Buffer
	buf.WriteByte('{')
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
		if _, isDelim := vt.(json.Delim); isDelim {
			if skipValue(dec, vt) != nil {
				return nil, false
			}
			continue
		}
		if _, keep := allowed[key]; !keep {
			continue
		}
		if str, isStr := vt.(string); isStr {
			vt = scrubText(str)
		}
		if !first {
			buf.WriteByte(',')
		}
		first = false
		if writeScalar(key, &buf) != nil {
			return nil, false
		}
		buf.WriteByte(':')
		if writeScalar(vt, &buf) != nil {
			return nil, false
		}
	}
	if tok, err := dec.Token(); err != nil || tok != json.Delim('}') || dec.InputOffset() != int64(len(obj)) {
		return nil, false
	}
	buf.WriteByte('}')
	return buf.Bytes(), true
}

// skipValue consumes the rest of an object or array whose opening delimiter was already read.
func skipValue(dec *json.Decoder, tok json.Token) error {
	d, ok := tok.(json.Delim)
	if !ok || (d != '{' && d != '[') {
		return nil
	}
	for depth := 1; depth > 0; {
		t, err := dec.Token()
		if err != nil {
			return err
		}
		if dd, isD := t.(json.Delim); isD {
			if dd == '{' || dd == '[' {
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
