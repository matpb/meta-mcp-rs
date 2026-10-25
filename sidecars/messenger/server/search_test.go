package server

import (
	"context"
	"strings"
	"testing"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

func TestSearchUsersValidation(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK(goodState)
	cases := []struct{ name, params string }{
		{"missing query", `{}`},
		{"empty query", `{"query":""}`},
		{"query over 200 runes", `{"query":"` + strings.Repeat("é", 201) + `"}`},
		{"limit 0", `{"query":"pat","limit":0}`},
		{"limit 51", `{"query":"pat","limit":51}`},
		{"null params", `null`},
	}
	for i, c := range cases {
		id := 10 + i
		rep := h.call(h.req(id, "search_users", c.params))
		wantErr(t, rep, uint64(id), "bad_params")
		if strings.Contains(rep.Error.Message, "pat") {
			t.Fatalf("%s: message echoes a value: %q", c.name, rep.Error.Message)
		}
	}
	h.f.searchUsers = func(context.Context, api.SearchUsersParams) (api.SearchUsersResult, error) {
		t.Error("the session must not see invalid params")
		return api.SearchUsersResult{}, nil
	}
	wantErr(t, h.call(h.req(30, "search_users", `{"query":"pat","limit":51}`)), 30, "bad_params")
}

func TestSearchUsersLimitBoundsAndDefault(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK(goodState)
	var got []api.SearchUsersParams
	h.f.searchUsers = func(_ context.Context, p api.SearchUsersParams) (api.SearchUsersResult, error) {
		got = append(got, p)
		return api.SearchUsersResult{}, nil
	}
	wantOK(t, h.call(h.req(2, "search_users", `{"query":"pat"}`)), 2)
	wantOK(t, h.call(h.req(3, "search_users", `{"query":"pat","limit":1}`)), 3)
	wantOK(t, h.call(h.req(4, "search_users", `{"query":"`+strings.Repeat("é", 200)+`","limit":50}`)), 4)
	if len(got) != 3 || got[0] != (api.SearchUsersParams{Query: "pat", Limit: 10}) || got[1].Limit != 1 ||
		got[2].Limit != 50 || len([]rune(got[2].Query)) != 200 {
		t.Fatalf("params reaching the session: %+v", got)
	}
}

func TestSearchUsersSuccessShape(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK(goodState)
	h.f.searchUsers = func(context.Context, api.SearchUsersParams) (api.SearchUsersResult, error) {
		return api.SearchUsersResult{Users: []api.Person{{ID: "5555", Name: "Pat", HasThread: true}, {ID: "6666", Name: "Sam"}}}, nil
	}
	rep := h.call(h.req(2, "search_users", `{"query":"pat"}`))
	wantOK(t, rep, 2)
	want := `{"users":[{"id":"5555","name":"Pat","has_thread":true},{"id":"6666","name":"Sam","has_thread":false}]}`
	if string(rep.Result) != want {
		t.Fatalf("result %s, want %s", rep.Result, want)
	}
}

func TestSearchUsersNilUsersBecomeArray(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK(goodState)
	h.f.searchUsers = func(context.Context, api.SearchUsersParams) (api.SearchUsersResult, error) {
		return api.SearchUsersResult{Users: nil}, nil
	}
	rep := h.call(h.req(2, "search_users", `{"query":"nobody"}`))
	wantOK(t, rep, 2)
	if string(rep.Result) != `{"users":[]}` {
		t.Fatalf("result %s", rep.Result)
	}
}

func TestSearchUsersEngineErrorPassesThrough(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK(goodState)
	h.f.searchUsers = func(context.Context, api.SearchUsersParams) (api.SearchUsersResult, error) {
		return api.SearchUsersResult{}, api.Errorf(api.CodeIO, "user search failed")
	}
	rep := h.call(h.req(2, "search_users", `{"query":"pat"}`))
	wantErr(t, rep, 2, "io")
	if rep.Error.Message != "user search failed" {
		t.Fatalf("message %q", rep.Error.Message)
	}
}

func TestSearchUsersNoSession(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK("null")
	wantErr(t, h.call(h.req(2, "search_users", `{"query":"pat"}`)), 2, "not_logged_in")
	wantErr(t, h.call(h.req(3, "list_threads", "null")), 3, "not_logged_in")
}
