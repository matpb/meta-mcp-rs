// Package ipc implements the line-delimited JSON framing shared with the Rust server.
package ipc

import (
	"bufio"
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"sync"
)

// MaxLine is the largest accepted frame in bytes.
const MaxLine = 16 << 20

var (
	// ErrTooLong means a line exceeded MaxLine.
	ErrTooLong = errors.New("ipc: frame exceeds 16 MiB")
	// ErrMalformed means a line was not a valid JSON frame.
	ErrMalformed = errors.New("ipc: malformed frame")
)

// Request is a frame from Rust to the sidecar.
type Request struct {
	ID     uint64          `json:"id"`
	Op     string          `json:"op"`
	Params json.RawMessage `json:"params"`
}

// ReplyError is the error body of a failed Reply.
type ReplyError struct {
	Code    string `json:"code"`
	Message string `json:"message"`
}

// Reply answers exactly one Request.
type Reply struct {
	ID     uint64          `json:"id"`
	OK     bool            `json:"ok"`
	Result json.RawMessage `json:"result,omitempty"`
	Error  *ReplyError     `json:"error,omitempty"`
}

// Event is an unsolicited frame from the sidecar.
type Event struct {
	Event string          `json:"event"`
	Data  json.RawMessage `json:"data"`
}

// Reader yields one frame line at a time, skipping blank lines.
type Reader struct {
	sc *bufio.Scanner
}

// NewReader wraps r with a MaxLine limit.
func NewReader(r io.Reader) *Reader {
	sc := bufio.NewScanner(r)
	sc.Buffer(make([]byte, 0, 64*1024), MaxLine)
	return &Reader{sc: sc}
}

// Next returns the next non-empty line, io.EOF at end of input, or ErrTooLong.
func (r *Reader) Next() ([]byte, error) {
	for r.sc.Scan() {
		line := bytes.TrimSpace(r.sc.Bytes())
		if len(line) == 0 {
			continue
		}
		out := make([]byte, len(line))
		copy(out, line)
		return out, nil
	}
	if err := r.sc.Err(); err != nil {
		if errors.Is(err, bufio.ErrTooLong) {
			return nil, ErrTooLong
		}
		return nil, err
	}
	return nil, io.EOF
}

// DecodeRequest parses one JSON object line into a Request without echoing its content in errors.
func DecodeRequest(line []byte) (Request, error) {
	if t := bytes.TrimLeft(line, " \t\r\n"); len(t) == 0 || t[0] != '{' {
		return Request{}, ErrMalformed
	}
	var req Request
	if err := json.Unmarshal(line, &req); err != nil {
		return Request{}, ErrMalformed
	}
	return req, nil
}

// Writer serialises frames to an io.Writer, one flushed line each.
type Writer struct {
	mu sync.Mutex
	bw *bufio.Writer
}

// NewWriter wraps w.
func NewWriter(w io.Writer) *Writer {
	return &Writer{bw: bufio.NewWriter(w)}
}

// Write encodes v, appends a newline and flushes; a line over MaxLine is refused with ErrTooLong.
func (w *Writer) Write(v any) error {
	b, err := json.Marshal(v)
	if err != nil {
		return fmt.Errorf("ipc: encode frame: %w", err)
	}
	if len(b)+1 > MaxLine {
		return ErrTooLong
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	if _, err := w.bw.Write(b); err != nil {
		return err
	}
	if err := w.bw.WriteByte('\n'); err != nil {
		return err
	}
	return w.bw.Flush()
}
