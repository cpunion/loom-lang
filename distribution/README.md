# Development releases

CI already builds and tests relocatable macOS, Linux and Windows archives.
Promote those exact bytes instead of rebuilding them for a release:

```sh
node scripts/draft-toolchain-release.mjs MAIN_CI_RUN v0.1.0-dev.1 --dry-run
node scripts/draft-toolchain-release.mjs MAIN_CI_RUN v0.1.0-dev.1
```

The GitHub CLI must be authenticated. The first command only reads GitHub and
checks downloaded archives in a temporary directory. The second creates a
**draft prerelease**, never a public release. Choose the version explicitly;
the example does not reserve or publish that tag. An existing release is not
updated or overwritten. Review and publish the draft separately.

The manual `draft-development-release` workflow exposes the same inputs and
defaults to dry-run. It runs only on this repository's `main`. Inputs must
identify a successful `compiler-ci` run on `main`, with all three host jobs and
unexpired toolchain artifacts. Archive hashes are recomputed, and each included
`BUILD_INFO.json` must match the CI commit, OS and architecture. No downloaded
compiler is executed. This verifies artifact provenance within trusted GitHub
CI; it is not a signed deployment receipt or a proof of program correctness.

See [installation](INSTALL.md) for host dependencies and side-by-side extraction.
LLVM 22 shared libraries and the Clang/linker/SDK remain external. This workflow
does not make the compiler self-contained, complete the language design, or
automatically install or upgrade anyone's toolchain.
