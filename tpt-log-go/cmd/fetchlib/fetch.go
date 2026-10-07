package main

import (
	"archive/tar"
	"compress/gzip"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
	"net/http"
	"os"
	"path"
	"path/filepath"
	"strings"
	"time"
)

// Release assets are named libtpt_log_ffi-<goos>_<goarch>.tar.gz, accompanied
// by <asset>.sha256 ("<hex>  <name>" or just the hex digest).
const (
	libName    = "libtpt_log_ffi.a"
	headerName = "tpt_microlog.h"

	maxDownload = 256 << 20 // refuse absurdly large downloads
	maxMember   = 512 << 20 // refuse tar members that inflate beyond this
)

// supported lists the platforms the release workflow builds.
var supported = map[string]bool{
	"linux_amd64":   true,
	"linux_arm64":   true,
	"darwin_amd64":  true,
	"darwin_arm64":  true,
	"windows_amd64": true,
}

// assetName is the archive file name for a platform.
func assetName(goos, goarch string) string {
	return fmt.Sprintf("libtpt_log_ffi-%s_%s.tar.gz", goos, goarch)
}

// options configures a fetch.
type options struct {
	baseURL string // directory URL holding the asset and its .sha256
	goos    string
	goarch  string
	dest    string // directory that receives libtpt_log_ffi.a (and the header)
	client  *http.Client
}

func download(client *http.Client, url string) ([]byte, error) {
	resp, err := client.Get(url)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("GET %s: %s", url, resp.Status)
	}
	data, err := io.ReadAll(io.LimitReader(resp.Body, maxDownload+1))
	if err != nil {
		return nil, fmt.Errorf("GET %s: %w", url, err)
	}
	if len(data) > maxDownload {
		return nil, fmt.Errorf("GET %s: larger than %d bytes", url, maxDownload)
	}
	return data, nil
}

// parseDigest accepts "<hex>" or "<hex>  <name>" (sha256sum output).
func parseDigest(raw []byte) (string, error) {
	fields := strings.Fields(string(raw))
	if len(fields) == 0 {
		return "", fmt.Errorf("empty checksum file")
	}
	digest := strings.ToLower(fields[0])
	if len(digest) != 64 {
		return "", fmt.Errorf("checksum %q is not a SHA-256 hex digest", fields[0])
	}
	if _, err := hex.DecodeString(digest); err != nil {
		return "", fmt.Errorf("checksum %q is not hex: %w", fields[0], err)
	}
	return digest, nil
}

// extract writes the library (required) and header (optional) from a .tar.gz
// into dest. Only those two exact base names are accepted, so a hostile or
// corrupt archive cannot write anywhere else.
func extract(archive []byte, dest string) error {
	gz, err := gzip.NewReader(strings.NewReader(string(archive)))
	if err != nil {
		return fmt.Errorf("not a gzip archive: %w", err)
	}
	defer gz.Close()
	tr := tar.NewReader(gz)
	if err := os.MkdirAll(dest, 0o755); err != nil {
		return err
	}
	foundLib := false
	for {
		hdr, err := tr.Next()
		if err == io.EOF {
			break
		}
		if err != nil {
			return fmt.Errorf("corrupt archive: %w", err)
		}
		if hdr.Typeflag != tar.TypeReg {
			continue
		}
		name := path.Base(hdr.Name)
		if name != libName && name != headerName {
			continue
		}
		if hdr.Size > maxMember {
			return fmt.Errorf("%s is unreasonably large (%d bytes)", name, hdr.Size)
		}
		tmp, err := os.CreateTemp(dest, name+".*.tmp")
		if err != nil {
			return err
		}
		_, copyErr := io.Copy(tmp, io.LimitReader(tr, maxMember))
		closeErr := tmp.Close()
		if copyErr != nil || closeErr != nil {
			os.Remove(tmp.Name())
			return fmt.Errorf("writing %s: %v %v", name, copyErr, closeErr)
		}
		if err := os.Rename(tmp.Name(), filepath.Join(dest, name)); err != nil {
			os.Remove(tmp.Name())
			return err
		}
		if name == libName {
			foundLib = true
		}
	}
	if !foundLib {
		return fmt.Errorf("archive does not contain %s", libName)
	}
	return nil
}

// fetch downloads, verifies and unpacks the prebuilt library.
func fetch(o options) error {
	key := o.goos + "_" + o.goarch
	if !supported[key] {
		return fmt.Errorf("no prebuilt library for %s (supported: linux/darwin amd64+arm64, windows amd64); build it with `cargo build --release -p tpt-log-ffi`", key)
	}
	if o.client == nil {
		o.client = &http.Client{Timeout: 5 * time.Minute}
	}
	asset := assetName(o.goos, o.goarch)
	base := strings.TrimRight(o.baseURL, "/")

	sumRaw, err := download(o.client, base+"/"+asset+".sha256")
	if err != nil {
		return fmt.Errorf("fetching checksum: %w", err)
	}
	want, err := parseDigest(sumRaw)
	if err != nil {
		return err
	}
	archive, err := download(o.client, base+"/"+asset)
	if err != nil {
		return fmt.Errorf("fetching library: %w", err)
	}
	sum := sha256.Sum256(archive)
	if got := hex.EncodeToString(sum[:]); got != want {
		return fmt.Errorf("checksum mismatch for %s: expected %s, got %s (refusing to install)", asset, want, got)
	}
	return extract(archive, o.dest)
}
