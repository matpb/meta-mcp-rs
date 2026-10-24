package server

import (
	"context"
	"errors"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/ipc"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/store"
)

type listThreadsParams struct {
	Limit  int     `json:"limit"`
	Cursor *string `json:"cursor"`
}

// listThreads pages through the stored chats, newest activity first.
func (s *server) listThreads(ctx context.Context, req ipc.Request) ipc.Reply {
	var p listThreadsParams
	if e := decode(req, &p); e != nil {
		return failErr(req.ID, e)
	}
	if p.Limit < 1 || p.Limit > 200 {
		return fail(req.ID, api.CodeBadParams, "limit must be 1 to 200")
	}
	cursor := ""
	if p.Cursor != nil {
		cursor = *p.Cursor
	}
	chats, next, err := s.store.ListChats(ctx, p.Limit, cursor)
	if errors.Is(err, store.ErrBadCursor) {
		return fail(req.ID, api.CodeBadParams, "cursor is not valid")
	}
	if err != nil {
		return fail(req.ID, api.CodeIO, "message database error")
	}
	threads := make([]api.Thread, len(chats))
	for i, c := range chats {
		threads[i] = c.API()
	}
	res := map[string]any{"threads": threads}
	if next != "" {
		res["cursor"] = next
	}
	return ok(req.ID, res)
}
