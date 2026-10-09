#!/bin/sh
# Build the monokulo package (.apk) for OpenWrt 25.12 on the GL.iNet Flint 2
# (mediatek/filogic, aarch64_cortex-a53), a signed apk repository holding it,
# and the landing page that explains how to install it.
#
# monokulo, with the engine inside it, is cross-compiled here with cargo. Its C
# parts (SQLite, aws-lc) are compiled with the gcc from the official OpenWrt
# SDK image, so they are built exactly as OpenWrt builds C. The same SDK (in
# Docker) then packages the binary, the init script and the LuCI page,
# signs the packages and the repository index.
#
# Needs: docker, rustup (the nightly rust-toolchain.toml names), Node 24 and
# npm (monokulo's build script builds the POS app), openssl.
#
# Output:
#   dist/          the package, the repository index and the public key,
#                  for installing by hand (CI uploads these as an artifact)
#   site/          the landing page with the package repository under it,
#                  ready for GitHub Pages
#
# Signing key (must match openwrt/keys/monokulo.pem), first found wins:
#   $APK_SIGNING_KEY       the PEM itself (how CI passes the secret)
#   $MONOKULO_SIGNING_KEY  path to the PEM
#   ~/.config/kringle/apk-signing-key.pem   (the same key signs kringle)
# Without one the packages are signed with a throwaway key: fine for testing,
# but routers that trust monokulo.pem will refuse them.
set -eu
cd "$(dirname "$0")/.."

SDK_IMAGE="${SDK_IMAGE:-openwrt/sdk:mediatek-filogic-25.12.5}"
RELEASE=25.12
ARCH=aarch64_cortex-a53
TARGET=aarch64-unknown-linux-musl
PUBKEY=openwrt/keys/monokulo.pem
PKG=openwrt/monokulo

# Where the repository is published, e.g. https://owner.github.io/monokulo
remote=$(git remote get-url origin 2>/dev/null || true)
slug=${GITHUB_REPOSITORY:-$(printf '%s' "$remote" | sed -E 's#^(git@github\.com:|https://github\.com/)##; s#\.git$##')}
REPO_URL="${REPO_URL:-https://github.com/$slug}"
if [ -z "${PAGES_URL:-}" ]; then
	owner=$(printf '%s' "${slug%%/*}" | tr '[:upper:]' '[:lower:]')
	PAGES_URL="https://$owner.github.io/${slug#*/}"
fi

# The package version is the crate's; the release number is the commit
# count, so every build from main upgrades the one before it.
VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' crates/monokulo/Cargo.toml | head -n 1)
PKG_RELEASE=$(git rev-list --count HEAD 2>/dev/null || echo 1)

key="${APK_SIGNING_KEY:-}"
if [ -z "$key" ]; then
	keyfile="${MONOKULO_SIGNING_KEY:-$HOME/.config/kringle/apk-signing-key.pem}"
	[ -f "$keyfile" ] && key=$(cat "$keyfile")
fi
if [ -n "$key" ]; then
	if ! printf '%s\n' "$key" | openssl ec -pubout 2>/dev/null | cmp -s - "$PUBKEY"; then
		echo "error: the signing key doesn't match $PUBKEY" >&2
		exit 1
	fi
else
	echo "warning: no signing key, so the packages are signed with a throwaway key" >&2
fi

