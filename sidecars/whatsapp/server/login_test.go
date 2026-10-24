package server

import (
	"errors"
	"strings"
	"testing"
	"time"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
)

// loginSetup queues two unpaired clients: c0 for hello, c1 for the first login.
func loginSetup(t *testing.T, opts ...opt) (*harness, *fakeFactory, *fakeClient, *fakeClient) {
	t.Helper()
	c0, c1 := newFakeClient(), newFakeClient()
	f := &fakeFactory{queue: []*fakeClient{c0, c1}}
	h := startServer(t, f, opts...)
	h.mustHello()
	return h, f, c0, c1
}

func codeItem(code string, d time.Duration) whatsmeow.QRChannelItem {
	return whatsmeow.QRChannelItem{Event: whatsmeow.QRChannelEventCode, Code: code, Timeout: d}
}

func TestLoginQRCodesAndTimeout(t *testing.T) {
	h, f, c0, c1 := loginSetup(t)
	h.mustCall("login_start", map[string]any{"method": "qr"})
	if c0.disconns == 0 {
		t.Fatal("the hello client must be dropped")
	}
	c1.qr <- codeItem("2@first", 60*time.Second)
	c1.qr <- codeItem("2@second", 20*time.Second)
	if ev := h.event("qr"); ev["code"] != "2@first" || ev["expires_in_s"] != float64(60) {
		t.Fatalf("first qr %v", ev)
	}
	if ev := h.event("qr"); ev["code"] != "2@second" || ev["expires_in_s"] != float64(20) {
		t.Fatalf("second qr %v", ev)
	}
	c1.qr <- whatsmeow.QRChannelTimeout
	ev := h.event("connection")
	if ev["state"] != "disconnected" || ev["reason"] != "qr_timeout" || ev["action"] != "retry" {
		t.Fatalf("timeout event %v", ev)
	}
	eventually(t, "fresh client", func() bool { return f.clientCount() == 3 })
	f.mu.Lock()
	opts, refreshes := f.opts[1], f.refreshes
	f.mu.Unlock()
	if opts == nil || opts.DeviceName != "meta-mcp" || opts.FullHistory || refreshes != 1 {
		t.Fatalf("login options %+v refreshes %d", opts, refreshes)
	}
	if c1.connects != 1 {
		t.Fatalf("connects %d", c1.connects)
	}
	h.mustCall("login_start", map[string]any{"method": "qr", "device_name": "Laptop", "full_history": true})
	f.mu.Lock()
	opts = f.opts[len(f.opts)-1]
	f.mu.Unlock()
	if opts == nil || opts.DeviceName != "Laptop" || !opts.FullHistory {
		t.Fatalf("second login options %+v", opts)
	}
}

func TestLoginSuccessWaitsForConnectedAndPrekeys(t *testing.T) {
	h, _, _, c1 := loginSetup(t, withTiming(func(tm *timing) { tm.setup = 5 * time.Second }))
	c1.prekeys = make(chan struct{})
	h.mustCall("login_start", map[string]any{"method": "qr", "full_history": true})
	c1.pair()
	c1.deliver(&events.PairSuccess{ID: mustJID(selfPN)})
	c1.qr <- whatsmeow.QRChannelSuccess
	h.sync()
	if got := h.pending("state_updated"); len(got) != 0 {
		t.Fatalf("state_updated before Connected: %v", got)
	}
	if !c1.deliver(&events.Connected{}) {
		t.Fatal("Connected refused")
	}
	if p := <-c1.presences; p != types.PresenceUnavailable {
		t.Fatalf("presence %v", p)
	}
	h.sync()
	if got := h.pending("state_updated"); len(got) != 0 {
		t.Fatalf("state_updated before prekeys: %v", got)
	}
	close(c1.prekeys)
	conn, connSeq := h.eventSeq("connection")
	st, stSeq := h.eventSeq("state_updated")
	li, liSeq := h.eventSeq("logged_in")
	if conn["state"] != "connected" || !(connSeq < stSeq && stSeq < liSeq) {
		t.Fatalf("order connection=%d state=%d logged_in=%d", connSeq, stSeq, liSeq)
	}
	state, _ := st["state"].(map[string]any)
	if state["jid"] != selfPN || state["lid"] != selfLID || state["push_name"] != "Me" || state["full_history"] != true || state["paired_at"] == float64(0) {
		t.Fatalf("state %v", state)
	}
	if acc, _ := li["account"].(map[string]any); acc["id"] != selfPN {
		t.Fatalf("logged_in %v", li)
	}
	owner, found, _ := h.srv.store.GetMeta(h.srv.ctx, metaOwnerJID)
	if !found || owner != selfPN {
		t.Fatalf("owner meta %q %v", owner, found)
	}
}

