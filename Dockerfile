# syntax=docker/dockerfile:1.7
FROM rust:1.90-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --locked --release --bins && \
    mkdir -p /out && cp target/release/cfcalc target/release/cfcalc-api target/release/curve_admin /out/

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10001 --create-home cfcalc
WORKDIR /app
COPY --from=build /out/ /usr/local/bin/
COPY data ./data
ENV CFCALC_ADDR=0.0.0.0:8080 \
    CFCALC_INDICATORS=CBKEY1M=/app/data/cbkey1m.csv
USER cfcalc
EXPOSE 8080
HEALTHCHECK --interval=10s --timeout=3s CMD curl -fsS http://127.0.0.1:8080/api/v1/health || exit 1
ENTRYPOINT ["/usr/local/bin/cfcalc-api"]
