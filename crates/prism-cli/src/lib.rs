// SPDX-License-Identifier: AGPL-3.0-only
//! # Prism CLI Library (`prism-cli`)
//!
//! Full command-line interface for Prism document processing, forensic hidden-content reporting,
//! container family-tree inspection, destructive redaction, and Bates-stamped productions.
//! All document parsing and rendering runs inside the `prism-sandbox` worker pool by default.

#![warn(missing_docs)]
#![warn(clippy::all)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions, clippy::too_many_lines)]

use clap::{Parser, Subcommand, ValueEnum};
use prism_core::ENGINE_VERSION;
use prism_production::{ProductionInput, ProductionManifest, ProductionOptions, run_production};
use prism_review::cache::{ArtifactCache, CacheArtifactKind, CacheKey, FilesystemCache};
use prism_review::redaction::{RedactionLog, verify_redacted_output};
use prism_review::sidecar::{AnnotationKind, AnnotationSidecar};
use prism_sandbox::{
    ExportOutputFormat, InProcessExecutor, RenderOutputFormat, SandboxConfig, SandboxError,
    SandboxExecutor, SandboxJob, SandboxManager, SandboxOutput,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Prism CLI version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Exit code `0`: command succeeded.
pub const EXIT_SUCCESS: u8 = 0;
/// Exit code `1`: general CLI, argument, or I/O error.
pub const EXIT_GENERAL_ERROR: u8 = 1;
/// Exit code `2`: document parse or unsupported format error.
pub const EXIT_PARSE_ERROR: u8 = 2;
/// Exit code `3`: sandbox isolation violation (timeout, memory limit, worker crash).
pub const EXIT_SANDBOX_VIOLATION: u8 = 3;
/// Exit code `4`: destructive redaction verification failed.
pub const EXIT_VERIFICATION_FAILED: u8 = 4;

/// Top-level CLI arguments parsed by `clap`.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "prism",
    version,
    about = "Prism — Pure-Rust document processing, review, redaction, and production CLI"
)]
pub struct Cli {
    /// Emit machine-readable JSON output to stdout.
    #[arg(long, global = true)]
    pub json: bool,

    /// Execute tasks in-process instead of spawning isolated `prism-worker` subprocesses.
    #[arg(long, global = true)]
    pub in_process: bool,

    /// Optional directory for the content-addressed artifact cache (`FilesystemCache`).
    #[arg(long, global = true)]
    pub cache_dir: Option<PathBuf>,

    /// Maximum size of `--cache-dir` in MiB before LRU eviction runs (default: `512`).
    #[arg(long, global = true, default_value_t = 512)]
    pub cache_max_mb: u64,

    /// Maximum memory per worker job in MiB (default: `512`).
    #[arg(long, global = true, default_value_t = 512)]
    pub max_memory_mb: usize,

    /// Maximum wall-clock timeout per worker job in seconds (default: `60`).
    #[arg(long, global = true, default_value_t = 60)]
    pub timeout_secs: u64,

    /// Subcommand to execute.
    #[command(subcommand)]
    pub command: Command,
}

/// Page render format for `prism render`.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CliRenderFormat {
    /// Standalone vector SVG page.
    Svg,
    /// Deterministic raster PNG page.
    Png,
}

impl From<CliRenderFormat> for RenderOutputFormat {
    fn from(value: CliRenderFormat) -> Self {
        match value {
            CliRenderFormat::Svg => Self::Svg,
            CliRenderFormat::Png => Self::Png,
        }
    }
}

/// Document export format for `prism export`.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CliExportFormat {
    /// Searchable PDF 1.7 document.
    Pdf,
    /// Multi-page TIFF raster document.
    Tiff,
}

impl From<CliExportFormat> for ExportOutputFormat {
    fn from(value: CliExportFormat) -> Self {
        match value {
            CliExportFormat::Pdf => Self::Pdf,
            CliExportFormat::Tiff => Self::Tiff,
        }
    }
}

