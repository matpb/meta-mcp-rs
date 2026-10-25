package engine

import (
	"context"
	"errors"
	"reflect"
	"strconv"
	"sync"
	"time"

	"go.mau.fi/mautrix-meta/pkg/messagix/methods"
	"go.mau.fi/mautrix-meta/pkg/messagix/socket"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
	mtypes "go.mau.fi/mautrix-meta/pkg/messagix/types"
	"go.mau.fi/whatsmeow/types"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

var (
	searchTimeout = 20 * time.Second
	vettedTTL     = time.Hour
)

const maxVetted = 1000

// searchTypes is the SupportedTypes list the connector's SearchUsers sends.
var searchTypes = []table.SearchType{
	table.SearchTypeContact, table.SearchTypeGroup, table.SearchTypePage, table.SearchTypeNonContact,
	table.SearchTypeIGContactFollowing, table.SearchTypeIGContactNonFollowing,
	table.SearchTypeIGNonContactFollowing, table.SearchTypeIGNonContactNonFollowing,
	table.SearchTypeCommunityMessagingThread,
}

var errNotMessageable = api.Errorf(api.CodeThreadNotFound, "thread not found; this id is not a person you can message")

// e2eeUp reports whether the encrypted client is connected; tests replace it.
var e2eeUp = func(s *session) bool {
	_, err := s.e2eeReady()
	return err == nil
}

type vetted struct {
	name string
	at   time.Time
}

// vettedPeople remembers the ids a user search returned as messageable, for vettedTTL.
type vettedPeople struct {
	mu sync.Mutex
	m  map[int64]vetted
}

func (v *vettedPeople) put(id int64, name string) {
	v.mu.Lock()
	defer v.mu.Unlock()
	if v.m == nil {
		v.m = map[int64]vetted{}
	}
	if len(v.m) >= maxVetted {
		for k, e := range v.m {
			if time.Since(e.at) > vettedTTL {
				delete(v.m, k)
			}
		}
		if len(v.m) >= maxVetted {
			clear(v.m)
		}
	}
	v.m[id] = vetted{name: name, at: time.Now()}
}

func (v *vettedPeople) get(id int64) (string, bool) {
	v.mu.Lock()
	defer v.mu.Unlock()
	e, ok := v.m[id]
	if !ok || time.Since(e.at) > vettedTTL {
		return "", false
	}
	return e.name, true
}

// messageable is the connector's SearchUsers filter, narrowed to Facebook people other than the account.
func messageable(r *table.LSInsertSearchResult, me int64) bool {
	if r == nil || (r.Type_ != table.SearchTypeContact && r.Type_ != table.SearchTypeNonContact) {
		return false
	}
	canMessage := r.CanViewerMessage || (r.OtherUserId != 0 && r.BlockedByViewerStatus == 0)
	id := r.GetFBID()
	return r.ThreadType.IsOneToOne() && canMessage && id > 0 && id != me
}

type person struct {
	id   int64
	name string
}

// searchPeople runs one SearchUserTask and returns the messageable people, remembering them as vetted.
func (s *session) searchPeople(ctx context.Context, cl metaClient, query string) ([]person, error) {
	sctx, cancel := context.WithTimeout(ctx, searchTimeout)
	defer cancel()
	tbl, err := cl.ExecuteTasks(sctx, &socket.SearchUserTask{Query: query, SupportedTypes: searchTypes, SurfaceType: 5})
	if err != nil {
		if e := s.accountFailure(err); e != nil {
			return nil, e
		}
		if errors.Is(err, mtypes.ErrPleaseReloadPage) {
			s.requestRecycle(false)
		}
		return nil, api.Errorf(api.CodeIO, "user search failed")
	}
	if tbl == nil {
		return nil, nil
	}
	me := s.me.Load()
	seen := map[int64]bool{}
	var out []person
	for _, r := range tbl.LSInsertSearchResult {
		if !messageable(r, me) || seen[r.GetFBID()] {
			continue
		}
		id := r.GetFBID()
		seen[id] = true
		out = append(out, person{id: id, name: r.DisplayName})
		s.people.put(id, r.DisplayName)
		s.names.set(id, r.DisplayName)
	}
	return out, nil
}

// SearchUsers finds messageable people by name or id.
func (s *session) SearchUsers(ctx context.Context, p api.SearchUsersParams) (api.SearchUsersResult, error) {
	res := api.SearchUsersResult{Users: []api.Person{}}
	if err := s.writable(); err != nil {
		return res, err
	}
	cl, err := s.sendClient(ctx)
	if err != nil {
		return res, err
	}
	found, err := s.searchPeople(ctx, cl, p.Query)
	if err != nil {
		return res, err
	}
	for _, f := range found {
		if len(res.Users) >= p.Limit {
			break
		}
		id := strconv.FormatInt(f.id, 10)
		_, has := s.threadInfo(id)
		res.Users = append(res.Users, api.Person{ID: id, Name: f.name, HasThread: has})
	}
	return res, nil
}

// resolveSendTarget is resolveThread, except that an unknown numeric id vetted as a messageable person becomes a new encrypted DM.
func (s *session) resolveSendTarget(ctx context.Context, id string) (threadTarget, error) {
	t, err := s.resolveThread(id)
	if err == nil || api.AsError(err).Code != api.CodeThreadNotFound {
		return t, err
	}
	key, canon, ok := canonicalThreadID(id)
	if !ok {
		return threadTarget{}, err
	}
	return s.openDM(ctx, key, canon)
}

// openDM vets the id, registers the encrypted DM, then creates it on Facebook the way the connector does.
func (s *session) openDM(ctx context.Context, key int64, canon string) (threadTarget, error) {
	s.openMu.Lock()
	defer s.openMu.Unlock()
	if t, err := s.resolveThread(canon); err == nil {
		return t, nil
	}
	cl, err := s.sendClient(ctx)
	if err != nil {
		return threadTarget{}, err
	}
	_, ok := s.people.get(key)
	if !ok {
		found, err := s.searchPeople(ctx, cl, canon)
		if err != nil {
			return threadTarget{}, err
		}
		for _, f := range found {
			ok = ok || f.id == key
		}
	}
	if !ok {
		s.log.Info().Msg("new chat refused: the id is not a person the account can message")
		return threadTarget{}, errNotMessageable
	}
	if !e2eeUp(s) {
		return threadTarget{}, api.Errorf(api.CodeNotConnected, "not connected")
	}
	if s.reg.openDM(canon, key) {
		s.emitThreads([]string{canon})
	}
	if err := s.createDM(ctx, cl, key); err != nil {
		return threadTarget{}, err
	}
	s.log.Info().Msg("opened a new encrypted chat for a send")
	return s.resolveThread(canon)
}

// createDM runs CreateThreadTask and the CreateWhatsAppThreadTask with its subtasks, as ResolveIdentifier does.
// Like the connector, only an account failure stops the send; other errors are logged.
func (s *session) createDM(ctx context.Context, cl metaClient, key int64) error {
	cctx, cancel := context.WithTimeout(ctx, sendExecTimeout)
	defer cancel()
	run := func(tasks ...socket.Task) (*table.LSTable, error) {
		tbl, err := cl.ExecuteTasks(cctx, tasks...)
		if err != nil {
			if e := s.accountFailure(err); e != nil {
				return nil, e
			}
			s.log.Warn().Msg("a new chat creation step failed")
			return nil, nil
		}
		s.processTable(tbl, modeBackfill)
		return tbl, nil
	}
	if _, err := run(&socket.CreateThreadTask{ThreadFBID: key, SyncGroup: 1}); err != nil {
		return err
	}
	tbl, err := run(&socket.CreateWhatsAppThreadTask{
		WAJID:            key,
		OfflineThreadKey: methods.GenerateEpochID(),
		ThreadType:       table.ENCRYPTED_OVER_WA_ONE_TO_ONE,
		FolderType:       table.INBOX,
		BumpTimestampMS:  time.Now().UnixMilli(),
	})
	if err != nil || tbl == nil || len(tbl.LSIssueNewTask) == 0 {
		return err
	}
	sub := make([]socket.Task, len(tbl.LSIssueNewTask))
	for i, t := range tbl.LSIssueNewTask {
		sub[i] = t
	}
	_, err = run(sub...)
	return err
}

// openDM registers an encrypted DM with a vetted person; an existing entry is only upgraded to encrypted.
func (r *registry) openDM(id string, key int64) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.loadLocked()
	e, ok := r.threads[id]
	var before api.Thread
	if ok {
		before = r.snapLocked(e)
	} else {
		e = &threadEntry{ID: id}
		r.threads[id] = e
	}
	e.Kind = api.KindDM
	e.ThreadType = table.ENCRYPTED_OVER_WA_ONE_TO_ONE
	e.E2EE = true
	e.WAServer = types.MessengerServer
	if e.FBThreadKey == 0 {
		e.FBThreadKey = key
	}
	return !ok || !reflect.DeepEqual(before, r.snapLocked(e))
}
