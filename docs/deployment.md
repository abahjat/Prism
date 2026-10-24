# Deploying Prism (`prism-server` and `prism-cli`)

This guide covers deploying `prism-server` and `prism-cli` safely in production environments.

## 1. Security Model & Network Isolation

- **Localhost Binding by Default**: `prism-server` binds to `127.0.0.1:8080` (`PRISM_HOST=127.0.0.1`) by default.
- **Authentication & TLS Termination**: `prism-server` is a stateless document-processing engine and intentionally delegates authentication, authorization, rate limiting, and TLS termination to your reverse proxy (e.g. Nginx, Envoy, Caddy, AWS ALB) or host application. **Never expose `prism-server` directly to the public internet without an authenticating reverse proxy.**
- **Zero Outbound Network Access**: Neither `prism-server` nor `prism-worker` makes outbound network requests during document parsing, layout, rendering, hidden-content inspection, or redaction. External images, external stylesheets, and remote hyperlinks in documents (HTML, DOCX, SVG, PDF, EML) are never fetched. This comes from the code containing no network client; the worker adds no OS-level network block. For a hard guarantee, run `prism-server` in an egress-blocked network policy or firewall rule allowing only ingress on port `8080` from your reverse proxy.

## 2. Process Isolation (`prism-worker`)

By default, `prism-server` and `prism-cli` execute all document jobs inside isolated `prism-worker` subprocesses managed by `prism-sandbox`:
- Ensure the `prism-worker` (or `prism-worker.exe`) binary is installed in the same directory as `prism-server` / `prism`, or set `PRISM_WORKER_BIN=/path/to/prism-worker`. Release builds do not search the source tree or build the worker; jobs fail with a protocol error if it cannot be found.
- On Linux containers (Docker / Kubernetes), `prism-worker` applies `PR_SET_NO_NEW_PRIVS`, disables core dumps, and sets `RLIMIT_AS` as a backstop at four times the memory limit (at least 1 GiB); the heap limit itself is enforced by the worker's allocator. Avoid overriding `ulimit` in ways that prevent `setrlimit`.
- On Windows Server, `prism-server` attaches a Windows Job Object to each `prism-worker` subprocess with memory, active-process (`1`), UI, and kill-on-close limits.

## 3. Configuration Reference (`prism-server`)

All settings can be configured via CLI flags or environment variables:

| CLI Flag | Environment Variable | Default | Description |
| :--- | :--- | :--- | :--- |
| `-H, --host` | `PRISM_HOST` | `127.0.0.1` | IP address to bind to |
| `-p, --port` | `PRISM_PORT` | `8080` | TCP port to listen on |
| `--cors-origins` | `PRISM_CORS_ORIGINS` | `""` | Comma-separated allowed CORS origins |
| `--max-file-size-mb` | `PRISM_MAX_FILE_SIZE_MB` | `64` | Maximum upload file size in MiB (`413` when exceeded) |
| `--timeout-seconds` | `PRISM_TIMEOUT_SECS` | `60` | Per-job worker wall-clock timeout in seconds (`504` when exceeded) |
| `--max-memory-mb` | `PRISM_MAX_MEMORY_MB` | `512` | Per-job worker memory ceiling in MiB (`503` when exceeded) |
| `--max-concurrent-requests` | `PRISM_MAX_CONCURRENCY` | `16` | Maximum concurrent requests (`429` when saturated) |
| `--worker-pool-size` | `PRISM_WORKER_POOL_SIZE` | `4` | Number of warm `prism-worker` processes in the pool |
| `--cache-dir` | `PRISM_CACHE_DIR` | *(disabled)* | Directory for content-addressed artifact cache (`FilesystemCache`) |
| `--cache-max-mb` | `PRISM_CACHE_MAX_MB` | `512` | Maximum cache size in MiB before LRU eviction |

## 4. HTTP API Endpoints

- `GET /api/health` — Liveness probe (`200 OK`, `{"status":"ok","version":"0.5.0"}`).
- `GET /api/version` — Engine and crate versions (`engine_version`, `commit`, etc.).
- `POST /api/convert` — Multipart `file`: renders self-contained HTML5 viewer output.
- `POST /api/render?page=1&format=svg|png&dpi=150` — Multipart `file`: renders a 1-based page to SVG or PNG.
- `POST /api/text` — Multipart `file`: extracts UTF-8 plain text.
- `POST /api/report` — Multipart `file`: returns `HiddenContentReport` JSON.
- `POST /api/tree` — Multipart `file`: returns container `FamilyTree` JSON.
- `POST /api/export?format=pdf|tiff&dpi=150` — Multipart `file` + optional `redactions` (`AnnotationSidecar` JSON): exports PDF or multi-page TIFF with destructive redactions burned in and verified automatically.
