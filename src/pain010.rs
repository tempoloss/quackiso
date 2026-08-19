//! pain.010 — Mandate Amendment Request. Changing a mandate that already
//! exists: a new collection amount, a new account, a new creditor.
//!
//! Shape:
//!
//! ```text
//! MndtAmdmntReq
//!   GrpHdr                     — the header all four mandate messages share
//!   UndrlygAmdmntDtls (1..n)   — one amendment per record
//!     AmdmntRsn                — why, and who asked
//!     Mndt                     — what the mandate BECOMES
//!     OrgnlMndt/OrgnlMndt      — the mandate as it stands, when repeated
//! ```
//!
//! The record carries both states, so a row names the mandate it changes
//! (`original_mandate_id`) beside what it changes it to. The mandate columns
//! are the **new** state: an amendment whose new `Mndt` states only the id and
//! the new IBAN leaves the rest NULL, because that is what changes.
//!
//! Grain: one row per `UndrlygAmdmntDtls`.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::pain009::{capture_group_header, GrpCtx, Mndt};
use crate::wire::{self, ReasonInfo};

// ── serde model: the record subtree only ─────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct UndrlygAmdmntDtls {
    #[serde(rename = "AmdmntRsn", default)]
    pub amdmnt_rsn: Vec<ReasonInfo>,
    #[serde(rename = "Mndt")]
    pub mndt: Option<Mndt>,
    #[serde(rename = "OrgnlMndt")]
    pub orgnl_mndt: Option<OrgnlMndt>,
}

/// The original mandate: either just its id, or the whole thing repeated in a
/// same-named element nested inside the choice wrapper. Shared with pain.011
/// and pain.012, which spell it identically.
#[derive(Debug, Deserialize)]
pub struct OrgnlMndt {
    #[serde(rename = "OrgnlMndtId")]
    pub orgnl_mndt_id: Option<String>,
    #[serde(rename = "OrgnlMndt")]
    pub mndt: Option<Mndt>,
}

impl OrgnlMndt {
    /// The id, from whichever arm the sender used.
    pub fn id(&self) -> Option<String> {
        self.orgnl_mndt_id
            .clone()
            .or_else(|| self.mndt.as_ref().and_then(|m| m.mndt_id.clone()))
    }
}

// ── flattened row ────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct AmdmntRow {
    pub msg_id: Option<String>,
    pub created: Option<String>,
    pub initiating_party: Option<String>,
    pub instructing_agent_bic: Option<String>,
    pub instructed_agent_bic: Option<String>,
    pub amendment_reason: Option<String>,
    pub amendment_originator: Option<String>,
    pub original_mandate_id: Option<String>,
    /// From here down: the mandate as amended, never the original.
    pub mandate_id: Option<String>,
    pub sequence_type: Option<String>,
    pub frequency: Option<String>,
    pub collection_amount: Option<i128>,
    pub currency: Option<String>,
    pub creditor_name: Option<String>,
    pub creditor_account: Option<String>,
    pub debtor_name: Option<String>,
    pub debtor_account: Option<String>,
    pub debtor_agent_bic: Option<String>,
    pub source_file: Option<String>,
}

pub fn row_from_amendment(
    a: &UndrlygAmdmntDtls,
    ctx: &GrpCtx,
    source: &str,
) -> Result<AmdmntRow, String> {
    let (amendment_reason, _, amendment_originator) = ReasonInfo::collapse(&a.amdmnt_rsn);
    let new = a.mndt.as_ref();
    let (collection_amount, currency) = new
        .map(Mndt::amount)
        .transpose()
        .map_err(|e| format!("{source}: {e}"))?
        .unwrap_or((None, None));

    Ok(AmdmntRow {
        msg_id: ctx.msg_id.clone(),
        created: ctx.created.clone(),
        initiating_party: ctx.initiating_party.clone(),
        instructing_agent_bic: ctx.instructing_agent_bic.clone(),
        instructed_agent_bic: ctx.instructed_agent_bic.clone(),
        amendment_reason,
        amendment_originator,
        original_mandate_id: a.orgnl_mndt.as_ref().and_then(OrgnlMndt::id),
        mandate_id: new.and_then(|m| m.mndt_id.clone()),
        sequence_type: new.and_then(Mndt::sequence_type),
        frequency: new.and_then(Mndt::frequency),
        collection_amount,
        currency,
        creditor_name: new.and_then(Mndt::creditor_name),
        creditor_account: new.and_then(Mndt::creditor_account),
        debtor_name: new.and_then(Mndt::debtor_name),
        debtor_account: new.and_then(Mndt::debtor_account),
        debtor_agent_bic: new.and_then(Mndt::debtor_agent_bic),
        source_file: Some(source.to_string()),
    })
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct AmdmntStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    ctx: GrpCtx,
    saw_request: bool,
    /// `path.len()` at the innermost open container of this family.
    in_request: Option<usize>,
}

impl<R: BufRead> AmdmntStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        AmdmntStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            ctx: GrpCtx::default(),
            saw_request: false,
            in_request: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<AmdmntRow>, Box<dyn Error>> {
        loop {
            self.buf.clear();
            let action = match wire::next_event(
                &mut self.reader,
                &mut self.buf,
                &self.path,
                &self.source,
            )? {
                Event::Eof => Act::Eof,
                Event::Start(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if name == "UndrlygAmdmntDtls" && self.in_request.is_some() {
                        Act::Amendment
                    } else {
                        Act::Push(name.into_owned())
                    }
                }
                Event::End(_) => Act::Pop,
                ev => match wire::event_text(&ev)? {
                    Some(t) => Act::Text(t),
                    None => Act::None,
                },
            };

            match action {
                Act::Eof => {
                    return if self.saw_request {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no <MndtAmdmntReq> found — is this a pain.010 mandate \
                             amendment request?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Amendment => {
                    let xml =
                        wire::record_subtree(&mut self.reader, &mut self.buf, "UndrlygAmdmntDtls")?;
                    let a: UndrlygAmdmntDtls = quick_xml::de::from_str(&xml)?;
                    return Ok(Some(row_from_amendment(&a, &self.ctx, &self.source)?));
                }
                Act::Push(name) => {
                    if name == "MndtAmdmntReq" || name.starts_with("pain.010.") {
                        self.saw_request = true;
                        self.in_request = Some(self.path.len());
                        self.ctx = GrpCtx::default();
                    }
                    self.path.push(name);
                }
                Act::Pop => self.pop(),
                Act::Text(t) => self.capture(&t),
                Act::None => {}
            }
        }
    }

    fn pop(&mut self) {
        self.path.pop();
        if self.in_request == Some(self.path.len()) {
            self.in_request = None;
        }
    }

    fn capture(&mut self, text: &str) {
        capture_group_header(&mut self.ctx, &self.path, text);
    }
}

enum Act {
    Eof,
    Amendment,
    Push(String),
    Pop,
    Text(String),
    None,
}
