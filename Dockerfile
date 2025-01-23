# ---------- Stage 1: Builder ----------
    FROM ubuntu:22.04 AS builder

    ARG RUST_VERSION=1.84.0
    
    # Install build essentials, curl, etc.
    RUN apt-get update && apt-get install -y \
        build-essential \
        curl \
        pkg-config \
        libssl-dev \
        ca-certificates \
        && rm -rf /var/lib/apt/lists/*
    
    # Install Rust via rustup
    RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
        sh -s -- -y --default-toolchain $RUST_VERSION
    ENV PATH="/root/.cargo/bin:${PATH}"
    
    WORKDIR /app
    
    # Copy Cargo files (caching)
    COPY Cargo.toml Cargo.lock ./
    RUN mkdir src && echo "fn main() {}" > src/main.rs
    RUN cargo build --release
    
    # Copy full source and .jeff
    COPY src ./src
    COPY .jeff ./.jeff
    
    # Final build
    RUN cargo build --release
    
    # ---------- Stage 2: Runtime ----------
    FROM ubuntu:22.04
    
    RUN apt-get update && apt-get install -y \
        ca-certificates \
        && rm -rf /var/lib/apt/lists/*
    
    WORKDIR /app
    
    # Copy the compiled binary and .jeff
    COPY --from=builder /app/target/release/global-entry-app /usr/local/bin/global-entry-app
    COPY .jeff ./.jeff
    
    CMD ["global-entry-app"]
    