package engine

import (
	"cmp"
	"context"
	"math"
	"reflect"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"go.mau.fi/mautrix-meta/pkg/messagix/socket"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

const (
	cacheCap          = 500
	fetchTimeout      = 30 * time.Second
	fetchRetryAfter   = 5 * time.Second
	maxFetchReRequest = 1
	noCursor          = int64(math.MaxInt64)
)

type cachedMsg struct {
	m  api.Message
	ts int64
}

type threadCache struct {
	msgs     map[string]cachedMsg
	covMin   int64
	covMax   int64
	hasCov   bool
	complete bool
}

// msgCache keeps the newest cacheCap unencrypted messages per thread plus the contiguous range they cover.
type msgCache struct {
	mu      sync.Mutex
	threads map[string]*threadCache
	index   map[string]string
}

func newMsgCache() *msgCache {
	return &msgCache{threads: map[string]*threadCache{}, index: map[string]string{}}
}

func (c *msgCache) threadLocked(id string) *threadCache {
	t := c.threads[id]
	if t == nil {
		t = &threadCache{msgs: map[string]cachedMsg{}}
		c.threads[id] = t
	}
	return t
}

func (c *msgCache) get(id string) (api.Message, int64, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	tid, ok := c.index[id]
	if !ok {
		return api.Message{}, 0, false
	}
	cm, ok := c.threads[tid].msgs[id]
	return cm.m, cm.ts, ok
}

// put stores a message and reports whether it changed the cache; live messages extend the covered tail.
func (c *msgCache) put(m api.Message, ts int64, live bool) bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	t := c.threadLocked(m.ThreadID)
	if old, ok := t.msgs[m.ID]; ok && old.ts == ts && reflect.DeepEqual(old.m, m) {
		return false
	}
	t.msgs[m.ID] = cachedMsg{m: m, ts: ts}
	c.index[m.ID] = m.ThreadID
	if live {
		switch {
		case !t.hasCov:
			t.hasCov, t.covMin, t.covMax = true, ts, ts
		case ts >= t.covMin && ts > t.covMax:
			t.covMax = ts
		}
	}
	c.trimLocked(t)
	return true
}

func (c *msgCache) trimLocked(t *threadCache) {
	if len(t.msgs) <= cacheCap {
		return
	}
	all := make([]cachedMsg, 0, len(t.msgs))
	for _, cm := range t.msgs {
		all = append(all, cm)
	}
	slices.SortFunc(all, func(a, b cachedMsg) int {
		if d := cmp.Compare(b.ts, a.ts); d != 0 {
			return d
		}
		return strings.Compare(b.m.ID, a.m.ID)
	})
	for _, cm := range all[cacheCap:] {
		delete(t.msgs, cm.m.ID)
		delete(c.index, cm.m.ID)
	}
	oldest := all[cacheCap-1].ts
	if t.hasCov && t.covMin < oldest {
		t.covMin = oldest
		t.complete = false
	}
}

// replace overwrites a cached message in place, keeping its timestamp.
func (c *msgCache) replace(m api.Message) bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	tid, ok := c.index[m.ID]
	if !ok {
		return false
	}
	t := c.threads[tid]
	cm := t.msgs[m.ID]
	cm.m = m
	t.msgs[m.ID] = cm
	return true
}

func (c *msgCache) remove(id string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	tid, ok := c.index[id]
	if !ok {
		return
	}
	delete(c.threads[tid].msgs, id)
	delete(c.index, id)
}

// addRange folds a server-reported message range into the covered interval of a thread.
func (c *msgCache) addRange(threadID string, minTS, maxTS int64, hasMoreBefore bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	t := c.threadLocked(threadID)
	switch {
	case !t.hasCov:
		t.hasCov, t.covMin, t.covMax, t.complete = true, minTS, maxTS, !hasMoreBefore
	case maxTS+1 >= t.covMin && minTS <= t.covMax+1:
		if minTS < t.covMin {
			t.covMin = minTS
			t.complete = !hasMoreBefore
		} else if minTS == t.covMin && !hasMoreBefore {
			t.complete = true
		}
		t.covMax = max(t.covMax, maxTS)
	case minTS > t.covMax:
		t.covMin, t.covMax, t.complete = minTS, maxTS, !hasMoreBefore
	}
}

