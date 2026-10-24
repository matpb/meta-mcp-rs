package engine

import (
	"strconv"
	"sync"
	"testing"
	"time"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

func recv(t *testing.T, ch <-chan api.SendOutcome) api.SendOutcome {
	t.Helper()
	select {
	case o := <-ch:
		return o
	case <-time.After(time.Second):
		t.Fatal("no outcome delivered")
	}
	return api.SendOutcome{}
}

func none(t *testing.T, ch <-chan api.SendOutcome) {
	t.Helper()
	select {
	case o := <-ch:
		t.Fatalf("unexpected outcome %+v", o)
	case <-time.After(20 * time.Millisecond):
	}
}

func TestSendWaiterResolvesMatchingOTID(t *testing.T) {
	var w sendWaiter
	a, b, c := w.register("1"), w.register("2"), w.register("3")
	w.resolve([]api.SendOutcome{{OTID: "2", MessageID: "fb:mid2"}, {OTID: "3", MessageID: "fb:mid3"}})
	if got := recv(t, b); got.MessageID != "fb:mid2" {
		t.Fatalf("b got %+v", got)
	}
	if got := recv(t, c); got.MessageID != "fb:mid3" {
		t.Fatalf("c got %+v", got)
	}
	none(t, a)
	if w.size() != 1 {
		t.Fatalf("pending %d, want 1", w.size())
	}
}

func TestSendWaiterFailedOutcome(t *testing.T) {
	var w sendWaiter
	ch := w.register("9")
	w.resolve([]api.SendOutcome{{OTID: "9", Failed: true, Reason: "server text"}})
	got := recv(t, ch)
	if !got.Failed || got.Reason != "server text" {
		t.Fatalf("got %+v", got)
	}
}

func TestSendWaiterFirstOutcomeWins(t *testing.T) {
	var w sendWaiter
	ch := w.register("5")
	w.resolve([]api.SendOutcome{{OTID: "5", MessageID: "fb:first"}, {OTID: "5", Failed: true}})
	w.resolve([]api.SendOutcome{{OTID: "5", MessageID: "fb:late"}})
	if got := recv(t, ch); got.MessageID != "fb:first" || got.Failed {
		t.Fatalf("got %+v", got)
	}
	none(t, ch)
	if w.size() != 0 {
		t.Fatalf("pending %d, want 0", w.size())
	}
}

func TestSendWaiterIgnoresUnknownOTID(t *testing.T) {
	var w sendWaiter
	w.resolve([]api.SendOutcome{{OTID: "404", MessageID: "fb:x"}})
	ch := w.register("1")
	w.resolve([]api.SendOutcome{{OTID: "404", MessageID: "fb:x"}})
	none(t, ch)
	if w.size() != 1 {
		t.Fatalf("pending %d, want 1", w.size())
	}
}

func TestSendWaiterCancel(t *testing.T) {
	var w sendWaiter
	ch := w.register("7")
	w.cancel("7")
	w.cancel("never-registered")
	w.resolve([]api.SendOutcome{{OTID: "7", MessageID: "fb:x"}})
	none(t, ch)
	if w.size() != 0 {
		t.Fatalf("pending %d, want 0", w.size())
	}
}

func TestSendWaiterConcurrent(t *testing.T) {
	var w sendWaiter
	const n = 200
	chans := make([]<-chan api.SendOutcome, n)
	var wg sync.WaitGroup
	for i := range chans {
		wg.Add(1)
		go func() {
			defer wg.Done()
			chans[i] = w.register(strconv.Itoa(i))
		}()
	}
	wg.Wait()
	for i := range chans {
		wg.Add(1)
		go func() {
			defer wg.Done()
			w.resolve([]api.SendOutcome{{OTID: strconv.Itoa(i), MessageID: "fb:" + strconv.Itoa(i)}})
		}()
	}
	wg.Wait()
	for i, ch := range chans {
		if got := recv(t, ch); got.MessageID != "fb:"+strconv.Itoa(i) {
			t.Fatalf("outcome %d got %+v", i, got)
		}
	}
	if w.size() != 0 {
		t.Fatalf("pending %d, want 0", w.size())
	}
}
