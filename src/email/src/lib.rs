//! The email message schema and a parser from raw MIME.
//!
//! This crate is pure: it has no Worker runtime and does no I/O. It turns the bytes of one
//! email into a [`Message`] plus the blobs that belong in the blob store. The design is in
//! docs/plans/email.md and docs/plans/ingest.md.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod parse;

pub use parse::parse;

/// The version of the [`Message`] schema. It is stored in every message.
pub const SCHEMA_VERSION: u32 = 1;

/// The document types the classifier picks from. The list is open: a classifier may add more.
pub const DOC_TYPES: &[&str] = &[
    "correspondence",
    "invoice",
    "receipt",
    "bank_statement",
    "card_statement",
    "contract",
    "tax",
    "payslip",
    "booking",
    "shipping",
    "newsletter",
    "notification",
    "other",
];

/// Bytes kept elsewhere (the blob store), named by their content.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ContentRef {
    /// The SHA-256 of the bytes, as 64 lowercase hex digits.
    pub sha256: String,
    /// The number of bytes.
    pub size: u64,
}

impl ContentRef {
    /// Names `bytes` by their SHA-256 and length.
    pub fn of(bytes: &[u8]) -> ContentRef {
        let digest = Sha256::digest(bytes);
        ContentRef {
            sha256: format!("{digest:x}"),
            size: bytes.len() as u64,
        }
    }

    /// The blob store key: `blobs/sha256/{first 2 hex}/{next 2 hex}/{all 64 hex}`.
    pub fn key(&self) -> String {
        let first = self.sha256.get(0..2).unwrap_or_default();
        let next = self.sha256.get(2..4).unwrap_or_default();
        format!("blobs/sha256/{first}/{next}/{}", self.sha256)
    }
}

/// Where a message came from. New sources are added as variants.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    /// Cloudflare Email Routing to an Email Worker.
    EmailRouting {
        /// The SMTP envelope sender.
        envelope_from: String,
        /// The SMTP envelope recipient.
        envelope_to: String,
    },
    /// An IMAP server (iCloud, Proton Mail Bridge, any IMAP).
    Imap {
        /// The server host name.
        server: String,
        /// The account name on the server.
        account: String,
        /// The mailbox the message was read from.
        mailbox: String,
        /// The UIDVALIDITY of the mailbox.
        uid_validity: u32,
        /// The message UID in the mailbox.
        uid: u32,
    },
    /// The Gmail API.
    Gmail {
        /// The account name.
        account: String,
        /// The Gmail message id.
        id: String,
        /// The Gmail thread id, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_id: Option<String>,
        /// The Gmail labels on the message.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        labels: Vec<String>,
    },
    /// A JMAP server (Fastmail).
    Jmap {
        /// The account name.
        account: String,
        /// The JMAP email id.
        id: String,
        /// The ids of the mailboxes holding the message.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mailbox_ids: Vec<String>,
    },
    /// A file given by hand (an .eml upload or an mbox import).
    Upload {
        /// The file name, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

/// One header line as it appears in the message, in order. Every header is kept, repeated
/// ones (Received) included.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Header {
    /// The name as written, such as `Received`.
    pub name: String,
    /// The raw value as written: folding kept, lossy UTF-8, the space after the colon and
    /// the final line break trimmed.
    pub value: String,
    /// The value with RFC 2047 words decoded and folding removed, only when it differs from
    /// `value`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decoded: Option<String>,
}

/// An email address ready for entity linking.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Address {
    /// The display name, decoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The address as written.
    pub address: String,
    /// The address trimmed and lowercased.
    pub normalized: String,
    /// The lowercased part after the last `@`, or empty if there is none.
    pub domain: String,
}

impl Address {
    /// Builds an address from a display name and the address as written.
    pub fn new(name: Option<String>, address: &str) -> Address {
        let normalized = address.trim().to_lowercase();
        let domain = normalized
            .rsplit_once('@')
            .map(|(_, domain)| domain.to_string())
            .unwrap_or_default();
        Address {
            name,
            address: address.to_string(),
            normalized,
            domain,
        }
    }
}

/// The header an address was found in.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The From header.
    From,
    /// The Sender header.
    Sender,
    /// The Reply-To header.
    ReplyTo,
    /// The To header.
    To,
    /// The Cc header.
    Cc,
    /// The Bcc header.
    Bcc,
    /// A Delivered-To header, or the envelope recipient.
    DeliveredTo,
}

/// A file carried by the message: an attachment, an inline part such as an image, or a
/// forwarded message (message/rfc822), kept as bytes in the blob store.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Attachment {
    /// The decoded bytes (after base64 or quoted-printable), by content.
    pub content: ContentRef,
    /// The media type, lowercased `type/subtype`.
    pub content_type: String,
    /// The file name, decoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// `attachment` or `inline`, lowercased, as the part declares it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<String>,
    /// The Content-ID without angle brackets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
    /// The part's index in the parsed message (mail-parser's part id).
    pub part: u32,
}

/// One result from an Authentication-Results header, such as `spf` and `pass`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AuthResult {
    /// The method, lowercased: spf, dkim, dmarc, arc and so on.
    pub method: String,
    /// The result, lowercased: pass, fail and so on.
    pub result: String,
}

