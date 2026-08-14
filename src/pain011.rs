//! pain.011 — Mandate Cancellation Request. Ending a mandate: after this the
//! creditor may not collect again.
//!
//! Shape:
//!
//! ```text
//! MndtCxlReq
//!   GrpHdr                  — the header all four mandate messages share
//!   UndrlygCxlDtls (1..n)   — one cancellation per record
//!     CxlRsn                — the code, and the text when the code is NARR
//!     OrgnlMndt             — the mandate being cancelled: its id, or the
//!                             whole thing at OrgnlMndt/OrgnlMndt
//! ```
//!
//! A cancellation names an existing mandate, so there is no
//! `mandate_request_id` here. The mandate-detail columns are populated only
//! when the sender repeated the mandate: the id-only form is legal and
//! complete, and every detail column being NULL is the correct reading of it.
//!
//! Grain: one row per `UndrlygCxlDtls`.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::pain009::{capture_group_header, GrpCtx, Mndt};
use crate::pain010::OrgnlMndt;
use crate::wire::{self, ReasonInfo};

// ── serde model: the record subtree only ─────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct UndrlygCxlDtls {
    #[serde(rename = "CxlRsn", default)]
    pub cxl_rsn: Vec<ReasonInfo>,
    #[serde(rename = "OrgnlMndt")]
    pub orgnl_mndt: Option<OrgnlMndt>,
}

// ── flattened row ────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct MndtCxlRow {
    pub msg_id: Option<String>,
    pub created: Option<String>,
    pub initiating_party: Option<String>,
    pub instructing_agent_bic: Option<String>,
    pub instructed_agent_bic: Option<String>,
    pub cancellation_reason: Option<String>,
    /// What NARR means in this message: the code says "see the text".
    pub cancellation_reason_info: Option<String>,
    pub original_mandate_id: Option<String>,
    /// From here down: the cancelled mandate, when the sender repeated it.
    pub creditor_name: Option<String>,
    pub creditor_account: Option<String>,
    pub debtor_name: Option<String>,
    pub debtor_account: Option<String>,
    pub debtor_agent_bic: Option<String>,
    pub ultimate_debtor_name: Option<String>,
    pub source_file: Option<String>,
}

pub fn row_from_cancellation(c: &UndrlygCxlDtls, ctx: &GrpCtx, source: &str) -> MndtCxlRow {
    let (cancellation_reason, cancellation_reason_info, _) = ReasonInfo::collapse(&c.cxl_rsn);
    let orgnl = c.orgnl_mndt.as_ref().and_then(|o| o.mndt.as_ref());

    MndtCxlRow {
        msg_id: ctx.msg_id.clone(),
        created: ctx.created.clone(),
        initiating_party: ctx.initiating_party.clone(),
        instructing_agent_bic: ctx.instructing_agent_bic.clone(),
        instructed_agent_bic: ctx.instructed_agent_bic.clone(),
        cancellation_reason,
        cancellation_reason_info,
        original_mandate_id: c.orgnl_mndt.as_ref().and_then(OrgnlMndt::id),
        creditor_name: orgnl.and_then(Mndt::creditor_name),
        creditor_account: orgnl.and_then(Mndt::creditor_account),
        debtor_name: orgnl.and_then(Mndt::debtor_name),
        debtor_account: orgnl.and_then(Mndt::debtor_account),
        debtor_agent_bic: orgnl.and_then(Mndt::debtor_agent_bic),
        ultimate_debtor_name: orgnl.and_then(Mndt::ultimate_debtor_name),
        source_file: Some(source.to_string()),
    }
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct MndtCxlStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    ctx: GrpCtx,
    saw_request: bool,
    /// `path.len()` at the innermost open container of this family.
    in_request: Option<usize>,
}

impl<R: BufRead> MndtCxlStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        MndtCxlStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            ctx: GrpCtx::default(),
            saw_request: false,
            in_request: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<MndtCxlRow>, Box<dyn Error>> {
        loop {
            self.buf.clear();
            let action = match self.reader.read_event_into(&mut self.buf)? {
                Event::Eof => Act::Eof,
                Event::Start(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if name == "UndrlygCxlDtls" && self.in_request.is_some() {
                        Act::Cancellation
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
                            "{}: no <MndtCxlReq> found — is this a pain.011 mandate \
                             cancellation request?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Cancellation => {
                    let xml =
                        wire::record_subtree(&mut self.reader, &mut self.buf, "UndrlygCxlDtls")?;
                    let c: UndrlygCxlDtls = quick_xml::de::from_str(&xml)?;
                    return Ok(Some(row_from_cancellation(&c, &self.ctx, &self.source)));
                }
                Act::Push(name) => {
                    if name == "MndtCxlReq" || name.starts_with("pain.011.") {
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
    Cancellation,
    Push(String),
    Pop,
    Text(String),
    None,
}
