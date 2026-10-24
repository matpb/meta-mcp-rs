package ipc

import (
	"bytes"
	"encoding/json"
	"errors"
	"io"
	"strings"
	"testing"
)

func TestRequestRoundTrip(t *testing.T) {
	var buf bytes.Buffer
	if err := NewWriter(&buf).Write(Request{ID: 7, Op: "ping", Params: json.RawMessage("null")}); err != nil {
		t.Fatal(err)
	}
	if got, want := buf.String(), `{"id":7,"op":"ping","params":null}`+"\n"; got != want {
		t.Fatalf("got %q want %q", got, want)
	}
	line, err := NewReader(&buf).Next()
	if err != nil {
		t.Fatal(err)
	}
	req, err := DecodeRequest(line)
	if err != nil {
		t.Fatal(err)
	}
	if req.ID != 7 || req.Op != "ping" || string(req.Params) != "null" {
		t.Fatalf("unexpected request %+v", req)
	}
}

func TestReplyOK(t *testing.T) {
	var buf bytes.Buffer
	if err := NewWriter(&buf).Write(Reply{ID: 7, OK: true, Result: json.RawMessage(`{"pong":true}`)}); err != nil {
		t.Fatal(err)
	}
	if got, want := buf.String(), `{"id":7,"ok":true,"result":{"pong":true}}`+"\n"; got != want {
		t.Fatalf("got %q want %q", got, want)
	}
	var back Reply
	if err := json.Unmarshal(buf.Bytes(), &back); err != nil {
		t.Fatal(err)
	}
	if !back.OK || back.Error != nil || string(back.Result) != `{"pong":true}` {
		t.Fatalf("unexpected reply %+v", back)
	}
}

func TestReplyError(t *testing.T) {
	var buf bytes.Buffer
	rep := Reply{ID: 7, Error: &ReplyError{Code: "unknown_op", Message: "no such op"}}
	if err := NewWriter(&buf).Write(rep); err != nil {
		t.Fatal(err)
	}
	want := `{"id":7,"ok":false,"error":{"code":"unknown_op","message":"no such op"}}` + "\n"
	if buf.String() != want {
		t.Fatalf("got %q want %q", buf.String(), want)
	}
	var back Reply
	if err := json.Unmarshal(buf.Bytes(), &back); err != nil {
		t.Fatal(err)
	}
	if back.OK || back.Error == nil || back.Error.Code != "unknown_op" {
		t.Fatalf("unexpected reply %+v", back)
	}
}

func TestEvent(t *testing.T) {
	var buf bytes.Buffer
	ev := Event{Event: "ready", Data: json.RawMessage(`{"pid":1,"protocol":1}`)}
	if err := NewWriter(&buf).Write(ev); err != nil {
		t.Fatal(err)
	}
	if got, want := buf.String(), `{"event":"ready","data":{"pid":1,"protocol":1}}`+"\n"; got != want {
		t.Fatalf("got %q want %q", got, want)
	}
	var back Event
	if err := json.Unmarshal(buf.Bytes(), &back); err != nil {
		t.Fatal(err)
	}
	if back.Event != "ready" {
		t.Fatalf("unexpected event %+v", back)
	}
}

func TestDecodeMalformed(t *testing.T) {
	_, err := DecodeRequest([]byte(`{"id": "secret-not-a-number"`))
	if !errors.Is(err, ErrMalformed) {
		t.Fatalf("got %v", err)
	}
	if strings.Contains(err.Error(), "secret") {
		t.Fatal("error leaks frame content")
	}
}

func TestDecodeRejectsNonObjects(t *testing.T) {
	for _, in := range []string{"null", "[]", `[{"id":1}]`, "7", `"x"`, "true", "", "  null"} {
		if _, err := DecodeRequest([]byte(in)); !errors.Is(err, ErrMalformed) {
			t.Fatalf("%q: got %v want ErrMalformed", in, err)
		}
	}
	if req, err := DecodeRequest([]byte(`  {"id":3,"op":"ping","params":null}`)); err != nil || req.ID != 3 {
		t.Fatalf("object with leading space: %+v, %v", req, err)
	}
}

func TestReaderSkipsBlankLinesAndEOF(t *testing.T) {
	r := NewReader(strings.NewReader("\n{\"a\":1}\n\n{\"b\":2}"))
	for _, want := range []string{`{"a":1}`, `{"b":2}`} {
		line, err := r.Next()
		if err != nil || string(line) != want {
			t.Fatalf("got %q, %v want %q", line, err, want)
		}
	}
	if _, err := r.Next(); err != io.EOF {
		t.Fatalf("got %v want EOF", err)
	}
}

func TestReaderLimit(t *testing.T) {
	ok := strings.Repeat("a", MaxLine-1) + "\n"
	line, err := NewReader(strings.NewReader(ok)).Next()
	if err != nil || len(line) != MaxLine-1 {
		t.Fatalf("line at limit: len %d, err %v", len(line), err)
	}
	atLimit := strings.Repeat("a", MaxLine) + "\n"
	if _, err := NewReader(strings.NewReader(atLimit)).Next(); !errors.Is(err, ErrTooLong) {
		t.Fatalf("MaxLine content plus newline: got %v want ErrTooLong", err)
	}
	tooLong := strings.Repeat("a", MaxLine+1) + "\n"
	if _, err := NewReader(strings.NewReader(tooLong)).Next(); !errors.Is(err, ErrTooLong) {
		t.Fatalf("got %v want ErrTooLong", err)
	}
}

func TestWriterRefusesOverlongLine(t *testing.T) {
	var buf bytes.Buffer
	w := NewWriter(&buf)
	if err := w.Write(strings.Repeat("a", MaxLine-3)); err != nil {
		t.Fatalf("line of exactly MaxLine bytes: %v", err)
	}
	if buf.Len() != MaxLine {
		t.Fatalf("wrote %d bytes, want %d", buf.Len(), MaxLine)
	}
	buf.Reset()
	if err := w.Write(strings.Repeat("a", MaxLine-2)); !errors.Is(err, ErrTooLong) {
		t.Fatalf("got %v want ErrTooLong", err)
	}
	if buf.Len() != 0 {
		t.Fatalf("a refused line must write nothing, wrote %d", buf.Len())
	}
	if err := w.Write("small"); err != nil || buf.String() != "\"small\"\n" {
		t.Fatalf("writer must stay usable: %v %q", err, buf.String())
	}
}
