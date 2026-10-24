package engine

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"image"
	"image/color"
	"image/draw"
	_ "image/gif"  // registers the GIF decoder for image.DecodeConfig
	_ "image/jpeg" // registers the JPEG decoder for image.DecodeConfig
	"image/png"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"go.mau.fi/mautrix-meta/pkg/messagix/httpclient"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
	"go.mau.fi/mautrix-meta/pkg/msgconv/mediadl"
	"go.mau.fi/util/exhttp"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waCommon"
	"go.mau.fi/whatsmeow/proto/waConsumerApplication"
	"go.mau.fi/whatsmeow/proto/waMediaTransport"
	"google.golang.org/protobuf/proto"
	"maunium.net/go/mautrix"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

// Size limits: the connector's capabilities (decimal megabytes) and the download cap.
const (
	maxImageBytes    = 8 * 1000 * 1000
	maxFileBytes     = 25 * 1000 * 1000
	maxE2EEBytes     = 100 * 1000 * 1000
	maxDownloadBytes = 100 << 20
	thumbMax         = 400
	sniffLen         = 512
	expirySlack      = 60
	uploadTimeout    = 2 * time.Minute
	downloadTimeout  = 5 * time.Minute
)

var errTooLarge = errors.New("attachment too large")

var mediaInit sync.Once

// downloadMedia fetches a CDN URL; tests replace it.
var downloadMedia = func(ctx context.Context, mime, url string, max int64) (int64, io.ReadCloser, error) {
	mediaInit.Do(func() { mediadl.SetHTTP(exhttp.SensibleClientSettings) })
	return mediadl.DownloadMedia(ctx, mime, url, max)
}

// readMediaFile reads the file to send, enforcing the size cap before and while reading.
func readMediaFile(path string, limit int64) ([]byte, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, api.Errorf(api.CodeIO, "cannot read the file")
	}
	defer f.Close()
	st, err := f.Stat()
	if err != nil || !st.Mode().IsRegular() {
		return nil, api.Errorf(api.CodeBadParams, "path is not a regular file")
	}
	if st.Size() > limit {
		return nil, api.Errorf(api.CodeBadParams, "file too large for messenger")
	}
	data, err := io.ReadAll(io.LimitReader(f, limit+1))
	if err != nil {
		return nil, api.Errorf(api.CodeIO, "cannot read the file")
	}
	if int64(len(data)) > limit {
		return nil, api.Errorf(api.CodeBadParams, "file too large for messenger")
	}
	return data, nil
}

func mediaMime(p api.SendMediaParams, data []byte) string {
	if p.Mime != "" {
		return p.Mime
	}
	return http.DetectContentType(data)
}

func mediaName(p api.SendMediaParams) string {
	if p.Filename != "" {
		return p.Filename
	}
	return filepath.Base(p.Path)
}

// SendMedia uploads a file and sends it with an optional caption.
func (s *session) SendMedia(ctx context.Context, p api.SendMediaParams) (api.SendResult, error) {
	if err := s.usable(); err != nil {
		return api.SendResult{}, err
	}
	t, err := s.resolveThread(p.ThreadID)
	if err != nil {
		return api.SendResult{}, err
	}
	if t.enc {
		return s.sendMediaE2EE(ctx, t, p)
	}
	limit := int64(maxFileBytes)
	if p.Kind == "image" {
		limit = maxImageBytes
	}
	cl, err := s.sendClient(ctx)
	if err != nil {
		return api.SendResult{}, err
	}
	data, err := readMediaFile(p.Path, limit)
	if err != nil {
		return api.SendResult{}, err
	}
	fbid, err := s.uploadMercury(ctx, cl, t.fbKey, mediaName(p), mediaMime(p, data), data)
	if err != nil {
		return api.SendResult{}, err
	}
	task := s.newSendTask(t, table.MEDIA)
	task.AttachmentFBIds = []int64{fbid}
	task.Text = p.Caption
	return s.executeSend(ctx, cl, task)
}

// uploadMercury uploads to Messenger and returns the attachment fbid; nothing is sent yet.
func (s *session) uploadMercury(ctx context.Context, cl metaClient, threadKey int64, name, mime string, data []byte) (int64, error) {
	uctx, cancel := context.WithTimeout(ctx, uploadTimeout)
	defer cancel()
	resp, err := cl.GetHTTP().SendMercuryUploadRequest(uctx, threadKey, &httpclient.MercuryUploadMedia{
		Filename: name, MimeType: mime, MediaData: data,
	})
	if err != nil {
		return 0, s.opError(err, "media upload")
	}
	if resp == nil || resp.Payload.RealMetadata == nil || resp.Payload.RealMetadata.GetFbId() == 0 {
		return 0, api.Errorf(api.CodeSendFailed, "media upload failed")
	}
	return resp.Payload.RealMetadata.GetFbId(), nil
}

