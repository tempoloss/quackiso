//! pacs.028 - FI-to-FI Payment Status Request. One bank asking another for the
//! status of a payment it already sent: the "where is my money?" message, and
//! the last payments-family grain quackiso did not cover.
//!
//! Structurally it is pacs.002 with the answer removed. A request carries no
//! status and no reason, only the references that identify the original payment
//! and, optionally, a carried copy of it (`OrgnlTxRef`). Like every exception
//! message it asks at two grains:
//!
//! * one `TRANSACTION` row per `TxInf`;
//! * one `GROUP` row when the message names a whole original message
//!   (message-level `OrgnlGrpInf`) and details no transactions - a request for
//!   the status of an entire batch. A reader whose grain is the transaction
//!   parses "where is batch X?" to zero rows.
//!
//! The GROUP row is emitted once, when the container closes, and only if no
//! transaction row was produced for that message: when transactions are present
//! they already inherit the message-level reference, so a separate group row
//! would be redundant. As in pacs.002, one Document may hold several
//! `FIToFIPmtStsReq` blocks; all carried context resets at each.
//!
//! Grain: one row per status request. `scope` is `GROUP` or `TRANSACTION`.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::wire::{self, OrgnlGrpInf, OrgnlTxRef, PartyName};

// ── serde model: the transaction subtree only ────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TxInf {
    #[serde(rename = "StsReqId")]
    pub sts_req_id: Option<String>,
    /// A request may reference the original message per transaction, exactly as
    /// CBPR+-era pacs.002 does.
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
    #[serde(rename = "OrgnlTxRef")]
    pub orgnl_tx_ref: Option<OrgnlTxRef>,
}

// ── flattened row ────────────────────────────────────────────────────────────

pub const SCOPE_GROUP: &str = "GROUP";
pub const SCOPE_TRANSACTION: &str = "TRANSACTION";

