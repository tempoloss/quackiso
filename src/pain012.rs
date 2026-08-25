//! pain.012 — Mandate Acceptance Report. The answer to a pain.009, pain.010
//! or pain.011: the debtor's bank saying whether the mandate request was
//! accepted, and why not when it was not.
//!
//! Shape:
//!
//! ```text
//! MndtAccptncRpt
//!   GrpHdr                        — the header all four mandate messages share
//!   UndrlygAccptncDtls (1..n)     — one answer per record
//!     OrgnlMsgInf                 — which message is being answered
//!     AccptncRslt                 — Accptd true/false, and RjctRsn when false
//!     OrgnlMndt                   — the mandate, id-only or repeated in full
//! ```
//!
//! `accepted` is text, as the wire spelled it, the same discipline as
//! `group_cancellation` in read_camt056: a boolean column would have to invent
//! a value for the absent case. `original_msg_name_id` is the only thing that
//! says *which* mandate message this answers — the same report shape acknowledges
//! an initiation, an amendment and a cancellation.
//!
//! Grain: one row per `UndrlygAccptncDtls`.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::pain009::{capture_group_header, GrpCtx, Mndt};
use crate::pain010::OrgnlMndt;
use crate::wire::{self, Reason};

// ── serde model: the record subtree only ─────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct UndrlygAccptncDtls {
    #[serde(rename = "OrgnlMsgInf")]
    pub orgnl_msg_inf: Option<OrgnlMsgInf>,
    #[serde(rename = "AccptncRslt")]
    pub accptnc_rslt: Option<AccptncRslt>,
    #[serde(rename = "OrgnlMndt")]
    pub orgnl_mndt: Option<OrgnlMndt>,
}

#[derive(Debug, Deserialize)]
pub struct OrgnlMsgInf {
    #[serde(rename = "MsgId")]
    pub msg_id: Option<String>,
    #[serde(rename = "MsgNmId")]
    pub msg_nm_id: Option<String>,
    #[serde(rename = "CreDtTm")]
    pub cre_dt_tm: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AccptncRslt {
    #[serde(rename = "Accptd")]
    pub accptd: Option<String>,
    #[serde(rename = "RjctRsn", default)]
    pub rjct_rsn: Vec<Reason>,
}

// ── flattened row ────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct AccptncRow {
    pub msg_id: Option<String>,
    pub created: Option<String>,
    pub initiating_party: Option<String>,
    pub instructing_agent_bic: Option<String>,
    pub instructed_agent_bic: Option<String>,
    pub original_msg_id: Option<String>,
    /// `pain.009.001.03`, `pain.011.001.03`, … — which request is answered.
    pub original_msg_name_id: Option<String>,
    pub original_created: Option<String>,
    /// As the wire spelled it: "true" or "false".
    pub accepted: Option<String>,
    pub rejection_reason: Option<String>,
    pub original_mandate_id: Option<String>,
    /// From here down: the mandate, when the report repeated it.
    pub sequence_type: Option<String>,
    pub frequency: Option<String>,
    pub first_collection_date: Option<String>,
    pub creditor_name: Option<String>,
    pub creditor_account: Option<String>,
    pub creditor_agent_bic: Option<String>,
    pub debtor_name: Option<String>,
    pub debtor_account: Option<String>,
    pub debtor_agent_bic: Option<String>,
    pub referred_document_number: Option<String>,
    pub source_file: Option<String>,
}

pub fn row_from_acceptance(a: &UndrlygAccptncDtls, ctx: &GrpCtx, source: &str) -> AccptncRow {
    let orgnl_msg = a.orgnl_msg_inf.as_ref();
    let rslt = a.accptnc_rslt.as_ref();
    let mndt = a.orgnl_mndt.as_ref().and_then(|o| o.mndt.as_ref());

    AccptncRow {
        msg_id: ctx.msg_id.clone(),
        created: ctx.created.clone(),
        initiating_party: ctx.initiating_party.clone(),
        instructing_agent_bic: ctx.instructing_agent_bic.clone(),
        instructed_agent_bic: ctx.instructed_agent_bic.clone(),
        original_msg_id: orgnl_msg.and_then(|o| o.msg_id.clone()),
        original_msg_name_id: orgnl_msg.and_then(|o| o.msg_nm_id.clone()),
        original_created: orgnl_msg.and_then(|o| o.cre_dt_tm.clone()),
        accepted: rslt.and_then(|r| r.accptd.clone()),
        rejection_reason: rslt.and_then(|r| r.rjct_rsn.iter().find_map(Reason::code)),
        original_mandate_id: a.orgnl_mndt.as_ref().and_then(OrgnlMndt::id),
        sequence_type: mndt.and_then(Mndt::sequence_type),
        frequency: mndt.and_then(Mndt::frequency),
        first_collection_date: mndt.and_then(Mndt::first_collection_date),
        creditor_name: mndt.and_then(Mndt::creditor_name),
        creditor_account: mndt.and_then(Mndt::creditor_account),
        creditor_agent_bic: mndt.and_then(Mndt::creditor_agent_bic),
        debtor_name: mndt.and_then(Mndt::debtor_name),
        debtor_account: mndt.and_then(Mndt::debtor_account),
        debtor_agent_bic: mndt.and_then(Mndt::debtor_agent_bic),
        referred_document_number: mndt.and_then(Mndt::referred_document_number),
        source_file: Some(source.to_string()),
    }
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct AccptncStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    ctx: GrpCtx,
    saw_report: bool,
    /// `path.len()` at the innermost open container of this family.
    in_report: Option<usize>,
}

impl<R: BufRead> AccptncStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        AccptncStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            ctx: GrpCtx::default(),
            saw_report: false,
            in_report: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<AccptncRow>, Box<dyn Error>> {
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
                    if name == "UndrlygAccptncDtls" && self.in_report.is_some() {
                        Act::Acceptance
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
                    return if self.saw_report {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no <MndtAccptncRpt> found — is this a pain.012 mandate \
                             acceptance report?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Acceptance => {
                    let xml = wire::record_subtree(
                        &mut self.reader,
                        &mut self.buf,
                        "UndrlygAccptncDtls",
                        &self.source,
                    )?;
                    let a: UndrlygAccptncDtls = quick_xml::de::from_str(&xml)?;
                    return Ok(Some(row_from_acceptance(&a, &self.ctx, &self.source)));
                }
                Act::Push(name) => {
                    if name == "MndtAccptncRpt" || name.starts_with("pain.012.") {
                        self.saw_report = true;
                        self.in_report = Some(self.path.len());
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
        if self.in_report == Some(self.path.len()) {
            self.in_report = None;
        }
    }

    fn capture(&mut self, text: &str) {
        capture_group_header(&mut self.ctx, &self.path, text);
    }
}

enum Act {
    Eof,
    Acceptance,
    Push(String),
    Pop,
    Text(String),
    None,
}