/// Supported `prism` subcommands.
#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    /// Detect the format of a document from magic bytes and filename.
    Detect {
        /// Path to the input document.
        file: PathBuf,
    },
    /// Extract document metadata, detected format, and page count as JSON.
    Info {
        /// Path to the input document.
        file: PathBuf,
    },
    /// Extract plain UTF-8 text from a document.
    Text {
        /// Path to the input document.
        file: PathBuf,
        /// Optional output file path (writes to stdout if omitted).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Render 1-based page ranges of a document to SVG or PNG files in an output directory.
    Render {
        /// Path to the input document.
        file: PathBuf,
        /// 1-based page range specification (e.g. `1-5`, `1,3,5`, or `all`). Defaults to `all`.
        #[arg(long, default_value = "all")]
        pages: String,
        /// Output format (`svg` or `png`).
        #[arg(long, value_enum, default_value_t = CliRenderFormat::Svg)]
        format: CliRenderFormat,
        /// Rasterization DPI for `png` output (default: `150.0`).
        #[arg(long, default_value_t = 150.0)]
        dpi: f32,
        /// Output directory where rendered page files (`page-0001.svg` / `page-0001.png`) are written.
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Export a document to PDF or multi-page TIFF, optionally burning in destructive redactions.
    Export {
        /// Path to the input document.
        file: PathBuf,
        /// Target export format (`pdf` or `tiff`).
        #[arg(long, value_enum, default_value_t = CliExportFormat::Pdf)]
        format: CliExportFormat,
        /// Rasterization DPI for `tiff` output or rasterized regions (default: `150.0`).
        #[arg(long, default_value_t = 150.0)]
        dpi: f32,
        /// Optional path to an `AnnotationSidecar` JSON file containing redactions to burn in.
        #[arg(long)]
        redactions: Option<PathBuf>,
        /// Output file path.
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Generate the cross-format hidden-content & forensic findings report.
    Report {
        /// Path to the input document.
        file: PathBuf,
        /// Optional output JSON file path (writes to stdout if omitted).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Build and output the container and embedded-object family tree.
    Tree {
        /// Path to the input document.
        file: PathBuf,
        /// Optional output JSON file path (writes to stdout if omitted).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Run or resume a deterministic Bates-numbered production from a JSON specification file.
    Produce {
        /// Path to the production specification JSON file.
        manifest: PathBuf,
        /// Output directory for `IMAGES/`, `TEXT/`, `NATIVES/`, `LOADFILES/`, and `manifest.json`.
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Verify that an exported artifact contains zero traces of any redacted strings from a sidecar or log.
    VerifyRedactions {
        /// Path to the exported artifact file (`.pdf`, `.tif`, `.png`, `.svg`, `.txt`).
        exported_file: PathBuf,
        /// Path to the `AnnotationSidecar` or `RedactionLog` JSON file.
        #[arg(long)]
        sidecar: PathBuf,
    },
    /// Convert a document to self-contained HTML5 (legacy helper command).
    Convert {
        /// Path to the input document.
        input: PathBuf,
        /// Path to the output `.html` file.
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Display Prism CLI and engine version information.
    Version,
}

/// JSON specification file format consumed by `prism produce <manifest.json> -o <dir>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductionCliSpec {
    /// Production options (Bates prefix/padding/start, endorsements, slip sheets, load file formats).
    pub options: ProductionOptions,
    /// Ordered list of documents to produce.
    pub documents: Vec<ProductionCliDocumentEntry>,
}

/// A single document entry inside [`ProductionCliSpec`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductionCliDocumentEntry {
    /// Stable document identifier.
    pub doc_id: String,
    /// Path to the source document file on disk (resolved relative to the spec file if relative).
    pub path: PathBuf,
    /// Optional filename override.
    #[serde(default)]
    pub filename: Option<String>,
    /// Optional path to an [`AnnotationSidecar`] JSON file for destructive redactions.
    #[serde(default)]
    pub sidecar_path: Option<PathBuf>,
    /// Optional custodian name for load files.
    #[serde(default)]
    pub custodian: Option<String>,
    /// Optional parent document ID for family/attachment tracking.
    #[serde(default)]
    pub parent_doc_id: Option<String>,
    /// Optional child document IDs for family/attachment tracking.
    #[serde(default)]
    pub attachment_doc_ids: Vec<String>,
}

/// Structured CLI execution result containing stdout text and a stable exit code.
#[derive(Debug, Clone)]
pub struct CliOutcome {
    /// Stable process exit code (`EXIT_*`).
    pub exit_code: u8,
    /// Standard output string produced by the command.
    pub stdout: String,
}

/// Parse command-line arguments from an iterator and execute the CLI command.
#[must_use]
pub fn run_from_iter<I, T>(itr: I) -> CliOutcome
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    match Cli::try_parse_from(itr) {
        Ok(cli) => execute_cli(&cli),
        Err(err) => {
            let code = if err.use_stderr() {
                EXIT_GENERAL_ERROR
            } else {
                EXIT_SUCCESS
            };
            CliOutcome {
                exit_code: code,
                stdout: err.to_string(),
            }
        }
    }
}

/// Execute a parsed [`Cli`] command and return a [`CliOutcome`].
#[must_use]
pub fn execute_cli(cli: &Cli) -> CliOutcome {
    match try_execute_cli(cli) {
        Ok(stdout) => CliOutcome {
            exit_code: EXIT_SUCCESS,
            stdout,
        },
        Err((code, msg)) => {
            let formatted = if cli.json {
                serde_json::to_string_pretty(&serde_json::json!({
                    "ok": false,
                    "exit_code": code,
                    "error": msg,
                }))
                .unwrap_or(msg)
            } else {
                format!("Error: {msg}")
            };
            CliOutcome {
                exit_code: code,
                stdout: formatted,
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
fn try_execute_cli(cli: &Cli) -> Result<String, (u8, String)> {
    let executor = build_executor(cli);
    let cache = build_cache(cli)?;

    match &cli.command {
        Command::Version => {
            if cli.json {
                let val = serde_json::json!({
                    "cli_version": VERSION,
                    "engine_version": ENGINE_VERSION,
                    "core_version": prism_core::VERSION,
                    "parsers_version": prism_parsers::VERSION,
                    "render_version": prism_render::VERSION,
                    "sandbox_version": prism_sandbox::VERSION,
                });
                Ok(serde_json::to_string_pretty(&val).unwrap_or_default())
            } else {
                Ok(format!(
                    "Prism CLI v{VERSION} (engine: {ENGINE_VERSION})\n  prism-core: v{}\n  prism-parsers: v{}\n  prism-render: v{}\n  prism-sandbox: v{}",
                    prism_core::VERSION,
                    prism_parsers::VERSION,
                    prism_render::VERSION,
                    prism_sandbox::VERSION
                ))
            }
        }
        Command::Detect { file } => {
            let (filename, bytes) = read_input_file(file)?;
            let out = run_sandbox_job(executor.as_ref(), SandboxJob::Detect { filename, bytes })?;
            let text = String::from_utf8_lossy(&out.data).into_owned();
            if cli.json {
                Ok(text)
            } else {
                let parsed: serde_json::Value =
                    serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
                let name = parsed["format"]["name"].as_str().unwrap_or("Unknown");
                let mime = parsed["format"]["mime_type"]
                    .as_str()
                    .unwrap_or("application/octet-stream");
                let ext = parsed["format"]["extension"].as_str().unwrap_or("");
                let conf = parsed["confidence"].as_f64().unwrap_or(0.0) * 100.0;
                Ok(format!(
                    "Format: {name}\nMIME type: {mime}\nExtension: {ext}\nConfidence: {conf:.1}%"
                ))
            }
        }
        Command::Info { file } => {
            let (filename, bytes) = read_input_file(file)?;
            let out = run_sandbox_job(executor.as_ref(), SandboxJob::Info { filename, bytes })?;
            Ok(String::from_utf8_lossy(&out.data).into_owned())
        }
        Command::Text { file, output } => {
            let (filename, bytes) = read_input_file(file)?;
            let key = CacheKey::new(
                &bytes,
                &cache_options(filename.as_deref()),
                CacheArtifactKind::ExtractedText,
            );
            let data = if let Some(ref c) = cache
                && let Ok(Some(cached)) = c.get(&key)
            {
                cached
            } else {
                let out = run_sandbox_job(
                    executor.as_ref(),
                    SandboxJob::ExtractText { filename, bytes },
                )?;
                if let Some(ref c) = cache {
                    let _ = c.put(&key, &out.data);
                }
                out.data
            };
            let text = String::from_utf8_lossy(&data).into_owned();
            if let Some(out_path) = output {
                write_output_file(out_path, &data)?;
                if cli.json {
                    return Ok(serde_json::to_string_pretty(&serde_json::json!({
                        "ok": true,
                        "output": out_path,
                        "bytes": data.len(),
                    }))
                    .unwrap_or_default());
                }
                Ok(format!(
                    "Wrote {} bytes to {}",
                    data.len(),
                    out_path.display()
                ))
            } else if cli.json {
                Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "ok": true,
                    "text": text,
                }))
                .unwrap_or_default())
            } else {
                Ok(text)
            }
        }
        Command::Render {
            file,
            pages,
            format,
            dpi,
            output,
        } => {
            let (filename, bytes) = read_input_file(file)?;
            fs::create_dir_all(output).map_err(|e| {
                (
                    EXIT_GENERAL_ERROR,
                    format!("Failed to create output dir: {e}"),
                )
            })?;

            let info_out = run_sandbox_job(
                executor.as_ref(),
                SandboxJob::Info {
                    filename: filename.clone(),
                    bytes: bytes.clone(),
                },
            )?;
            let info_val: serde_json::Value = serde_json::from_slice(&info_out.data)
                .map_err(|e| (EXIT_PARSE_ERROR, format!("Invalid info JSON: {e}")))?;
            #[allow(clippy::cast_possible_truncation)]
            let total_pages = info_val["page_count"].as_u64().unwrap_or(1).max(1) as u32;

            let page_indices = parse_page_range(pages, total_pages)?;
            let mut written_files = Vec::with_capacity(page_indices.len());

            for page_index in page_indices {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let kind = match format {
                    CliRenderFormat::Svg => CacheArtifactKind::PageSvg { page_index },
                    CliRenderFormat::Png => CacheArtifactKind::PagePng {
                        page_index,
                        dpi_milli: (*dpi * 1000.0).round() as u32,
                    },
                };
                let key = CacheKey::new(&bytes, &cache_options(filename.as_deref()), kind);
                let page_bytes = if let Some(ref c) = cache
                    && let Ok(Some(cached)) = c.get(&key)
                {
                    cached
                } else {
                    let rendered = run_sandbox_job(
                        executor.as_ref(),
                        SandboxJob::RenderPage {
                            filename: filename.clone(),
                            bytes: bytes.clone(),
                            page_index,
                            format: (*format).into(),
                            dpi: *dpi,
                        },
                    )?;
                    if let Some(ref c) = cache {
                        let _ = c.put(&key, &rendered.data);
                    }
                    rendered.data
                };

                let ext = match format {
                    CliRenderFormat::Svg => "svg",
                    CliRenderFormat::Png => "png",
                };
                let out_file = output.join(format!("page-{:04}.{ext}", page_index + 1));
                write_output_file(&out_file, &page_bytes)?;
                written_files.push(out_file);
            }

            if cli.json {
                Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "ok": true,
                    "pages_rendered": written_files.len(),
                    "files": written_files,
                }))
                .unwrap_or_default())
            } else {
                Ok(format!(
                    "Rendered {} page(s) to {}",
                    written_files.len(),
                    output.display()
                ))
            }
        }
        Command::Export {
            file,
            format,
            dpi,
            redactions,
            output,
        } => {
            let (filename, bytes) = read_input_file(file)?;
            let sidecar_json = if let Some(sidecar_path) = redactions {
                Some(fs::read_to_string(sidecar_path).map_err(|e| {
                    (
                        EXIT_GENERAL_ERROR,
                        format!(
                            "Failed to read redaction sidecar '{}': {e}",
                            sidecar_path.display()
                        ),
                    )
                })?)
            } else {
                None
            };

            let out = run_sandbox_job(
                executor.as_ref(),
                SandboxJob::ExportDocument {
                    filename,
                    bytes,
                    format: (*format).into(),
                    dpi: *dpi,
                    sidecar_json,
                },
            )?;
            write_output_file(output, &out.data)?;

            let mut log_path_written: Option<PathBuf> = None;
            if let Some(ref log_json) = out.redaction_log_json {
                let log_path = PathBuf::from(format!("{}.redaction-log.json", output.display()));
                write_output_file(&log_path, log_json.as_bytes())?;
                log_path_written = Some(log_path);
            }

            if cli.json {
                Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "ok": true,
                    "output": output,
                    "bytes": out.data.len(),
                    "redaction_log": log_path_written,
                }))
                .unwrap_or_default())
            } else {
                Ok(format!(
                    "Exported {} bytes to {}",
                    out.data.len(),
                    output.display()
                ))
            }
        }
        Command::Report { file, output } => {
            let (filename, bytes) = read_input_file(file)?;
            let key = CacheKey::new(
                &bytes,
                &cache_options(filename.as_deref()),
                CacheArtifactKind::HiddenContentReport,
            );
            let data = if let Some(ref c) = cache
                && let Ok(Some(cached)) = c.get(&key)
            {
                cached
            } else {
                let out = run_sandbox_job(
                    executor.as_ref(),
                    SandboxJob::HiddenContentReport { filename, bytes },
                )?;
                if let Some(ref c) = cache {
                    let _ = c.put(&key, &out.data);
                }
                out.data
            };
            let json_str = String::from_utf8_lossy(&data).into_owned();
            if let Some(out_path) = output {
                write_output_file(out_path, &data)?;
            }
            Ok(json_str)
        }
        Command::Tree { file, output } => {
            let (filename, bytes) = read_input_file(file)?;
            let key = CacheKey::new(
                &bytes,
                &cache_options(filename.as_deref()),
                CacheArtifactKind::FamilyTree,
            );
            let data = if let Some(ref c) = cache
                && let Ok(Some(cached)) = c.get(&key)
            {
                cached
            } else {
                let out = run_sandbox_job(
                    executor.as_ref(),
                    SandboxJob::FamilyTree { filename, bytes },
                )?;
                if let Some(ref c) = cache {
                    let _ = c.put(&key, &out.data);
                }
                out.data
            };
            let json_str = String::from_utf8_lossy(&data).into_owned();
            if let Some(out_path) = output {
                write_output_file(out_path, &data)?;
            }
            Ok(json_str)
        }
        Command::Produce { manifest, output } => {
            let spec_str = fs::read_to_string(manifest).map_err(|e| {
                (
                    EXIT_GENERAL_ERROR,
                    format!(
                        "Failed to read production spec '{}': {e}",
                        manifest.display()
                    ),
                )
            })?;
            let spec: ProductionCliSpec = serde_json::from_str(&spec_str).map_err(|e| {
                (
                    EXIT_GENERAL_ERROR,
                    format!("Invalid production spec JSON '{}': {e}", manifest.display()),
                )
            })?;
            let base_dir = manifest.parent().unwrap_or_else(|| Path::new("."));
            let mut inputs = Vec::with_capacity(spec.documents.len());
            for entry in spec.documents {
                let doc_path = if entry.path.is_relative() {
                    base_dir.join(&entry.path)
                } else {
                    entry.path.clone()
                };
                let bytes = fs::read(&doc_path).map_err(|e| {
                    (
                        EXIT_GENERAL_ERROR,
                        format!("Failed to read document '{}': {e}", doc_path.display()),
                    )
                })?;
                let file_name = entry
                    .filename
                    .or_else(|| {
                        doc_path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| "document.bin".to_string());
                let sidecar = if let Some(sp) = entry.sidecar_path {
                    let resolved_sp = if sp.is_relative() {
                        base_dir.join(&sp)
                    } else {
                        sp
                    };
                    let sc_json = fs::read_to_string(&resolved_sp).map_err(|e| {
                        (
                            EXIT_GENERAL_ERROR,
                            format!("Failed to read sidecar '{}': {e}", resolved_sp.display()),
                        )
                    })?;
                    Some(AnnotationSidecar::from_json(&sc_json).map_err(|e| {
                        (
                            EXIT_PARSE_ERROR,
                            format!("Invalid sidecar '{}': {e}", resolved_sp.display()),
                        )
                    })?)
                } else {
                    None
                };

                let mut input = ProductionInput::new(entry.doc_id, file_name, bytes);
                input.sidecar = sidecar;
                input.custodian = entry.custodian;
                input.parent_doc_id = entry.parent_doc_id;
                inputs.push(input);
            }

            let prod_manifest: ProductionManifest = run_production(&inputs, &spec.options, output)
                .map_err(|e| {
                    (
                        EXIT_PARSE_ERROR,
                        format!("Production execution failed: {e}"),
                    )
                })?;

            serde_json::to_string_pretty(&prod_manifest)
                .map_err(|e| (EXIT_GENERAL_ERROR, e.to_string()))
        }
        Command::VerifyRedactions {
            exported_file,
            sidecar,
        } => {
            let exported_bytes = fs::read(exported_file).map_err(|e| {
                (
                    EXIT_GENERAL_ERROR,
                    format!(
                        "Failed to read exported file '{}': {e}",
                        exported_file.display()
                    ),
                )
            })?;
            let sidecar_str = fs::read_to_string(sidecar).map_err(|e| {
                (
                    EXIT_GENERAL_ERROR,
                    format!("Failed to read sidecar/log '{}': {e}", sidecar.display()),
                )
            })?;

            let redacted_strings = extract_redacted_strings_from_json(&sidecar_str)?;
            let hint = exported_file.extension().and_then(|s| s.to_str());

            if let Err(err) = verify_redacted_output(&exported_bytes, hint, &redacted_strings) {
                return Err((EXIT_VERIFICATION_FAILED, err.to_string()));
            }

            if cli.json {
                Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "ok": true,
                    "verified": true,
                    "checked_strings": redacted_strings.len(),
                }))
                .unwrap_or_default())
            } else {
                Ok(format!(
                    "Verification passed: {} redacted string(s) confirmed absent from {}",
                    redacted_strings.len(),
                    exported_file.display()
                ))
            }
        }
        Command::Convert { input, output } => {
            let (filename, bytes) = read_input_file(input)?;
            let out = run_sandbox_job(
                executor.as_ref(),
                SandboxJob::RenderHtml { filename, bytes },
            )?;
            write_output_file(output, &out.data)?;
            Ok(format!(
                "Converted {} to {}",
                input.display(),
                output.display()
            ))
        }
    }
}

