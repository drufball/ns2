FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    git \
    curl \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# The ns2 binary is mounted at runtime via:
#   docker run -v /path/to/ns2:/usr/local/bin/ns2 ...
# No source files are copied into this image.

WORKDIR /root
