package engine

import (
	"cmp"
	"context"
	"encoding/base64"
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"go.mau.fi/mautrix-meta/pkg/messagix/table"
	"go.mau.fi/whatsmeow/types"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
	"github.com/matpb/meta-mcp-rs/sidecars/messenger/normalize"
)

const threadsFileName = "threads.json"

// contacts is the fbid to display name cache.
type contacts struct {
	mu sync.RWMutex
	m  map[int64]string
}

func newContacts() *contacts { return &contacts{m: map[int64]string{}} }

func (c *contacts) set(id int64, name string) {
	if id == 0 || name == "" {
		return
	}
	c.mu.Lock()
	c.m[id] = name
	c.mu.Unlock()
}

func (c *contacts) get(id int64) string {
	c.mu.RLock()
	defer c.mu.RUnlock()
	return c.m[id]
}

type hybridEntry struct {
	Key  int64 `json:"key"`
	Type int64 `json:"type"`
}

type threadsFile struct {
	Version int                    `json:"version"`
	FBID    string                 `json:"fbid,omitempty"`
	Hybrid  map[string]hybridEntry `json:"hybrid"`
}

// threadEntry is the registry record behind one api.Thread.
type threadEntry struct {
	ID             string
	Kind           string
	Name           string
	Members        []int64
	E2EE           bool
	Preview        string
	LastActivityMS int64
	LastReadMS     int64
	ReadKnown      bool
	ThreadType     table.ThreadType
	FBThreadKey    int64
	WAServer       string
	Admins         []int64
}

func (e *threadEntry) encrypted() bool { return e.E2EE || e.ThreadType.IsWhatsApp() }

// registry holds the known threads and the persisted hybrid thread map.
type registry struct {
	mu      sync.Mutex
	me      int64
	fbid    string
	path    string
	loaded  bool
	threads map[string]*threadEntry
	hybrid  map[int64]hybridEntry
	names   func(int64) string
}

func newRegistry(dataDir, fbid string, me int64, names func(int64) string) *registry {
	return &registry{
		me:      me,
		fbid:    fbid,
		path:    filepath.Join(dataDir, threadsFileName),
		threads: map[string]*threadEntry{},
		hybrid:  map[int64]hybridEntry{},
		names:   names,
	}
}

func (r *registry) setMe(me int64) {
	r.mu.Lock()
	r.me = me
	r.mu.Unlock()
}

func (r *registry) loadLocked() {
	if r.loaded {
		return
	}
	r.loaded = true
	b, err := os.ReadFile(r.path)
	if err != nil {
		return
	}
	var f threadsFile
	if json.Unmarshal(b, &f) != nil || (f.FBID != "" && f.FBID != r.fbid) {
		return
	}
	for k, v := range f.Hybrid {
		if key, err := strconv.ParseInt(k, 10, 64); err == nil && v.Key != 0 {
			r.hybrid[key] = v
		}
	}
}

func (r *registry) saveLocked() error {
	f := threadsFile{Version: 1, FBID: r.fbid, Hybrid: map[string]hybridEntry{}}
	for k, v := range r.hybrid {
		f.Hybrid[strconv.FormatInt(k, 10)] = v
	}
	b, err := json.Marshal(f)
	if err != nil {
		return err
	}
	tmp := r.path + ".tmp"
	fh, err := os.OpenFile(tmp, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o600)
	if err != nil {
		return err
	}
	if _, err := fh.Write(b); err != nil {
		_ = fh.Close()
		_ = os.Remove(tmp)
		return err
	}
	if err := fh.Close(); err != nil {
		_ = os.Remove(tmp)
		return err
	}
	if err := os.Rename(tmp, r.path); err != nil {
		_ = os.Remove(tmp)
		return err
	}
	return nil
}

// forget drops every thread and the hybrid map from memory; the file is removed by factory.Wipe.
func (r *registry) forget() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.threads = map[string]*threadEntry{}
	r.hybrid = map[int64]hybridEntry{}
	r.loaded = true
}

func (r *registry) canonLocked(fbKey int64) int64 {
	r.loadLocked()
	if h, ok := r.hybrid[fbKey]; ok && h.Key != 0 {
		return h.Key
	}
	return fbKey
}

// threadID is the canonical thread id of a Messenger thread key.
func (r *registry) threadID(fbKey int64) string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return strconv.FormatInt(r.canonLocked(fbKey), 10)
}

func (r *registry) empty() bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	return len(r.threads) == 0
}

func skipThreadType(tt table.ThreadType) bool {
	switch tt {
	case table.FOLDER, table.MONTAGE, table.COMMUNITY_FOLDER, table.COMMUNITY_CHANNEL_CATEGORY:
		return true
	}
	return false
}