// extendAdjacent grows the covered range with a page fetched "older than ref" when ref lies inside it.
func (c *msgCache) extendAdjacent(threadID string, ref, minTS int64, hasMoreBefore bool) bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	t := c.threads[threadID]
	if t == nil || !t.hasCov || ref < t.covMin || ref > t.covMax+1 {
		return false
	}
	if minTS < t.covMin {
		t.covMin = minTS
		t.complete = !hasMoreBefore
	} else if !hasMoreBefore {
		t.complete = true
	}
	return true
}

func olderThan(m cachedMsg, cursor int64, beforeID string) bool {
	if m.ts < cursor {
		return true
	}
	return beforeID != "" && m.ts == cursor && m.m.ID < beforeID
}

func sortNewestFirst(msgs []cachedMsg) {
	slices.SortFunc(msgs, func(a, b cachedMsg) int {
		if d := cmp.Compare(b.ts, a.ts); d != 0 {
			return d
		}
		return strings.Compare(b.m.ID, a.m.ID)
	})
}

// list serves a page from the cache when the cursor lies inside the contiguous covered range.
func (c *msgCache) list(threadID string, cursor int64, beforeID string, limit int, lastActivityMS int64) ([]api.Message, bool, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	t := c.threads[threadID]
	if t == nil || !t.hasCov {
		return nil, false, false
	}
	if cursor == noCursor {
		if lastActivityMS > t.covMax {
			return nil, false, false
		}
	} else if cursor > t.covMax+1 {
		return nil, false, false
	}
	var cand []cachedMsg
	for _, cm := range t.msgs {
		if cm.ts >= t.covMin && olderThan(cm, cursor, beforeID) {
			cand = append(cand, cm)
		}
	}
	if len(cand) < limit && !t.complete {
		return nil, false, false
	}
	sortNewestFirst(cand)
	hasMore := len(cand) > limit || !t.complete
	if len(cand) > limit {
		cand = cand[:limit]
	}
	out := make([]api.Message, len(cand))
	for i, cm := range cand {
		out[i] = cm.m
	}
	return out, hasMore, true
}

// collector gathers the pages answering one FetchMessagesTask series for a thread.
type collector struct {
	threadID string
	want     int
	cursor   int64
	beforeID string

	mu            sync.Mutex
	got           map[string]cachedMsg
	minTS         int64
	minMsgID      string
	hasMoreBefore bool
	gotRange      bool
	progress      int
	isDone        bool
	done          chan struct{}
	more          chan struct{}
}

func newCollector(threadID string, want int, cursor int64, beforeID string, refTS int64) *collector {
	return &collector{
		threadID: threadID, want: want, cursor: cursor, beforeID: beforeID,
		got: map[string]cachedMsg{}, minTS: refTS, hasMoreBefore: true,
		done: make(chan struct{}), more: make(chan struct{}, 1),
	}
}

func (c *collector) finishLocked() {
	if !c.isDone {
		c.isDone = true
		close(c.done)
	}
}

// feed adds one page; maxTemplate is the range's MaxTimestampMsTemplate used to ignore stray upserts.
func (c *collector) feed(msgs []cachedMsg, rng *api.MessageRange, maxTemplate int64) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.isDone {
		return
	}
	if rng != nil && maxTemplate > c.minTS {
		return
	}
	for _, cm := range msgs {
		if olderThan(cm, c.cursor, c.beforeID) {
			c.got[cm.m.ID] = cm
		}
	}
	c.progress++
	if rng == nil {
		return
	}
	c.gotRange = true
	c.minTS = rng.MinTSMS
	c.minMsgID = rng.MinMessageID
	c.hasMoreBefore = rng.HasMoreBefore
	if len(c.got) >= c.want || !rng.HasMoreBefore {
		c.finishLocked()
		return
	}
	select {
	case c.more <- struct{}{}:
	default:
	}
}

// existingRange handles LSUpdateExistingMessageRange, which closes a collection.
func (c *collector) existingRange(tsMS int64, bool2, bool3 bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.isDone || c.minTS != tsMS {
		return
	}
	if len(c.got) == 0 || (bool2 && !bool3) {
		c.hasMoreBefore = false
	}
	c.progress++
	c.finishLocked()
}