fn build_executor(cli: &Cli) -> Arc<dyn SandboxExecutor> {
    if cli.in_process {
        Arc::new(InProcessExecutor::default())
    } else {
        let cfg = SandboxConfig {
            max_memory: cli.max_memory_mb.saturating_mul(1024 * 1024),
            max_execution_time: Duration::from_secs(cli.timeout_secs.max(1)),
            ..SandboxConfig::default()
        };
        Arc::new(SandboxManager::new(cfg))
    }
}

fn build_cache(cli: &Cli) -> Result<Option<FilesystemCache>, (u8, String)> {
    let Some(ref dir) = cli.cache_dir else {
        return Ok(None);
    };
    let max_bytes = cli.cache_max_mb.saturating_mul(1024 * 1024);
    let cache = FilesystemCache::new(dir, max_bytes).map_err(|e| {
        (
            EXIT_GENERAL_ERROR,
            format!("Failed to initialize cache at '{}': {e}", dir.display()),
        )
    })?;
    Ok(Some(cache))
}

fn read_input_file(path: &Path) -> Result<(Option<String>, Vec<u8>), (u8, String)> {
    let bytes = fs::read(path).map_err(|e| {
        (
            EXIT_GENERAL_ERROR,
            format!("Failed to read '{}': {e}", path.display()),
        )
    })?;
    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string);
    Ok((filename, bytes))
}

