// SPDX-License-Identifier: AGPL-3.0-only
//! Unified container and embedded-object family tree (`FamilyTree`).
//!
//! Represents nested archive members, email attachments, embedded OLE objects/packages,
//! PDF embedded files/portfolios, and listed embedded fonts/media with bounded depth,
//! total expanded byte size, member count, and SHA-256 cycle/quine detection.

use crate::hash::{md5_hex, sha1_hex, sha256_hex};
use serde::{Deserialize, Serialize};

/// Default maximum container recursion depth.
pub const DEFAULT_MAX_CONTAINER_DEPTH: u32 = 8;
/// Default maximum cumulative expanded bytes across a container family tree (`256 MiB`).
pub const DEFAULT_MAX_CONTAINER_BYTES: u64 = 256 * 1024 * 1024;
/// Default maximum total member nodes across a container family tree.
pub const DEFAULT_MAX_CONTAINER_MEMBERS: usize = 1_000;

/// Resource limits for container and embedded-object recursion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerLimits {
    /// Maximum recursion depth (`0` = root only).
    pub max_depth: u32,
    /// Maximum cumulative expanded bytes across all child members.
    pub max_total_bytes: u64,
    /// Maximum total child member nodes in the tree.
    pub max_members: usize,
}

impl Default for ContainerLimits {
    fn default() -> Self {
        Self {
            max_depth: DEFAULT_MAX_CONTAINER_DEPTH,
            max_total_bytes: DEFAULT_MAX_CONTAINER_BYTES,
            max_members: DEFAULT_MAX_CONTAINER_MEMBERS,
        }
    }
}

/// Unified container and embedded-object family tree rooted at a source document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FamilyTree {
    /// Root document node (`depth == 0`, `id == "0"`).
    pub root: ContainerNode,
    /// Total number of descendant nodes in the tree (excluding root).
    pub total_members: usize,
    /// Total expanded bytes across all descendant nodes.
    pub total_expanded_bytes: u64,
    /// Active resource limits used when constructing this tree.
    pub limits: ContainerLimits,
}

impl FamilyTree {
    /// Construct a new [`FamilyTree`] from its `root` node and `limits`.
    #[must_use]
    pub fn new(root: ContainerNode, limits: ContainerLimits) -> Self {
        let mut total_members = 0;
        let mut total_expanded_bytes = 0;
        Self::accumulate_stats(&root, &mut total_members, &mut total_expanded_bytes);
        Self {
            root,
            total_members,
            total_expanded_bytes,
            limits,
        }
    }

    fn accumulate_stats(node: &ContainerNode, members: &mut usize, bytes: &mut u64) {
        for child in &node.children {
            *members = members.saturating_add(1);
            *bytes = bytes.saturating_add(child.size);
            Self::accumulate_stats(child, members, bytes);
        }
    }

    /// Find a node by its hierarchical `id` (e.g. `"0"`, `"0/0"`, `"0/1/0"`) or `path`.
    #[must_use]
    pub fn find_node(&self, id_or_path: &str) -> Option<&ContainerNode> {
        Self::find_in_node(&self.root, id_or_path)
    }

    fn find_in_node<'a>(node: &'a ContainerNode, id_or_path: &str) -> Option<&'a ContainerNode> {
        if node.id == id_or_path || node.path == id_or_path {
            return Some(node);
        }
        for child in &node.children {
            if let Some(found) = Self::find_in_node(child, id_or_path) {
                return Some(found);
            }
        }
        None
    }

    /// Flatten all nodes (root followed by descendants in pre-order traversal).
    #[must_use]
    pub fn flatten(&self) -> Vec<&ContainerNode> {
        let mut out = Vec::with_capacity(self.total_members + 1);
        Self::flatten_rec(&self.root, &mut out);
        out
    }

    fn flatten_rec<'a>(node: &'a ContainerNode, out: &mut Vec<&'a ContainerNode>) {
        out.push(node);
        for child in &node.children {
            Self::flatten_rec(child, out);
        }
    }
}

