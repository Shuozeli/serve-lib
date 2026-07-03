FROM rust:1.95-bookworm

WORKDIR /workspace

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl openssl \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY docs ./docs
COPY tests ./tests
COPY README.md index.html ./

ENV CARGO_TERM_COLOR=always
ENV CARGO_INCREMENTAL=0
ENV PATH=/usr/local/cargo/bin:$PATH

CMD ["bash", "tests/e2e/run.sh"]
