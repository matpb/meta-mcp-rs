// Package engine implements api.Factory and api.Session on top of mautrix-meta messagix and whatsmeow.
package engine

import (
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"strconv"

	"github.com/rs/zerolog"
	"go.mau.fi/mautrix-meta/pkg/messagix/cookies"
	"go.mau.fi/mautrix-meta/pkg/messagix/types"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
	"github.com/matpb/meta-mcp-rs/sidecars/messenger/e2ee"
)

// NewFactory returns the production factory.
func NewFactory(log zerolog.Logger) api.Factory {
	return &factory{log: log, newClient: newMessagixClient}
}

type factory struct {
	log       zerolog.Logger
	newClient func(c *cookies.Cookies, log zerolog.Logger) metaClient
}

// Open builds a session from stored state without any network or file access.
func (f *factory) Open(state api.State, dataDir string, sink api.Sink) (api.Session, error) {
	jar := &cookies.Cookies{Platform: types.Facebook}
	values := make(map[cookies.MetaCookieName]string, len(state.Cookies))
	for k, v := range state.Cookies {
		values[cookies.MetaCookieName(k)] = v
	}
	jar.UpdateValues(values)
	me, _ := strconv.ParseInt(state.FBID, 10, 64)
	return newSession(f, state, dataDir, sink, jar, me), nil
}

// Wipe deletes e2ee.db with its journal files and threads.json under dataDir; absent files are fine.
// It needs no session, and it leaves sidecar.lock alone.
func (f *factory) Wipe(dataDir string) error {
	failed := e2ee.Wipe(dataDir) != nil
	for _, name := range []string{threadsFileName, threadsFileName + ".tmp"} {
		if err := os.Remove(filepath.Join(dataDir, name)); err != nil && !errors.Is(err, fs.ErrNotExist) {
			failed = true
		}
	}
	if failed {
		return api.Errorf(api.CodeIO, "failed to delete local session data")
	}
	return nil
}
