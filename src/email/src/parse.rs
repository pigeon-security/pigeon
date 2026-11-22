//! The parser from raw MIME to a [`Message`], on top of mail-parser.

use std::collections::HashSet;

use mail_parser::{
    AddressList, HeaderForm, MessageParser, MessageRef, PartKind, PartRole, TextList,
};

use crate::{
    Address, Attachment, AuthResult, ContentRef, Error, Header, Message, Parsed, SCHEMA_VERSION,
    Signals, Source, Taxonomy,
};

/// Parses `raw` MIME from `source`. The same bytes and source always give the same result.
pub fn parse(raw: &[u8], source: Source) -> Result<Parsed, Error> {
    if raw.is_empty() {
        return Err(Error::Empty);
    }
    let parsed = MessageParser::default()
        .parse(raw)
        .ok_or_else(|| Error::NotMime("no header section".to_string()))?;
    let root = parsed.root();
    if root.headers().is_empty() {
        return Err(Error::NotMime("no headers".to_string()));
    }

    let headers: Vec<Header> = root
        .headers()
        .iter()
        .map(|h| {
            let value = clean_raw(h.raw_value());
            let decoded = HeaderForm::Text
                .parse(h.raw_value())
                .value()
                .as_text()
                .map(str::to_string)
                .filter(|d| *d != value);
            Header {
                name: h.raw_name().to_string(),
                value,
                decoded,
            }
        })
        .collect();

    let mut blobs: Vec<(ContentRef, Vec<u8>)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut keep = |bytes: &[u8]| -> ContentRef {
        let content = ContentRef::of(bytes);
        if seen.insert(content.sha256.clone()) {
            blobs.push((content.clone(), bytes.to_vec()));
        }
        content
    };

    let html = root
        .html_body()
        .find(|p| matches!(p.kind(), PartKind::Html))
        .map(|p| keep(&p.decoded()));

    let mut attachments = Vec::new();
    let mut has_attachments = false;
    for part in root.parts() {
        let carried = match part.role() {
            PartRole::Attachment | PartRole::MediaBody => true,
            PartRole::Dropped => matches!(
                part.kind(),
                PartKind::Binary | PartKind::InlineBinary | PartKind::Message(_)
            ),
            _ => false,
        };
        if !carried || part.is_multipart() {
            continue;
        }
        let disposition = part
            .content_disposition()
            .map(|d| d.ctype().trim().to_ascii_lowercase());
        if !part.is_inline() || disposition.as_deref() == Some("attachment") {
            has_attachments = true;
        }
        let content_type = match part.content_type() {
            Some(ct) => match ct.subtype() {
                Some(sub) => format!("{}/{}", ct.ctype(), sub).to_ascii_lowercase(),
                None => ct.ctype().to_ascii_lowercase(),
            },
            None if part.is_message() => "message/rfc822".to_string(),
            None => "application/octet-stream".to_string(),
        };
        attachments.push(Attachment {
            content: keep(&part.decoded()),
            content_type,
            filename: part.attachment_name().map(str::to_string),
            disposition,
            content_id: part.content_id().map(strip_brackets),
            part: part.id(),
        });
    }

    let subject = root.subject().map(str::to_string);
    let in_reply_to = ids(root.in_reply_to());
    let references = ids(root.references());
    let lower_subject = subject
        .as_deref()
        .map(|s| s.trim_start().to_ascii_lowercase())
        .unwrap_or_default();

    let mut delivered_to: Vec<Address> = Vec::new();
    for h in root.headers().iter() {
        if h.raw_name().eq_ignore_ascii_case("delivered-to") {
            let parsed = HeaderForm::Addresses.parse(h.raw_value());
            delivered_to.extend(addresses(parsed.value().as_address()));
        }
    }
    if let Source::EmailRouting { envelope_to, .. } = &source {
        let extra = Address::new(None, envelope_to);
        let present = delivered_to
            .iter()
            .any(|a| a.normalized == extra.normalized);
        if !extra.normalized.is_empty() && !present {
            delivered_to.push(extra);
        }
    }

    let signals = Signals {
        is_reply: !in_reply_to.is_empty()
            || !references.is_empty()
            || has_header(&headers, "in-reply-to")
            || has_header(&headers, "references")
            || lower_subject.starts_with("re:"),
        is_forward: lower_subject.starts_with("fwd:")
            || lower_subject.starts_with("fw:")
            || attachments
                .iter()
                .any(|a| a.content_type == "message/rfc822"),
        has_attachments,
        list_id: header_value(&headers, "list-id"),
        list_unsubscribe: has_header(&headers, "list-unsubscribe"),
        auto_submitted: header_value(&headers, "auto-submitted")
            .filter(|v| !v.eq_ignore_ascii_case("no")),
        precedence: header_value(&headers, "precedence").map(|v| v.to_ascii_lowercase()),
        auth: auth_results(&headers),
        mailer: header_value(&headers, "x-mailer").or_else(|| header_value(&headers, "user-agent")),
    };

    let message = Message {
        schema: SCHEMA_VERSION,
        raw: ContentRef::of(raw),
        source,
        message_id: root.message_id().map(strip_brackets),
        in_reply_to,
        references,
        subject,
        date: date(&root),
        from: root.all_from().flat_map(|l| addresses(Some(l))).collect(),
        sender: addresses(root.sender()).into_iter().next(),
        reply_to: addresses(root.reply_to()),
        to: root.all_to().flat_map(|l| addresses(Some(l))).collect(),
        cc: root.all_cc().flat_map(|l| addresses(Some(l))).collect(),
        bcc: root.all_bcc().flat_map(|l| addresses(Some(l))).collect(),
        delivered_to,
        headers,
        text: root.body_text(0).map(|t| t.into_owned()),
        html,
        attachments,
        taxonomy: Taxonomy {
            signals,
            ..Taxonomy::default()
        },
    };
    Ok(Parsed { message, blobs })
}

