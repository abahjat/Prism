# Prism - Modern Document Processing SDK

> **"Any document, any platform, in milliseconds."**

Prism is a document processing SDK built in Rust for viewing, converting and extracting content from documents. It aims to become an open-source alternative to Oracle Outside In for forensic and eDiscovery review. About 70 formats are supported today. The long-term target is 600+ (see [docs/ROADMAP.md](docs/ROADMAP.md)).

## 🖼️ Visuals

<p align="center">
  <img src="assets/screenshots/viewer_home.png" width="800" alt="Prism Home">
</p>

<p align="center">
  <img src="assets/screenshots/viewer_mht.png" width="400" alt="MHT Viewing">
  <img src="assets/screenshots/viewer_image.png" width="400" alt="Image Viewing">
</p>


## 🚀 Features

- **Comprehensive Format Support**: 68+ document formats currently implemented
  - **Documents:** PDF, RTF, XPS, EPUB
  - **Microsoft Office:** DOCX, XLSX, PPTX, DOC, XLS, PPT, ONE, VSDX, MPP
  - **OpenDocument:** ODT, ODS, ODP, ODG
  - **Images:** PNG, JPEG, JPEG 2000, TIFF, GIF, WebP, BMP, ICO, CUR, TGA, PSD, PCX, WBMP
  - **Vector:** SVG, SVGZ, EPS, EMF, EMZ, WMF, AI
  - **Email & Calendar:** EML, MSG, MBOX, VCF, ICS, MHT
  - **Archives:** ZIP, TAR, GZIP, 7z, Bzip2, UNIX Compress (.z)
  - **Database:** SQLite, DBF
  - **Code:** Rust, Python, JavaScript, TypeScript, C, C++, CSS (syntax highlighting)
  - **CAD:** DXF
  - **Text:** TXT, HTML, JSON, XML, CSV, Markdown, LOG

- **Modern Architecture**: Built with Rust for memory safety, performance, and reliability
- **Forensically safe output**: Rendered HTML loads nothing from the network and runs no document content. Untrusted HTML is sanitized, remote images (read-receipt pixels) are blocked, and a strict Content Security Policy is applied. Bundled pdf.js means the viewer works air-gapped.
- **Robust**: Every parser is tested against truncated and corrupted inputs (no panics or hangs), and fuzz targets are included.
- **Embeddable**: Rust API, a C ABI (`prism-bindings`) and a .NET wrapper (`Prism.Native`)
- **Cloud-Ready**: REST server and container image

## 📦 Components

### Core Components

| Component | Description | Status |
|-----------|-------------|--------|
| **prism-core** | Core engine, Unified Document Model (UDM), parser/renderer traits | ✅ Foundation complete |
| **prism-parsers** | Format parser implementations | 🚧 In development |
| **prism-render** | Rendering engine (HTML, PDF, Image output) | 🚧 Basic HTML renderer |
| **prism-sandbox** | Parser isolation (WebAssembly planned) | 🚧 Configuration types only; parsers are **not** sandboxed yet |
| **prism-server** | REST API server (Axum-based) | 🚧 `/api/health`, `/api/version`, `/api/convert` |
| **prism-bindings** | C ABI for native hosts (.NET, Java, C/C++) | ✅ Detect, HTML, text, metadata, last-error |
| **prism-cli** | Command-line interface | 🚧 Only `version` works; other commands are placeholders |

## 🛠️ Installation

### Prerequisites

- Rust 1.85 or later (Edition 2024)
- Cargo (comes with Rust)

### Building from Source

```bash
# Clone the repository
git clone https://github.com/abahjat/prism.git
cd prism

# Build all crates
cargo build --release

# Run tests
cargo test

# Build optimized binaries
cargo build --release
```

### Binaries

After building, you'll find the binaries in `target/release/`:
- `prism` - CLI tool
- `prism-server` - REST API server

## 🚀 Quick Start

### Using the CLI

> The CLI is not functional yet: argument parsing is a placeholder and only prints the version. The commands below show the planned interface.