func (r *registry) snapLocked(e *threadEntry) api.Thread {
	t := api.Thread{ID: e.ID, Kind: e.Kind, Encrypted: e.encrypted(), Participants: []api.Participant{}}
	if e.Name != "" {
		n := e.Name
		t.Name = &n
	}
	if e.LastActivityMS > 0 {
		ts := e.LastActivityMS / 1000
		t.LastMessageTS = &ts
	}
	if e.Preview != "" {
		p := e.Preview
		t.LastMessagePreview = &p
	}
	if e.ReadKnown && e.LastActivityMS > 0 {
		u := e.LastReadMS < e.LastActivityMS
		t.Unread = &u
	}
	ids := e.Members
	if e.Kind == api.KindDM && len(ids) == 0 {
		ids = []int64{r.me}
		if other, err := strconv.ParseInt(e.ID, 10, 64); err == nil && other != r.me {
			ids = append(ids, other)
		}
	}
	for _, id := range ids {
		name := ""
		if r.names != nil {
			name = r.names(id)
		}
		t.Participants = append(t.Participants, api.Participant{ID: strconv.FormatInt(id, 10), Name: name, IsMe: id == r.me})
	}
	return t
}

// snapshot returns the api view of one thread.
func (r *registry) snapshot(id string) (api.Thread, bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	e, ok := r.threads[id]
	if !ok {
		return api.Thread{}, false
	}
	return r.snapLocked(e), true
}

// entry returns a copy of the registry record.
func (r *registry) entry(id string) (threadEntry, bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	e, ok := r.threads[id]
	if !ok {
		return threadEntry{}, false
	}
	c := *e
	c.Members = slices.Clone(e.Members)
	c.Admins = slices.Clone(e.Admins)
	return c, true
}

type threadRow struct {
	key      int64
	tt       table.ThreadType
	name     string
	snippet  string
	activity int64
	read     int64
	folder   string
	replace  bool
}

func (r *registry) upsertLocked(row threadRow, changed map[string]bool) {
	if skipThreadType(row.tt) {
		return
	}
	canon := r.canonLocked(row.key)
	id := strconv.FormatInt(canon, 10)
	e := r.threads[id]
	var before api.Thread
	created := e == nil
	if created {
		e = &threadEntry{ID: id, Kind: api.KindGroup}
		r.threads[id] = e
	} else {
		before = r.snapLocked(e)
	}
	tt := row.tt
	if h, ok := r.hybrid[row.key]; ok && h.Type != 0 && table.ThreadType(h.Type).IsWhatsApp() {
		tt = table.ThreadType(h.Type)
	}
	if tt != table.UNKNOWN_THREAD_TYPE && !(e.ThreadType.IsWhatsApp() && !tt.IsWhatsApp()) {
		e.ThreadType = tt
		if tt.IsOneToOne() {
			e.Kind = api.KindDM
		} else {
			e.Kind = api.KindGroup
		}
	}
	if e.ThreadType.IsWhatsApp() && e.WAServer == "" {
		if e.Kind == api.KindDM {
			e.WAServer = types.MessengerServer
		} else {
			e.WAServer = types.GroupServer
		}
	}
	if row.replace || row.name != "" {
		e.Name = row.name
	}
	if row.replace || row.snippet != "" {
		e.Preview = row.snippet
	}
	if row.activity > 0 {
		e.LastActivityMS = row.activity
	}
	if row.read > 0 {
		e.LastReadMS = row.read
	}
	e.ReadKnown = true
	e.FBThreadKey = row.key
	if canon != row.key || strings.HasPrefix(row.folder, "e2ee_cutover") {
		e.E2EE = true
	}
	if created || !reflect.DeepEqual(before, r.snapLocked(e)) {
		changed[id] = true
	}
}

func (r *registry) setHybridLocked(fbKey, canon int64, tt table.ThreadType, changed map[string]bool) bool {
	cur, had := r.hybrid[fbKey]
	next := hybridEntry{Key: canon, Type: int64(tt)}
	if had && tt == 0 {
		next.Type = cur.Type
	}
	if had && cur == next {
		return false
	}
	r.hybrid[fbKey] = next
	if fbKey != canon {
		oldID := strconv.FormatInt(fbKey, 10)
		newID := strconv.FormatInt(canon, 10)
		if old, ok := r.threads[oldID]; ok {
			delete(r.threads, oldID)
			if _, exists := r.threads[newID]; !exists {
				old.ID = newID
				old.E2EE = true
				r.threads[newID] = old
				changed[newID] = true
			}
			delete(changed, oldID)
		}
	}
	return true
}

