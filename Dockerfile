# syntax=docker/dockerfile:1.27@sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e
# Stages: dev is the development environment, check is the single gate, build compiles the
# release binary, and runtime is the deployable image.

ARG COMPONENT=veoveo-mcp-media

FROM rust:1.99.0-trixie@sha256:6ff07edce8775d0f64be7aba9197229407301bddf2054d62c27b541a6238a181 AS dev
# PostgreSQL 18.6 from the PGDG repository, whose signing key Debian's postgresql-common
# ships. Tests start their own clusters, so the packages create none.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates clang cmake git pkg-config procps postgresql-common \
    && /usr/share/postgresql-common/pgdg/apt.postgresql.org.sh -y \
    && mkdir -p /etc/postgresql-common/createcluster.d \
    && printf 'create_main_cluster = false\n' > /etc/postgresql-common/createcluster.d/veoveo.conf \
    && apt-get install -y --no-install-recommends \
        postgresql-18=18.6-1.pgdg13+2 postgresql-client-18=18.6-1.pgdg13+2 libpq5=18.6-1.pgdg13+2 \
    && rm -rf /var/lib/apt/lists/* /var/log/apt /var/log/dpkg.log \
    && rustup component add clippy rustfmt
ENV PATH=/usr/lib/postgresql/18/bin:${PATH}
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo install --locked \
        cargo-nextest@0.9.148 \
        cargo-deny@0.20.2 \
        cargo-hack@0.6.45 \
        cargo-semver-checks@0.51.0 \
        cargo-insta@1.49.0 \
    && cargo install --locked sqlx-cli@0.9.0 --no-default-features --features postgres,rustls,sqlx-toml
COPY env/profile.d/rust.sh /etc/profile.d/rust.sh
# SeaweedFS 4.48, the installation's object store. The run tests store outputs through an
# in-process Artifact over a private `weed mini` per test process, as they start PostgreSQL.
COPY --from=chrislusf/seaweedfs:4.48@sha256:4e61d15fd35994cb1e43e1e553dff106794841fd9a99ade2fc8c8bfce4d7872d \
    /usr/bin/weed /usr/local/bin/weed
# A development environment installs toolchains, components and crates, so the sandbox user owns them.
RUN useradd --create-home --uid 1000 --shell /bin/bash sandbox \
    && chown -R sandbox:sandbox /usr/local/rustup /usr/local/cargo
USER sandbox

FROM dev AS source
WORKDIR /home/sandbox/src
COPY --chown=sandbox:sandbox . .

FROM source AS check
# The committed .sqlx query metadata must match the crate's queries against the kernel
# migrations of the pinned veoveo-core checkout and the crate's own migrations, applied
# in the server's schema. The tests compile Artifact's queries too, so Artifact's
# migrations of the pinned checkout run in its schema first.
RUN cargo fetch --locked \
    && store="$(echo /usr/local/cargo/git/checkouts/veoveo-core-*/*/platform/store)" \
    && artifact="$(echo /usr/local/cargo/git/checkouts/veoveo-mcp-artifact-*/*/crates/veoveo-mcp-artifact)" \
    && initdb --pgdata /tmp/sqlx/data --username postgres --auth=trust --no-instructions --no-sync \
    && pg_ctl start --pgdata /tmp/sqlx/data --wait --silent \
        --options "-c listen_addresses=127.0.0.1 -c unix_socket_directories=/tmp/sqlx -c fsync=off" \
    && createdb --host 127.0.0.1 --username postgres veoveo \
    && DATABASE_URL=postgres://postgres@127.0.0.1/veoveo cargo sqlx migrate run \
        --source "${store}/migrations" --config "${store}/sqlx.toml" \
    && psql --host 127.0.0.1 --username postgres --dbname veoveo --quiet \
        --command "SELECT kernel.ensure_server('artifact')" \
        --command "SELECT kernel.ensure_server('media')" \
    && DATABASE_URL=postgres://postgres@127.0.0.1/veoveo cargo sqlx migrate run \
        --source "${artifact}/migrations" --config "${artifact}/sqlx.toml" \
    && DATABASE_URL=postgres://postgres@127.0.0.1/veoveo cargo sqlx migrate run \
        --source crates/veoveo-mcp-media/migrations \
        --config crates/veoveo-mcp-media/sqlx.toml \
    && DATABASE_URL=postgres://postgres@127.0.0.1/veoveo cargo sqlx prepare --workspace --check \
        -- --all-targets --all-features \
    && pg_ctl stop --pgdata /tmp/sqlx/data --mode fast --wait --silent \
    && rm -rf /tmp/sqlx
RUN cargo fmt --check \
    && cargo clippy --locked --all-targets --all-features -- -D warnings \
    && cargo deny --locked check \
    && cargo hack check --each-feature --no-dev-deps \
    && cargo nextest run --locked --profile ci --all-features

FROM source AS build
ARG COMPONENT
RUN cargo build --locked --release --bin "${COMPONENT}" \
    && cp "target/release/${COMPONENT}" "/home/sandbox/${COMPONENT}"

FROM debian:trixie-slim@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f AS runtime
ARG COMPONENT
ARG VERSION
ARG REVISION
RUN useradd --system --uid 10001 --no-create-home "${COMPONENT}"
COPY --from=build "/home/sandbox/${COMPONENT}" "/usr/local/bin/${COMPONENT}"
USER ${COMPONENT}
# The exec form cannot expand build arguments, so the binary path is fixed here.
ENTRYPOINT ["/usr/local/bin/veoveo-mcp-media"]
LABEL org.opencontainers.image.title="${COMPONENT}" \
      org.opencontainers.image.source="https://github.com/VeoveoAI/${COMPONENT}" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.licenses="Apache-2.0"
