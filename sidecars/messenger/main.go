// Command meta-messenger-sidecar serves Messenger over stdin and stdout to the Rust daemon.
package main

import (
	"context"
	"errors"
	"io"
	"os"
	"os/signal"
	"syscall"

	"github.com/rs/zerolog"
	zlog "github.com/rs/zerolog/log"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/engine"
	"github.com/matpb/meta-mcp-rs/sidecars/messenger/logscrub"
	"github.com/matpb/meta-mcp-rs/sidecars/messenger/server"
)

func main() {
	os.Exit(run())
}

// newLogger returns the Info-only logger whose output passes through the scrubber.
func newLogger(dst io.Writer) (zerolog.Logger, *logscrub.Writer) {
	zerolog.SetGlobalLevel(zerolog.InfoLevel)
	scrub := logscrub.New(dst)
	logger := zerolog.New(scrub).With().Timestamp().Logger().Level(zerolog.InfoLevel)
	zlog.Logger = logger
	return logger, scrub
}

func run() int {
	log, scrub := newLogger(os.Stderr)
	defer func() { _ = scrub.Flush() }()

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	err := server.Run(ctx, os.Stdin, os.Stdout, log, engine.NewFactory(log))
	switch {
	case err == nil, errors.Is(err, context.Canceled):
		return 0
	case errors.Is(err, server.ErrFrame):
		return 2
	default:
		log.Error().Msg("sidecar stopped on I/O error")
		return 1
	}
}