/// Options that change a cached result besides the bytes: the filename steers format
/// detection, so the same bytes read as `.csv` and `.txt` must not share an entry.
fn cache_options(filename: Option<&str>) -> String {
    serde_json::json!({ "filename": filename }).to_string()
}

fn write_output_file(path: &Path, bytes: &[u8]) -> Result<(), (u8, String)> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|e| {
            (
                EXIT_GENERAL_ERROR,
                format!(
                    "Failed to create parent directory '{}': {e}",
                    parent.display()
                ),
            )
        })?;
    }
    fs::write(path, bytes).map_err(|e| {
        (
            EXIT_GENERAL_ERROR,
            format!("Failed to write '{}': {e}", path.display()),
        )
    })
}

fn run_sandbox_job(
    executor: &dyn SandboxExecutor,
    job: SandboxJob,
) -> Result<SandboxOutput, (u8, String)> {
    executor.execute(job).map_err(|err| match err {
        SandboxError::Timeout(_)
        | SandboxError::MemoryLimit { .. }
        | SandboxError::Crashed { .. }
        | SandboxError::OutputTooLarge { .. }
        | SandboxError::ProtocolError(_) => (EXIT_SANDBOX_VIOLATION, err.to_string()),
        SandboxError::TaskFailed(msg) => {
            if msg.contains("Redaction verification failed") {
                (EXIT_VERIFICATION_FAILED, msg)
            } else {
                (EXIT_PARSE_ERROR, msg)
            }
        }
    })
}

