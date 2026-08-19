//! sniff_iso20022 — inventory a pile of files before choosing a reader.
//!
//! One row per file: what message this is, which quackiso function reads it,
//! and how many transaction-level records are on the wire. The readers answer
//! "give me the rows of a pacs.008"; the sniffer answers the question that
//! comes first — "which of these 4,000 files *are* pacs.008?".
//!
//! Unlike the readers, the sniffer never fails the scan on file content: a
//! truncated download, a stray XSD, a non-ISO payload all produce a row whose
//! `error` column says why, with whatever facts were established before the
//! problem kept. Failing loudly is the readers' job — after the sniffer told
//! you which reader to point where.
//!
//! What a file is gets decided on its first bytes, before quick-xml sees any of
//! them, because quick-xml cannot unread. Three answers: SWIFT MT, when a `{1:`
//! block header or a line-initial `:20:` comes before any markup; not XML at
//! all, when there is no `<` in the prefix; or the XML path below. The middle
//! answer is load-bearing rather than tidy - a file with no markup in it comes
//! back from quick-xml as one text event holding the whole file, so the
//! streaming bound only holds while that case is answered here.
//!
//! MT is identified by its blocks, and [`SniffStream::sniff_mt`] says how: an
//! `mt.nnn` family extended by the block-3 validation flag, a NULL `namespace`
//! and a NULL `created` because MT carries neither, and a `records` count that
//! is the rows the named reader would return.
//!
//! Identity is resolved in the order the corpus demands:
//!
//! 1. the `xmlns` on the `<Document>` element itself — the schema binding;
//! 2. the first child of `<Document>`, which in the earliest editions is the
//!    versioned identifier itself (`<pain.002.001.02>`), and otherwise maps to
//!    a family through the same container names the readers accept;
//! 3. the first ISO-identifier namespace declared on an envelope ancestor —
//!    enveloped messages (BizMsgEnvlp, SWIFTNet DataPDU, Fedwire) often bind
//!    the message namespace on the envelope. `head.001` is never a candidate:
//!    that is the AppHdr envelope header, not the message;
//! 4. a message with no `<Document>` at all — issettled and montran RTGS
//!    traffic puts the container element directly under its own envelope —
//!    is identified by the container, exactly as the readers identify it.
//!
//! Identifiers are recognised by shape (`camt.053.001.02`) wherever they
//! appear, so national variants whose namespace is not the ISO URN — the Swiss
//! `…/camt.029.001.09.ch.03` — still resolve; the raw namespace is reported
//! beside the extracted type.
//!
//! `records` counts the family's record element (`Ntry`, `CdtTrfTxInf`,
//! `DrctDbtTxInf`, `TxInf`, `TxInfAndSts`, `Mndt`, `UndrlygAmdmntDtls`,
//! `UndrlygCxlDtls`, `UndrlygAccptncDtls`) where a reader would turn it into a
//! row, which means `Event::Start` and not `Event::Empty`: a self-closing
//! `<Ntry/>` is on the wire and produces nothing, so it is not counted. Status
//! and cancellation readers emit group-level rows on top of this count, and a
//! family with no repeatable record element — the seven investigation messages,
//! whose grain is the message — reports NULL rather than 0.

use std::error::Error;
use std::io::{BufRead, Cursor, Read};

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::mt;
use crate::wire;

/// One file, sniffed. `error` is NULL when the file is a recognisable ISO
/// 20022 message; the other columns hold whatever was established either way.
#[derive(Debug, Default, Clone)]
pub struct SniffRow {
    pub message_type: Option<String>,
    pub family: Option<String>,
    pub namespace: Option<String>,
    pub msg_id: Option<String>,
    pub created: Option<String>,
    pub records: Option<i64>,
    pub reader: Option<String>,
    pub error: Option<String>,
    pub source_file: Option<String>,
}

