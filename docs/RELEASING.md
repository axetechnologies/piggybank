# Releasing piggybank

## Prerequisites

- **npm trusted publishing**: no token or secret is needed.  On npmjs.com open
  `piggybank-mcp → Settings → Trusted Publisher` and add a GitHub Actions
  publisher with organization `axetechnologies`, repository `piggybank`,
  workflow filename `release.yml`, environment left blank.  The
  `publish-npm` job authenticates with GitHub's OIDC token (`id-token: write`)
  and npm >= 11.5.1, which the job installs.

## Release checklist

1. **Bump versions** — update all three crate versions and `npm/package.json`
   to the same value in a single PR:

   ```bash
   # Cargo.toml files
   sed -i '' 's/^version = ".*"/version = "0.3.0"/' \
       crates/piggybank-core/Cargo.toml \
       crates/piggybank-cli/Cargo.toml \
       crates/axelrod-cli/Cargo.toml

   # npm wrapper
   node -e "
     const p = require('./npm/package.json');
     p.version = '0.3.0';
     require('fs').writeFileSync('./npm/package.json', JSON.stringify(p, null, 2) + '\n');
   "
   ```

   CI enforces that these match via the `version-check` job in `ci.yml`.

2. **Verify** — push the branch, confirm `version-check` passes.

3. **Merge** the version-bump PR to `main`.

4. **Tag** on `main`:

   ```bash
   git tag v0.3.0
   git push origin v0.3.0
   ```

5. **CI publishes automatically** — `release.yml` triggers on the tag and:
   - Builds binaries for all five targets (macOS arm64, macOS x64, Linux musl
     x64, Linux musl arm64, Windows x64).
   - Packages each binary as a `.tar.gz` (or `.zip` on Windows) with a SHA-256
     checksum file.
   - Creates a GitHub Release with all assets and auto-generated release notes.
   - Verifies `npm/package.json` version equals the tag, then publishes to npm
     with provenance.

## What you must do before the first release

| Step | Where |
|---|---|
| Add GitHub Actions trusted publisher (`axetechnologies/piggybank`, `release.yml`) | npmjs.com → piggybank-mcp → Settings → Trusted Publisher |
| Confirm `id-token: write` permission is set | already in `release.yml` |

## Targets

| Target | OS | Archive |
|---|---|---|
| `aarch64-apple-darwin` | macOS (Apple Silicon) | `.tar.gz` |
| `x86_64-apple-darwin` | macOS (Intel) | `.tar.gz` |
| `x86_64-unknown-linux-musl` | Linux x64 (static) | `.tar.gz` |
| `aarch64-unknown-linux-musl` | Linux arm64 (static) | `.tar.gz` |
| `x86_64-pc-windows-msvc` | Windows x64 | `.zip` |

## Notes

- Never push to `main` directly.  Tag only from the merged version-bump PR.
- The npm `postinstall` script in `npm/bin/install.js` downloads the binary for
  the matching tag (`v<version>`) and falls back to `latest` with a warning if
  the exact release is not yet published.
- SHA-256 checksums are uploaded alongside each binary for verification.