```bash
# Detect document format
prism detect document.pdf

# Convert a document
prism convert input.docx --output output.pdf

# Extract text
prism extract-text document.pdf --output text.txt

# Extract metadata
prism metadata document.pdf
```

### Using the REST API Server

```bash
# Start the server (default: 127.0.0.1:8080)
cargo run --bin prism-server

# Custom host and port
cargo run --bin prism-server -- --host 0.0.0.0 --port 3000

# Or use environment variables
PRISM_HOST=0.0.0.0 PRISM_PORT=3000 cargo run --bin prism-server

# Health check
curl http://localhost:8080/api/health

# Version information
curl http://localhost:8080/api/version
```

### CORS Configuration

By default, the server allows requests from:
- `documain.ai` and all subdomains
- `localhost` and `127.0.0.1` (any port) for local development

#### Development Mode (Allow All Origins)

```bash
# Using CLI flag
cargo run --bin prism-server -- --cors-origins "*"

# Using environment variable
PRISM_CORS_ORIGINS="*" cargo run --bin prism-server
```

#### Production Deployment

When deploying to a different domain, specify your allowed origins:

```bash
# Single origin
cargo run --bin prism-server -- --cors-origins "https://yourdomain.com"

# Multiple origins (comma-separated)
cargo run --bin prism-server -- --cors-origins "https://app.example.com,https://admin.example.com"

# Using environment variable
PRISM_CORS_ORIGINS="https://yourdomain.com" cargo run --bin prism-server
```

#### Docker/Kubernetes

```yaml
# Docker Compose
environment:
  - PRISM_CORS_ORIGINS=https://yourdomain.com

# Kubernetes
env:
  - name: PRISM_CORS_ORIGINS
    value: "https://yourdomain.com"
```

### Using the Rust Library

Add Prism to your `Cargo.toml`:

```toml
[dependencies]
prism-core = "0.5.0"
prism-parsers = "0.5.0"
prism-render = "0.5.0"
```

Example usage:

```rust
use prism_core::format::detect_format;
use prism_core::Document;

#[tokio::main]
async fn main() -> prism_core::Result<()> {
    // Initialize Prism
    prism_core::init();

    // Read a document
    let data = std::fs::read("document.pdf")?;

    // Detect the format
    let format_result = detect_format(&data, Some("document.pdf"))
        .ok_or_else(|| prism_core::Error::DetectionFailed("Unknown format".to_string()))?;

    println!("Detected format: {}", format_result.format.name);
    println!("MIME type: {}", format_result.format.mime_type);
    println!("Confidence: {:.2}%", format_result.confidence * 100.0);

    Ok(())
}
```

### Format Detection

```rust
use prism_core::format::detect_format;

// Detect from bytes
let data = std::fs::read("document.pdf")?;
let result = detect_format(&data, Some("document.pdf"));

if let Some(detection) = result {
    println!("Format: {}", detection.format.name);
    println!("MIME: {}", detection.format.mime_type);
    println!("Confidence: {:.2}%", detection.confidence * 100.0);
}
```

### Document Rendering

```rust
use prism_core::Document;
use prism_render::html::HtmlRenderer;
use prism_core::render::{Renderer, RenderContext};

async fn render_to_html(document: &Document) -> prism_core::Result<String> {
    let renderer = HtmlRenderer::new();

    let context = RenderContext {
        options: Default::default(),
        filename: Some("output.html".to_string()),
    };

    let html_bytes = renderer.render(document, context).await?;
    Ok(String::from_utf8(html_bytes.to_vec())?)
}
```

### .NET Integration (Windows)

Prism can be integrated into .NET applications (Windows Forms, WPF, MAUI) via the `prism-bindings` crate, which exposes a standard C API.

1.  **Build the DLL**:
    ```bash
    cargo build -p prism-bindings --release
    ```
    This produces `target/release/prism_bindings.dll`.

2.  **Add to C# Project**:
    -   Copy the DLL to your project output directory.
    -   Use `[DllImport]` to call the functions.

    See `examples/dotnet/` for a complete working example.