/// Facts read from the headers alone, without a model. They feed the taxonomy.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Signals {
    /// In-Reply-To or References is present, or the subject starts with `Re:`.
    #[serde(default)]
    pub is_reply: bool,
    /// The subject starts with `Fwd:` or `Fw:`, or a message/rfc822 part is attached.
    #[serde(default)]
    pub is_forward: bool,
    /// Some part is an attachment, or a non-inline part outside the body.
    #[serde(default)]
    pub has_attachments: bool,
    /// The List-Id value, angle brackets kept as written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_id: Option<String>,
    /// A List-Unsubscribe header is present.
    #[serde(default)]
    pub list_unsubscribe: bool,
    /// The Auto-Submitted value, unless it is `no`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_submitted: Option<String>,
    /// The Precedence value, lowercased.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precedence: Option<String>,
    /// The results of every Authentication-Results header, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auth: Vec<AuthResult>,
    /// The X-Mailer or User-Agent value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailer: Option<String>,
}

/// One judgement about the message, from a rule or a model. `scheme` names the vocabulary,
/// such as `doc_type`, `category` or `language`; new schemes need no schema change.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Label {
    /// The vocabulary the value belongs to.
    pub scheme: String,
    /// The label itself.
    pub value: String,
    /// How sure the judge was, from 0 to 1, if it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    /// Who judged: `rule:...`, or a model id and version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

/// A key field pulled from the message, such as amount, currency, date or counterparty.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Field {
    /// The field name.
    pub name: String,
    /// The field value, as text.
    pub value: String,
    /// Who extracted it: `rule:...`, or a model id and version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

/// The taxonomy: what the message is. `signals` are filled by [`parse`]; `doc_type`, `labels`
/// and `fields` stay empty until a classifier fills them (docs/plans/ingest.md).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Taxonomy {
    /// The document type, usually one of [`DOC_TYPES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_type: Option<String>,
    /// Judgements about the message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<Label>,
    /// Key fields pulled from the message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Field>,
    /// Facts read from the headers.
    #[serde(default)]
    pub signals: Signals,
}

/// One email, as pigeon keeps it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Message {
    /// The schema version, [`SCHEMA_VERSION`].
    pub schema: u32,
    /// The original MIME, byte for byte.
    pub raw: ContentRef,
    /// Where the message came from.
    pub source: Source,
    /// The Message-ID without angle brackets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// The In-Reply-To ids without angle brackets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub in_reply_to: Vec<String>,
    /// The References ids without angle brackets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
    /// The subject, decoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The Date header as RFC 3339 in UTC, such as `2026-11-23T10:15:00Z`, if it parses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// The From addresses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub from: Vec<Address>,
    /// The Sender address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<Address>,
    /// The Reply-To addresses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reply_to: Vec<Address>,
    /// The To addresses, groups flattened.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<Address>,
    /// The Cc addresses, groups flattened.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cc: Vec<Address>,
    /// The Bcc addresses, groups flattened.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bcc: Vec<Address>,
    /// The Delivered-To addresses, plus the envelope recipient of Email Routing if absent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delivered_to: Vec<Address>,
    /// Every header, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<Header>,
    /// The plain text body, for classification and chunking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The decoded bytes of the first HTML body part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<ContentRef>,
    /// The files carried by the message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// What the message is.
    pub taxonomy: Taxonomy,
}

impl Message {
    /// The message as JSON, the form it is stored in.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Reads a message from the JSON that [`Message::to_json`] gives.
    pub fn from_json(json: &str) -> Result<Message, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Every address with the header it came from, de-duplicated by (role, normalized).
    /// Roles come in the order From, Sender, Reply-To, To, Cc, Bcc, Delivered-To.
    pub fn participants(&self) -> Vec<(Role, &Address)> {
        let groups: [(Role, Vec<&Address>); 7] = [
            (Role::From, self.from.iter().collect()),
            (Role::Sender, self.sender.iter().collect()),
            (Role::ReplyTo, self.reply_to.iter().collect()),
            (Role::To, self.to.iter().collect()),
            (Role::Cc, self.cc.iter().collect()),
            (Role::Bcc, self.bcc.iter().collect()),
            (Role::DeliveredTo, self.delivered_to.iter().collect()),
        ];
        let mut out: Vec<(Role, &Address)> = Vec::new();
        for (role, addresses) in groups {
            for address in addresses {
                let seen = out
                    .iter()
                    .any(|(r, a)| *r == role && a.normalized == address.normalized);
                if !seen {
                    out.push((role, address));
                }
            }
        }
        out
    }
}

/// What parsing gives: the message and the bytes to put in the blob store, one entry per
/// distinct sha256 (attachments and the HTML body, not the raw message, which the caller has).
#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    /// The parsed message.
    pub message: Message,
    /// The blobs, in order of first appearance.
    pub blobs: Vec<(ContentRef, Vec<u8>)>,
}

/// Why a message could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The input has no bytes.
    Empty,
    /// The input has no header section, so it is not MIME. The text says why.
    NotMime(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Empty => f.write_str("the message is empty"),
            Error::NotMime(why) => write!(f, "not a MIME message: {why}"),
        }
    }
}

impl std::error::Error for Error {}
