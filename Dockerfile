# ============================================================================
# Prism Server - Dockerfile for Google Cloud Run
# ============================================================================
# Multi-stage build for minimal image size
# ============================================================================

# Stage 1: Build the Rust binary
FROM rust:1.75-bookworm AS builder

WORKDIR /app

# Copy source code
COPY . .

# Build release binary
RUN cargo build --release --bin prism-server

# ============================================================================
# Stage 2: Create minimal runtime image
# ============================================================================
FROM debian:bookworm-slim

# Install minimal runtime dependencies
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# Copy binary from builder
COPY --from=builder /app/target/release/prism-server /usr/local/bin/

# Copy web viewer files
COPY --from=builder /app/web-viewer /web-viewer

# Set working directory (web-viewer is served relative to this)
WORKDIR /

# Expose port
EXPOSE 8080

# Configure for Cloud Run (bind to 0.0.0.0)
ENV PRISM_HOST=0.0.0.0
ENV PRISM_PORT=8080

# Health check for Cloud Run
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8080/api/health || exit 1

# Run the server
CMD ["prism-server"]