func TestLoginConnectedBeforeSuccessIsLatched(t *testing.T) {
	h, _, _, c1 := loginSetup(t)
	h.mustCall("login_start", map[string]any{"method": "qr"})
	c1.pair()
	c1.deliver(&events.Connected{})
	c1.qr <- whatsmeow.QRChannelSuccess
	h.event("state_updated")
	h.event("logged_in")
}

func TestLoginSuccessTimeoutEmitsError(t *testing.T) {
	h, _, _, c1 := loginSetup(t, withTiming(func(*timing) {}))
	h.mustCall("login_start", map[string]any{"method": "qr"})
	c1.pair()
	c1.qr <- whatsmeow.QRChannelSuccess
	ev := h.event("error")
	if ev["code"] != "login_failed" || !strings.Contains(ev["message"].(string), "setup did not finish") {
		t.Fatalf("error event %v", ev)
	}
	if got := h.pending("logged_in"); len(got) != 0 {
		t.Fatalf("logged_in after a failed setup: %v", got)
	}
}

func TestLoginSuccessResetsStoreOfAnotherOwner(t *testing.T) {
	h, _, _, c1 := loginSetup(t)
	ctx := h.srv.ctx
	_ = h.srv.store.SetMeta(ctx, metaOwnerJID, "4155550000@s.whatsapp.net")
	if _, _, err := h.srv.store.EnsureChat(ctx, "4155550001@s.whatsapp.net", "", api.KindDM); err != nil {
		t.Fatal(err)
	}
	h.mustCall("login_start", map[string]any{"method": "qr"})
	c1.pair()
	c1.deliver(&events.PairSuccess{ID: mustJID("15551234567:7@s.whatsapp.net")})
	c1.deliver(&events.Connected{})
	c1.qr <- whatsmeow.QRChannelSuccess
	h.event("logged_in")
	res := h.mustCall("list_threads", map[string]any{"limit": 10})
	if th, _ := res["threads"].([]any); len(th) != 0 {
		t.Fatalf("threads of the previous owner survived: %v", th)
	}
}

func TestLoginPairErrorEvents(t *testing.T) {
	h, f, _, c1 := loginSetup(t)
	h.mustCall("login_start", map[string]any{"method": "qr"})
	c1.qr <- whatsmeow.QRChannelErrUnexpectedEvent
	ev := h.event("error")
	if ev["code"] != "login_failed" || !strings.Contains(ev["message"].(string), "err-unexpected-state") {
		t.Fatalf("error event %v", ev)
	}
	conn := h.event("connection")
	if conn["reason"] != "pair_error" || conn["action"] != "retry" {
		t.Fatalf("connection %v", conn)
	}
	eventually(t, "fresh client", func() bool { return f.clientCount() == 3 })
}

func TestLoginClientOutdatedIsFatal(t *testing.T) {
	h, _, _, c1 := loginSetup(t)
	h.mustCall("login_start", map[string]any{"method": "qr"})
	c1.qr <- whatsmeow.QRChannelClientOutdated
	ev := h.event("connection")
	if ev["state"] != "fatal" || ev["reason"] != "client_outdated" {
		t.Fatalf("connection %v", ev)
	}
}

func TestLoginPairCodeBadPhone(t *testing.T) {
	h, f, _, _ := loginSetup(t)
	for _, phone := range []string{"", "123", "0123456789", "12ab5678901", "1234567890123456", "+0155501234"} {
		h.wantErr(h.call("login_start", map[string]any{"method": "pair_code", "phone": phone}), api.CodeBadParams)
	}
	h.wantErr(h.call("login_start", map[string]any{"method": "other"}), api.CodeBadParams)
	if f.clientCount() != 1 {
		t.Fatalf("bad params must not create clients, got %d", f.clientCount())
	}
}

func TestLoginPairCodeFlowAndPrePairCallback(t *testing.T) {
	h, _, _, c1 := loginSetup(t)
	c1.pairCode = "ABCDEFGH"
	id := h.send("login_start", map[string]any{"method": "pair_code", "phone": "+1 555 000 1234"})
	c1.qr <- codeItem("2@ignored", 60*time.Second)
	c1.qr <- codeItem("2@ignored2", 20*time.Second)
	res := h.mustOK(h.reply(id))
	if res["code"] != "ABCD-EFGH" {
		t.Fatalf("pair code reply %v", res)
	}
	c1.mu.Lock()
	phone, cb := c1.pairPhoneIn, c1.prePair
	c1.mu.Unlock()
	if phone != "15550001234" {
		t.Fatalf("PairPhone got %q", phone)
	}
	if !cb(mustJID("15550001234@s.whatsapp.net"), "Chrome", "") || cb(mustJID("15559999999@s.whatsapp.net"), "Chrome", "") {
		t.Fatal("PrePairCallback must accept only the requested number")
	}
	h.sync()
	if got := h.pending("qr"); len(got) != 0 {
		t.Fatalf("pair_code login must not emit qr events: %v", got)
	}
}

