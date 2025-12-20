# Prism - Modern Document Processing SDK

> **"Any document, any platform, in milliseconds."**

Prism is a next-generation document processing SDK built in Rust, designed to view, convert, and extract content from 600+ file formats. It's the modern, developer-friendly alternative to Oracle Outside In.

## 🚀 Features

- **Comprehensive Format Support**: Support for 600+ document formats (200+ in Phase 1)
  - Office: DOCX, XLSX, PPTX, DOC, XLS, PPT, RTF
  - PDF: PDF 1.x-2.0, PDF/A
  - Email: MSG, EML, PST
  - Images: JPEG, PNG, TIFF, GIF, BMP, WebP, HEIC
  - Archives: ZIP, RAR, 7z, TAR, GZIP
  - CAD: DWG, DXF
  - And many more...

- **Modern Architecture**: Built with Rust for memory safety, performance, and reliability
- **Cloud-Native**: Designed for containerization, horizontal scaling, and serverless deployment
- **Secure by Default**: WebAssembly sandboxing for parser isolation
- **Developer-Friendly**: Clean APIs with SDKs for 10+ languages
- **High Performance**: Parallel processing, streaming support, and optimized rendering

## 📦 Components

### Core Components

| Component | Description | Status |
|-----------|-------------|--------|
| **prism-core** | Core engine, Unified Document Model (UDM), parser/renderer traits | ✅ Foundation complete |
| **prism-parsers** | Format parser implementations | 🚧 In development |
| **prism-render** | Rendering engine (HTML, PDF, Image output) | 🚧 Basic HTML renderer |
| **prism-sandbox** | WebAssembly sandboxing for secure parser execution | 🚧 Framework ready |
| **prism-server** | REST API server (Axum-based) | 🚧 Basic endpoints |
| **prism-cli** | Command-line interface | 🚧 Structure ready |

## 🛠️ Installation

### Prerequisites

- Rust 1.75 or later
- Cargo (comes with Rust)

### Building from Source

```bash
# Clone the repository
git clone https://github.com/your-org/prism.git
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
# Start the server
cargo run --bin prism-server

# Server runs on http://localhost:8080

# Health check
curl http://localhost:8080/health

# Version information
curl http://localhost:8080/version
```

### Using the Rust Library

Add Prism to your `Cargo.toml`:

```toml
[dependencies]
prism-core = "0.1.0"
prism-parsers = "0.1.0"
prism-render = "0.1.0"
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
CMD ["prism-server"]
```

Build and run:

```bash
# Build image
docker build -t prism-server .

# Run container
docker run -p 8080:8080 prism-server
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
      - PRISM_WORKERS=4
      - PRISM_MAX_FILE_SIZE=100MB
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

- **Parser Sandboxing**: All parsers run in WebAssembly sandboxes with strict memory/CPU limits
- **No Code Execution**: Documents cannot execute code; macros are parsed but not run
- **Memory Limits**: Configurable memory limits per parser instance
- **Timeout Protection**: Execution time limits prevent infinite loops
- **No I/O Access**: Sandboxed parsers cannot access filesystem or network

## 🗺️ Roadmap

### Phase 1 (Current - Year 1): Foundation
- ✅ Core architecture and UDM
- ✅ Basic format detection
- ✅ HTML renderer
- 🚧 200 format support
- 🚧 REST API
- 🚧 CLI tool

### Phase 2 (Year 2): Expansion
- 400 format support
- AI-powered features (classification, summarization)
- SOC 2 Type II compliance
- Enterprise features

### Phase 3 (Year 3): Parity
- 600+ format support
- FedRAMP certification
- Format parity with Oracle Outside In

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

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## 🙏 Acknowledgments

Prism is inspired by and aims to be a modern alternative to:
- Oracle Outside In
- Apache POI
- LibreOffice
- Various document processing libraries

## 📞 Support

- **Documentation**: [docs.prism.dev](https://docs.prism.dev) (planned)
- **Issues**: [GitHub Issues](https://github.com/your-org/prism/issues)
- **Discussions**: [GitHub Discussions](https://github.com/your-org/prism/discussions)
- **Discord**: [Join our community](https://discord.gg/prism) (planned)

## 🌟 Status

**Current Status**: Early Development (v0.1.0)

- ✅ Core architecture complete
- ✅ Format detection working
- ✅ Basic HTML renderer
- 🚧 Parser implementations in progress
- 🚧 Additional renderers in development
- 🚧 REST API under construction

---

**Built with ❤️ in Rust**

For more information, see:
- [CLAUDE.md](CLAUDE.md) - Project context for AI assistants
- [Prism-PRD-Document-SDK.md](Prism-PRD-Document-SDK.md) - Full Product Requirements Document