## 🏗️ Architecture

### Unified Document Model (UDM)

All document formats are parsed into a common intermediate representation:

```
Document
├── Metadata (title, author, dates, custom properties)
├── Pages[]
│   ├── Dimensions
│   ├── Content Blocks[]
│   │   ├── Text (runs, styles, positions)
│   │   ├── Images (embedded, linked)
│   │   ├── Tables (rows, cols, cells)
│   │   └── Vectors (paths, shapes)
│   └── Annotations
├── Styles (fonts, colors, paragraph styles)
├── Resources (fonts, images, embeddings)
└── Structure (headings, TOC, bookmarks)
```

### Parser Architecture

Each format parser implements the `Parser` trait:

```rust
#[async_trait]
pub trait Parser: Send + Sync {
    fn format(&self) -> Format;
    fn can_parse(&self, data: &[u8]) -> bool;
    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document>;
}
```

### Renderer Architecture

Renderers implement the `Renderer` trait to produce output in various formats:

```rust
#[async_trait]
pub trait Renderer: Send + Sync {
    fn output_format(&self) -> Format;
    async fn render(&self, document: &Document, context: RenderContext) -> Result<Bytes>;
}
```

## 🔧 Development

### Project Structure

```
prism/
├── Cargo.toml              # Workspace root
├── crates/
│   ├── prism-core/        # Core engine, UDM, traits
│   ├── prism-parsers/     # Format parser implementations
│   ├── prism-render/      # Rendering engine
│   ├── prism-sandbox/     # WASM sandboxing
│   ├── prism-server/      # REST API server
│   └── prism-cli/         # Command-line interface
├── tests/                 # Integration tests
└── docs/                  # Documentation
```

### Running Tests

```bash
# Run all tests
cargo test

# Run tests for a specific crate
cargo test --package prism-core

# Run tests with output
cargo test -- --nocapture

# Run only unit tests
cargo test --lib

# Run only documentation tests
cargo test --doc
```

### Code Quality

```bash
# Check code without building
cargo check

# Run Clippy linter
cargo clippy --all-targets --all-features

# Format code
cargo fmt

# Check formatting
cargo fmt -- --check
```

### Watching for Changes

Install `cargo-watch`:

```bash
cargo install cargo-watch

# Watch and run checks
cargo watch -x check

# Watch and run tests
cargo watch -x test
```

## 🌐 REST API

### Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/health` | GET | Health check |
| `/version` | GET | Version information |
| `/detect` | POST | Detect document format |
| `/convert` | POST | Convert document to another format |
| `/extract/text` | POST | Extract text from document |
| `/extract/metadata` | POST | Extract metadata from document |
| `/render` | POST | Render document to output format |

### Example API Usage

```bash
# Health check
curl http://localhost:8080/health

# Get version information
curl http://localhost:8080/version

# Detect format (planned)
curl -X POST http://localhost:8080/detect \
  -F "file=@document.pdf"

# Convert document (planned)
curl -X POST http://localhost:8080/convert \
  -F "file=@document.docx" \
  -F "output_format=pdf" \
  -o output.pdf
```

## 🐳 Docker Deployment

### Building Docker Image

```dockerfile
FROM rust:1.75 as builder
WORKDIR /app
COPY . .
RUN cargo build --release --bin prism-server

FROM debian:bookworm-slim
COPY --from=builder /app/target/release/prism-server /usr/local/bin/
EXPOSE 8080
CMD ["prism-server", "--host", "0.0.0.0"]
```

Build and run:

```bash
# Build image
docker build -t prism-server .

# Run container
docker run -p 8080:8080 prism-server

# Custom port
docker run -p 3000:3000 -e PRISM_PORT=3000 prism-server
```

### Docker Compose

```yaml
version: '3.8'
services:
  prism:
    image: prism/server:latest
    ports:
      - "8080:8080"
    environment:
      - PRISM_HOST=0.0.0.0
      - PRISM_PORT=8080
    volumes:
      - ./data:/data
      - ./cache:/cache
```

