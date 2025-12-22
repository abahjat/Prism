//! Email format parsers
//!
//! Parsers for various email and contact formats:
//! - EML: RFC 822/MIME email messages
//! - MSG: Microsoft Outlook message format
//! - MBOX: Unix mailbox format (multiple emails)
//! - VCF: vCard contact format

pub mod eml;
pub mod ics;
pub mod mbox;
pub mod msg;
pub mod vcf;

pub use eml::EmlParser;
pub use ics::IcsParser;
pub use mbox::MboxParser;
pub use msg::MsgParser;
pub use vcf::VcfParser;