/// A raw header value as text: lossy UTF-8, the space after the colon and the final line
/// break removed.
fn clean_raw(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw)
        .trim_start_matches([' ', '\t'])
        .trim_end_matches(['\r', '\n'])
        .to_string()
}

/// Removes angle brackets and spaces around an id.
fn strip_brackets(id: &str) -> String {
    id.trim().trim_matches(['<', '>']).to_string()
}

/// The ids of a Message-ID style list, without angle brackets.
fn ids(list: Option<TextList<'_>>) -> Vec<String> {
    list.map(|l| l.iter().map(strip_brackets).collect())
        .unwrap_or_default()
}

/// The addresses of a list, groups flattened. Entries without an address are skipped.
fn addresses(list: Option<AddressList<'_>>) -> Vec<Address> {
    let Some(list) = list else {
        return Vec::new();
    };
    list.mailboxes()
        .filter_map(|m| {
            let address = m.address()?;
            Some(Address::new(m.name().map(str::to_string), address))
        })
        .collect()
}

/// The Date header as RFC 3339 in UTC.
fn date(root: &MessageRef<'_>) -> Option<String> {
    let date = root.date()?;
    if !date.is_valid() {
        return None;
    }
    Some(mail_parser::DateTime::from_timestamp(date.to_timestamp()).to_rfc3339())
}

/// Whether a header with this name is present.
fn has_header(headers: &[Header], name: &str) -> bool {
    headers.iter().any(|h| h.name.eq_ignore_ascii_case(name))
}

/// The value of the first header with this name, unfolded and trimmed. Empty values are
/// treated as absent.
fn header_value(headers: &[Header], name: &str) -> Option<String> {
    let h = headers.iter().find(|h| h.name.eq_ignore_ascii_case(name))?;
    let value = h.value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!value.is_empty()).then_some(value)
}

/// The method and result pairs of every Authentication-Results header, in order. The first
/// segment of each header is the server id and is skipped.
fn auth_results(headers: &[Header]) -> Vec<AuthResult> {
    let mut out = Vec::new();
    for h in headers
        .iter()
        .filter(|h| h.name.eq_ignore_ascii_case("authentication-results"))
    {
        for segment in h.value.split(';').skip(1) {
            let Some(first) = segment.split_whitespace().next() else {
                continue;
            };
            let Some((method, result)) = first.split_once('=') else {
                continue;
            };
            let result = if result.is_empty() {
                // "spf= pass": the result follows a space.
                let rest = segment.trim_start();
                let after = rest.split_once('=').map(|(_, r)| r).unwrap_or_default();
                after.split_whitespace().next().unwrap_or_default()
            } else {
                result
            };
            if method.is_empty() || result.is_empty() {
                continue;
            }
            out.push(AuthResult {
                method: method.to_ascii_lowercase(),
                result: result.to_ascii_lowercase(),
            });
        }
    }
    out
}