/// The transaction-level element names across all supported families. Ten
/// counters run unconditionally; which one is *the* record count is decided
/// at end of file, once the family is known — element names repeat across
/// families, but each family owns exactly one of these.
const RECORD_ELEMS: [&str; 11] = [
    "Ntry",
    "CdtTrfTxInf",
    "CdtTrfTx",
    "DrctDbtTxInf",
    "TxInf",
    "TxInfAndSts",
    "Mndt",
    "UndrlygAmdmntDtls",
    "UndrlygCxlDtls",
    "UndrlygAccptncDtls",
    "Itm",
];

/// The family a `<Document>` child element announces — the same container
/// names the readers accept as message identity, era spellings included, plus
/// the `…V01` suffix the earliest editions appended to the type name.
fn family_of_container(name: &str) -> Option<&'static str> {
    Some(match strip_version_suffix(name) {
        "ClmNonRct" => "camt.027",
        "AddtlPmtInf" => "camt.028",
        "RsltnOfInvstgtn" => "camt.029",
        "NtfctnOfCaseAssgnmt" => "camt.030",
        "RjctInvstgtn" => "camt.031",
        "DbtAuthstnRspn" => "camt.036",
        "DbtAuthstnReq" => "camt.037",
        "BkToCstmrAcctRpt" => "camt.052",
        "BkToCstmrStmt" => "camt.053",
        "BkToCstmrDbtCdtNtfctn" => "camt.054",
        "CstmrPmtCxlReq" => "camt.055",
        "FIToFIPmtCxlReq" => "camt.056",
        "NtfctnToRcv" => "camt.057",
        "ReqToModfyPmt" => "camt.087",
        "FIToFIPmtStsRpt" => "pacs.002",
        "FIToFICstmrDrctDbt" => "pacs.003",
        "PmtRtr" => "pacs.004",
        "FIToFIPmtRvsl" => "pacs.007",
        "FIToFICstmrCdtTrf" => "pacs.008",
        "FICdtTrf" | "FinInstnCdtTrf" | "FinInstToFinInstCdtTrf" => "pacs.009",
        "FIDrctDbt" => "pacs.010",
        "FIToFIPmtStsReq" => "pacs.028",
        "CstmrCdtTrfInitn" => "pain.001",
        "CstmrPmtStsRpt" => "pain.002",
        "CstmrDrctDbtInitn" => "pain.008",
        "MndtInitnReq" => "pain.009",
        "MndtAmdmntReq" => "pain.010",
        "MndtCxlReq" => "pain.011",
        "MndtAccptncRpt" => "pain.012",
        "CdtrPmtActvtnReq" => "pain.013",
        "CdtrPmtActvtnReqStsRpt" => "pain.014",
        _ => return None,
    })
}

/// Which table function reads a family, when one does. NULL for a valid ISO
/// message quackiso has no reader for — that is inventory, not an error.
fn reader_of(family: &str) -> Option<&'static str> {
    Some(match family {
        "camt.027" => "read_camt027",
        "camt.028" => "read_camt028",
        "camt.029" => "read_camt029",
        "camt.030" => "read_camt030",
        "camt.031" => "read_camt031",
        "camt.036" => "read_camt036",
        "camt.037" => "read_camt037",
        "camt.052" | "camt.053" | "camt.054" => "read_iso20022",
        "camt.055" => "read_camt055",
        "camt.056" => "read_camt056",
        "camt.057" => "read_camt057",
        "camt.087" => "read_camt087",
        "pacs.002" => "read_pacs002",
        "pacs.003" => "read_pacs003",
        "pacs.004" => "read_pacs004",
        "pacs.007" => "read_pacs007",
        "pacs.008" => "read_pacs008",
        "pacs.009" => "read_pacs009",
        "pacs.010" => "read_pacs010",
        "pacs.028" => "read_pacs028",
        "pain.001" => "read_pain001",
        "pain.002" => "read_pain002",
        "pain.008" => "read_pain008",
        "pain.009" => "read_pain009",
        "pain.010" => "read_pain010",
        "pain.011" => "read_pain011",
        "pain.012" => "read_pain012",
        "pain.013" => "read_pain013",
        "pain.014" => "read_pain014",
        "mt.103" => "read_mt103",
        "mt.202" => "read_mt202",
        "mt.940" => "read_mt940",
        "mt.942" => "read_mt942",
        _ => return None,
    })
}