# The SDK's cross toolchain, copied out of its image once per image tag.
# Kept outside target/ (about 900 MB), so CI's Rust cache doesn't carry it.
tag=${SDK_IMAGE##*:}
toolchain_root="${XDG_CACHE_HOME:-$HOME/.cache}/monokulo/openwrt-sdk/$tag"
if [ ! -d "$toolchain_root/staging_dir" ]; then
	rm -rf "$toolchain_root"
	mkdir -p "$toolchain_root"
	container=$(docker create "$SDK_IMAGE")
	trap 'docker rm -f "$container" >/dev/null 2>&1 || true' EXIT
	docker cp "$container:/builder/staging_dir" "$toolchain_root/staging_dir.partial" >/dev/null
	docker rm -f "$container" >/dev/null
	trap - EXIT
	mv "$toolchain_root/staging_dir.partial" "$toolchain_root/staging_dir"
fi
toolchain=$(ls -d "$toolchain_root"/staging_dir/toolchain-aarch64_cortex-a53_gcc-*_musl | head -n 1)
[ -x "$toolchain/bin/aarch64-openwrt-linux-musl-gcc" ] || {
	echo "error: no aarch64 musl gcc in $SDK_IMAGE" >&2
	exit 1
}

# monokulo's build script builds the POS app from its locked dependencies.
[ -d crates/monokulo/pos-ui/node_modules ] ||
	npm ci --prefix crates/monokulo/pos-ui --no-audit --no-fund

# randomx-rs (the engine's proof-of-work check) asks for C++'s standard
# library as a shared library, which would make monokulo a dynamic
# executable that OpenWrt can't run. A search directory holding only the
# toolchain's static libstdc++, searched first, makes -lstdc++ resolve to it.
static_cxx="$toolchain_root/static-cxx"
mkdir -p "$static_cxx"
ln -sf "$toolchain/lib/libstdc++.a" "$static_cxx/libstdc++.a"

rustup target add "$TARGET"
(
	export STAGING_DIR="$toolchain_root/staging_dir"
	export PATH="$toolchain/bin:$PATH"
	export CC_aarch64_unknown_linux_musl=aarch64-openwrt-linux-musl-gcc
	export CXX_aarch64_unknown_linux_musl=aarch64-openwrt-linux-musl-g++
	export AR_aarch64_unknown_linux_musl=aarch64-openwrt-linux-musl-ar
	export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-openwrt-linux-musl-gcc
	# RandomX also calls libgcc's __clear_cache after writing JIT code; Rust's
	# musl target links no libgcc of its own, so link the toolchain's static
	# one. And libc once more after both, for what libstdc++ needs from it
	# (the static link resolves left to right).
	export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-L native=$static_cxx -C link-arg=-lgcc -C link-arg=-lc"
	export CARGO_PROFILE_RELEASE_STRIP=symbols
	# The engine is built into monokulo (its default `embedded-engine`
	# feature), so there is one binary to ship, with the engine's default
	# `zmq` feature: a node's ZMQ announcements wake the scan at once
	# (docs/monero_zmq.md).
	cargo build --release --locked --target "$TARGET" \
		-p monokulo --bin monokulo
)
if ! file "target/$TARGET/release/monokulo" | grep -q 'statically linked'; then
	echo "error: monokulo isn't a static executable, so it won't run on OpenWrt:" >&2
	file "target/$TARGET/release/monokulo" >&2
	exit 1
fi
install -m 0755 "target/$TARGET/release/monokulo" "$PKG/files/monokulo"

rm -rf dist site
mkdir -p dist/repo
chmod 0777 dist dist/repo # the SDK runs as its own user

# The SDK's working tree lives in a Docker volume so it is set up once, not
# on every run. SDK_VOLUME= (empty) for a clean, throwaway SDK; CI does that
# since each job starts fresh anyway.
SDK_VOLUME="${SDK_VOLUME-monokulo-sdk-$tag}"
set -- -v "$PWD/openwrt:/feed:ro" -v "$PWD/dist:/dist"
[ -n "$SDK_VOLUME" ] && set -- "$@" -v "$SDK_VOLUME:/builder"

# The key reaches the container through the environment (-e APK_KEY without a
# value), never on docker's command line where other local users could see it.
APK_KEY="$key" docker run --rm -e APK_KEY \
	-e MONOKULO_VERSION="$VERSION" -e MONOKULO_RELEASE="$PKG_RELEASE" \
	"$@" "$SDK_IMAGE" sh -euc '
	[ -x ./setup.sh ] && [ ! -f rules.mk ] && ./setup.sh

	# Remove the private key from the (possibly persistent) SDK tree on exit.
	trap "rm -f private-key.pem" EXIT
	rm -f private-key.pem public-key.pem
	if [ -n "$APK_KEY" ]; then
		(umask 077 && printf "%s\n" "$APK_KEY" > private-key.pem)
		openssl ec -in private-key.pem -pubout -out public-key.pem 2>/dev/null
	fi

	# Only our own feed: the package needs nothing built from the others, so
	# the build never waits on git.openwrt.org.
	grep -q "^src-link monokulo " feeds.conf.default || echo "src-link monokulo /feed" >> feeds.conf.default
	./scripts/feeds update monokulo
	./scripts/feeds install monokulo
	make defconfig
	# Rebuild ours from scratch each time; drop earlier versions from the repository.
	rm -rf bin/packages/*/monokulo build_dir/target-*/monokulo-*
	make package/monokulo/compile -j"$(nproc)" \
		MONOKULO_VERSION="$MONOKULO_VERSION" MONOKULO_RELEASE="$MONOKULO_RELEASE"
	# The SDK builds packages unsigned (only the index is signed). Sign them too,
	# so a downloaded .apk installs directly (apk add, or LuCI Upload Package)
	# on any router that trusts monokulo.pem.
	if [ -n "$APK_KEY" ]; then
		mkdir -p /tmp/monokulo-keys
		cp public-key.pem /tmp/monokulo-keys/monokulo.pem
		for f in bin/packages/*/monokulo/*.apk; do
			staging_dir/host/bin/apk --allow-untrusted adbsign --sign-key private-key.pem "$f"
			staging_dir/host/bin/apk --keys-dir /tmp/monokulo-keys verify "$f"
		done
	fi
	make package/index
	cp bin/packages/*/monokulo/* /dist/repo/
'

apk=$(basename "$(ls dist/repo/monokulo-*.apk)")
cp "dist/repo/$apk" dist/repo/packages.adb dist/
cp "$PUBKEY" dist/monokulo.pem
(cd dist && sha256sum "$apk" packages.adb monokulo.pem > SHA256SUMS)

# The landing page, its assets (monokulo's own theme, fonts and logo) and the
# package repository.
repo_path="openwrt/$RELEASE/$ARCH"
repo="site/$repo_path"
mkdir -p "$repo" site/assets
cp dist/repo/* "$repo/"
cp "$PUBKEY" site/monokulo.pem
cp crates/monokulo/src/views/theme.css site/assets/theme.css
cp crates/monokulo/static/logo.svg crates/monokulo/static/favicon.svg \
	crates/monokulo/static/manrope-500.woff2 crates/monokulo/static/manrope-700.woff2 \
	crates/monokulo/static/manrope-800.woff2 site/assets/
size=$(du -k "$repo/$apk" | cut -f1)
awk -v key="$(cat "$PUBKEY")" '{ gsub(/@PUBKEY@/, key); print }' web/index.html |
	sed -e "s#@PAGES_URL@#$PAGES_URL#g" -e "s#@REPO_PATH@#$repo_path#g" -e "s#@REPO_URL@#$REPO_URL#g" \
		-e "s#@APK@#$apk#g" -e "s#@APK_SIZE@#$((size / 1024)) MB#g" -e "s#@VERSION@#$VERSION-r$PKG_RELEASE#g" \
	> site/index.html
touch site/.nojekyll

ls -l dist "$repo"