/// Parse a 1-based page specification string (`all`, `1-5`, `1,3,5-7`) into sorted 0-based page indices.
fn parse_page_range(spec: &str, total_pages: u32) -> Result<Vec<u32>, (u8, String)> {
    let trimmed = spec.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("all") {
        return Ok((0..total_pages).collect());
    }

    let mut pages = std::collections::BTreeSet::new();
    for part in trimmed.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((start_s, end_s)) = part.split_once('-') {
            let start: u32 = start_s.trim().parse().map_err(|_| {
                (
                    EXIT_GENERAL_ERROR,
                    format!("Invalid page range start in '{part}'"),
                )
            })?;
            let end: u32 = end_s.trim().parse().map_err(|_| {
                (
                    EXIT_GENERAL_ERROR,
                    format!("Invalid page range end in '{part}'"),
                )
            })?;
            if start == 0 || end < start {
                return Err((
                    EXIT_GENERAL_ERROR,
                    format!("Page numbers must be 1-based and end >= start in '{part}'"),
                ));
            }
            for p in start..=end.min(total_pages) {
                pages.insert(p - 1);
            }
        } else {
            let p: u32 = part.parse().map_err(|_| {
                (
                    EXIT_GENERAL_ERROR,
                    format!("Invalid 1-based page number '{part}'"),
                )
            })?;
            if p == 0 || p > total_pages {
                return Err((
                    EXIT_GENERAL_ERROR,
                    format!("Page {p} is out of bounds (document has {total_pages} page(s))"),
                ));
            }
            pages.insert(p - 1);
        }
    }

    if pages.is_empty() {
        return Err((
            EXIT_GENERAL_ERROR,
            format!("No valid pages matched '{spec}' (total pages: {total_pages})"),
        ));
    }
    Ok(pages.into_iter().collect())
}

