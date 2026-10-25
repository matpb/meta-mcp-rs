// Command meta-whatsapp-sidecar serves WhatsApp over stdin and stdout to the Rust daemon.
package main

import (
	"context"
	"errors"
	"io"
	"os"
	"os/signal"
	"syscall"

	"github.com/rs/zerolog"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/logscrub"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/server"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/wa"
)

func main() {
	setUmask()
	os.Exit(run())
}

// newLogger returns the Info-only logger whose output passes through the scrubber.
func newLogger(dst io.Writer) (zerolog.Logger, *logscrub.Writer) {
	zerolog.SetGlobalLevel(zerolog.InfoLevel)
	scrub := logscrub.New(dst)
	return zerolog.New(scrub).With().Timestamp().Logger().Level(zerolog.InfoLevel), scrub
}

// open adapts wa.Open to the factory type the server expects.
func open(ctx context.Context, dataDir string, log zerolog.Logger) (wa.Factory, error) {
	s, err := wa.Open(ctx, dataDir, log)
	if err != nil {
		return nil, err
	}
	return s, nil
}

func run() int {
	log, scrub := newLogger(os.Stderr)
	defer func() { _ = scrub.Flush() }()

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	err := server.Run(ctx, os.Stdin, os.Stdout, log, open)
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
