package engine

import (
	"context"
	"time"

	"github.com/rs/zerolog"
	"go.mau.fi/mautrix-meta/pkg/messagix"
	"go.mau.fi/mautrix-meta/pkg/messagix/cookies"
	"go.mau.fi/mautrix-meta/pkg/messagix/httpclient"
	"go.mau.fi/mautrix-meta/pkg/messagix/socket"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
	"go.mau.fi/mautrix-meta/pkg/messagix/types"
	"go.mau.fi/util/exhttp"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/store"
)

// metaClient is the subset of *messagix.Client the engine calls; tests substitute a fake.
type metaClient interface {
	LoadMessagesPage(ctx context.Context) (types.UserInfo, *table.LSTable, error)
	Connect(ctx context.Context) error
	Disconnect()
	IsConnected() bool
	SetEventHandler(handler messagix.EventHandler)
	ExecuteTasks(ctx context.Context, tasks ...socket.Task) (*table.LSTable, error)
	ExecuteStatelessTask(ctx context.Context, task socket.Task) error
	FetchMoreThreads(ctx context.Context, syncGroup int64) (*socket.KeyStoreData, *table.LSTable, error)
	WaitUntilCanSendMessages(ctx context.Context, timeout time.Duration) error
	PostHandlePublishResponse(tbl *table.LSTable)
	GetCookies() *cookies.Cookies
	GetCursor(db int64) string
	GetSyncGroupKeyStore(syncGroup int64) *socket.KeyStoreData
	SetDevice(dev *store.Device)
	RegisterE2EE(ctx context.Context, fbid int64) error
	PrepareE2EEClient() (*whatsmeow.Client, error)
	GetHTTP() *httpclient.HTTPClient
}

// newMessagixClient is the production constructor: Facebook platform, default library config.
func newMessagixClient(c *cookies.Cookies, log zerolog.Logger) metaClient {
	return messagix.NewClient(c, log, &messagix.Config{ClientSettings: exhttp.SensibleClientSettings})
}

var _ metaClient = (*messagix.Client)(nil)