func (c *collector) ref() (int64, string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.minTS, c.minMsgID
}

func (c *collector) progressCount() int {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.progress
}

func (c *collector) result(limit int) ([]api.Message, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	all := make([]cachedMsg, 0, len(c.got))
	for _, cm := range c.got {
		all = append(all, cm)
	}
	sortNewestFirst(all)
	hasMore := len(all) > limit || c.hasMoreBefore
	if len(all) > limit {
		all = all[:limit]
	}
	out := make([]api.Message, len(all))
	for i, cm := range all {
		out[i] = cm.m
	}
	return out, hasMore
}

func (s *session) addCollector(c *collector) bool {
	s.collMu.Lock()
	defer s.collMu.Unlock()
	if _, ok := s.collectors[c.threadID]; ok {
		return false
	}
	s.collectors[c.threadID] = c
	return true
}

func (s *session) dropCollector(c *collector) {
	s.collMu.Lock()
	defer s.collMu.Unlock()
	if s.collectors[c.threadID] == c {
		delete(s.collectors, c.threadID)
	}
}

func (s *session) collectorFor(threadID string) *collector {
	s.collMu.Lock()
	defer s.collMu.Unlock()
	return s.collectors[threadID]
}

// FetchMessages serves history from the E2EE store, the in-memory cache or a Messenger fetch.
func (s *session) FetchMessages(ctx context.Context, p api.FetchMessagesParams) (api.FetchMessagesResult, error) {
	key, err := strconv.ParseInt(p.ThreadID, 10, 64)
	if err != nil || key <= 0 {
		return api.FetchMessagesResult{}, api.Errorf(api.CodeThreadNotFound, "thread not found")
	}
	if p.Limit < 1 {
		return api.FetchMessagesResult{}, api.Errorf(api.CodeBadParams, "limit must be positive")
	}
	entry, known := s.reg.entry(p.ThreadID)
	if known && entry.encrypted() {
		return s.fetchEncrypted(ctx, p)
	}

	cursor, refMid, err := s.resolveCursor(p)
	if err != nil {
		return api.FetchMessagesResult{}, err
	}
	if msgs, more, ok := s.hist.list(p.ThreadID, cursor, p.BeforeID, p.Limit, entry.LastActivityMS); ok {
		return api.FetchMessagesResult{Messages: msgs, HasMore: more}, nil
	}
	cl := s.messagix()
	if cl == nil || !cl.IsConnected() {
		return api.FetchMessagesResult{}, api.Errorf(api.CodeNotConnected, "not connected")
	}
	fbKey := key
	if known && entry.FBThreadKey > 0 {
		fbKey = entry.FBThreadKey
	}
	return s.fetchRemote(ctx, cl, p, fbKey, cursor, refMid)
}

func (s *session) fetchEncrypted(ctx context.Context, p api.FetchMessagesParams) (api.FetchMessagesResult, error) {
	st, err := s.ensureStore(ctx)
	if err != nil {
		return api.FetchMessagesResult{}, err
	}
	msgs, more, err := st.List(ctx, p.ThreadID, p.BeforeTSMS, p.BeforeID, p.Limit)
	if err != nil {
		return api.FetchMessagesResult{}, api.AsError(err)
	}
	if msgs == nil {
		msgs = []api.Message{}
	}
	return api.FetchMessagesResult{Messages: msgs, HasMore: more}, nil
}

// resolveCursor turns before_ts_ms/before_id into a timestamp and the library message id of the reference.
func (s *session) resolveCursor(p api.FetchMessagesParams) (int64, string, error) {
	cursor := noCursor
	refMid := ""
	if p.BeforeID != "" {
		refMid = midOf(p.BeforeID)
		m, ts, ok := s.hist.get(p.BeforeID)
		switch {
		case ok && m.ThreadID == p.ThreadID:
			cursor = ts
		case p.BeforeTSMS == nil:
			return 0, "", api.Errorf(api.CodeMessageNotFound, "message not found")
		}
	}
	if p.BeforeTSMS != nil {
		cursor = *p.BeforeTSMS
	}
	return cursor, refMid, nil
}

