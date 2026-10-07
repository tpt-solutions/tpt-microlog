package main

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"crypto/sha256"
	"encoding/hex"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

type member struct {
	name string
	body string
	kind byte
}

func makeArchive(t *testing.T, members ...member) []byte {
	t.Helper()
	var buf bytes.Buffer
	gz := gzip.NewWriter(&buf)
	tw := tar.NewWriter(gz)
	for _, m := range members {
		kind := m.kind
		if kind == 0 {
			kind = tar.TypeReg
		}
		hdr := &tar.Header{Name: m.name, Mode: 0o644, Size: int64(len(m.body)), Typeflag: kind}
		if kind == tar.TypeSymlink {
			hdr.Linkname = "/etc/passwd"
			hdr.Size = 0
		}
		if err := tw.WriteHeader(hdr); err != nil {
			t.Fatal(err)
		}
		if kind == tar.TypeReg {
			if _, err := tw.Write([]byte(m.body)); err != nil {
				t.Fatal(err)
			}
		}
	}
	tw.Close()
	gz.Close()
	return buf.Bytes()
}

func digest(b []byte) string {
	sum := sha256.Sum256(b)
	return hex.EncodeToString(sum[:])
}

// release serves one asset (and its checksum) the way GitHub would.
func release(t *testing.T, goos, goarch string, archive []byte, checksum string) *httptest.Server {
	t.Helper()
	asset := assetName(goos, goarch)
	mux := http.NewServeMux()
	mux.HandleFunc("/dl/"+asset, func(w http.ResponseWriter, r *http.Request) { w.Write(archive) })
	mux.HandleFunc("/dl/"+asset+".sha256", func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(checksum))
	})
	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)
	return srv
}

func TestFetchInstallsAndVerifies(t *testing.T) {
	archive := makeArchive(t,
		member{name: "libtpt_log_ffi-linux_amd64/libtpt_log_ffi.a", body: "!<arch>\nfake static lib"},
		member{name: "libtpt_log_ffi-linux_amd64/tpt_microlog.h", body: "/* header */"},
		member{name: "README.txt", body: "ignored"},
	)
	srv := release(t, "linux", "amd64", archive, digest(archive)+"  "+assetName("linux", "amd64")+"\n")
	dest := filepath.Join(t.TempDir(), "lib", "linux_amd64")
	err := fetch(options{baseURL: srv.URL + "/dl/", goos: "linux", goarch: "amd64", dest: dest})
	if err != nil {
		t.Fatalf("fetch: %v", err)
	}
	lib, err := os.ReadFile(filepath.Join(dest, "libtpt_log_ffi.a"))
	if err != nil || string(lib) != "!<arch>\nfake static lib" {
		t.Fatalf("library not installed correctly: %q %v", lib, err)
	}
	if _, err := os.Stat(filepath.Join(dest, "tpt_microlog.h")); err != nil {
		t.Fatalf("header missing: %v", err)
	}
	if _, err := os.Stat(filepath.Join(dest, "README.txt")); err == nil {
		t.Fatal("unexpected member extracted")
	}
	entries, _ := os.ReadDir(dest)
	for _, e := range entries {
		if strings.HasSuffix(e.Name(), ".tmp") {
			t.Fatalf("temp file left behind: %s", e.Name())
		}
	}
}

func TestChecksumMismatchRefusesToInstall(t *testing.T) {
	archive := makeArchive(t, member{name: "libtpt_log_ffi.a", body: "payload"})
	wrong := strings.Repeat("0", 64)
	srv := release(t, "darwin", "arm64", archive, wrong)
	dest := t.TempDir()
	err := fetch(options{baseURL: srv.URL + "/dl", goos: "darwin", goarch: "arm64", dest: dest})
	if err == nil || !strings.Contains(err.Error(), "checksum mismatch") {
		t.Fatalf("want a checksum mismatch, got %v", err)
	}
	if _, statErr := os.Stat(filepath.Join(dest, "libtpt_log_ffi.a")); statErr == nil {
		t.Fatal("a library failing verification must not be installed")
	}
}

func TestArchiveHardening(t *testing.T) {
	cases := map[string][]member{
		"no library":        {member{name: "tpt_microlog.h", body: "x"}},
		"only a symlink":    {member{name: "libtpt_log_ffi.a", kind: tar.TypeSymlink}},
		"unrelated members": {member{name: "../../evil.sh", body: "rm -rf /"}},
	}
	for name, members := range cases {
		t.Run(name, func(t *testing.T) {
			archive := makeArchive(t, members...)
			srv := release(t, "linux", "arm64", archive, digest(archive))
			root := t.TempDir()
			dest := filepath.Join(root, "dest")
			if err := fetch(options{baseURL: srv.URL + "/dl", goos: "linux", goarch: "arm64", dest: dest}); err == nil {
				t.Fatal("expected an error")
			}
			if _, err := os.Stat(filepath.Join(root, "evil.sh")); err == nil {
				t.Fatal("path traversal succeeded")
			}
		})
	}
}

func TestTraversalNamesAreFlattenedToBaseNames(t *testing.T) {
	// A traversal-looking path for an expected name only ever lands in dest.
	archive := makeArchive(t, member{name: "../../outside/libtpt_log_ffi.a", body: "lib"})
	srv := release(t, "windows", "amd64", archive, digest(archive))
	root := t.TempDir()
	dest := filepath.Join(root, "a", "b")
	if err := fetch(options{baseURL: srv.URL + "/dl", goos: "windows", goarch: "amd64", dest: dest}); err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if _, err := os.Stat(filepath.Join(dest, "libtpt_log_ffi.a")); err != nil {
		t.Fatalf("not installed in dest: %v", err)
	}
	if _, err := os.Stat(filepath.Join(root, "outside")); err == nil {
		t.Fatal("wrote outside dest")
	}
}

func TestUnsupportedPlatformAndHTTPErrors(t *testing.T) {
	if err := fetch(options{baseURL: "http://127.0.0.1:1", goos: "plan9", goarch: "386", dest: t.TempDir()}); err == nil || !strings.Contains(err.Error(), "no prebuilt library") {
		t.Fatalf("want an unsupported-platform error, got %v", err)
	}
	srv := httptest.NewServer(http.NotFoundHandler())
	defer srv.Close()
	if err := fetch(options{baseURL: srv.URL, goos: "linux", goarch: "amd64", dest: t.TempDir()}); err == nil || !strings.Contains(err.Error(), "404") {
		t.Fatalf("want a 404 error, got %v", err)
	}
}

func TestParseDigest(t *testing.T) {
	good := strings.Repeat("ab", 32)
	for _, raw := range []string{good, good + "  file.tar.gz\n", strings.ToUpper(good)} {
		if d, err := parseDigest([]byte(raw)); err != nil || d != good {
			t.Fatalf("parseDigest(%q) = %q, %v", raw, d, err)
		}
	}
	for _, raw := range []string{"", "zz", strings.Repeat("g", 64), good[:62]} {
		if _, err := parseDigest([]byte(raw)); err == nil {
			t.Fatalf("parseDigest(%q) should fail", raw)
		}
	}
}