func TestLoginPairCodeRateLimitedAndTimeout(t *testing.T) {
	h, f, _, c1 := loginSetup(t, withTiming(func(*timing) {}))
	c1.pairErr = whatsmeow.ErrIQRateOverLimit
	id := h.send("login_start", map[string]any{"method": "pair_code", "phone": "15550001234"})
	c1.qr <- codeItem("2@x", 60*time.Second)
	h.wantErr(h.reply(id), api.CodeRateLimited)
	eventually(t, "fresh client", func() bool { return f.clientCount() == 3 })

	rep := h.call("login_start", map[string]any{"method": "pair_code", "phone": "15550001234"})
	h.wantErr(rep, api.CodeLoginFailed)
}

func TestLoginPasskeyRoundTrip(t *testing.T) {
	h, _, _, c1 := loginSetup(t)
	h.mustCall("login_start", map[string]any{"method": "qr"})
	h.wantErr(h.call("login_finish", map[string]any{"passkey_response": map[string]any{"id": "x"}}), api.CodeBadParams)

	c1.qr <- whatsmeow.QRChannelItem{Event: whatsmeow.QRChannelEventPasskeyRequest, PasskeyRequest: &events.PairPasskeyRequest{
		PublicKey: &types.WebAuthnPublicKey{RelyingPartID: "web.whatsapp.com", UserVerification: "preferred"},
	}}
	ev := h.event("passkey_request")
	pk, _ := ev["public_key"].(map[string]any)
	if pk["rpId"] != "web.whatsapp.com" || !strings.Contains(ev["instructions"].(string), "web.whatsapp.com") {
		t.Fatalf("passkey_request %v", ev)
	}
	h.wantErr(h.call("login_finish", map[string]any{"confirm": true}), api.CodeBadParams)
	h.wantErr(h.call("login_finish", map[string]any{"passkey_response": "not an object"}), api.CodeBadParams)

	resp := map[string]any{"id": "cred", "rawId": "AQID", "type": "public-key", "response": map[string]any{
		"clientDataJSON": "AQID", "authenticatorData": "AQID", "signature": "AQID",
	}}
	h.mustCall("login_finish", map[string]any{"passkey_response": resp})
	c1.mu.Lock()
	got := append([]*types.WebAuthnResponse(nil), c1.passkeys...)
	c1.mu.Unlock()
	if len(got) != 1 || got[0].ID != "cred" || len(got[0].Response.Signature) != 3 {
		t.Fatalf("SendPasskeyResponse got %+v", got)
	}
	h.wantErr(h.call("login_finish", map[string]any{"passkey_response": resp}), api.CodeBadParams)

	c1.qr <- whatsmeow.QRChannelItem{Event: whatsmeow.QRChannelEventPasskeyResponse, PasskeyConfirmation: &events.PairPasskeyConfirmation{Code: "123-456"}}
	if ev := h.event("passkey_confirmation"); ev["code"] != "123-456" {
		t.Fatalf("passkey_confirmation %v", ev)
	}
	h.mustCall("login_finish", map[string]any{"confirm": true})
	c1.mu.Lock()
	confirms := c1.confirms
	c1.mu.Unlock()
	if confirms != 1 {
		t.Fatalf("confirmations %d", confirms)
	}
	h.wantErr(h.call("login_finish", map[string]any{"confirm": true}), api.CodeBadParams)
	h.wantErr(h.call("login_finish", map[string]any{}), api.CodeBadParams)
}

func TestLoginFinishWithoutLogin(t *testing.T) {
	h, _, _, _ := loginSetup(t)
	h.wantErr(h.call("login_finish", map[string]any{"confirm": true}), api.CodeBadParams)
}

func TestLoginAlreadyLoggedIn(t *testing.T) {
	h, _ := startPaired(t, true)
	h.wantErr(h.call("login_start", map[string]any{"method": "qr"}), api.CodeAlreadyLoggedIn)
}