/// The transaction-level element of a family. Only meaningful for supported
/// families; an unsupported message gets a NULL count, not a guess.
fn record_elem_of(family: &str) -> Option<&'static str> {
    Some(match family {
        "camt.052" | "camt.053" | "camt.054" => "Ntry",
        "pacs.008" | "pacs.009" | "pain.001" => "CdtTrfTxInf",
        "pacs.003" | "pain.008" | "pacs.010" => "DrctDbtTxInf",
        "pacs.004" | "pacs.007" | "camt.055" | "camt.056" | "pacs.028" => "TxInf",
        "pacs.002" | "pain.002" | "pain.014" | "camt.029" => "TxInfAndSts",
        "pain.009" => "Mndt",
        "pain.010" => "UndrlygAmdmntDtls",
        "pain.011" => "UndrlygCxlDtls",
        "pain.012" => "UndrlygAccptncDtls",
        "pain.013" => "CdtTrfTx",
        "camt.057" => "Itm",
        _ => return None,
    })
}

/// `BkToCstmrStmtV01` → `BkToCstmrStmt`: the first editions suffixed the
/// version onto the type name itself.
fn strip_version_suffix(name: &str) -> &str {
    let b = name.as_bytes();
    if b.len() > 3
        && b[b.len() - 3] == b'V'
        && b[b.len() - 2].is_ascii_digit()
        && b[b.len() - 1].is_ascii_digit()
    {
        &name[..b.len() - 3]
    } else {
        name
    }
}

/// Find a message-definition identifier — `aaaa.nnn.nnn.nn`, fifteen
/// characters — anywhere in `s`. Shape-based on purpose: the identifier
/// appears at the tail of the ISO URN, as an element name in the earliest
/// editions, and embedded in national-variant namespaces.
fn find_identifier(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    for i in 0..b.len().checked_sub(14)? {
        let w = &b[i..i + 15];
        let shaped = w[..4].iter().all(u8::is_ascii_lowercase)
            && w[4] == b'.'
            && w[5..8].iter().all(u8::is_ascii_digit)
            && w[8] == b'.'
            && w[9..12].iter().all(u8::is_ascii_digit)
            && w[12] == b'.'
            && w[13..].iter().all(u8::is_ascii_digit);
        let left_clear = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        let right_clear = i + 15 >= b.len() || !b[i + 15].is_ascii_digit();
        if shaped && left_clear && right_clear {
            return Some(&s[i..i + 15]);
        }
    }
    None
}

/// `camt.053.001.02` → `camt.053`.
fn family_of_identifier(ident: &str) -> &str {
    &ident[..8]
}

/// The first `xmlns`/`xmlns:*` attribute of `e` whose value carries an
/// identifier. Namespace declarations are where schema bindings live; other
/// attributes are never scanned. `head.001` declarations are passed over,
/// not returned: an envelope declares the AppHdr binding *beside* the
/// message binding, and skipping the whole element would lose the latter.
fn identifier_ns(e: &BytesStart) -> Option<String> {
    for attr in e.attributes().with_checks(false).flatten() {
        if !attr.key.as_ref().starts_with(b"xmlns") {
            continue;
        }
        let value = String::from_utf8_lossy(&attr.value);
        match find_identifier(&value) {
            Some(ident) if !ident.starts_with("head.") => return Some(value.into_owned()),
            _ => {}
        }
    }
    None
}

