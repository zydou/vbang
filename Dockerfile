FROM rust:1-slim-bookworm AS build

# The musl target links musl's libc into the binary, so the result is
# statically linked: no glibc version has to match at runtime. Each RUN stays
# on one line so no continuation can silently break the chain.
RUN rustup target add x86_64-unknown-linux-musl

WORKDIR /src
COPY src src
COPY Cargo.toml Cargo.lock ./
RUN cargo build --locked --release --target x86_64-unknown-linux-musl && install -m 0755 /src/target/x86_64-unknown-linux-musl/release/vbang /vbang

FROM scratch
COPY --from=build /vbang /app/vbang
COPY user.json /app/user.json
# MIT requires the notice to travel with the copy.
COPY LICENSE /app/LICENSE
WORKDIR /app
# scratch has no passwd database and therefore no default user, so without this
# a bare `docker run` would run the server as root.
USER 65534:65534
ENV PORT=3000
EXPOSE 3000
ENTRYPOINT ["/app/vbang"]