// sendMediaE2EE sends an image into an encrypted thread; v1 supports images only and drops captions.
func (s *session) sendMediaE2EE(ctx context.Context, t threadTarget, p api.SendMediaParams) (api.SendResult, error) {
	if p.Kind != "image" {
		return api.SendResult{}, api.Errorf(api.CodeUnsupported, "encrypted threads accept images only in v1")
	}
	wa, err := s.e2eeReady()
	if err != nil {
		return api.SendResult{}, err
	}
	data, err := readMediaFile(p.Path, maxE2EEBytes)
	if err != nil {
		return api.SendResult{}, err
	}
	mime := mediaMime(p, data)
	if !strings.HasPrefix(mime, "image/") {
		return api.SendResult{}, api.Errorf(api.CodeBadParams, "mime is not an image type")
	}
	info := inspectImage(data, mime)
	mediaType := whatsmeow.MediaImage
	if mime == "image/gif" {
		mediaType = whatsmeow.MediaVideo
	}
	uctx, cancel := context.WithTimeout(ctx, uploadTimeout)
	defer cancel()
	up, err := wa.Upload(uctx, info.data, mediaType)
	if err != nil {
		return api.SendResult{}, api.Errorf(api.CodeSendFailed, "media upload failed")
	}
	app, err := e2eeImageApp(up, info, mime)
	if err != nil {
		return api.SendResult{}, api.Errorf(api.CodeInternal, "internal error")
	}
	return s.sendE2EE(ctx, wa, t, app, nil)
}

// imageInfo is an image prepared for an encrypted send.
type imageInfo struct {
	data          []byte
	width, height int
}

// clampTo400 scales dimensions down to fit 400x400, keeping the aspect ratio.
func clampTo400(w, h int) (int, int) {
	if w > thumbMax {
		h = h * thumbMax / w
		w = thumbMax
	}
	if h > thumbMax {
		w = w * thumbMax / h
		h = thumbMax
	}
	return w, h
}

// reencodeNRGBA rewrites a PNG in a color model Messenger accepts; it returns data unchanged on failure.
func reencodeNRGBA(data []byte) []byte {
	img, err := png.Decode(bytes.NewReader(data))
	if err != nil {
		return data
	}
	b := img.Bounds()
	nrgba := image.NewNRGBA(image.Rect(0, 0, b.Dx(), b.Dy()))
	draw.Draw(nrgba, nrgba.Bounds(), img, b.Min, draw.Src)
	var buf bytes.Buffer
	if err := png.Encode(&buf, nrgba); err != nil {
		return data
	}
	return buf.Bytes()
}

// inspectImage reads the size and re-encodes PNGs in a color model Messenger accepts; undecodable input gets size 0.
func inspectImage(data []byte, mime string) imageInfo {
	info := imageInfo{data: data}
	if mime == "image/png" {
		if cfg, err := png.DecodeConfig(bytes.NewReader(data)); err == nil && cfg.ColorModel != color.NRGBAModel {
			info.data = reencodeNRGBA(data)
		}
	}
	if cfg, _, err := image.DecodeConfig(bytes.NewReader(info.data)); err == nil {
		info.width, info.height = cfg.Width, cfg.Height
	}
	return info
}

