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

link() {
	local file="$1-${version}-$2.zip"
	echo "[\`$file\`]($base/$file) ([SHA-256]($base/$file.sha256))"
}

row() {
	echo "| $1 | $(link monokulo "$2") |"
}

# The push relay's and the Bell's zips, side by side.
services() {
	echo "| $1 | $(link monokulo-push-relay "$2") | $(link monokulo-bell "$2") |"
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

Then open http://127.0.0.1:8081 to create the admin account.

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

## The push relay and the Monokular Bell

Two services that run without monokulo, each its own zip for the same
computers: \`monokulo-push-relay\` hands a till app's pushes on to APNs and
FCM ([the push relay](https://docs.monokulo.com/push-relay/)), and
\`monokulo-bell\` wakes Monokular tills at a Square or Clover counter when a
payment lands ([the Monokular Bell](https://docs.monokulo.com/bell/)). Each
zip holds the binary, its README, a starter options file and service files
for systemd, launchd and a Windows startup task.

| Computer | Push relay | Bell |
| --- | --- | --- |
$(services 'Windows (x86_64)' x86_64-pc-windows-msvc)
$(services 'macOS (Apple silicon)' aarch64-apple-darwin)
$(services 'macOS (Intel)' x86_64-apple-darwin)
$(services 'Linux (x86_64)' x86_64-unknown-linux-gnu)
$(services 'Linux (ARM64)' aarch64-unknown-linux-gnu)
EOF