func TestLoginStartReplacesPreviousLogin(t *testing.T) {
	h, f, _, c1 := loginSetup(t)
	h.mustCall("login_start", map[string]any{"method": "qr"})
	h.mustCall("login_start", map[string]any{"method": "qr"})
	c1.mu.Lock()
	d := c1.disconns
	c1.mu.Unlock()
	if d == 0 {
		t.Fatal("the first login's client must be disconnected")
	}
	if f.clientCount() < 3 {
		t.Fatalf("clients %d", f.clientCount())
	}
}

func TestLoginCancel(t *testing.T) {
	h, f, _, c1 := loginSetup(t)
	h.mustCall("login_start", map[string]any{"method": "qr"})
	h.mustCall("login_cancel", nil)
	c1.mu.Lock()
	d := c1.disconns
	c1.mu.Unlock()
	if d == 0 || f.clientCount() != 3 {
		t.Fatalf("disconnects %d clients %d", d, f.clientCount())
	}
	h.mustCall("login_cancel", nil)
	if f.clientCount() != 3 {
		t.Fatal("a second cancel must not create clients")
	}
}

func TestLoginCancelAbortsPendingPairCode(t *testing.T) {
	h, _, _, _ := loginSetup(t, withTiming(func(tm *timing) { tm.pairCode = 5 * time.Second }))
	id := h.send("login_start", map[string]any{"method": "pair_code", "phone": "15550001234"})
	eventually(t, "login registered", func() bool { return h.srv.loginActive() })
	h.mustCall("login_cancel", nil)
	h.wantErr(h.reply(id), api.CodeLoginFailed)
}

func TestLoginCancelLeavesLinkedClientAlone(t *testing.T) {
	h, c := startPaired(t, true)
	h.mustCall("login_cancel", nil)
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.disconns != 0 || !c.connected {
		t.Fatal("login_cancel without a login must not touch the linked client")
	}
}

func TestLogoutConnected(t *testing.T) {
	h, c := startPaired(t, true)
	h.incoming(c, "4155550001@s.whatsapp.net", "M1", 1700000000, "hi")
	h.mustCall("logout", nil)
	ev := h.event("state_updated")
	if v, present := ev["state"]; !present || v != nil {
		t.Fatalf("state_updated %v", ev)
	}
	c.mu.Lock()
	logouts, deletes := c.logouts, c.deletes
	c.mu.Unlock()
	if logouts != 1 || deletes != 0 {
		t.Fatalf("logouts %d deletes %d", logouts, deletes)
	}
	res := h.mustCall("list_threads", map[string]any{"limit": 10})
	if th, _ := res["threads"].([]any); len(th) != 0 {
		t.Fatalf("store not reset: %v", th)
	}
	if h.f.clientCount() != 2 {
		t.Fatalf("clients %d", h.f.clientCount())
	}
	h.wantErr(h.call("connect", nil), api.CodeNotLoggedIn)
	if h.f.scrubs != 1 {
		t.Fatalf("logout must wipe the session database leftovers, scrubs=%d", h.f.scrubs)
	}
}

func TestLogoutFallsBackToDeleteDevice(t *testing.T) {
	h, c := startPaired(t, false)
	h.mustCall("logout", nil)
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.logouts != 0 || c.deletes != 1 || c.disconns == 0 {
		t.Fatalf("logouts %d deletes %d disconnects %d", c.logouts, c.deletes, c.disconns)
	}
}

func TestLogoutServerRejectsThenDeletes(t *testing.T) {
	h, c := startPaired(t, true)
	c.logoutErr = errors.New("server said no")
	h.mustCall("logout", nil)
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.logouts != 1 || c.deletes != 1 {
		t.Fatalf("logouts %d deletes %d", c.logouts, c.deletes)
	}
}

func TestPairSuccessSwitchesOwnerBeforeHistory(t *testing.T) {
	h, _, _, c1 := loginSetup(t)
	ctx := h.srv.ctx
	_ = h.srv.store.SetMeta(ctx, metaOwnerJID, "4155550000@s.whatsapp.net")
	if _, _, err := h.srv.store.EnsureChat(ctx, "4155550001@s.whatsapp.net", "", api.KindDM); err != nil {
		t.Fatal(err)
	}
	h.mustCall("login_start", map[string]any{"method": "qr"})
	c1.pair()
	if !c1.deliver(&events.PairSuccess{ID: mustJID("15551234567:9@s.whatsapp.net")}) {
		t.Fatal("PairSuccess refused")
	}
	if _, found, _ := h.srv.store.GetChat(ctx, "4155550001@s.whatsapp.net"); found {
		t.Fatal("the previous owner's chat must be gone right after PairSuccess")
	}
	if owner, _, _ := h.srv.store.GetMeta(ctx, metaOwnerJID); owner != selfPN {
		t.Fatalf("owner %q", owner)
	}
	c1.deliver(&events.Connected{})
	h.incoming(c1, peerPN, "A1", 1700000100, "after pairing")
	c1.qr <- whatsmeow.QRChannelSuccess
	h.event("logged_in")
	if got := h.threads(); len(got) != 1 || got[0]["id"] != peerPN {
		t.Fatalf("a message ingested after Connected must survive setup: %v", got)
	}
}

