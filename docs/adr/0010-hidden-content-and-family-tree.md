# ADR 0010: Cross-Format Hidden-Content Report and Unified Container Family Tree

## Status

Accepted (Phase 4, Milestone M0 / M1 / M4)

## Context

In eDiscovery and forensic review, a reviewer viewing a document "as printed" can easily miss content that is concealed by formatting or structure: tracked revisions, comments, hidden text (`w:vanish`), white-on-white or `< 2 pt` text, `;;;`-formatted spreadsheet cells, hidden/veryHidden worksheets, hidden slides, speaker notes, macros (VBA/XLM), external links, PDF Optional Content Groups (OCGs) defaulted to off, incremental PDF updates, or nested container attachments.

Previously, each format parser captured format-specific forensic indicators (`ForensicFacts`, `PresentationForensicFacts`, `WorkbookForensicFacts`), and container parsers (`zip`, `tar`, `7z`, `eml`, `msg`, `mbox`) expanded members ad hoc without a unified tree or cycle detection.

## Decision

1. **Cross-Format Hidden-Content Report (`prism_core::review`)**:
   - Define `HiddenContentReport { items: Vec<Finding>, coverage: Vec<FormatCoverage> }` in `prism-core`.
   - Each `Finding` records a typed `FindingKind`, `Severity` (`Info`, `Notable`, `High`), `Location` (`PageRects`, `Slide`, `SheetRange`, `PartPath`, `Document`), an optional `SourceRef` into the layout model so the viewer can jump directly to the element, a bounded `excerpt`, and structured JSON `detail`.
   - Every format provider explicitly publishes a `FormatCoverage` entry listing the `FindingKind`s it inspects, ensuring "zero findings" is never confused with "not checked".

2. **Unified Container Family Tree (`prism_core::tree`)**:
   - Represent container and embedded-object hierarchies via `FamilyTree` and `ContainerNode`.
   - Enforce bounded recursion via `ParseOptions` (`max_container_depth`, `max_container_bytes`, `max_container_members`).
   - Track ancestor and sibling content SHA-256 digests to detect cycles and archive quines (`ContainerNodeStatus::CycleDetected`), and report any resource cap as an explicit `ContainerNodeStatus` (`DepthLimitReached`, `SizeLimitReached`, `MemberLimitReached`) rather than silently dropping members.

## Consequences

- Reviewers get a single, uniform JSON and Rust report across Word, PowerPoint, Excel, PDF, Email, and Archive formats, backed by jump-to-location coordinates in the viewer.
- Container traversal is guaranteed to terminate in bounded memory and time even against recursive zip bombs or quines.