func (s *session) fetchRemote(ctx context.Context, cl metaClient, p api.FetchMessagesParams, fbKey, cursor int64, refMid string) (api.FetchMessagesResult, error) {
	refTS := cursor
	if refTS == noCursor {
		refTS = time.Now().UnixMilli()
	}
	col := newCollector(p.ThreadID, p.Limit, cursor, p.BeforeID, refTS)
	if !s.addCollector(col) {
		return api.FetchMessagesResult{}, api.Errorf(api.CodeIO, "a history fetch is already running for this thread")
	}
	defer s.dropCollector(col)

	request := func(ts int64, mid string) error {
		tbl, err := cl.ExecuteTasks(ctx, &socket.FetchMessagesTask{
			ThreadKey: fbKey, Direction: 0, ReferenceTimestampMs: ts, ReferenceMessageId: mid,
			SyncGroup: 1, Cursor: cl.GetCursor(1),
		})
		if err != nil {
			return err
		}
		if tbl != nil {
			s.processTable(tbl, modeBackfill)
		}
		return nil
	}
	if err := request(refTS, refMid); err != nil {
		return api.FetchMessagesResult{}, api.Errorf(api.CodeIO, "history fetch failed")
	}

	timeout := time.NewTimer(fetchTimeout)
	defer timeout.Stop()
	tick := time.NewTicker(fetchRetryAfter)
	defer tick.Stop()
	reRequests := 0
	seen := col.progressCount()
	for {
		select {
		case <-col.done:
			msgs, more := col.result(p.Limit)
			return api.FetchMessagesResult{Messages: msgs, HasMore: more}, nil
		case <-col.more:
			ts, mid := col.ref()
			if err := request(ts, midOf(mid)); err != nil {
				return api.FetchMessagesResult{}, api.Errorf(api.CodeIO, "history fetch failed")
			}
			seen = col.progressCount()
		case <-tick.C:
			if col.progressCount() == seen && reRequests < maxFetchReRequest {
				reRequests++
				ts, mid := col.ref()
				if err := request(ts, midOf(mid)); err != nil {
					return api.FetchMessagesResult{}, api.Errorf(api.CodeIO, "history fetch failed")
				}
			}
			seen = col.progressCount()
		case <-timeout.C:
			msgs, _ := col.result(p.Limit)
			if len(msgs) == 0 {
				return api.FetchMessagesResult{}, api.Errorf(api.CodeIO, "history fetch timed out")
			}
			return api.FetchMessagesResult{Messages: msgs, HasMore: true}, nil
		case <-ctx.Done():
			return api.FetchMessagesResult{}, api.Errorf(api.CodeIO, "history fetch cancelled")
		}
	}
}

// midOf strips the fb: prefix from a normalised message id; "" for other forms.
func midOf(id string) string {
	mid, ok := strings.CutPrefix(id, "fb:")
	if !ok {
		return ""
	}
	return mid
}

// fetchAround runs one FetchMessagesTask page around a timestamp so caches refresh.
func (s *session) fetchAround(ctx context.Context, threadID string, tsMS int64) error {
	key, err := strconv.ParseInt(threadID, 10, 64)
	if err != nil || key <= 0 {
		return api.Errorf(api.CodeThreadNotFound, "thread not found")
	}
	cl := s.messagix()
	if cl == nil || !cl.IsConnected() {
		return api.Errorf(api.CodeNotConnected, "not connected")
	}
	if e, ok := s.reg.entry(threadID); ok && e.FBThreadKey > 0 {
		key = e.FBThreadKey
	}
	tbl, err := cl.ExecuteTasks(ctx, &socket.FetchMessagesTask{
		ThreadKey: key, Direction: 0, ReferenceTimestampMs: tsMS + 1, SyncGroup: 1, Cursor: cl.GetCursor(1),
	})
	if err != nil {
		return api.Errorf(api.CodeIO, "history fetch failed")
	}
	if tbl != nil {
		s.processTable(tbl, modeBackfill)
	}
	return nil
}

// cachedMessage returns an unencrypted cached message with its timestamp in ms.
func (s *session) cachedMessage(threadID, msgID string) (api.Message, int64, bool) {
	m, ts, ok := s.hist.get(msgID)
	if !ok || m.ThreadID != threadID {
		return api.Message{}, 0, false
	}
	return m, ts, true
}