func TestLoginFinishRearmsAfterFailure(t *testing.T) {
	h, _, _, c1 := loginSetup(t)
	h.mustCall("login_start", map[string]any{"method": "qr"})
	c1.qr <- whatsmeow.QRChannelItem{Event: whatsmeow.QRChannelEventPasskeyRequest, PasskeyRequest: &events.PairPasskeyRequest{PublicKey: &types.WebAuthnPublicKey{}}}
	h.event("passkey_request")
	resp := map[string]any{"id": "cred", "rawId": "AQID", "type": "public-key", "response": map[string]any{"clientDataJSON": "AQID", "authenticatorData": "AQID", "signature": "AQID"}}
	c1.mu.Lock()
	c1.passkeyErr = errors.New("rejected")
	c1.mu.Unlock()
	h.wantErr(h.call("login_finish", map[string]any{"passkey_response": resp}), api.CodeLoginFailed)
	c1.mu.Lock()
	c1.passkeyErr = nil
	c1.mu.Unlock()
	h.mustCall("login_finish", map[string]any{"passkey_response": resp})

	c1.qr <- whatsmeow.QRChannelItem{Event: whatsmeow.QRChannelEventPasskeyResponse, PasskeyConfirmation: &events.PairPasskeyConfirmation{Code: "1"}}
	h.event("passkey_confirmation")
	c1.mu.Lock()
	c1.confirmErr = errors.New("rejected")
	c1.mu.Unlock()
	h.wantErr(h.call("login_finish", map[string]any{"confirm": true}), api.CodeLoginFailed)
	c1.mu.Lock()
	c1.confirmErr = nil
	c1.mu.Unlock()
	h.mustCall("login_finish", map[string]any{"confirm": true})
}

func TestLoginCancelAfterPairingLetsSetupFinish(t *testing.T) {
	h, f, _, c1 := loginSetup(t)
	c1.prekeys = make(chan struct{})
	h.mustCall("login_start", map[string]any{"method": "qr"})
	c1.pair()
	c1.deliver(&events.PairSuccess{ID: mustJID(selfPN)})
	c1.deliver(&events.Connected{})
	c1.qr <- whatsmeow.QRChannelSuccess
	h.sync()
	h.mustCall("login_cancel", nil)
	close(c1.prekeys)
	h.event("state_updated")
	h.event("logged_in")
	c1.mu.Lock()
	d := c1.disconns
	c1.mu.Unlock()
	if d != 0 || f.clientCount() != 2 {
		t.Fatalf("cancel must not drop the paired client: disconnects %d clients %d", d, f.clientCount())
	}
}

func TestLogoutReportsIOWhenTheSessionWipeFails(t *testing.T) {
	h, c := startPaired(t, true)
	h.incoming(c, "4155550001@s.whatsapp.net", "M1", 1700000000, "hi")
	h.f.mu.Lock()
	h.f.scrubErr = errors.New("disk full at /home/someone/data")
	h.f.mu.Unlock()
	rep := h.call("logout", nil)
	h.wantErr(rep, api.CodeIO)
	if rep.Error.Message != "could not wipe the session database" {
		t.Fatalf("the error text is fixed: %q", rep.Error.Message)
	}
	if ev := h.event("state_updated"); ev["state"] != nil {
		t.Fatalf("the unlink itself succeeded, the daemon must forget the session: %v", ev)
	}
	if res := h.mustCall("list_threads", map[string]any{"limit": 10}); len(res["threads"].([]any)) != 0 {
		t.Fatalf("the message store is reset: %v", res)
	}
	h.f.mu.Lock()
	h.f.scrubErr = nil
	h.f.mu.Unlock()
	h.mustCall("logout", nil)
	if h.f.scrubs != 2 {
		t.Fatalf("a retried logout wipes again, scrubs=%d", h.f.scrubs)
	}
}

func TestLogoutDeleteFailureStopsBeforeTheWipe(t *testing.T) {
	h, c := startPaired(t, false)
	c.mu.Lock()
	c.deleteErr = errors.New("db locked")
	c.mu.Unlock()
	h.wantErr(h.call("logout", nil), api.CodeIO)
	if h.f.scrubs != 0 {
		t.Fatalf("scrubs=%d", h.f.scrubs)
	}
}
