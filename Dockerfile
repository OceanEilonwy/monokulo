# Monokulo for production: one image holding the engine (`scanner`), the
# control plane (`monokulo`) and the optional key storage service
# (`key-custody-server`). Run the engine and monokulo as two containers from
# it, as compose.yaml does; `docker build -t monokulo .` builds it.
#
# The default command runs monokulo; `scanner` and `key-custody-server` are
# on the PATH for the other containers. Both keep their SQLite databases in
# /var/lib/monokulo, the image's one volume.

# The base image only supplies rustup: the build installs the latest nightly,
# as rust-toolchain.toml names it.
ARG RUST_VERSION=1
ARG NODE_VERSION=24
ARG DEBIAN_VERSION=bookworm

# Building monokulo builds its POS app (crates/monokulo/build.rs), which needs
# Node and the app's dependencies from its lockfile.
FROM node:${NODE_VERSION}-${DEBIAN_VERSION}-slim AS node

FROM rust:${RUST_VERSION}-${DEBIAN_VERSION} AS build
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm
WORKDIR /src
# The POS app's dependencies first, so they stay cached until its lockfile changes.
COPY crates/monokulo/pos-ui/package.json crates/monokulo/pos-ui/package-lock.json crates/monokulo/pos-ui/
RUN npm ci --prefix crates/monokulo/pos-ui --no-audit --no-fund
COPY rust-toolchain.toml ./
RUN rustup toolchain install
COPY . .
RUN cargo build --release --locked \
        -p scanner --bin scanner \
        -p monokulo --bin monokulo \
        -p key-custody-server --bin key-custody-server \
 && mkdir /out \
 && cp target/release/scanner target/release/monokulo target/release/key-custody-server /out/

FROM debian:${DEBIAN_VERSION}-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tini \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --user-group --home-dir /var/lib/monokulo --create-home monokulo
COPY --from=build /out/ /usr/local/bin/
USER monokulo
WORKDIR /var/lib/monokulo
VOLUME /var/lib/monokulo
# Listen on every interface inside the container; publish only monokulo's
# port. The engine's API is for monokulo alone.
ENV MONOKULO_BIND=0.0.0.0:8081 \
    MONOKULO_DB_PATH=/var/lib/monokulo/monokulo.db \
    SCANNER_SERVER_BIND=0.0.0.0:8443 \
    SCANNER_DB_PATH=/var/lib/monokulo/scanner.db
EXPOSE 8081
# tini passes SIGTERM on, so `docker stop` lets requests in flight finish.
ENTRYPOINT ["tini", "--"]
CMD ["monokulo"]