/// The two identity leaves, whichever spelling the family uses. Text and CDATA
/// both feed it, already trimmed and never empty.
fn probe_identity(row: &mut SniffRow, path: &[String], t: &str) {
    if row.msg_id.is_none()
        && (wire::ends_with(path, &["GrpHdr", "MsgId"])
            || wire::ends_with(path, &["Assgnmt", "Id"]))
    {
        row.msg_id = Some(t.to_string());
    } else if row.created.is_none()
        && (wire::ends_with(path, &["GrpHdr", "CreDtTm"])
            || wire::ends_with(path, &["Assgnmt", "CreDtTm"]))
    {
        row.created = Some(t.to_string());
    }
}

/// How many bytes the shape decision may look at, and how much of a file a
/// non-XML verdict is worth: one `Source` buffer.
const PREFIX_BYTES: u64 = 64 * 1024;

/// What the first bytes of a file say it is. Deciding this before the parser
/// sees a byte is not an optimisation: quick-xml cannot unread, and a file with
/// no markup in it comes back as one text event holding the whole file, so the
/// streaming bound only holds while this branch is taken first.
enum Shape {
    /// A `{1:` block header, or a line-initial `:20:`, ahead of any markup.
    Mt,
    /// No `<` anywhere in the prefix.
    NotXml,
    Xml,
}

fn shape_of(prefix: &[u8]) -> Shape {
    let mt = [find_bytes(prefix, b"{1:"), field_20(prefix)]
        .into_iter()
        .flatten()
        .min();
    let markup = prefix.iter().position(|b| *b == b'<');
    match (mt, markup) {
        (Some(at), Some(lt)) if at < lt => Shape::Mt,
        (Some(_), None) => Shape::Mt,
        (_, None) => Shape::NotXml,
        _ => Shape::Xml,
    }
}

