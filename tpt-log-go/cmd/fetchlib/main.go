// Command fetchlib downloads the prebuilt tpt-microlog static library for the
// current platform from a GitHub release, verifies its SHA-256 and unpacks it.
//
//	go run github.com/tpt-solutions/tpt-microlog/tpt-log-go/cmd/fetchlib@v0.1.0 -version v0.1.0
//
// It then prints the CGO_LDFLAGS to use. Without a prebuilt library for your
// platform, build one with `cargo build --release -p tpt-log-ffi`.
package main

import (
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
)

func main() {
	var o options
	var version, repo string
	flag.StringVar(&version, "version", "", "release tag to fetch, e.g. v0.1.0 (required unless -base-url is set)")
	flag.StringVar(&repo, "repo", "tpt-solutions/tpt-microlog", "GitHub repository (owner/name)")
	flag.StringVar(&o.baseURL, "base-url", "", "directory URL holding the assets (overrides -repo/-version; for mirrors)")
	flag.StringVar(&o.goos, "os", runtime.GOOS, "target GOOS")
	flag.StringVar(&o.goarch, "arch", runtime.GOARCH, "target GOARCH")
	flag.StringVar(&o.dest, "dest", "", "destination directory (default ./tpt-lib/<os>_<arch>)")
	flag.Parse()

	if o.baseURL == "" {
		if version == "" {
			fmt.Fprintln(os.Stderr, "fetchlib: -version is required (for example -version v0.1.0)")
			os.Exit(2)
		}
		o.baseURL = fmt.Sprintf("https://github.com/%s/releases/download/%s", repo, version)
	}
	if o.dest == "" {
		o.dest = filepath.Join("tpt-lib", o.goos+"_"+o.goarch)
	}
	if err := fetch(o); err != nil {
		fmt.Fprintln(os.Stderr, "fetchlib:", err)
		os.Exit(1)
	}
	abs, err := filepath.Abs(o.dest)
	if err != nil {
		abs = o.dest
	}
	fmt.Printf("installed %s into %s\n\nBuild with:\n  export CGO_LDFLAGS=\"-L%s\"\n", libName, abs, filepath.ToSlash(abs))
}
