//! camt.056 — FI-to-FI Payment Cancellation Request. One agent asking another
//! to cancel, or return, payments that were already sent.
//!
//! A cancellation request moves no money, so there is **no `amount` column at
//! all** — every monetary column here is `original_*`, describing the payment
//! it asks to undo. Summing a camt.056 tells you how much was *asked back*,
//! not how much moved.
//!
//! Shape:
//!
//! ```text
//! FIToFIPmtCxlReq
//!   Assgnmt              — who assigns the case to whom (Assgnr → Assgne)
//!   Undrlyg (1..n)       — one underlying original message
//!     OrgnlGrpInfAndCxl  — reference to it; can cancel the WHOLE batch (GrpCxl)
//!     TxInf (0..n)       — one transaction to cancel
//! ```
//!
//! A batch-wide cancellation (`GrpCxl` true) may list no transactions at all,
//! so the grain is the statement, as in the status readers: one row per
//! `OrgnlGrpInfAndCxl` and one per `TxInf`, with `scope` naming which. A reader
//! whose grain is the transaction would parse "cancel the entire batch" to
//! zero rows.
//!
//! Grain: one row per cancellation statement.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::wire::{
    self, join, AcctRef, AssignCtx, Case, OrgnlGrpInf, OrgnlTxRef, PartyName, ReasonInfo, RmtInf,
};

// ── serde model: the transaction subtree only ────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TxInf {
    #[serde(rename = "CxlId")]
    pub cxl_id: Option<String>,
    #[serde(rename = "Case")]
    pub case: Option<Case>,
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
    #[serde(rename = "OrgnlIntrBkSttlmAmt")]
    pub orgnl_sttlm_amt: Option<wire::Money>,
    #[serde(rename = "OrgnlIntrBkSttlmDt")]
    pub orgnl_sttlm_dt: Option<String>,
    #[serde(rename = "CxlRsnInf", default)]
    pub rsn_inf: Vec<ReasonInfo>,
    #[serde(rename = "OrgnlTxRef")]
    pub orgnl_tx_ref: Option<OrgnlTxRef>,
}

// ── flattened row ────────────────────────────────────────────────────────────

pub const SCOPE_GROUP: &str = "GROUP";
pub const SCOPE_TRANSACTION: &str = "TRANSACTION";

