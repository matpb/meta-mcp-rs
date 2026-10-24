package engine

import (
	"bytes"
	"crypto/sha256"
	"errors"
	"image"
	"image/color"
	"image/jpeg"
	"image/png"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"go.mau.fi/mautrix-meta/pkg/msgconv/mediadl"
	"go.mau.fi/whatsmeow"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

func entries(t *testing.T, dir string) []string {
	t.Helper()
	des, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	var names []string
	for _, d := range des {
		names = append(names, d.Name())
	}
	return names
}

func TestWriteReaderHashModeAndHead(t *testing.T) {
	dir := t.TempDir()
	dest := filepath.Join(dir, "out.bin")
	payload := bytes.Repeat([]byte("abc"), 1000)
	info, err := writeReader(dest, bytes.NewReader(payload), 1<<20)
	if err != nil {
		t.Fatal(err)
	}
	if info.n != int64(len(payload)) || len(info.sha) != 64 || len(info.head) != sniffLen {
		t.Fatalf("info n=%d sha=%q head=%d", info.n, info.sha, len(info.head))
	}
	st, err := os.Stat(dest)
	if err != nil {
		t.Fatal(err)
	}
	if st.Mode().Perm() != 0o600 {
		t.Fatalf("mode %v, want 0600", st.Mode().Perm())
	}
	got, _ := os.ReadFile(dest)
	if !bytes.Equal(got, payload) {
		t.Fatal("content mismatch")
	}
	again, err := writeReader(filepath.Join(dir, "again.bin"), bytes.NewReader(payload), 1<<20)
	if err != nil || again.sha != info.sha {
		t.Fatalf("sha not stable: %v %q %q", err, again.sha, info.sha)
	}
	if names := entries(t, dir); len(names) != 2 {
		t.Fatalf("leftover files %v", names)
	}
}

func TestWriteReaderKnownDigest(t *testing.T) {
	dest := filepath.Join(t.TempDir(), "x")
	info, err := writeReader(dest, strings.NewReader("hello"), 100)
	if err != nil {
		t.Fatal(err)
	}
	if info.sha != "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824" {
		t.Fatalf("sha %q", info.sha)
	}
}

// endless yields zero bytes forever and counts what was read.
type endless struct{ read int64 }

func (e *endless) Read(p []byte) (int, error) {
	e.read += int64(len(p))
	clear(p)
	return len(p), nil
}

func TestWriteReaderCapEnforced(t *testing.T) {
	dir := t.TempDir()
	dest := filepath.Join(dir, "big.bin")
	src := &endless{}
	_, err := writeReader(dest, src, 1000)
	if !errors.Is(err, errTooLarge) {
		t.Fatalf("err %v, want errTooLarge", err)
	}
	if src.read > 1000+64*1024 {
		t.Fatalf("read %d bytes past the cap", src.read)
	}
	if names := entries(t, dir); len(names) != 0 {
		t.Fatalf("temp file left behind: %v", names)
	}
	if _, err := writeReader(dest, io.LimitReader(&endless{}, 1000), 1000); err != nil {
		t.Fatalf("exactly at the cap must pass: %v", err)
	}
}

type failingReader struct{ sent bool }

func (f *failingReader) Read(p []byte) (int, error) {
	if f.sent {
		return 0, errors.New("connection reset")
	}
	f.sent = true
	return copy(p, "partial"), nil
}

func TestWriteReaderCleansUpOnFailure(t *testing.T) {
	dir := t.TempDir()
	dest := filepath.Join(dir, "x.bin")
	if err := os.WriteFile(dest, []byte("old"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := writeReader(dest, &failingReader{}, 1000); err == nil {
		t.Fatal("expected an error")
	}
	if got, _ := os.ReadFile(dest); string(got) != "old" {
		t.Fatalf("destination changed to %q", got)
	}
	if names := entries(t, dir); len(names) != 1 {
		t.Fatalf("temp file left behind: %v", names)
	}
}

func TestWriteReaderMissingDirectory(t *testing.T) {
	_, err := writeReader(filepath.Join(t.TempDir(), "nope", "x"), strings.NewReader("x"), 10)
	var ae *api.Error
	if !errors.As(err, &ae) || ae.Code != api.CodeIO {
		t.Fatalf("err %v, want io", err)
	}
}

func TestWriteWithCapAndCleanup(t *testing.T) {
	dir := t.TempDir()
	dest := filepath.Join(dir, "e.bin")
	info, err := writeWith(dest, 100, func(f *os.File) error {
		_, err := f.WriteAt([]byte("hello"), 0)
		return err
	})
	if err != nil || info.n != 5 || info.sha != "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824" {
		t.Fatalf("info %+v err %v", info, err)
	}
	if st, _ := os.Stat(dest); st.Mode().Perm() != 0o600 {
		t.Fatalf("mode %v", st.Mode().Perm())
	}
	_, err = writeWith(filepath.Join(dir, "f.bin"), 3, func(f *os.File) error {
		_, err := f.Write([]byte("hello"))
		return err
	})
	if !errors.Is(err, errTooLarge) {
		t.Fatalf("err %v, want errTooLarge", err)
	}
	_, err = writeWith(filepath.Join(dir, "g.bin"), 3, func(*os.File) error { return errors.New("boom") })
	if err == nil {
		t.Fatal("expected fill error")
	}
	if names := entries(t, dir); len(names) != 1 || names[0] != "e.bin" {
		t.Fatalf("files %v", names)
	}
}

func TestClampTo400(t *testing.T) {
	cases := [][4]int{{1000, 500, 400, 200}, {500, 1000, 200, 400}, {300, 200, 300, 200}, {800, 800, 400, 400}, {0, 0, 0, 0}}
	for _, c := range cases {
		if w, h := clampTo400(c[0], c[1]); w != c[2] || h != c[3] {
			t.Errorf("clamp(%d,%d) = %d,%d want %d,%d", c[0], c[1], w, h, c[2], c[3])
		}
	}
}

func encodePNG(t *testing.T, img image.Image) []byte {
	t.Helper()
	var buf bytes.Buffer
	if err := png.Encode(&buf, img); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

func gradient(w, h int) *image.NRGBA {
	img := image.NewNRGBA(image.Rect(0, 0, w, h))
	for y := 0; y < h; y++ {
		for x := 0; x < w; x++ {
			img.SetNRGBA(x, y, color.NRGBA{R: uint8(x * 255 / w), G: uint8(y * 255 / h), B: 128, A: 255})
		}
	}
	return img
}

func TestInspectImageSizes(t *testing.T) {
	data := encodePNG(t, gradient(1000, 500))
	info := inspectImage(data, "image/png")
	if info.width != 1000 || info.height != 500 || !bytes.Equal(info.data, data) {
		t.Fatalf("png info %dx%d unchanged=%v", info.width, info.height, bytes.Equal(info.data, data))
	}
	var buf bytes.Buffer
	if err := jpeg.Encode(&buf, gradient(300, 900), nil); err != nil {
		t.Fatal(err)
	}
	info = inspectImage(buf.Bytes(), "image/jpeg")
	if info.width != 300 || info.height != 900 {
		t.Fatalf("jpeg info %dx%d", info.width, info.height)
	}
	if info = inspectImage([]byte("not an image"), "image/webp"); info.width != 0 || info.height != 0 {
		t.Fatalf("undecodable info %+v", info)
	}
}

func TestInspectImageReencodesPalettedPNG(t *testing.T) {
	pal := image.NewPaletted(image.Rect(0, 0, 50, 40), color.Palette{color.Transparent, color.White, color.RGBA{R: 200, A: 255}})
	for i := range pal.Pix {
		pal.Pix[i] = uint8(i % 3)
	}
	data := encodePNG(t, pal)
	info := inspectImage(data, "image/png")
	if bytes.Equal(info.data, data) {
		t.Fatal("a paletted png must be re-encoded")
	}
	cfg, err := png.DecodeConfig(bytes.NewReader(info.data))
	if err != nil || cfg.ColorModel != color.NRGBAModel {
		t.Fatalf("re-encoded model %v err %v", cfg.ColorModel, err)
	}
	if info.width != 50 || info.height != 40 {
		t.Fatalf("size %dx%d", info.width, info.height)
	}
}

func TestE2EEImageAppRoundTrip(t *testing.T) {
	data := encodePNG(t, gradient(800, 600))
	info := inspectImage(data, "image/png")
	up := whatsmeow.UploadResponse{DirectPath: "/v/x", ObjectID: "obj", MediaKey: []byte{1}, FileEncSHA256: []byte{2}, FileSHA256: []byte{3}}
	app, err := e2eeImageApp(up, info, "image/png")
	if err != nil {
		t.Fatal(err)
	}
	th := app.GetPayload().GetContent().GetImageMessage()
	tr, err := th.Decode()
	if err != nil {
		t.Fatal(err)
	}
	thumb := tr.GetIntegral().GetTransport().GetAncillary().GetThumbnail()
	if thumb.GetThumbnailWidth() != 400 || thumb.GetThumbnailHeight() != 300 || len(thumb.GetJPEGThumbnail()) != 0 {
		t.Fatalf("thumbnail %+v", thumb)
	}
	m := normalizeSent(t, app, nil)
	if m.Kind != api.MsgMedia || len(m.Attachments) != 1 {
		t.Fatalf("message %+v", m)
	}
	a := m.Attachments[0]
	if a.Kind != api.AttImage || a.Mime == nil || *a.Mime != "image/png" || a.Width == nil || *a.Width != 800 || a.Height == nil || *a.Height != 600 {
		t.Fatalf("attachment %+v", a)
	}
}

func TestE2EEImageAppUnknownSizeFallsBackTo400(t *testing.T) {
	app, err := e2eeImageApp(whatsmeow.UploadResponse{}, inspectImage([]byte("x"), "image/webp"), "image/webp")
	if err != nil {
		t.Fatal(err)
	}
	tr, err := app.GetPayload().GetContent().GetImageMessage().Decode()
	if err != nil {
		t.Fatal(err)
	}
	thumb := tr.GetIntegral().GetTransport().GetAncillary().GetThumbnail()
	if thumb.GetThumbnailWidth() != 400 || thumb.GetThumbnailHeight() != 400 {
		t.Fatalf("thumbnail %+v", thumb)
	}
}

func TestResultMimeAndDownloadable(t *testing.T) {
	if resultMime("image/png", nil) != "image/png" {
		t.Fatal("known mime must win")
	}
	if got := resultMime("", encodePNG(t, gradient(4, 4))); got != "image/png" {
		t.Fatalf("sniffed %q", got)
	}
	if got := resultMime("", []byte{0, 1, 2, 3}); got != "" {
		t.Fatalf("unknown bytes gave %q", got)
	}
	u := "https://cdn.example/x"
	for _, a := range []api.Attachment{
		{ID: "0", Kind: api.AttLink, URL: &u},
		{ID: "0", Kind: api.AttLocation, URL: &u},
		{ID: "0", Kind: api.AttImage},
		{ID: "0", Kind: api.AttImage, URL: ptr("http://cdn.example/x")},
	} {
		if err := downloadable(a); err == nil {
			t.Errorf("attachment %+v must not be downloadable", a)
		}
	}
	if err := downloadable(api.Attachment{ID: "0", Kind: api.AttImage, URL: &u}); err != nil {
		t.Fatal(err)
	}
}

func ptr[T any](v T) *T { return &v }

func TestIsExpiredErr(t *testing.T) {
	if !isExpiredErr(mediadl.ErrForbidden) || !isExpiredErr(whatsmeow.ErrMediaDownloadFailedWith410) {
		t.Fatal("403 and 410 must count as expired")
	}
	if isExpiredErr(errors.New("boom")) || isExpiredErr(mediadl.ErrTooLargeFile) {
		t.Fatal("other errors must not count as expired")
	}
}

func TestReadMediaFileCaps(t *testing.T) {
	dir := t.TempDir()
	p := filepath.Join(dir, "a.bin")
	if err := os.WriteFile(p, make([]byte, 100), 0o600); err != nil {
		t.Fatal(err)
	}
	if data, err := readMediaFile(p, 100); err != nil || len(data) != 100 {
		t.Fatalf("at the cap: %v", err)
	}
	_, err := readMediaFile(p, 99)
	var ae *api.Error
	if !errors.As(err, &ae) || ae.Code != api.CodeBadParams || ae.Msg != "file too large for messenger" {
		t.Fatalf("err %v", err)
	}
	if _, err := readMediaFile(dir, 100); err == nil {
		t.Fatal("a directory must be refused")
	}
	if _, err := readMediaFile(filepath.Join(dir, "missing"), 100); err == nil {
		t.Fatal("a missing file must fail")
	}
}

func TestCappedFileRefusesGrowthAndWriteWithCleansUp(t *testing.T) {
	dir := t.TempDir()
	dest := filepath.Join(dir, "e.bin")
	var cf *cappedFile
	_, err := writeWith(dest, 10, func(f *os.File) error {
		cf = newCappedFile(f, 10)
		if _, err := cf.Write([]byte("12345")); err != nil {
			t.Fatalf("write within the cap: %v", err)
		}
		if _, err := cf.WriteAt([]byte("67890"), 5); err != nil {
			t.Fatalf("writeAt up to the cap: %v", err)
		}
		if cf.tripped.Load() {
			t.Fatal("tripped before exceeding the cap")
		}
		if _, err := cf.Seek(0, io.SeekEnd); err != nil {
			t.Fatal(err)
		}
		_, werr := cf.Write([]byte("x"))
		if !errors.Is(werr, errTooLarge) {
			t.Fatalf("write past the cap: %v", werr)
		}
		return werr
	})
	if err == nil || !cf.tripped.Load() {
		t.Fatalf("err %v tripped %v", err, cf.tripped.Load())
	}
	if names := entries(t, dir); len(names) != 0 {
		t.Fatalf("temp file left behind: %v", names)
	}
}

func TestCappedFileLimits(t *testing.T) {
	f, err := os.CreateTemp(t.TempDir(), "c")
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	cf := newCappedFile(f, 8)
	if err := cf.Truncate(8); err != nil {
		t.Fatalf("truncate to the cap: %v", err)
	}
	if err := cf.Truncate(9); !errors.Is(err, errTooLarge) {
		t.Fatalf("truncate past the cap: %v", err)
	}
	if _, err := cf.WriteAt([]byte("ab"), 7); !errors.Is(err, errTooLarge) {
		t.Fatalf("writeAt straddling the cap: %v", err)
	}
	if _, err := cf.WriteAt([]byte("a"), -1); err == nil {
		t.Fatal("negative offset must fail")
	}
	if _, err := cf.Seek(6, io.SeekStart); err != nil {
		t.Fatal(err)
	}
	if _, err := cf.Write([]byte("abc")); !errors.Is(err, errTooLarge) {
		t.Fatalf("write straddling the cap: %v", err)
	}
	if st, _ := cf.Stat(); st.Size() != 8 {
		t.Fatalf("size %d", st.Size())
	}
}

// whatsmeow copies with io.Copy(file, io.TeeReader(body, hasher)); the cap has to hold on that path.
func TestCappedFileHoldsUnderIOCopy(t *testing.T) {
	const limit = 1000
	sources := map[string]func() io.Reader{
		"tee reader as whatsmeow uses": func() io.Reader {
			return io.TeeReader(bytes.NewReader(make([]byte, 1<<20)), sha256.New())
		},
		"bytes reader with WriteTo": func() io.Reader { return bytes.NewReader(make([]byte, 1<<20)) },
		"plain reader":              func() io.Reader { return io.LimitReader(zeroReader{}, 1<<20) },
	}
	for name, src := range sources {
		t.Run(name, func(t *testing.T) {
			f, err := os.CreateTemp(t.TempDir(), "c")
			if err != nil {
				t.Fatal(err)
			}
			defer f.Close()
			cf := newCappedFile(f, limit)
			n, err := io.Copy(cf, src())
			if !errors.Is(err, errTooLarge) || !cf.tripped.Load() {
				t.Fatalf("copied %d bytes, err %v, tripped %v", n, err, cf.tripped.Load())
			}
			if st, _ := f.Stat(); st.Size() > limit {
				t.Fatalf("%d bytes reached the disk, cap is %d", st.Size(), limit)
			}
		})
	}
}

func TestCappedFileAllowsExactlyTheLimitUnderIOCopy(t *testing.T) {
	f, err := os.CreateTemp(t.TempDir(), "c")
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	cf := newCappedFile(f, 4096)
	if n, err := io.Copy(cf, io.TeeReader(bytes.NewReader(make([]byte, 4096)), sha256.New())); err != nil || n != 4096 || cf.tripped.Load() {
		t.Fatalf("n %d err %v tripped %v", n, err, cf.tripped.Load())
	}
}

// Embedding *os.File would promote ReadFrom and WriterTo, which io.Copy prefers over Write.
func TestCappedFileExposesNoFastCopyPaths(t *testing.T) {
	var w any = newCappedFile(nil, 1)
	if _, ok := w.(io.ReaderFrom); ok {
		t.Fatal("cappedFile implements io.ReaderFrom, so io.Copy can bypass Write")
	}
	if _, ok := w.(io.WriterTo); ok {
		t.Fatal("cappedFile implements io.WriterTo")
	}
}

func TestWriteWithStopsAnIOCopyPastTheCapAndCleansUp(t *testing.T) {
	dir := t.TempDir()
	dest := filepath.Join(dir, "big.bin")
	_, err := writeWith(dest, 1000, func(f *os.File) error {
		cf := newCappedFile(f, 1000)
		_, err := io.Copy(cf, io.TeeReader(bytes.NewReader(make([]byte, 1<<20)), sha256.New()))
		if cf.tripped.Load() {
			return errTooLarge
		}
		return err
	})
	if !errors.Is(err, errTooLarge) {
		t.Fatalf("err %v", err)
	}
	if names := entries(t, dir); len(names) != 0 {
		t.Fatalf("files left behind: %v", names)
	}
}

type zeroReader struct{}

func (zeroReader) Read(p []byte) (int, error) { clear(p); return len(p), nil }