// e2eeImageApp wraps an uploaded image like the connector's image branch; GIFs go out as GIF videos.
func e2eeImageApp(up whatsmeow.UploadResponse, info imageInfo, mime string) (*waConsumerApplication.ConsumerApplication, error) {
	tw, th := clampTo400(info.width, info.height)
	if tw == 0 {
		tw, th = thumbMax, thumbMax
	}
	transport := &waMediaTransport.WAMediaTransport{
		Integral: &waMediaTransport.WAMediaTransport_Integral{
			FileSHA256:        up.FileSHA256,
			MediaKey:          up.MediaKey,
			FileEncSHA256:     up.FileEncSHA256,
			DirectPath:        &up.DirectPath,
			MediaKeyTimestamp: proto.Int64(time.Now().Unix()),
		},
		Ancillary: &waMediaTransport.WAMediaTransport_Ancillary{
			FileLength: proto.Uint64(uint64(len(info.data))),
			Mimetype:   &mime,
			Thumbnail: &waMediaTransport.WAMediaTransport_Ancillary_Thumbnail{
				ThumbnailWidth:  proto.Uint32(uint32(tw)),
				ThumbnailHeight: proto.Uint32(uint32(th)),
			},
			ObjectID: &up.ObjectID,
		},
	}
	caption := &waCommon.MessageText{}
	var content *waConsumerApplication.ConsumerApplication_Content
	if mime == "image/gif" {
		msg := &waConsumerApplication.ConsumerApplication_VideoMessage{Caption: caption}
		err := msg.Set(&waMediaTransport.VideoTransport{
			Integral: &waMediaTransport.VideoTransport_Integral{Transport: transport},
			Ancillary: &waMediaTransport.VideoTransport_Ancillary{
				Height:      proto.Uint32(uint32(info.height)),
				Width:       proto.Uint32(uint32(info.width)),
				GifPlayback: proto.Bool(true),
			},
		})
		if err != nil {
			return nil, err
		}
		content = &waConsumerApplication.ConsumerApplication_Content{Content: &waConsumerApplication.ConsumerApplication_Content_VideoMessage{VideoMessage: msg}}
	} else {
		msg := &waConsumerApplication.ConsumerApplication_ImageMessage{Caption: caption}
		err := msg.Set(&waMediaTransport.ImageTransport{
			Integral: &waMediaTransport.ImageTransport_Integral{Transport: transport},
			Ancillary: &waMediaTransport.ImageTransport_Ancillary{
				Height: proto.Uint32(uint32(info.height)),
				Width:  proto.Uint32(uint32(info.width)),
			},
		})
		if err != nil {
			return nil, err
		}
		content = &waConsumerApplication.ConsumerApplication_Content{Content: &waConsumerApplication.ConsumerApplication_Content_ImageMessage{ImageMessage: msg}}
	}
	return consumerContent(content), nil
}

// tempFile is a 0600 file next to its destination that is removed unless committed.
type tempFile struct {
	f    *os.File
	dest string
	done bool
}

func newTempFile(dest string) (*tempFile, error) {
	f, err := os.CreateTemp(filepath.Dir(dest), ".dl-*.part")
	if err != nil {
		return nil, api.Errorf(api.CodeIO, "cannot write the destination file")
	}
	return &tempFile{f: f, dest: dest}, nil
}

func (t *tempFile) discard() {
	if t.done {
		return
	}
	_ = t.f.Close()
	_ = os.Remove(t.f.Name())
}

// commit flushes, closes and moves the file to its destination with mode 0600.
func (t *tempFile) commit() error {
	if err := t.f.Sync(); err != nil {
		return err
	}
	if err := t.f.Close(); err != nil {
		return err
	}
	if err := os.Chmod(t.f.Name(), 0o600); err != nil {
		return err
	}
	if err := os.Rename(t.f.Name(), t.dest); err != nil {
		return err
	}
	t.done = true
	return nil
}

// fileInfo describes a written attachment.
type fileInfo struct {
	n    int64
	sha  string
	head []byte
}

// headCapture keeps the first sniffLen bytes written through it.
type headCapture struct{ buf []byte }

func (h *headCapture) Write(p []byte) (int, error) {
	if room := sniffLen - len(h.buf); room > 0 {
		h.buf = append(h.buf, p[:min(room, len(p))]...)
	}
	return len(p), nil
}

// writeReader streams r into dest through a temp file, hashing while writing; over limit fails with errTooLarge.
func writeReader(dest string, r io.Reader, limit int64) (fileInfo, error) {
	tf, err := newTempFile(dest)
	if err != nil {
		return fileInfo{}, err
	}
	defer tf.discard()
	h, head := sha256.New(), &headCapture{}
	n, err := io.Copy(io.MultiWriter(tf.f, h, head), io.LimitReader(r, limit+1))
	if err != nil {
		return fileInfo{}, err
	}
	if n > limit {
		return fileInfo{}, errTooLarge
	}
	if err := tf.commit(); err != nil {
		return fileInfo{}, err
	}
	return fileInfo{n: n, sha: hex.EncodeToString(h.Sum(nil)), head: head.buf}, nil
}