// ingest applies the thread, participant and contact-independent rows of one table and reports changed thread ids.
func (r *registry) ingest(tbl *table.LSTable) []string {
	if tbl == nil {
		return nil
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	r.loadLocked()
	changed := map[string]bool{}
	mapChanged := false
	for _, h := range tbl.LSUpdateThreadAuthorityAndMappingWithOTIDFromJID {
		mapChanged = r.setHybridLocked(h.ThreadKey, h.ThreadJID, 0, changed) || mapChanged
	}
	for _, h := range tbl.LSVerifyHybridThreadExists {
		mapChanged = r.setHybridLocked(h.ThreadKey, h.ThreadJID, h.ThreadType, changed) || mapChanged
	}
	if mapChanged {
		_ = r.saveLocked()
	}

	active := map[int64]bool{}
	for _, v := range tbl.LSVerifyThreadExists {
		active[v.ThreadKey] = true
	}
	for _, v := range tbl.LSUpdateOrInsertThread {
		active[v.ThreadKey] = true
	}
	for _, v := range tbl.LSInsertNewMessageRange {
		active[v.ThreadKey] = true
	}
	for _, v := range tbl.LSDeleteThenInsertThread {
		active[v.ThreadKey] = true
	}
	for _, v := range tbl.LSUpdateThreadAuthorityAndMappingWithOTIDFromJID {
		active[v.ThreadJID] = true
	}
	for _, v := range tbl.LSVerifyHybridThreadExists {
		active[v.ThreadJID] = true
	}

	for _, t := range tbl.LSDeleteThenInsertThread {
		r.upsertLocked(threadRow{
			key: t.ThreadKey, tt: t.ThreadType, name: t.ThreadName, snippet: t.Snippet,
			activity: t.LastActivityTimestampMs, read: t.LastReadWatermarkTimestampMs, folder: t.FolderName, replace: true,
		}, changed)
	}
	for _, t := range tbl.LSUpdateOrInsertThread {
		r.upsertLocked(threadRow{
			key: t.ThreadKey, tt: t.ThreadType, name: t.ThreadName, snippet: t.Snippet,
			activity: t.LastActivityTimestampMs, read: t.LastReadWatermarkTimestampMs, folder: t.FolderName,
		}, changed)
	}
	for _, m := range tbl.LSMoveThreadToE2EECutoverFolder {
		id := strconv.FormatInt(r.canonLocked(m.ThreadKey), 10)
		if e, ok := r.threads[id]; ok && !e.E2EE {
			e.E2EE = true
			changed[id] = true
		}
	}
	for _, p := range tbl.LSAddParticipantIdToGroupThread {
		id := strconv.FormatInt(r.canonLocked(p.ThreadKey), 10)
		e, ok := r.threads[id]
		if !ok || slices.Contains(e.Members, p.ContactId) {
			continue
		}
		e.Members = append(e.Members, p.ContactId)
		changed[id] = true
	}
	for _, p := range tbl.LSRemoveParticipantFromThread {
		id := strconv.FormatInt(r.canonLocked(p.ThreadKey), 10)
		e, ok := r.threads[id]
		if !ok {
			continue
		}
		if p.ParticipantId == r.me {
			delete(r.threads, id)
			delete(changed, id)
			continue
		}
		if i := slices.Index(e.Members, p.ParticipantId); i >= 0 {
			e.Members = slices.Delete(e.Members, i, i+1)
			changed[id] = true
		}
	}
	for _, m := range tbl.LSMarkThreadReadV2 {
		id := strconv.FormatInt(r.canonLocked(m.ThreadKey), 10)
		e, ok := r.threads[id]
		if !ok || m.LastReadWatermarkTimestampMs <= e.LastReadMS {
			continue
		}
		before := r.snapLocked(e)
		e.LastReadMS = m.LastReadWatermarkTimestampMs
		e.ReadKnown = true
		if !reflect.DeepEqual(before, r.snapLocked(e)) {
			changed[id] = true
		}
	}
	del := func(key int64) {
		if active[key] {
			return
		}
		id := strconv.FormatInt(r.canonLocked(key), 10)
		delete(r.threads, id)
		delete(changed, id)
	}
	for _, d := range tbl.LSDeleteThread {
		del(d.ThreadKey)
	}
	for _, d := range tbl.LSDeletePartialThread {
		del(d.ThreadKey)
	}
	for _, d := range tbl.LSDeleteMessageRequest {
		del(d.ThreadKey)
	}

	out := make([]string, 0, len(changed))
	for id := range changed {
		if _, ok := r.threads[id]; ok {
			out = append(out, id)
		}
	}
	slices.Sort(out)
	return out
}

// touch records a message on a known thread: newer activity, preview, and read state for own messages.
func (r *registry) touch(id string, tsMS int64, preview string, fromMe bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	e, ok := r.threads[id]
	if !ok || tsMS < e.LastActivityMS {
		return
	}
	e.LastActivityMS = tsMS
	if preview != "" {
		e.Preview = preview
	}
	if fromMe {
		e.LastReadMS = tsMS
	}
}

// noteE2EE adds or updates a thread seen through the encrypted client and reports whether it changed.
func (r *registry) noteE2EE(chat types.JID, sender int64, tsMS int64, preview string) (id string, created, changed bool) {
	id = normalize.ThreadIDFromJID(chat)
	r.mu.Lock()
	defer r.mu.Unlock()
	e, ok := r.threads[id]
	var before api.Thread
	if ok {
		before = r.snapLocked(e)
	} else {
		e = &threadEntry{ID: id, Kind: api.KindDM}
		if chat.Server == types.GroupServer {
			e.Kind = api.KindGroup
			e.Members = []int64{r.me}
		}
		r.threads[id] = e
	}
	e.E2EE = true
	if e.WAServer == "" {
		e.WAServer = chat.Server
	}
	if e.Kind == api.KindGroup && sender != 0 && !slices.Contains(e.Members, sender) {
		e.Members = append(e.Members, sender)
	}
	if tsMS >= e.LastActivityMS {
		e.LastActivityMS = tsMS
		if preview != "" {
			e.Preview = preview
		}
	}
	return id, !ok, !ok || !reflect.DeepEqual(before, r.snapLocked(e))
}

// setGroupInfo fills a group thread from the encrypted group info and reports whether it changed.
func (r *registry) setGroupInfo(id, name string, members, admins []int64) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	e, ok := r.threads[id]
	if !ok {
		return false
	}
	before := r.snapLocked(e)
	if name != "" {
		e.Name = name
	}
	if len(members) > 0 {
		e.Members = members
	}
	e.Admins = admins
	return !reflect.DeepEqual(before, r.snapLocked(e))
}

