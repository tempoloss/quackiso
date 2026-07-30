//! camt.029 — Resolution of Investigation. The answer to a camt.056: whether
//! the cancellation was done, refused, or is pending — and, when refused, why.
//!
//! The answer lives at up to three places, and 9 of the 16 real camt.029 files
//! in the corpus answer at the **message level only**: an `Assgnmt`, a resolved
//! case id, and one `<Sts><Conf>` code — no transaction details at all. A
//! reader whose grain is the transaction parses "your cancellation was done"
//! to zero rows. So the grain is the statement, once more:
//!
//! * one `RESOLUTION` row per message — the assignment plus the confirmation
//!   code (`CNCL` cancelled, `RJCR` cancellation rejected, …);
//! * one `GROUP` row per `OrgnlGrpInfAndSts` inside `CxlDtls`;
//! * one `TRANSACTION` row per `TxInfAndSts`, with its own cancellation status
//!   (`TxCxlSts`) and refusal reason.
//!
//! The RESOLUTION row is emitted when the message's container closes, because
//! `<Sts>` may sit after the details in document order.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::wire::{self, OrgnlGrpInf, OrgnlTxRef, PartyName, ReasonInfo};

// ── serde model: the transaction subtree only ────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TxInfAndSts {
    #[serde(rename = "CxlStsId")]
    pub cxl_sts_id: Option<String>,
    #[serde(rename = "RslvdCase")]
    pub rslvd_case: Option<Case>,
    #[serde(rename = "OrgnlGrpInf")]
    pub orgnl_grp_inf: Option<OrgnlGrpInf>,
    #[serde(rename = "OrgnlInstrId")]
    pub orgnl_instr_id: Option<String>,
    #[serde(rename = "OrgnlEndToEndId")]
    pub orgnl_end_to_end_id: Option<String>,
    #[serde(rename = "OrgnlTxId")]
    pub orgnl_tx_id: Option<String>,
    #[serde(rename = "OrgnlUETR")]
    pub orgnl_uetr: Option<String>,
    /// What happened to the cancellation of this one transaction.
    #[serde(rename = "TxCxlSts")]
    pub tx_cxl_sts: Option<String>,
    #[serde(rename = "CxlStsRsnInf", default)]
    pub rsn_inf: Vec<ReasonInfo>,
    #[serde(rename = "OrgnlTxRef")]
    pub orgnl_tx_ref: Option<OrgnlTxRef>,
}

#[derive(Debug, Deserialize)]
pub struct Case {
    #[serde(rename = "Id")]
    pub id: Option<String>,
}

// ── flattened row ────────────────────────────────────────────────────────────

pub const SCOPE_RESOLUTION: &str = "RESOLUTION";
pub const SCOPE_GROUP: &str = "GROUP";
pub const SCOPE_TRANSACTION: &str = "TRANSACTION";

/// Message-level context: the assignment and the message-level answer.
#[derive(Debug, Default, Clone)]
pub struct MsgCtx {
    pub assignment_id: Option<String>,
    pub created: Option<String>,
    pub assigner: Option<String>,
    pub assignee: Option<String>,
    pub case_id: Option<String>,
    pub confirmation: Option<String>,
}