// writeWith lets fill write the temp file itself (encrypted downloads), then hashes and moves it.
func writeWith(dest string, limit int64, fill func(f *os.File) error) (fileInfo, error) {
	tf, err := newTempFile(dest)
	if err != nil {
		return fileInfo{}, err
	}
	defer tf.discard()
	if err := fill(tf.f); err != nil {
		return fileInfo{}, err
	}
	st, err := tf.f.Stat()
	if err != nil {
		return fileInfo{}, err
	}
	if st.Size() > limit {
		return fileInfo{}, errTooLarge
	}
	h, head := sha256.New(), &headCapture{}
	if _, err := io.Copy(io.MultiWriter(h, head), io.NewSectionReader(tf.f, 0, st.Size())); err != nil {
		return fileInfo{}, err
	}
	if err := tf.commit(); err != nil {
		return fileInfo{}, err
	}
	return fileInfo{n: st.Size(), sha: hex.EncodeToString(h.Sum(nil)), head: head.buf}, nil
}

// cappedFile is a whatsmeow.File that refuses to grow past limit and remembers that it did.
type cappedFile struct {
	*os.File
	limit   int64
	tripped atomic.Bool
}

func (c *cappedFile) deny() error {
	c.tripped.Store(true)
	return errTooLarge
}

func (c *cappedFile) Write(p []byte) (int, error) {
	off, err := c.File.Seek(0, io.SeekCurrent)
	if err != nil {
		return 0, err
	}
	if off+int64(len(p)) > c.limit {
		return 0, c.deny()
	}
	return c.File.Write(p)
}

func (c *cappedFile) WriteAt(p []byte, off int64) (int, error) {
	if off < 0 || off+int64(len(p)) > c.limit {
		return 0, c.deny()
	}
	return c.File.WriteAt(p, off)
}

func (c *cappedFile) Truncate(size int64) error {
	if size > c.limit {
		return c.deny()
	}
	return c.File.Truncate(size)
}

var _ whatsmeow.File = (*cappedFile)(nil)

func findAttachment(m api.Message, id string) (api.Attachment, bool) {
	for _, a := range m.Attachments {
		if a.ID == id {
			return a, true
		}
	}
	return api.Attachment{}, false
}

// resultMime prefers the attachment's mime and falls back to sniffing the written bytes.
func resultMime(known string, head []byte) string {
	if known != "" {
		return known
	}
	if m := http.DetectContentType(head); m != "application/octet-stream" {
		return m
	}
	return ""
}

func strVal(p *string) string {
	if p == nil {
		return ""
	}
	return *p
}

// DownloadAttachment writes one attachment to dest_path (0600) and returns its mime, size and sha256.
func (s *session) DownloadAttachment(ctx context.Context, p api.DownloadParams) (api.DownloadResult, error) {
	if err := s.usable(); err != nil {
		return api.DownloadResult{}, err
	}
	t, err := s.resolveThread(p.ThreadID)
	if err != nil {
		return api.DownloadResult{}, err
	}
	if _, err := s.checkMessageID(t, p.MessageID); err != nil {
		return api.DownloadResult{}, err
	}
	if t.enc {
		return s.downloadE2EE(ctx, t, p)
	}
	return s.downloadFB(ctx, t, p)
}

func downloadFailure(err error) error {
	switch {
	case errors.Is(err, errTooLarge), errors.Is(err, mediadl.ErrTooLargeFile):
		return api.Errorf(api.CodeIO, "attachment exceeds 100 MiB")
	case isExpiredErr(err):
		return api.Errorf(api.CodeAttachmentExpired, "attachment link expired")
	}
	var ae *api.Error
	if errors.As(err, &ae) {
		return ae
	}
	return api.Errorf(api.CodeIO, "download failed")
}

// isExpiredErr reports a CDN refusal that a fresh URL can fix.
func isExpiredErr(err error) bool {
	if errors.Is(err, mediadl.ErrForbidden) ||
		errors.Is(err, whatsmeow.ErrMediaDownloadFailedWith403) ||
		errors.Is(err, whatsmeow.ErrMediaDownloadFailedWith404) ||
		errors.Is(err, whatsmeow.ErrMediaDownloadFailedWith410) {
		return true
	}
	var re mautrix.RespError
	return errors.As(err, &re) && re.StatusCode == http.StatusGone
}

// downloadable rejects attachments that carry a link or place instead of media bytes.
func downloadable(a api.Attachment) error {
	u := strVal(a.URL)
	if a.Kind == api.AttLink || a.Kind == api.AttLocation || !strings.HasPrefix(u, "https://") {
		return api.Errorf(api.CodeUnsupported, "attachment has no downloadable media")
	}
	return nil
}

