FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    git \
    curl \
    python3 \
    && rm -rf /var/lib/apt/lists/* \
    && git config --system user.email "test@example.com" \
    && git config --system user.name "ns2-tester"