## ☸️ Kubernetes Deployment

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: prism-server
spec:
  replicas: 3
  selector:
    matchLabels:
      app: prism
  template:
    metadata:
      labels:
        app: prism
    spec:
      containers:
      - name: prism
        image: prism/server:latest
        ports:
        - containerPort: 8080
        env:
        - name: PRISM_HOST
          value: "0.0.0.0"
        - name: PRISM_PORT
          value: "8080"
        resources:
          requests:
            memory: "2Gi"
            cpu: "1000m"
          limits:
            memory: "4Gi"
            cpu: "2000m"
```

## 📊 Performance

Current performance targets:

| Operation | Target (p95) | Status |
|-----------|--------------|--------|
| Format Detection | <10ms | ✅ Achieved |
| Simple Conversion (10 pages) | <500ms | 🚧 In progress |
| Text Extraction | <100ms | 🚧 In progress |
| Thumbnail Generation | <200ms | 🚧 In progress |

## 🔒 Security

- **No code execution**: Macros are never run. Scripts, event handlers, frames, forms and plugins are removed from document HTML. Untrusted SVG is displayed as an image, so it cannot run scripts.
- **No network access from rendered output**: A Content Security Policy (`default-src 'none'`, nonce-only scripts) and the sanitizer block remote images, fonts and stylesheets. Links are neutralized, so the target stays visible but can't be followed.
- **Malformed input**: Parsers must return errors rather than panic or hang. This is enforced by `crates/prism-tests/tests/robustness_tests.rs` and the fuzz targets in `fuzz/`.
- **Not yet implemented**: Parsers run in-process without memory or time limits. Process isolation and WASM sandboxing are on the roadmap.
- **Known exception**: Some image conversions shell out to ImageMagick when it is installed (`prism-parsers/src/image/converter.rs`).

## 🗺️ Roadmap

See [docs/ROADMAP.md](docs/ROADMAP.md). In short: Phase 0 foundations (safe output, robustness, bindings), then a native Rust layout engine (`prism-layout`) with a shared page model, then Word, PowerPoint and Excel fidelity, and finally review and production features.

## 🤝 Contributing

We welcome contributions! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for details.

### Development Guidelines

1. Follow Rust best practices and idioms
2. Write tests for new functionality
3. Document public APIs with rustdoc comments
4. Run `cargo clippy` before submitting
5. Ensure `cargo test` passes
6. Update documentation as needed

## 📝 License

Prism is dual-licensed under:

- **AGPL-3.0**: For open source projects and internal use where source availability is acceptable ([LICENSE](LICENSE)).
- **Commercial**: For proprietary/closed-source applications ([LICENSE_COMMERCIAL](LICENSE_COMMERCIAL)).

See [DUAL_LICENSING.md](DUAL_LICENSING.md) for a detailed guide on which license you need.

## 🙏 Acknowledgments

Prism is inspired by and aims to be a modern alternative to:
- Oracle Outside In
- Apache POI
- LibreOffice
- Various document processing libraries

## 📞 Support

- **Documentation**: [docs.prism.dev](https://docs.prism.dev) (planned)
- **Issues**: [GitHub Issues](https://github.com/abahjat/prism/issues)
- **Discussions**: [GitHub Discussions](https://github.com/abahjat/prism/discussions)
- **Discord**: [Join our community](https://discord.gg/bD6SdpU7)

## 🌟 Status

**Current Status**: Early development (v0.5.0). Office documents render as approximate HTML: there is no layout engine yet, so DOCX pagination is synthetic. Legacy DOC/PPT give text only.

- ✅ Core architecture complete
- ✅ Format detection working
- ✅ Basic HTML renderer
- 🚧 Parser implementations in progress
- 🚧 Additional renderers in development
- 🚧 REST API under construction


<a href="https://cla-assistant.io/abahjat/Prism"><img src="https://cla-assistant.io/readme/badge/abahjat/Prism" alt="CLA assistant" /></a>

---

**Built with ❤️ in Rust**