fn extract_redacted_strings_from_json(json_str: &str) -> Result<Vec<String>, (u8, String)> {
    // 1. Try parsing as `RedactionLog` first
    if let Ok(log) = serde_json::from_str::<RedactionLog>(json_str) {
        return Ok(log.redacted_strings);
    }

    // 2. Try parsing as `AnnotationSidecar`
    if let Ok(sidecar) = AnnotationSidecar::from_json(json_str) {
        let mut strings = Vec::new();
        for ann in sidecar.annotations {
            if ann.proposed {
                continue;
            }
            if matches!(ann.kind, AnnotationKind::Redaction { .. })
                && let Some(ta) = ann.anchor.text_anchor
                && !ta.quoted_text.trim().is_empty()
            {
                strings.push(ta.quoted_text);
            }
        }
        return Ok(strings);
    }

    Err((
        EXIT_GENERAL_ERROR,
        "Could not parse --sidecar JSON as either AnnotationSidecar or RedactionLog".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_core::review::SourceRef;
    use prism_review::sidecar::{
        Annotation, AnnotationAnchor, AnnotationSidecar, RedactionTarget, TextAnchor,
    };

    #[test]
    fn cli_subcommands_end_to_end_with_cache_and_redaction_verification() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cache_dir = temp.path().join("cache");
        let doc_path = temp.path().join("sample.txt");
        let doc_bytes =
            b"Confidential Project Apollo Launch Code ZX-9918-OMEGA for internal review.";
        fs::write(&doc_path, doc_bytes).expect("write sample.txt");

        // 1. detect
        let out_detect = run_from_iter([
            "prism",
            "--in-process",
            "--json",
            "detect",
            doc_path.to_str().unwrap(),
        ]);
        assert_eq!(out_detect.exit_code, EXIT_SUCCESS, "{}", out_detect.stdout);
        assert!(out_detect.stdout.contains("text/plain"));

        // 2. info
        let out_info = run_from_iter(["prism", "--in-process", "info", doc_path.to_str().unwrap()]);
        assert_eq!(out_info.exit_code, EXIT_SUCCESS);
        assert!(out_info.stdout.contains("\"page_count\": 1"));

        // 3. text (with cache miss then cache hit)
        let out_text1 = run_from_iter([
            "prism",
            "--in-process",
            "--cache-dir",
            cache_dir.to_str().unwrap(),
            "text",
            doc_path.to_str().unwrap(),
        ]);
        assert_eq!(out_text1.exit_code, EXIT_SUCCESS);
        assert!(out_text1.stdout.contains("ZX-9918-OMEGA"));

        let out_text2 = run_from_iter([
            "prism",
            "--in-process",
            "--cache-dir",
            cache_dir.to_str().unwrap(),
            "text",
            doc_path.to_str().unwrap(),
        ]);
        assert_eq!(out_text1.stdout, out_text2.stdout);

        // 4. report & tree
        let out_report = run_from_iter([
            "prism",
            "--in-process",
            "report",
            doc_path.to_str().unwrap(),
        ]);
        assert_eq!(out_report.exit_code, EXIT_SUCCESS);
        assert!(out_report.stdout.contains("\"coverage\""));

        let out_tree = run_from_iter(["prism", "--in-process", "tree", doc_path.to_str().unwrap()]);
        assert_eq!(out_tree.exit_code, EXIT_SUCCESS);
        assert!(out_tree.stdout.contains("\"root\""));

        // 5. render
        let render_dir = temp.path().join("rendered");
        let out_render = run_from_iter([
            "prism",
            "--in-process",
            "render",
            doc_path.to_str().unwrap(),
            "--pages",
            "1",
            "--format",
            "svg",
            "-o",
            render_dir.to_str().unwrap(),
        ]);
        assert_eq!(out_render.exit_code, EXIT_SUCCESS);
        assert!(render_dir.join("page-0001.svg").exists());

        // 6. export with redactions + verify-redactions
        let mut sidecar = AnnotationSidecar::new(doc_bytes);
        sidecar.add(Annotation {
            id: "red-1".to_string(),
            kind: AnnotationKind::Redaction {
                reason_code: "SECRET".to_string(),
                label: "REDACTED".to_string(),
                fill_color: None,
            },
            anchor: AnnotationAnchor {
                target: RedactionTarget::TextRange,
                page_index: 0,
                rects_pt: Vec::new(),
                text_anchor: Some(TextAnchor {
                    start_source: SourceRef::default(),
                    end_source: SourceRef::default(),
                    quoted_text: "ZX-9918-OMEGA".to_string(),
                    prefix: String::new(),
                    suffix: String::new(),
                }),
            },
            author: None,
            created_at: None,
            proposed: false,
        });
        let sidecar_path = temp.path().join("sidecar.json");
        fs::write(&sidecar_path, sidecar.to_json_deterministic().unwrap()).unwrap();

        let redacted_pdf = temp.path().join("redacted.pdf");
        let out_export = run_from_iter([
            "prism",
            "--in-process",
            "export",
            doc_path.to_str().unwrap(),
            "--format",
            "pdf",
            "--redactions",
            sidecar_path.to_str().unwrap(),
            "-o",
            redacted_pdf.to_str().unwrap(),
        ]);
        assert_eq!(out_export.exit_code, EXIT_SUCCESS, "{}", out_export.stdout);
        assert!(redacted_pdf.exists());

        // Verify exported PDF passes verify-redactions
        let out_verify_ok = run_from_iter([
            "prism",
            "--in-process",
            "verify-redactions",
            redacted_pdf.to_str().unwrap(),
            "--sidecar",
            sidecar_path.to_str().unwrap(),
        ]);
        assert_eq!(
            out_verify_ok.exit_code, EXIT_SUCCESS,
            "{}",
            out_verify_ok.stdout
        );

        // Verify unredacted source file FAILS verify-redactions with EXIT_VERIFICATION_FAILED
        let out_verify_fail = run_from_iter([
            "prism",
            "--in-process",
            "verify-redactions",
            doc_path.to_str().unwrap(),
            "--sidecar",
            sidecar_path.to_str().unwrap(),
        ]);
        assert_eq!(out_verify_fail.exit_code, EXIT_VERIFICATION_FAILED);
    }
}