/// One underlying original message's group block, while it is open — and after
/// it closes, the reference its transactions fall back to. Reset per `Undrlyg`.
#[derive(Debug, Default, Clone)]
pub struct GroupCtx {
    pub grp_cxl_id: Option<String>,
    pub group_cancellation: Option<String>,
    pub number_of_txs: Option<String>,
    pub orgnl_msg_id: Option<String>,
    pub orgnl_msg_nm_id: Option<String>,
    pub reason_code: Option<String>,
    pub reason_info: Vec<String>,
    pub reason_originator: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct CxlRow {
    pub assignment_id: Option<String>,
    pub assignment_created: Option<String>,
    pub assigner: Option<String>,
    pub assignee: Option<String>,
    pub scope: Option<String>,
    pub cancellation_id: Option<String>,
    pub case_id: Option<String>,
    /// `true` when the whole original batch is to be cancelled; as the wire
    /// spelled it.
    pub group_cancellation: Option<String>,
    pub original_number_of_txs: Option<String>,
    pub original_msg_id: Option<String>,
    pub original_msg_name_id: Option<String>,
    pub original_instr_id: Option<String>,
    pub original_end_to_end_id: Option<String>,
    pub original_tx_id: Option<String>,
    pub original_uetr: Option<String>,
    /// The settled amount of the payment to cancel; scaled, never a float.
    pub original_amount: Option<i128>,
    pub original_currency: Option<String>,
    pub original_settlement_date: Option<String>,
    pub cancellation_reason_code: Option<String>,
    pub cancellation_reason_info: Option<String>,
    pub cancellation_originator: Option<String>,
    pub original_debtor_name: Option<String>,
    pub original_debtor_account: Option<String>,
    pub original_creditor_name: Option<String>,
    pub original_creditor_account: Option<String>,
    pub remittance_info: Option<String>,
    pub source_file: Option<String>,
}

fn row_from_group(grp: &GroupCtx, a: &AssignCtx, source: &str) -> CxlRow {
    CxlRow {
        assignment_id: a.id.clone(),
        assignment_created: a.created.clone(),
        assigner: a.assigner.clone(),
        assignee: a.assignee.clone(),
        scope: Some(SCOPE_GROUP.to_string()),
        cancellation_id: grp.grp_cxl_id.clone(),
        group_cancellation: grp.group_cancellation.clone(),
        original_number_of_txs: grp.number_of_txs.clone(),
        original_msg_id: grp.orgnl_msg_id.clone(),
        original_msg_name_id: grp.orgnl_msg_nm_id.clone(),
        cancellation_reason_code: grp.reason_code.clone(),
        cancellation_reason_info: join(&grp.reason_info),
        cancellation_originator: grp.reason_originator.clone(),
        source_file: Some(source.to_string()),
        ..Default::default()
    }
}

pub fn row_from_tx(
    tx: &TxInf,
    a: &AssignCtx,
    grp: &GroupCtx,
    source: &str,
) -> Result<CxlRow, String> {
    let orgnl = tx.orgnl_tx_ref.as_ref();
    let at = |e: String| format!("{source}: {e}");

    // Stated on the transaction, else inside the copy of the original.
    let (original_amount, original_currency) = {
        let own = wire::money(&[tx.orgnl_sttlm_amt.as_ref()]).map_err(at)?;
        if own.0.is_some() {
            own
        } else {
            orgnl
                .map(OrgnlTxRef::amount)
                .transpose()
                .map_err(|e| format!("{source}: {e}"))?
                .unwrap_or((None, None))
        }
    };

    let (reason_code, reason_info, reason_originator) = if tx.rsn_inf.is_empty() {
        (
            grp.reason_code.clone(),
            join(&grp.reason_info),
            grp.reason_originator.clone(),
        )
    } else {
        ReasonInfo::collapse(&tx.rsn_inf)
    };

    Ok(CxlRow {
        assignment_id: a.id.clone(),
        assignment_created: a.created.clone(),
        assigner: a.assigner.clone(),
        assignee: a.assignee.clone(),
        scope: Some(SCOPE_TRANSACTION.to_string()),
        cancellation_id: tx.cxl_id.clone(),
        case_id: tx.case.as_ref().and_then(|c| c.id.clone()),
        group_cancellation: None,
        original_number_of_txs: None,
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
        original_settlement_date: tx
            .orgnl_sttlm_dt
            .clone()
            .or_else(|| orgnl.and_then(|r| r.sttlm_dt.clone())),
        cancellation_reason_code: reason_code,
        cancellation_reason_info: reason_info,
        cancellation_originator: reason_originator,
        original_debtor_name: orgnl
            .and_then(|r| r.dbtr.as_ref())
            .and_then(PartyName::name),
        original_debtor_account: orgnl
            .and_then(|r| r.dbtr_acct.as_ref())
            .and_then(AcctRef::value),
        original_creditor_name: orgnl
            .and_then(|r| r.cdtr.as_ref())
            .and_then(PartyName::name),
        original_creditor_account: orgnl
            .and_then(|r| r.cdtr_acct.as_ref())
            .and_then(AcctRef::value),
        remittance_info: orgnl
            .and_then(|r| r.rmt_inf.as_ref())
            .and_then(RmtInf::text),
        source_file: Some(source.to_string()),
    })
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct CxlStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    assign: AssignCtx,
    grp: GroupCtx,
    /// Whether a `FIToFIPmtCxlReq` container was seen at all.
    saw_request: bool,
    /// `path.len()` at the innermost open container of this family.
    /// A `<TxInf>` outside it belongs to another message and is not a
    /// cancellation: pacs.004 and pacs.007 name their transaction element the
    /// same.
    in_request: Option<usize>,
}

impl<R: BufRead> CxlStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        CxlStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            assign: AssignCtx::default(),
            grp: GroupCtx::default(),
            saw_request: false,
            in_request: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<CxlRow>, Box<dyn Error>> {
        loop {
            self.buf.clear();
            let action = match self.reader.read_event_into(&mut self.buf)? {
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
                    if wire::local(qname.as_ref()) == "OrgnlGrpInfAndCxl"
                        && self.in_request.is_some()
                    {
                        Act::CloseGroup
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
                            "{}: no <FIToFIPmtCxlReq> found — is this a camt.056 \
                             cancellation request?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Tx => {
                    let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "TxInf")?;
                    let tx: TxInf = quick_xml::de::from_str(&xml)?;
                    return Ok(Some(row_from_tx(
                        &tx,
                        &self.assign,
                        &self.grp,
                        &self.source,
                    )?));
                }
                Act::Push(name) => {
                    if name == "FIToFIPmtCxlReq" || name.starts_with("camt.056.") {
                        self.saw_request = true;
                        self.in_request = Some(self.path.len());
                        self.assign = AssignCtx::default();
                        self.grp = GroupCtx::default();
                    }
                    // A new underlying message replaces the previous group
                    // reference, or a transaction would answer under the wrong
                    // original message.
                    if name == "Undrlyg" {
                        self.grp = GroupCtx::default();
                    }
                    self.path.push(name);
                }
                Act::CloseGroup => {
                    self.pop();
                    // The block is complete: emit its row. The reference and the
                    // reason are KEPT for this Undrlyg's transactions.
                    return Ok(Some(row_from_group(&self.grp, &self.assign, &self.source)));
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

    /// Capture assignment- and group-level leaves by path tail.
    fn capture(&mut self, text: &str) {
        if wire::capture_assignment(&mut self.assign, &self.path, text) {
            return;
        }
        let p = &self.path;
        let tail = |suffix: &[&str]| wire::ends_with(p, suffix);

        if tail(&["OrgnlGrpInfAndCxl", "GrpCxlId"]) {
            self.grp.grp_cxl_id = Some(text.to_string());
        } else if tail(&["OrgnlGrpInfAndCxl", "GrpCxl"]) {
            self.grp.group_cancellation = Some(text.to_string());
        } else if tail(&["OrgnlGrpInfAndCxl", "NbOfTxs"]) {
            self.grp.number_of_txs = Some(text.to_string());
        } else if tail(&["OrgnlGrpInfAndCxl", "OrgnlMsgId"]) {
            self.grp.orgnl_msg_id = Some(text.to_string());
        } else if tail(&["OrgnlGrpInfAndCxl", "OrgnlMsgNmId"]) {
            self.grp.orgnl_msg_nm_id = Some(text.to_string());
        } else if tail(&["OrgnlGrpInfAndCxl", "CxlRsnInf", "Rsn", "Cd"])
            || tail(&["OrgnlGrpInfAndCxl", "CxlRsnInf", "Rsn", "Prtry"])
        {
            self.grp.reason_code.get_or_insert_with(|| text.to_string());
        } else if tail(&["OrgnlGrpInfAndCxl", "CxlRsnInf", "AddtlInf"]) {
            self.grp.reason_info.push(text.to_string());
        } else if tail(&["OrgnlGrpInfAndCxl", "CxlRsnInf", "Orgtr", "Nm"]) {
            self.grp.reason_originator = Some(text.to_string());
        }
    }
}

enum Act {
    Eof,
    Tx,
    Push(String),
    Pop,
    CloseGroup,
    Text(String),
    None,
}