/// Message-level context: the requesting pair and the original message the
/// request is about. Reset at every `FIToFIPmtStsReq`.
#[derive(Debug, Default, Clone)]
pub struct MsgCtx {
    pub msg_id: Option<String>,
    pub instg_bic: Option<String>,
    pub instd_bic: Option<String>,
    pub orgnl_msg_id: Option<String>,
    pub orgnl_msg_nm_id: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct StsReqRow {
    pub msg_id: Option<String>,
    pub instructing_agent_bic: Option<String>,
    pub instructed_agent_bic: Option<String>,
    pub scope: Option<String>,
    pub status_request_id: Option<String>,
    pub original_msg_id: Option<String>,
    pub original_msg_name_id: Option<String>,
    pub original_instr_id: Option<String>,
    pub original_end_to_end_id: Option<String>,
    pub original_tx_id: Option<String>,
    pub original_uetr: Option<String>,
    /// From the carried copy of the original; scaled, never a float.
    pub original_amount: Option<i128>,
    pub original_currency: Option<String>,
    pub original_settlement_date: Option<String>,
    pub original_debtor_name: Option<String>,
    pub original_creditor_name: Option<String>,
    pub source_file: Option<String>,
}

fn row_from_group(msg: &MsgCtx, source: &str) -> StsReqRow {
    StsReqRow {
        msg_id: msg.msg_id.clone(),
        instructing_agent_bic: msg.instg_bic.clone(),
        instructed_agent_bic: msg.instd_bic.clone(),
        scope: Some(SCOPE_GROUP.to_string()),
        original_msg_id: msg.orgnl_msg_id.clone(),
        original_msg_name_id: msg.orgnl_msg_nm_id.clone(),
        source_file: Some(source.to_string()),
        ..Default::default()
    }
}

pub fn row_from_tx(tx: &TxInf, msg: &MsgCtx, source: &str) -> Result<StsReqRow, String> {
    let orgnl = tx.orgnl_tx_ref.as_ref();
    let (original_amount, original_currency) = orgnl
        .map(OrgnlTxRef::amount)
        .transpose()
        .map_err(|e| format!("{source}: {e}"))?
        .unwrap_or((None, None));

    Ok(StsReqRow {
        msg_id: msg.msg_id.clone(),
        instructing_agent_bic: msg.instg_bic.clone(),
        instructed_agent_bic: msg.instd_bic.clone(),
        scope: Some(SCOPE_TRANSACTION.to_string()),
        status_request_id: tx.sts_req_id.clone(),
        original_msg_id: tx
            .orgnl_grp_inf
            .as_ref()
            .and_then(|g| g.msg_id.clone())
            .or_else(|| msg.orgnl_msg_id.clone()),
        original_msg_name_id: tx
            .orgnl_grp_inf
            .as_ref()
            .and_then(|g| g.msg_nm_id.clone())
            .or_else(|| msg.orgnl_msg_nm_id.clone()),
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
        source_file: Some(source.to_string()),
    })
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct StsReqStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    msg: MsgCtx,
    /// Whether a `FIToFIPmtStsReq` container was seen at all.
    saw_request: bool,
    /// `path.len()` at the innermost open container of this family. A `<TxInf>`
    /// outside it belongs to another message: pacs.004, pacs.007, camt.055 and
    /// camt.056 all name their transaction element the same.
    in_request: Option<usize>,
    /// Whether a transaction row was produced for the current message. When it
    /// is false at container close, the message asked at the group grain and
    /// gets one GROUP row.
    tx_emitted: bool,
}

impl<R: BufRead> StsReqStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        StsReqStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            msg: MsgCtx::default(),
            saw_request: false,
            in_request: None,
            tx_emitted: false,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<StsReqRow>, Box<dyn Error>> {
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
                    if name == "TxInf" && self.in_request.is_some() {
                        Act::Tx
                    } else {
                        Act::Push(name.into_owned())
                    }
                }
                Event::End(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if self.in_request.is_some()
                        && (name == "FIToFIPmtStsReq" || name.starts_with("pacs.028."))
                    {
                        Act::CloseRequest
                    } else {
                        Act::Pop
                    }
                }
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
                            "{}: no <FIToFIPmtStsReq> found - is this a pacs.028 payment \
                             status request?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Tx => {
                    let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "TxInf")?;
                    let tx: TxInf = quick_xml::de::from_str(&xml)?;
                    self.tx_emitted = true;
                    return Ok(Some(row_from_tx(&tx, &self.msg, &self.source)?));
                }
                Act::Push(name) => {
                    // One Document may hold several complete requests; nothing
                    // may leak from one into the next.
                    if name == "FIToFIPmtStsReq" || name.starts_with("pacs.028.") {
                        self.saw_request = true;
                        self.in_request = Some(self.path.len());
                        self.msg = MsgCtx::default();
                        self.tx_emitted = false;
                    }
                    self.path.push(name);
                }
                Act::CloseRequest => {
                    self.pop();
                    // A request that named no transaction asked at the group
                    // grain: emit one GROUP row so the ask is not invisible.
                    // When transactions were emitted their rows already carry
                    // the message-level reference, so no group row is produced.
                    if !self.tx_emitted {
                        return Ok(Some(row_from_group(&self.msg, &self.source)));
                    }
                }
                Act::Pop => {
                    self.pop();
                }
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

    /// Capture message-level leaves by path tail. Per-transaction copies live
    /// inside the `<TxInf>` subtree, which never enters `path`, so the
    /// message-level `OrgnlGrpInf` tail cannot match a transaction's.
    fn capture(&mut self, text: &str) {
        let p = &self.path;
        let tail = |suffix: &[&str]| wire::ends_with(p, suffix);

        if tail(&["GrpHdr", "MsgId"]) {
            self.msg.msg_id = Some(text.to_string());
        } else if tail(&["GrpHdr", "InstgAgt", "FinInstnId", "BICFI"])
            || tail(&["GrpHdr", "InstgAgt", "FinInstnId", "BIC"])
        {
            self.msg.instg_bic = Some(text.to_string());
        } else if tail(&["GrpHdr", "InstgAgt", "FinInstnId", "ClrSysMmbId", "MmbId"])
            || tail(&["GrpHdr", "InstgAgt", "FinInstnId", "Othr", "Id"])
        {
            self.msg.instg_bic.get_or_insert_with(|| text.to_string());
        } else if tail(&["GrpHdr", "InstdAgt", "FinInstnId", "BICFI"])
            || tail(&["GrpHdr", "InstdAgt", "FinInstnId", "BIC"])
        {
            self.msg.instd_bic = Some(text.to_string());
        } else if tail(&["GrpHdr", "InstdAgt", "FinInstnId", "ClrSysMmbId", "MmbId"])
            || tail(&["GrpHdr", "InstdAgt", "FinInstnId", "Othr", "Id"])
        {
            self.msg.instd_bic.get_or_insert_with(|| text.to_string());
        } else if tail(&["OrgnlGrpInf", "OrgnlMsgId"]) {
            self.msg.orgnl_msg_id = Some(text.to_string());
        } else if tail(&["OrgnlGrpInf", "OrgnlMsgNmId"]) {
            self.msg.orgnl_msg_nm_id = Some(text.to_string());
        }
    }
}

enum Act {
    Eof,
    Tx,
    Push(String),
    Pop,
    CloseRequest,
    Text(String),
    None,
}