/// The first line-initial `:20:`, the transaction reference a bare statement
/// body opens with. Position, not prefix: `jejik/abnamro.sta` writes three
/// preamble lines above its first `:20:`.
fn field_20(prefix: &[u8]) -> Option<usize> {
    match prefix.starts_with(b":20:") {
        true => Some(0),
        false => find_bytes(prefix, b"\n:20:").map(|at| at + 1),
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

pub struct SniffStream<R: BufRead> {
    /// Taken by the first `next_row`; `None` afterwards, which is what makes the
    /// second call the end of the file.
    input: Option<R>,
    source: String,
}

impl<R: BufRead> SniffStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        SniffStream {
            input: Some(reader),
            source: source.to_string(),
        }
    }

    /// One row per file, always: content problems land in `error`, they never
    /// abort the scan. (An unopenable file still aborts — that is I/O, and
    /// the shared machinery reports it before this reader exists.)
    pub fn next_row(&mut self) -> Result<Option<SniffRow>, Box<dyn Error>> {
        let Some(input) = self.input.take() else {
            return Ok(None);
        };
        Ok(Some(self.sniff(input)))
    }

    fn sniff(&self, mut input: R) -> SniffRow {
        let mut row = SniffRow {
            source_file: Some(self.source.clone()),
            ..SniffRow::default()
        };

        // What the file is, decided on its first bytes. They are read rather than
        // peeked because a `BufRead` refills only what it was handed -- the magic
        // check above this leaves two bytes in the first fill -- and handed back
        // below, because quick-xml cannot unread. Gzip is unwrapped underneath,
        // so these are decompressed bytes.
        let mut prefix = Vec::new();
        if let Err(e) = (&mut input).take(PREFIX_BYTES).read_to_end(&mut prefix) {
            row.error = Some(format!("cannot read: {e}"));
            return row;
        }
        let shape = shape_of(&prefix);
        let stream = Cursor::new(prefix).chain(input);
        match shape {
            Shape::Mt => {
                self.sniff_mt(stream, &mut row);
                return row;
            }
            Shape::NotXml => {
                row.error = Some("not XML: no markup in the first 64 KiB".to_string());
                return row;
            }
            Shape::Xml => {}
        }
        let mut reader = Reader::from_reader(stream);
        let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
        // element local-names from root to cursor
        let mut path: Vec<String> = Vec::with_capacity(16);
        // the most recent identifier-bearing namespace on any ancestor,
        // envelope headers excluded
        let mut ancestor_ns: Option<String> = None;
        let mut document_seen = false;
        // a recognised top-level container outside any <Document>: issettled
        // and montran envelopes carry the message without a Document wrapper
        let mut container: Option<String> = None;
        // whether the cursor has entered message content — Document or a
        // recognised container — which is where GrpHdr/Assgnmt leaves live
        let mut in_message = false;
        // the next start element after <Document> is its first child
        let mut awaiting_child = false;
        let mut doc_child: Option<String> = None;
        let mut counts = [0i64; RECORD_ELEMS.len()];
        let mut broke: Option<String> = None;

        loop {
            buf.clear();
            let ev = match reader.read_event_into(&mut buf) {
                Ok(ev) => ev,
                Err(e) => {
                    broke = Some(format!("not well-formed XML: {e}"));
                    break;
                }
            };
            // A self-closing element has no matching End, so it never enters
            // the path -- and no reader turns one into a row, so it is not a
            // record either. One flag, both meanings.
            let push = matches!(ev, Event::Start(_));
            match ev {
                Event::Eof => {
                    if let Some(open) = wire::cut_short(&path) {
                        broke = Some(format!("not well-formed XML: end of input inside <{open}>"));
                    }
                    break;
                }
                Event::Start(e) | Event::Empty(e) => {
                    let name = wire::local(e.name().as_ref()).into_owned();
                    if push {
                        if let Some(i) = RECORD_ELEMS.iter().position(|r| *r == name) {
                            counts[i] += 1;
                        }
                    }
                    if awaiting_child {
                        doc_child = Some(name.clone());
                        awaiting_child = false;
                    }
                    if name == "Document" && !document_seen {
                        document_seen = true;
                        in_message = true;
                        awaiting_child = true;
                        // the Document's own binding wins over any ancestor's
                        if let Some(ns) = identifier_ns(&e) {
                            row.namespace = Some(ns);
                        } else {
                            row.namespace = ancestor_ns.take();
                        }
                    } else {
                        if row.namespace.is_none() && ancestor_ns.is_none() {
                            ancestor_ns = identifier_ns(&e);
                        }
                        if !document_seen
                            && container.is_none()
                            && family_of_container(&name).is_some()
                        {
                            container = Some(name.clone());
                            in_message = true;
                        }
                    }
                    if push {
                        path.push(name);
                    }
                }
                Event::End(_) => {
                    path.pop();
                }
                ev => {
                    if in_message && (row.msg_id.is_none() || row.created.is_none()) {
                        match wire::event_text(&ev) {
                            Ok(Some(t)) => probe_identity(&mut row, &path, &t),
                            Ok(None) => {}
                            Err(e) => {
                                broke = Some(format!("not well-formed XML: {e}"));
                                break;
                            }
                        }
                    }
                }
            }
        }

        // a Document-less message: the binding the envelope declared is the
        // only namespace there is
        if !document_seen {
            row.namespace = ancestor_ns.take();
        }
        // what announces the message: the Document's first child, or the
        // bare container an envelope carried without a Document
        let announce = if document_seen {
            &doc_child
        } else {
            &container
        };

        // identity: the message namespace, else the era element that *is*
        // the identifier, else the container name the readers accept
        if let Some(ident) = row.namespace.as_deref().and_then(find_identifier) {
            row.message_type = Some(ident.to_string());
        } else if let Some(ident) = announce.as_deref().and_then(find_identifier) {
            row.message_type = Some(ident.to_string());
        }
        row.family = row
            .message_type
            .as_deref()
            .map(|i| family_of_identifier(i).to_string())
            .or_else(|| {
                announce
                    .as_deref()
                    .and_then(family_of_container)
                    .map(str::to_string)
            });
        row.reader = row
            .family
            .as_deref()
            .and_then(reader_of)
            .map(str::to_string);
        // a count from a file that broke mid-stream would be a plausible lie
        if broke.is_none() {
            row.records = row
                .family
                .as_deref()
                .and_then(record_elem_of)
                .and_then(|elem| RECORD_ELEMS.iter().position(|r| *r == elem))
                .map(|i| counts[i]);
        }

        row.error = broke.or_else(|| {
            // A family resolved from a namespace is not yet a message: an
            // envelope may declare the binding beside no content at all, and
            // routing that to a reader is the abort this function prevents.
            if !document_seen && container.is_none() && row.records.unwrap_or(0) == 0 {
                Some(
                    "no ISO 20022 message found — no <Document> and no known message \
                     container"
                        .to_string(),
                )
            } else if row.family.is_none() {
                match announce {
                    Some(c) => Some(format!(
                        "unrecognised message: <Document> child <{c}> matches no known \
                         ISO 20022 message type"
                    )),
                    None => Some("empty <Document> — no message content".to_string()),
                }
            } else {
                None
            }
        });
        row
    }

    /// Fill the row from a SWIFT MT file.
    ///
    /// MT has no namespace and no ISO timestamp, so `namespace` and `created`
    /// stay NULL; the family is the MT number, dotted so it stays a prefix of
    /// `message_type` the way `pain.013` is a prefix of `pain.013.001.12`.
    /// `records` is what the named reader would return, which is why the whole
    /// file is read - the same reason the XML path reads it. The peak is one
    /// message, the bound the MT readers have.
    fn sniff_mt(&self, stream: impl BufRead, row: &mut SniffRow) {
        let mut reader = mt::MtReader::new(stream, &self.source);
        let mut kind: Option<String> = None;
        let mut records = 0i64;

        loop {
            let msg = match reader.next_message() {
                Ok(Some(msg)) => msg,
                Ok(None) => break,
                Err(e) => {
                    row.error = Some(e.to_string());
                    return;
                }
            };
            let body = mt::block(&msg, 4).unwrap_or(&msg);
            let (fields, entries) = mt::Fields::without_entries(body, "61");
            if row.msg_id.is_none() {
                row.msg_id = fields.value("20").map(str::to_string);
            }

            if kind.is_none() {
                // Block 2 names the type. A bare body has none, and then the
                // mandatory field it carries names it instead, statement types
                // first: those are the ones banks ship without headers.
                kind = mt::message_type(&msg).map(str::to_string).or_else(|| {
                    ["940", "942", "103", "202"]
                        .into_iter()
                        .find(|number| mt::claims(&msg, &fields, number))
                        .map(str::to_string)
                });
                if let Some(number) = kind.as_deref() {
                    let family = format!("mt.{number}");
                    row.message_type = Some(match mt::user_header_field(&msg, "119") {
                        Some(flag) => format!("{family}.{}", flag.to_lowercase()),
                        None => family.clone(),
                    });
                    row.reader = reader_of(&family).map(str::to_string);
                    row.family = Some(family);
                }
            }

            let Some(number) = kind.as_deref() else {
                continue;
            };
            if !mt::claims(&msg, &fields, number) {
                continue;
            }
            records += match number {
                // A claimed MT940 statement with no entries in it still produces
                // one row, the balances the bank did report; MT942 produces none,
                // because an interim report with nothing in it is nothing.
                "940" => entries.max(1) as i64,
                "942" => entries as i64,
                _ => 1,
            };
        }

        match kind.is_some() {
            // A recognised MT number with no reader is inventory, exactly as an
            // ISO family with no reader is, and it has no row count to report.
            true => row.records = row.reader.as_ref().map(|_| records),
            false => {
                row.error = Some(
                    "unrecognised SWIFT MT message: no block 2 and no statement field \
                     names it"
                        .to_string(),
                )
            }
        }
    }
}