// downloadFB fetches an unencrypted attachment, refreshing its expiring URL once through a message fetch.
func (s *session) downloadFB(ctx context.Context, t threadTarget, p api.DownloadParams) (api.DownloadResult, error) {
	refreshed := false
	for {
		msg, ts, ok := s.cachedMessage(t.id, p.MessageID)
		if !ok {
			return api.DownloadResult{}, api.Errorf(api.CodeMessageNotFound, "message not found")
		}
		att, ok := findAttachment(msg, p.AttachmentID)
		if !ok {
			return api.DownloadResult{}, api.Errorf(api.CodeBadParams, "unknown attachment")
		}
		if err := downloadable(att); err != nil {
			return api.DownloadResult{}, err
		}
		now := time.Now().Unix()
		if att.ExpiresAt != nil && *att.ExpiresAt-now <= expirySlack && !refreshed {
			refreshed = true
			if err := s.fetchAround(ctx, t.id, ts); err != nil {
				s.log.Warn().Msg("refreshing an attachment link failed")
			}
			continue
		}
		if att.ExpiresAt != nil && *att.ExpiresAt <= now {
			return api.DownloadResult{}, api.Errorf(api.CodeAttachmentExpired, "attachment link expired")
		}
		res, err := s.fetchURL(ctx, att, p.DestPath)
		if err != nil && isExpiredErr(err) && !refreshed {
			refreshed = true
			if ferr := s.fetchAround(ctx, t.id, ts); ferr != nil {
				s.log.Warn().Msg("refreshing an attachment link failed")
			}
			continue
		}
		if err != nil {
			return api.DownloadResult{}, downloadFailure(err)
		}
		return res, nil
	}
}

func (s *session) fetchURL(ctx context.Context, att api.Attachment, dest string) (api.DownloadResult, error) {
	dctx, cancel := context.WithTimeout(ctx, downloadTimeout)
	defer cancel()
	known := strVal(att.Mime)
	_, rc, err := downloadMedia(dctx, known, strVal(att.URL), maxDownloadBytes)
	if err != nil {
		return api.DownloadResult{}, err
	}
	defer rc.Close()
	info, err := writeReader(dest, rc, maxDownloadBytes)
	if err != nil {
		return api.DownloadResult{}, err
	}
	return api.DownloadResult{Path: dest, Mime: resultMime(known, info.head), Bytes: info.n, SHA256: info.sha}, nil
}

// downloadE2EE decrypts a stored encrypted attachment straight into a temp file.
func (s *session) downloadE2EE(ctx context.Context, t threadTarget, p api.DownloadParams) (api.DownloadResult, error) {
	st, err := s.ensureStore(ctx)
	if err != nil {
		return api.DownloadResult{}, err
	}
	msg, found, err := st.Get(ctx, p.MessageID)
	if err != nil {
		return api.DownloadResult{}, api.AsError(err)
	}
	if !found || msg.ThreadID != t.id {
		return api.DownloadResult{}, api.Errorf(api.CodeMessageNotFound, "message not found")
	}
	att, ok := findAttachment(msg, p.AttachmentID)
	if !ok {
		return api.DownloadResult{}, api.Errorf(api.CodeBadParams, "unknown attachment")
	}
	md, found, err := st.Media(ctx, p.MessageID, p.AttachmentID)
	if err != nil {
		return api.DownloadResult{}, api.AsError(err)
	}
	if !found {
		return api.DownloadResult{}, api.Errorf(api.CodeUnsupported, "attachment has no downloadable media")
	}
	if md.Size > maxDownloadBytes {
		return api.DownloadResult{}, downloadFailure(errTooLarge)
	}
	integral := &waMediaTransport.WAMediaTransport_Integral{}
	if err := proto.Unmarshal(md.Integral, integral); err != nil {
		return api.DownloadResult{}, api.Errorf(api.CodeIO, "stored media record is unreadable")
	}
	wa, err := s.e2eeReady()
	if err != nil {
		return api.DownloadResult{}, err
	}
	dctx, cancel := context.WithTimeout(ctx, downloadTimeout)
	defer cancel()
	info, err := writeWith(p.DestPath, maxDownloadBytes, func(f *os.File) error {
		cf := &cappedFile{File: f, limit: maxDownloadBytes}
		err := wa.DownloadFBToFile(dctx, integral, whatsmeow.MediaType(md.MediaType), cf)
		if cf.tripped.Load() {
			return errTooLarge
		}
		return err
	})
	if err != nil {
		return api.DownloadResult{}, downloadFailure(err)
	}
	known := md.Mime
	if known == "" {
		known = strVal(att.Mime)
	}
	return api.DownloadResult{Path: p.DestPath, Mime: resultMime(known, info.head), Bytes: info.n, SHA256: info.sha}, nil
}