/// One `CxlDtls` group block: the original message the answer is about.
#[derive(Debug, Default, Clone)]
pub struct GroupCtx {
    pub orgnl_msg_id: Option<String>,
    pub orgnl_msg_nm_id: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct RoiRow {
    pub assignment_id: Option<String>,
    pub assignment_created: Option<String>,
    pub assigner: Option<String>,
    pub assignee: Option<String>,
    pub scope: Option<String>,
    /// The message-level answer, on the RESOLUTION row.
    pub resolution_status: Option<String>,
    pub case_id: Option<String>,
    pub cancellation_status_id: Option<String>,
    /// The per-transaction answer, on TRANSACTION rows.
    pub cancellation_status: Option<String>,
    pub reason_code: Option<String>,
    pub reason_info: Option<String>,
    pub reason_originator: Option<String>,
    pub original_msg_id: Option<String>,
    pub original_msg_name_id: Option<String>,
    pub original_instr_id: Option<String>,
    pub original_end_to_end_id: Option<String>,
    pub original_tx_id: Option<String>,
    pub original_uetr: Option<String>,
    pub original_amount: Option<i128>,
    pub original_currency: Option<String>,
    pub original_settlement_date: Option<String>,
    pub original_debtor_name: Option<String>,
    pub original_creditor_name: Option<String>,
    pub source_file: Option<String>,
}

fn base_row(msg: &MsgCtx, scope: &str, source: &str) -> RoiRow {
    RoiRow {
        assignment_id: msg.assignment_id.clone(),
        assignment_created: msg.created.clone(),
        assigner: msg.assigner.clone(),
        assignee: msg.assignee.clone(),
        scope: Some(scope.to_string()),
        source_file: Some(source.to_string()),
        ..Default::default()
    }
}

pub fn row_from_tx(
    tx: &TxInfAndSts,
    msg: &MsgCtx,
    grp: &GroupCtx,
    source: &str,
) -> Result<RoiRow, String> {
    let orgnl = tx.orgnl_tx_ref.as_ref();
    let (original_amount, original_currency) = orgnl
        .map(OrgnlTxRef::amount)
        .transpose()
        .map_err(|e| format!("{source}: {e}"))?
        .unwrap_or((None, None));
    let (reason_code, reason_info, reason_originator) = ReasonInfo::collapse(&tx.rsn_inf);

    Ok(RoiRow {
        case_id: tx.rslvd_case.as_ref().and_then(|c| c.id.clone()),
        cancellation_status_id: tx.cxl_sts_id.clone(),
        cancellation_status: tx.tx_cxl_sts.clone(),
        reason_code,
        reason_info,
        reason_originator,
        original_msg_id: tx
            .orgnl_grp_inf
            .as_ref()
            .and_then(|g| g.msg_id.clone())
            .or_else(|| grp.orgnl_msg_id.clone()),
        original_msg_name_id: tx
            .orgnl_grp_inf
            .as_ref()
            .and_then(|g| g.msg_nm_id.clone())
            .or_else(|| grp.orgnl_msg_nm_id.clone()),
        original_instr_id: tx.orgnl_instr_id.clone(),
        original_end_to_end_id: tx.orgnl_end_to_end_id.clone(),
        original_tx_id: tx.orgnl_tx_id.clone(),
        original_uetr: tx.orgnl_uetr.clone(),
        original_amount,
        original_currency,
        original_settlement_date: orgnl.and_then(|r| r.sttlm_dt.clone()),
        original_debtor_name: orgnl
            .and_then(|r| r.dbtr.as_ref())
            .and_then(PartyName::name),
        original_creditor_name: orgnl
            .and_then(|r| r.cdtr.as_ref())
            .and_then(PartyName::name),
        ..base_row(msg, SCOPE_TRANSACTION, source)
    })
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct RoiStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    msg: MsgCtx,
    grp: GroupCtx,
    in_resolution: bool,
    /// The RESOLUTION row is emitted once, when the container closes.
    resolution_emitted: bool,
}

impl<R: BufRead> RoiStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        RoiStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            msg: MsgCtx::default(),
            grp: GroupCtx::default(),
            in_resolution: false,
            resolution_emitted: false,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<RoiRow>, Box<dyn Error>> {
        loop {
            self.buf.clear();
            let action = match self.reader.read_event_into(&mut self.buf)? {
                Event::Eof => Act::Eof,
                Event::Start(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if name == "TxInfAndSts" && self.in_resolution {
                        Act::Tx
                    } else {
                        Act::Push(name.into_owned())
                    }
                }
                Event::End(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if !self.in_resolution {
                        Act::Pop
                    } else if name == "OrgnlGrpInfAndSts" {
                        Act::CloseGroup
                    } else if name == "RsltnOfInvstgtn" || name.starts_with("camt.029.") {
                        Act::CloseResolution
                    } else {
                        Act::Pop
                    }
                }
                Event::Text(e) => {
                    let t = e.unescape()?;
                    let t = t.trim();
                    if t.is_empty() {
                        Act::None
                    } else {
                        Act::Text(t.to_string())
                    }
                }
                _ => Act::None,
            };

            match action {
                Act::Eof => {
                    return if self.resolution_emitted {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no <RsltnOfInvstgtn> found — is this a camt.029 resolution \
                             of investigation?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Tx => {
                    let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "TxInfAndSts")?;
                    let tx: TxInfAndSts = quick_xml::de::from_str(&xml)?;
                    return Ok(Some(row_from_tx(&tx, &self.msg, &self.grp, &self.source)?));
                }
                Act::Push(name) => {
                    if name == "RsltnOfInvstgtn" || name.starts_with("camt.029.") {
                        self.in_resolution = true;
                        self.msg = MsgCtx::default();
                        self.grp = GroupCtx::default();
                    }
                    if name == "CxlDtls" {
                        self.grp = GroupCtx::default();
                    }
                    self.path.push(name);
                }
                Act::CloseGroup => {
                    self.path.pop();
                    // The group answer: which original message this block is
                    // about. Kept for the transactions that follow it.
                    let mut row = base_row(&self.msg, SCOPE_GROUP, &self.source);
                    row.original_msg_id = self.grp.orgnl_msg_id.clone();
                    row.original_msg_name_id = self.grp.orgnl_msg_nm_id.clone();
                    return Ok(Some(row));
                }
                Act::CloseResolution => {
                    self.path.pop();
                    self.resolution_emitted = true;
                    let mut row = base_row(&self.msg, SCOPE_RESOLUTION, &self.source);
                    row.resolution_status = self.msg.confirmation.clone();
                    row.case_id = self.msg.case_id.clone();
                    return Ok(Some(row));
                }
                Act::Pop => {
                    self.path.pop();
                }
                Act::Text(t) => self.capture(&t),
                Act::None => {}
            }
        }
    }

    /// Capture assignment- and message-level leaves by path tail.
    fn capture(&mut self, text: &str) {
        let p = &self.path;
        let tail = |suffix: &[&str]| wire::ends_with(p, suffix);

        if tail(&["Assgnmt", "Id"]) {
            self.msg.assignment_id = Some(text.to_string());
        } else if tail(&["Assgnmt", "CreDtTm"]) {
            self.msg.created = Some(text.to_string());
        } else if tail(&["Assgnr", "Agt", "FinInstnId", "BICFI"])
            || tail(&["Assgnr", "Agt", "FinInstnId", "BIC"])
            || tail(&["Assgnr", "Pty", "Nm"])
        {
            self.msg.assigner = Some(text.to_string());
        } else if tail(&["Assgnr", "Agt", "FinInstnId", "ClrSysMmbId", "MmbId"]) {
            self.msg.assigner.get_or_insert_with(|| text.to_string());
        } else if tail(&["Assgne", "Agt", "FinInstnId", "BICFI"])
            || tail(&["Assgne", "Agt", "FinInstnId", "BIC"])
            || tail(&["Assgne", "Pty", "Nm"])
        {
            self.msg.assignee = Some(text.to_string());
        } else if tail(&["Assgne", "Agt", "FinInstnId", "ClrSysMmbId", "MmbId"]) {
            self.msg.assignee.get_or_insert_with(|| text.to_string());
        } else if tail(&["RslvdCase", "Id"]) {
            self.msg.case_id = Some(text.to_string());
        } else if tail(&["Sts", "Conf"]) {
            self.msg.confirmation = Some(text.to_string());
        } else if tail(&["OrgnlGrpInfAndSts", "OrgnlMsgId"]) {
            self.grp.orgnl_msg_id = Some(text.to_string());
        } else if tail(&["OrgnlGrpInfAndSts", "OrgnlMsgNmId"]) {
            self.grp.orgnl_msg_nm_id = Some(text.to_string());
        }
    }
}

enum Act {
    Eof,
    Tx,
    Push(String),
    Pop,
    CloseGroup,
    CloseResolution,
    Text(String),
    None,
}
