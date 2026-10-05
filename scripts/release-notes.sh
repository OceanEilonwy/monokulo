#!/usr/bin/env bash
# The top of a GitHub release's notes: which file to download for which
# computer, and how to start it. CI's release jobs call it.
#
# Usage: scripts/release-notes.sh TAG VERSION
#   TAG      the release's tag (v1.2.3, or latest-main)
#   VERSION  the version in its file names (1.2.3, or main)
# Needs GITHUB_REPOSITORY (owner/name), as GitHub Actions sets it.
set -euo pipefail

tag=$1
version=$2
base="https://github.com/${GITHUB_REPOSITORY}/releases/download/${tag}"

row() {
	local file="monokulo-${version}-$2.zip"
	echo "| $1 | [\`$file\`]($base/$file) ([SHA-256]($base/$file.sha256)) |"
}

cat <<EOF
## Download

monokulo is one executable with the engine inside it. Pick the file for your
computer:

| Computer | File |
| --- | --- |
$(row 'Windows (x86_64)' x86_64-pc-windows-msvc)
$(row 'macOS (Apple silicon)' aarch64-apple-darwin)
$(row 'macOS (Intel)' x86_64-apple-darwin)
$(row 'Linux (x86_64)' x86_64-unknown-linux-gnu)
$(row 'Linux (ARM64)' aarch64-unknown-linux-gnu)

Unzip it and start monokulo with a key of your own for its database
(64 hex characters; \`openssl rand -hex 32\` makes one, and keep it):

\`\`\`sh
MONOKULO_ENCRYPTION_KEY=<64 hex chars> ./monokulo
\`\`\`

On Windows (PowerShell):

\`\`\`powershell
\$env:MONOKULO_ENCRYPTION_KEY = "<64 hex chars>"; .\\monokulo.exe
\`\`\`

Then open http://127.0.0.1:8081 to create the admin account. The README in
the zip has the rest.

- **macOS**: the binary isn't signed by Apple, so macOS refuses one
  downloaded in a browser. Clear that once with
  \`xattr -d com.apple.quarantine monokulo\`.
- **Windows**: the binary isn't signed, so SmartScreen may ask first
  (More info > Run anyway). It needs the Microsoft Visual C++ runtime,
  which nearly every Windows machine already has.
- **Linux**: built against glibc 2.35 (Ubuntu 22.04) and runs on
  distributions at least that new.

\`key-custody-cli\` (encrypting keys for an SEV-SNP engine without a browser)
is attached too, for each of the same computers.
EOF