// updateAdmins applies the promotions and the demotions or departures of a group-info change to the admin list.
func (r *registry) updateAdmins(id string, promote, demote []int64) {
	r.mu.Lock()
	defer r.mu.Unlock()
	e, ok := r.threads[id]
	if !ok || e.Kind != api.KindGroup {
		return
	}
	admins := slices.DeleteFunc(slices.Clone(e.Admins), func(a int64) bool { return slices.Contains(demote, a) })
	for _, u := range promote {
		if u != 0 && !slices.Contains(admins, u) {
			admins = append(admins, u)
		}
	}
	e.Admins = admins
}

// isAdmin reports whether user is a known admin of the group thread id.
func (r *registry) isAdmin(id string, user int64) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	e, ok := r.threads[id]
	return ok && e.Kind == api.KindGroup && user != 0 && slices.Contains(e.Admins, user)
}

type pageCursor struct {
	activity int64
	id       string
}

func encodeCursor(c pageCursor) string {
	return base64.RawURLEncoding.EncodeToString([]byte(strconv.FormatInt(c.activity, 10) + ":" + c.id))
}

func decodeCursor(s string) (pageCursor, bool) {
	b, err := base64.RawURLEncoding.DecodeString(s)
	if err != nil {
		return pageCursor{}, false
	}
	act, id, ok := strings.Cut(string(b), ":")
	if !ok || id == "" {
		return pageCursor{}, false
	}
	a, err := strconv.ParseInt(act, 10, 64)
	if err != nil {
		return pageCursor{}, false
	}
	return pageCursor{activity: a, id: id}, true
}

// cmpID orders ids numerically when both parse, else as strings.
func cmpID(a, b string) int {
	x, errA := strconv.ParseInt(a, 10, 64)
	y, errB := strconv.ParseInt(b, 10, 64)
	if errA == nil && errB == nil {
		return cmp.Compare(x, y)
	}
	return strings.Compare(a, b)
}

// order sorts by last activity descending, then id ascending.
func order(a, b *threadEntry) int {
	if c := cmp.Compare(b.LastActivityMS, a.LastActivityMS); c != 0 {
		return c
	}
	return cmpID(a.ID, b.ID)
}