/// A single node in a [`FamilyTree`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerNode {
    /// Deterministic hierarchical identifier (e.g. `"0"` for root, `"0/0"`, `"0/1"` for children).
    pub id: String,
    /// Path within the parent container (e.g. `"attachments/invoice.pdf"`, `"word/embeddings/oleObject1.bin"`).
    pub path: String,
    /// Display filename or object name.
    pub name: String,
    /// Detected format identifier (e.g. `"pdf"`, `"docx"`, `"zip"`, `"font"`, `"media"`).
    pub detected_format: String,
    /// MIME type string.
    pub mime_type: String,
    /// Uncompressed size in bytes.
    pub size: u64,
    /// Lowercase hex MD5 digest of the node's raw bytes.
    pub md5: String,
    /// Lowercase hex SHA-1 digest of the node's raw bytes.
    pub sha1: String,
    /// Lowercase hex SHA-256 digest of the node's raw bytes.
    pub sha256: String,
    /// Nesting depth (`0` for the root document).
    pub depth: u32,
    /// Parse and expansion status of this node.
    pub status: ContainerNodeStatus,
    /// Child nodes in deterministic order.
    pub children: Vec<ContainerNode>,
}

impl ContainerNode {
    /// Construct a new [`ContainerNode`] computing `MD5`, `SHA-1`, and `SHA-256` from `data`.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn from_bytes(
        id: impl Into<String>,
        path: impl Into<String>,
        name: impl Into<String>,
        detected_format: impl Into<String>,
        mime_type: impl Into<String>,
        data: &[u8],
        depth: u32,
        status: ContainerNodeStatus,
    ) -> Self {
        Self {
            id: id.into(),
            path: path.into(),
            name: name.into(),
            detected_format: detected_format.into(),
            mime_type: mime_type.into(),
            size: data.len() as u64,
            md5: md5_hex(data),
            sha1: sha1_hex(data),
            sha256: sha256_hex(data),
            depth,
            status,
            children: Vec::new(),
        }
    }
}

/// Expansion and parse outcome for a [`ContainerNode`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ContainerNodeStatus {
    /// Successfully parsed (and expanded if a container).
    Parsed,
    /// Resource (such as an embedded font or media asset) listed in the tree but not recursively expanded.
    ListedOnly,
    /// Recursion stopped because `depth` reached `max_depth`.
    DepthLimitReached {
        /// Configured maximum container depth.
        max_depth: u32,
    },
    /// Recursion stopped because cumulative expanded bytes would exceed `max_bytes`.
    SizeLimitReached {
        /// Configured maximum cumulative container bytes.
        max_bytes: u64,
    },
    /// Recursion stopped because total member count reached `max_members`.
    MemberLimitReached {
        /// Configured maximum container member count.
        max_members: usize,
    },
    /// Recursion stopped because this node's SHA-256 matches an ancestor container (cycle / quine).
    CycleDetected {
        /// SHA-256 digest of the repeated ancestor container.
        ancestor_sha256: String,
    },
    /// Node is password-protected / encrypted and could not be expanded.
    Encrypted,
    /// Node failed to parse or decompress.
    Error {
        /// Diagnostic error description.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_searches_family_tree() {
        let mut root = ContainerNode::from_bytes(
            "0",
            "root.zip",
            "root.zip",
            "zip",
            "application/zip",
            b"root-bytes",
            0,
            ContainerNodeStatus::Parsed,
        );
        let child = ContainerNode::from_bytes(
            "0/0",
            "inner/doc.txt",
            "doc.txt",
            "txt",
            "text/plain",
            b"hello world",
            1,
            ContainerNodeStatus::Parsed,
        );
        root.children.push(child);

        let tree = FamilyTree::new(root, ContainerLimits::default());
        assert_eq!(tree.total_members, 1);
        assert_eq!(tree.total_expanded_bytes, 11);
        assert!(tree.find_node("0/0").is_some());
        assert!(tree.find_node("inner/doc.txt").is_some());
        assert_eq!(tree.flatten().len(), 2);
    }
}
