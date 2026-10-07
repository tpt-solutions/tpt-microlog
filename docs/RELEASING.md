# Releasing

How a version is cut. Nothing here publishes to crates.io, PyPI or npm: those
are separate, deliberately manual workflows (`publish-py.yml`,
`publish-npm.yml`).

## 1. Prepare (local, reversible)

```sh
python scripts/release.py 0.2.0 --dry-run    # see what would change
python scripts/release.py 0.2.0              # do it
git diff                                      # review
cargo test --workspace --exclude tpt-log-py
```

The script bumps, in one pass: `[workspace.package]` version, the `version = ".."`
on the `tpt-log-core`/`tpt-log-transport` path dependencies, `package.json`, the
Python `__version__`, the Go README's fetch example and `Cargo.lock`; and it
rolls every `CHANGELOG.md`'s `## [Unreleased]` into `## [0.2.0] - <date>`. It
refuses a version that is not newer, drifted versions, and an empty root
changelog. It never commits, tags or pushes. `python scripts/release.py --check`
(run in CI) fails when any version disagrees.

## 2. Tag (you push; the workflow builds)

```sh
git commit -am "release: v0.2.0"
git tag v0.2.0
git tag tpt-log-go/v0.2.0        # the Go module lives in tpt-log-go/, so it needs its own tag
git push origin master v0.2.0 tpt-log-go/v0.2.0
```

Pushing `v0.2.0` runs [`release.yml`](https://github.com/tpt-solutions/tpt-microlog/blob/master/.github/workflows/release.yml), which

1. checks that the tag equals the workspace version and extracts the release
   notes from the changelog,
2. builds the `tpt-microlog` CLI and `tpt-microlog-agent` for Linux
   (x86_64, aarch64), macOS (x86_64, arm64) and Windows (x86_64),
3. builds the static library `libtpt_log_ffi.a` for the same platforms, named
   `libtpt_log_ffi-<goos>_<goarch>.tar.gz` with a `.sha256`: the assets the Go
   fetcher (`tpt-log-go/cmd/fetchlib`) downloads (Windows is built with the
   MinGW target because cgo uses MinGW),
4. generates CycloneDX SBOMs for the shipped crates,
5. signs every archive and SBOM with Sigstore (keyless `cosign sign-blob`),
   adds a build-provenance attestation, and
6. creates a **draft** GitHub release. You read it and press *Publish*.

Run the workflow manually with *dry_run* checked to build everything without
signing or creating a release.

## 3. Verify an artifact

```sh
# Sigstore bundle published next to each artifact
cosign verify-blob tpt-microlog-0.2.0-linux_amd64.tar.gz \
  --bundle tpt-microlog-0.2.0-linux_amd64.tar.gz.sigstore.json \
  --certificate-identity-regexp 'https://github.com/tpt-solutions/tpt-microlog/' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com

# GitHub build-provenance attestation
gh attestation verify tpt-microlog-0.2.0-linux_amd64.tar.gz --repo tpt-solutions/tpt-microlog
sha256sum -c tpt-microlog-0.2.0-linux_amd64.tar.gz.sha256
```

## 4. Registries (manual, when you decide to)

* **Go** needs nothing beyond the two tags: `go get
  github.com/tpt-solutions/tpt-microlog/tpt-log-go@v0.2.0` resolves through the
  `tpt-log-go/v0.2.0` tag, and users fetch the native library from the release.
* **crates.io, PyPI, npm**: trigger the existing publish workflows yourself.

> **Status.** `scripts/release.py` and its tests, the Go fetcher (including an
> end-to-end run against a local server) and the version checks are verified.
> `release.yml` has not been run on GitHub: the cross-compilation steps, the
> `cargo-cyclonedx` invocation and the signing/attestation actions are written
> against their documentation. Use the dry-run mode first.