// page returns up to limit threads after the cursor and how many known threads remain beyond them.
func (r *registry) page(after *pageCursor, limit int) ([]api.Thread, int) {
	r.mu.Lock()
	defer r.mu.Unlock()
	all := make([]*threadEntry, 0, len(r.threads))
	for _, e := range r.threads {
		all = append(all, e)
	}
	slices.SortFunc(all, order)
	start := 0
	if after != nil {
		probe := &threadEntry{ID: after.id, LastActivityMS: after.activity}
		start = len(all)
		for i, e := range all {
			if order(probe, e) < 0 {
				start = i
				break
			}
		}
	}
	rest := all[start:]
	n := min(limit, len(rest))
	out := make([]api.Thread, 0, n)
	for _, e := range rest[:n] {
		out = append(out, r.snapLocked(e))
	}
	return out, len(rest) - n
}

// remaining counts known threads after the cursor.
func (r *registry) remaining(after *pageCursor) int {
	_, rest := r.page(after, 0)
	return rest
}

const listFetchRounds = 3

var listRoundDelay = 2 * time.Second

// ListThreads pages the known threads, asking Messenger for older ones when the page is short.
func (s *session) ListThreads(ctx context.Context, p api.ListThreadsParams) (api.ListThreadsResult, error) {
	if p.Limit < 1 {
		return api.ListThreadsResult{}, api.Errorf(api.CodeBadParams, "limit must be positive")
	}
	var after *pageCursor
	if p.Cursor != "" {
		c, ok := decodeCursor(p.Cursor)
		if !ok {
			return api.ListThreadsResult{}, api.Errorf(api.CodeBadParams, "invalid cursor")
		}
		after = &c
	}
	if s.reg.empty() && !s.connected() {
		return api.ListThreadsResult{}, api.Errorf(api.CodeNotConnected, "not connected")
	}

	s.listMu.Lock()
	defer s.listMu.Unlock()
	for round := 0; round < listFetchRounds; round++ {
		if s.reg.remaining(after) >= p.Limit || s.threadsExhaustedLocked() {
			break
		}
		cl := s.messagix()
		if cl == nil || !cl.IsConnected() {
			break
		}
		if round > 0 {
			select {
			case <-time.After(listRoundDelay):
			case <-ctx.Done():
				return api.ListThreadsResult{}, api.Errorf(api.CodeIO, "request cancelled")
			}
		}
		if err := s.fetchMoreThreadsLocked(ctx, cl); err != nil {
			s.log.Warn().Msg("fetching more threads failed")
			if s.reg.empty() {
				return api.ListThreadsResult{}, api.Errorf(api.CodeIO, "facebook unreachable")
			}
			break
		}
	}

	threads, rest := s.reg.page(after, p.Limit)
	res := api.ListThreadsResult{Threads: threads}
	if res.Threads == nil {
		res.Threads = []api.Thread{}
	}
	switch {
	case len(threads) == 0:
		if after != nil && (rest > 0 || (!s.threadsExhaustedLocked() && s.connected())) {
			res.Cursor = p.Cursor
		}
	case rest > 0 || (!s.threadsExhaustedLocked() && s.connected()):
		last := threads[len(threads)-1]
		act := int64(0)
		if last.LastMessageTS != nil {
			if e, ok := s.reg.entry(last.ID); ok {
				act = e.LastActivityMS
			}
		}
		res.Cursor = encodeCursor(pageCursor{activity: act, id: last.ID})
	}
	return res, nil
}

func (s *session) threadGroups() []int64 {
	if s.e2eeActive() {
		return []int64{1, 95}
	}
	return []int64{1}
}

func (s *session) threadsExhaustedLocked() bool {
	for _, g := range s.threadGroups() {
		if gf := s.groups[g]; gf == nil || !gf.exhausted {
			return false
		}
	}
	return true
}

// fetchMoreThreadsLocked runs one FetchMoreThreads call per sync group and processes the tables.
func (s *session) fetchMoreThreadsLocked(ctx context.Context, cl metaClient) error {
	for _, g := range s.threadGroups() {
		gf := s.groups[g]
		if gf == nil {
			gf = &groupFetch{}
			s.groups[g] = gf
		}
		if gf.exhausted {
			continue
		}
		ks, tbl, err := cl.FetchMoreThreads(ctx, g)
		if err != nil {
			return err
		}
		if tbl == nil {
			gf.exhausted = true
			continue
		}
		s.processTable(tbl, modeBackfill)
		cl.PostHandlePublishResponse(tbl)
		if ks == nil || !ks.HasMoreBefore || ks.MinThreadKey == gf.prevMin {
			gf.exhausted = true
		}
		if ks != nil {
			gf.prevMin = ks.MinThreadKey
		}
	}
	return nil
}
