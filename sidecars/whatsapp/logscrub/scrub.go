// Package logscrub removes sensitive keys and WhatsApp identifiers from zerolog JSON lines before they reach stderr.
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

// DenyKeys are the log keys removed at any depth, matched case-insensitively after trimming.
var DenyKeys = []string{
	"data", "resp_body", "response_body", "response_proto_err",
	"body", "payload", "cookies", "cookie", "token",
	"tachyon_token", "auth", "message_data", "decrypted_data",
	"header", "headers", "content", "text",
	"evt_data", "devices", "response", "parsed_data", "raw", "url",
	"qr", "code", "pair_code", "phone", "jid", "passkey", "public_key", "caption",
}

// valueRe matches, leftmost and in this order: QR payload, group JID (kept), user JID, pair code, phone number.
var valueRe = regexp.MustCompile(
	`https://wa\.me/settings/linked_devices#\S*` +
		`|\b\d+(?:-\d+)?@g\.us\b` +
		`|\b\d+(?::\d+)?@(?:s\.whatsapp\.net|lid|c\.us)\b` +
		`|\b[A-Z0-9]{4}-[A-Z0-9]{4}\b` +
		`|\+?\b\d{7,15}\b`)

const qrPrefix = "https://wa.me/settings/linked_devices#"

// scrubString redacts QR payloads, user JIDs, pair codes and phone numbers in s.
func scrubString(s string) string {
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

var blocked = func() map[string]struct{} {
	m := make(map[string]struct{}, len(DenyKeys))
	for _, k := range DenyKeys {
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

// scrubLine returns line without blocked keys and with values scrubbed; other lines get value scrubbing only.
func scrubLine(line []byte) []byte {
	trimmed := bytes.TrimSpace(line)
	if len(trimmed) == 0 || trimmed[0] != '{' {
		return []byte(scrubString(string(line)))
	}
	dec := json.NewDecoder(bytes.NewReader(trimmed))
	dec.UseNumber()
	var out bytes.Buffer
	tok, err := dec.Token()
	if err != nil || writeValue(dec, tok, &out) != nil {
		return []byte(scrubString(string(line)))
	}
	if dec.InputOffset() != int64(len(trimmed)) {
		return []byte(scrubString(string(line)))
	}
	return out.Bytes()
}

func writeValue(dec *json.Decoder, tok json.Token, out *bytes.Buffer) error {
	delim, isDelim := tok.(json.Delim)
	if !isDelim {
		if str, ok := tok.(string); ok {
			tok = scrubString(str)
		}
		return writeScalar(tok, out)
	}
	closer := byte(']')
	if delim == '{' {
		closer = '}'
	}
	out.WriteByte(byte(delim))
	first := true
	for dec.More() {
		var key string
		if delim == '{' {
			kt, err := dec.Token()
			if err != nil {
				return err
			}
			key, _ = kt.(string)
		}
		vt, err := dec.Token()
		if err != nil {
			return err
		}
		if delim == '{' {
			if _, drop := blocked[strings.ToLower(strings.TrimSpace(key))]; drop {
				if err := skipValue(dec, vt); err != nil {
					return err
				}
				continue
			}
		}
		if !first {
			out.WriteByte(',')
		}
		first = false
		if delim == '{' {
			if err := writeScalar(key, out); err != nil {
				return err
			}
			out.WriteByte(':')
		}
		if err := writeValue(dec, vt, out); err != nil {
			return err
		}
	}
	if _, err := dec.Token(); err != nil {
		return err
	}
	out.WriteByte(closer)
	return nil
}

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
