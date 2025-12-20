# CLAUDE.md - Project Context for Claude Code

## Project: Prism - Modern Document Processing SDK

### Vision
Build a modern, developer-friendly document processing SDK that competes with Oracle Outside In, supporting 600+ file formats. Codename: "Prism"

### Tagline
"Any document, any platform, in milliseconds."

---

## Architecture Overview

### Core Design Principles
1. **Rust Core, Polyglot Surface** - Core engine in Rust for performance and safety; bindings for all major languages
2. **Parser Isolation** - Each format parser runs in WebAssembly sandboxes for security and stability
3. **Unified Document Model (UDM)** - All formats parse to a common intermediate representation
4. **Streaming First** - Process documents without loading entirely into memory
5. **Parallelism by Default** - Leverage all available cores
6. **Cloud-Native** - Designed for containerization, horizontal scaling, serverless

### Project Structure
```
prism/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── prism-core/              # Core engine, UDM, traits
│   ├── prism-parsers/           # Format parser implementations
│   ├── prism-render/            # Rendering engine (HTML, PDF, Image output)
│   ├── prism-sandbox/           # WASM sandboxing for parsers
│   ├── prism-server/            # REST API server (Axum)
│   └── prism-cli/               # Command-line interface
├── sdks/
│   ├── python/                  # Python SDK
│   ├── node/                    # Node.js SDK
│   └── java/                    # Java SDK
├── tests/
│   ├── corpus/                  # Test documents
│   └── integration/             # Integration tests
└── docker/
    └── Dockerfile
```

### Key Components

#### Unified Document Model (UDM)
All formats parse into this common representation:
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

#### Parser Trait
Every format parser implements:
- `id()` - Unique identifier
- `extensions()` - File extensions handled
- `mime_types()` - MIME types handled
- `signatures()` - Magic bytes for detection
- `can_parse()` - Confidence score for input
- `parse()` - Main parsing to UDM
- `extract_text()` - Optimized text extraction
- `extract_metadata()` - Optimized metadata extraction

---

## Technology Stack

| Layer | Technology | Rationale |
|-------|------------|-----------|
| Core Engine | Rust | Memory safety, performance, no GC |
| Parser Sandbox | WebAssembly (Wasmtime) | Isolation, security, cross-platform |
| API Server | Axum | Async, type-safe, performant |
| Job Queue | Redis / NATS | Speed, pub/sub |
| Storage | S3-compatible | Scalability |
| Rendering | Skia (rust-skia) | High quality, cross-platform |

---

## Format Roadmap

### Phase 1 (Year 1): 200 Formats - 90% real-world coverage
- Office: DOCX, XLSX, PPTX, DOC, XLS, PPT, RTF
- PDF: PDF 1.x-2.0, PDF/A
- Email: MSG, EML, PST
- Images: JPEG, PNG, TIFF, GIF, BMP, WebP, HEIC
- Archives: ZIP, RAR, 7z, TAR, GZIP
- CAD: DWG, DXF

### Phase 2 (Year 2): 400 Formats
- Legacy Office: WordPerfect, Lotus 1-2-3, Works
- Additional CAD: SolidWorks, CATIA
- Specialized: DICOM, AFP/MO:DCA

### Phase 3 (Year 3): 600+ Formats - Parity with Outside In
- All remaining legacy formats
- Full CAD coverage
- Mainframe/IBM formats

---

## Competitive Positioning

### vs Oracle Outside In
| Aspect | Prism | Outside In |
|--------|-------|------------|
| Architecture | Modern Rust | Legacy C/C++ |
| Deployment | Cloud-native, containers | On-premise focused |
| Developer Experience | Clean APIs, 10+ language SDKs | Complex, limited |
| Pricing | Transparent, usage-based | Opaque enterprise |
| Security | WASM sandboxed parsers | Process isolation |

---

## Current Implementation Status

### Completed (in this template)
- [x] Workspace structure with all crates
- [x] VS Code configuration (settings, tasks, launch, extensions)
- [x] `prism-core` crate structure
- [x] Unified Document Model (`document.rs`)
- [x] Error handling (`error.rs`)
- [x] Format detection (`format.rs`)
- [x] Parser traits (`parser.rs`) - partial

### Next Steps
1. Complete `parser.rs` with full trait definition
2. Create `metadata.rs` module
3. Create `render.rs` module with output traits
4. Set up `prism-parsers` crate with first parser (PDF or PNG)
5. Implement basic format detection tests
6. Create `prism-server` with health check endpoint
7. Create `prism-cli` with basic commands

---

## Commands Reference

### Development
```bash
# Build all crates
cargo build

# Run tests
cargo test

# Run with logging
RUST_LOG=debug cargo run --package prism-server

# Check code quality
cargo clippy --all-targets --all-features

# Format code
cargo fmt

# Watch mode (requires cargo-watch)
cargo watch -x check
```

### Testing
```bash
# Run specific test
cargo test test_name

# Run tests with output
cargo test -- --nocapture

# Run tests for specific crate
cargo test --package prism-core
```

---

## Design Decisions Log

1. **Rust over C++** - Memory safety without GC, modern tooling, easier cross-compilation
2. **WASM for sandboxing** - Better security than process isolation, portable, predictable resource limits
3. **Axum over Actix** - Better ergonomics, tower ecosystem, async-first
4. **Workspace structure** - Clear separation of concerns, independent versioning possible
5. **Trait-based parsers** - Extensibility, testability, potential for plugin system

---

## Resources

### Specifications Needed
- ECMA-376 (OOXML) - Free
- ISO 32000 (PDF) - ~$200
- MS Open Specifications (OLE, MSG, PST) - Free
- Open Design Alliance membership for DWG - $2,500+/year

### Reference Implementations to Study
- Apache POI (Java) - Office parsing
- pdf.js / pdfium - PDF
- libarchive - Archives
- LibreOffice core - Multiple formats

---

## Notes for Claude Code

When working on this project:
1. Always run `cargo check` after making changes
2. Use `cargo clippy` before committing
3. Follow existing code style (rust-analyzer handles formatting)
4. Add tests for new functionality
5. Document public APIs with rustdoc comments
6. Use `thiserror` for error types, `anyhow` in binaries
