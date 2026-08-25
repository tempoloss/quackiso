//! camt.055 — Customer Payment Cancellation Request. The customer-side twin of
//! camt.056: the initiating party asking its own bank to cancel payments it
//! initiated with a pain.001 or pain.008, before or after execution. The
//! assigner is therefore usually a **customer party**, not a bank.
//!
//! Being pain-side, it has the payment-info level camt.056 lacks:
//!
//! ```text
//! CstmrPmtCxlReq
//!   Assgnmt                — who asks whom (Pty → Agt, typically)
//!   Undrlyg (1..n)
//!     OrgnlGrpInfAndCxl    — the original message; can cancel the whole batch
//!     OrgnlPmtInfAndCxl    — one payment group of it (0..n)
//!       TxInf (0..n)       — one transaction to cancel
//! ```
//!
//! 3 of the 18 real camt.055 files in the corpus cancel at group level only —
//! `GrpCxl` with no transactions — so, as everywhere in this crate, the grain
//! is the statement: one row per group block, per payment-info block, and per
//! transaction, with `scope` naming which.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::wire::{
    self, join, money, AcctRef, AssignCtx, DateOrText, Money, OrgnlTxRef, PartyName, ReasonInfo,
    RmtInf,
};

// ── serde model: the transaction subtree only ────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TxInf {
    #[serde(rename = "CxlId")]
    pub cxl_id: Option<String>,
    #[serde(rename = "OrgnlInstrId")]
    pub orgnl_instr_id: Option<String>,
    #[serde(rename = "OrgnlEndToEndId")]
    pub orgnl_end_to_end_id: Option<String>,
    #[serde(rename = "OrgnlUETR")]
    pub orgnl_uetr: Option<String>,
    #[serde(rename = "OrgnlInstdAmt")]
    pub orgnl_instd_amt: Option<Money>,
    /// The pain.001 side says execution date, the pain.008 side collection date.
    #[serde(rename = "OrgnlReqdExctnDt")]
    pub orgnl_reqd_exctn_dt: Option<DateOrText>,
    #[serde(rename = "OrgnlReqdColltnDt")]
    pub orgnl_reqd_colltn_dt: Option<String>,
    #[serde(rename = "CxlRsnInf", default)]
    pub rsn_inf: Vec<ReasonInfo>,
    #[serde(rename = "OrgnlTxRef")]
    pub orgnl_tx_ref: Option<OrgnlTxRef>,
}

// ── flattened row ────────────────────────────────────────────────────────────

pub const SCOPE_GROUP: &str = "GROUP";
pub const SCOPE_PAYMENT_INFO: &str = "PAYMENT_INFO";
pub const SCOPE_TRANSACTION: &str = "TRANSACTION";

/// One `OrgnlGrpInfAndCxl` block, and after it closes, the reference its
/// payment groups and transactions fall back to. Reset per `Undrlyg`.
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

/// One `OrgnlPmtInfAndCxl` block: which payment group of the original message,
/// possibly with its own original-message reference and reason.
#[derive(Debug, Default, Clone)]
pub struct PmtCtx {
    pub id: Option<String>,
    pub cancellation_id: Option<String>,
    pub orgnl_msg_id: Option<String>,
    pub orgnl_msg_nm_id: Option<String>,
    pub reason_code: Option<String>,
    pub reason_info: Vec<String>,
    pub reason_originator: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct CclRow {
    pub assignment_id: Option<String>,
    pub assignment_created: Option<String>,
    pub assigner: Option<String>,
    pub assignee: Option<String>,
    pub scope: Option<String>,
    pub cancellation_id: Option<String>,
    pub group_cancellation: Option<String>,
    pub original_number_of_txs: Option<String>,
    pub original_msg_id: Option<String>,
    pub original_msg_name_id: Option<String>,
    pub original_payment_info_id: Option<String>,
    pub original_instr_id: Option<String>,
    pub original_end_to_end_id: Option<String>,
    pub original_uetr: Option<String>,
    /// The instructed amount of the payment to cancel; scaled, never a float.
    pub original_amount: Option<i128>,
    pub original_currency: Option<String>,
    /// Execution date on the pain.001 side, collection date on the pain.008 side.
    pub original_execution_date: Option<String>,
    pub cancellation_reason_code: Option<String>,
    pub cancellation_reason_info: Option<String>,
    pub cancellation_originator: Option<String>,
    pub original_debtor_name: Option<String>,
    pub original_creditor_name: Option<String>,
    pub original_creditor_account: Option<String>,
    pub remittance_info: Option<String>,
    pub source_file: Option<String>,
}

fn base_row(a: &AssignCtx, scope: &str, source: &str) -> CclRow {
    CclRow {
        assignment_id: a.id.clone(),
        assignment_created: a.created.clone(),
        assigner: a.assigner.clone(),
        assignee: a.assignee.clone(),
        scope: Some(scope.to_string()),
        source_file: Some(source.to_string()),
        ..Default::default()
    }
}

pub fn row_from_tx(
    tx: &TxInf,
    a: &AssignCtx,
    grp: &GroupCtx,
    pmt: &PmtCtx,
    source: &str,
) -> Result<CclRow, String> {
    let orgnl = tx.orgnl_tx_ref.as_ref();
    let (original_amount, original_currency) = {
        let own = money(&[tx.orgnl_instd_amt.as_ref()]).map_err(|e| format!("{source}: {e}"))?;
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

    // Whole-block reason inheritance: transaction, else its payment group, else
    // the underlying group.
    let (reason_code, reason_info, reason_originator) = if !tx.rsn_inf.is_empty() {
        ReasonInfo::collapse(&tx.rsn_inf)
    } else if pmt.reason_code.is_some() || !pmt.reason_info.is_empty() {
        (
            pmt.reason_code.clone(),
            join(&pmt.reason_info),
            pmt.reason_originator.clone(),
        )
    } else {
        (
            grp.reason_code.clone(),
            join(&grp.reason_info),
            grp.reason_originator.clone(),
        )
    };

    Ok(CclRow {
        cancellation_id: tx.cxl_id.clone(),
        original_msg_id: pmt
            .orgnl_msg_id
            .clone()
            .or_else(|| grp.orgnl_msg_id.clone()),
        original_msg_name_id: pmt
            .orgnl_msg_nm_id
            .clone()
            .or_else(|| grp.orgnl_msg_nm_id.clone()),
        original_payment_info_id: pmt.id.clone(),
        original_instr_id: tx.orgnl_instr_id.clone(),
        original_end_to_end_id: tx.orgnl_end_to_end_id.clone(),
        original_uetr: tx.orgnl_uetr.clone(),
        original_amount,
        original_currency,
        original_execution_date: tx
            .orgnl_reqd_exctn_dt
            .as_ref()
            .and_then(DateOrText::value)
            .or_else(|| tx.orgnl_reqd_colltn_dt.clone()),
        cancellation_reason_code: reason_code,
        cancellation_reason_info: reason_info,
        cancellation_originator: reason_originator,
        original_debtor_name: orgnl
            .and_then(|r| r.dbtr.as_ref())
            .and_then(PartyName::name),
        original_creditor_name: orgnl
            .and_then(|r| r.cdtr.as_ref())
            .and_then(PartyName::name),
        original_creditor_account: orgnl
            .and_then(|r| r.cdtr_acct.as_ref())
            .and_then(AcctRef::value),
        remittance_info: orgnl
            .and_then(|r| r.rmt_inf.as_ref())
            .and_then(RmtInf::text),
        ..base_row(a, SCOPE_TRANSACTION, source)
    })
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct CclStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    assign: AssignCtx,
    grp: GroupCtx,
    pmt: PmtCtx,
    /// Seen anywhere in the file; only the EOF check reads it.
    saw_request: bool,
    /// `path.len()` at the innermost open container of this family.
    /// A `<TxInf>` outside it belongs to another message and is not a customer
    /// cancellation.
    in_request: Option<usize>,
}

impl<R: BufRead> CclStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        CclStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            assign: AssignCtx::default(),
            grp: GroupCtx::default(),
            pmt: PmtCtx::default(),
            saw_request: false,
            in_request: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<CclRow>, Box<dyn Error>> {
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
                    if self.in_request.is_none() {
                        Act::Pop
                    } else if name == "OrgnlGrpInfAndCxl" {
                        Act::CloseGroup
                    } else if name == "OrgnlPmtInfAndCxl" {
                        Act::ClosePmtInf
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
                            "{}: no <CstmrPmtCxlReq> found — is this a camt.055 customer \
                             cancellation request?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Tx => {
                    let xml = wire::record_subtree(
                        &mut self.reader,
                        &mut self.buf,
                        "TxInf",
                        &self.source,
                    )?;
                    let tx: TxInf = quick_xml::de::from_str(&xml)?;
                    return Ok(Some(row_from_tx(
                        &tx,
                        &self.assign,
                        &self.grp,
                        &self.pmt,
                        &self.source,
                    )?));
                }
                Act::Push(name) => {
                    if name == "CstmrPmtCxlReq" || name.starts_with("camt.055.") {
                        self.saw_request = true;
                        self.in_request = Some(self.path.len());
                        self.assign = AssignCtx::default();
                        self.grp = GroupCtx::default();
                        self.pmt = PmtCtx::default();
                    }
                    if name == "Undrlyg" {
                        self.grp = GroupCtx::default();
                        self.pmt = PmtCtx::default();
                    }
                    if name == "OrgnlPmtInfAndCxl" {
                        self.pmt = PmtCtx::default();
                    }
                    self.path.push(name);
                }
                Act::CloseGroup => {
                    self.pop();
                    let mut row = base_row(&self.assign, SCOPE_GROUP, &self.source);
                    row.cancellation_id = self.grp.grp_cxl_id.clone();
                    row.group_cancellation = self.grp.group_cancellation.clone();
                    row.original_number_of_txs = self.grp.number_of_txs.clone();
                    row.original_msg_id = self.grp.orgnl_msg_id.clone();
                    row.original_msg_name_id = self.grp.orgnl_msg_nm_id.clone();
                    row.cancellation_reason_code = self.grp.reason_code.clone();
                    row.cancellation_reason_info = join(&self.grp.reason_info);
                    row.cancellation_originator = self.grp.reason_originator.clone();
                    // The reference and reason are KEPT for this Undrlyg's
                    // payment groups and transactions.
                    return Ok(Some(row));
                }
                Act::ClosePmtInf => {
                    self.pop();
                    let mut row = base_row(&self.assign, SCOPE_PAYMENT_INFO, &self.source);
                    row.cancellation_id = self.pmt.cancellation_id.clone();
                    row.original_payment_info_id = self.pmt.id.clone();
                    row.original_msg_id = self
                        .pmt
                        .orgnl_msg_id
                        .clone()
                        .or_else(|| self.grp.orgnl_msg_id.clone());
                    row.original_msg_name_id = self
                        .pmt
                        .orgnl_msg_nm_id
                        .clone()
                        .or_else(|| self.grp.orgnl_msg_nm_id.clone());
                    row.cancellation_reason_code = self.pmt.reason_code.clone();
                    row.cancellation_reason_info = join(&self.pmt.reason_info);
                    row.cancellation_originator = self.pmt.reason_originator.clone();
                    // Cleared on close as well as on open, so a transaction
                    // outside any payment group inherits nothing stale.
                    self.pmt = PmtCtx::default();
                    return Ok(Some(row));
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

    /// Capture assignment-, group- and payment-group-level leaves by path tail.
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
        } else if tail(&["OrgnlPmtInfAndCxl", "OrgnlPmtInfId"]) {
            self.pmt.id = Some(text.to_string());
        } else if tail(&["OrgnlPmtInfAndCxl", "PmtInfCxlId"]) {
            self.pmt.cancellation_id = Some(text.to_string());
        } else if tail(&["OrgnlPmtInfAndCxl", "OrgnlGrpInf", "OrgnlMsgId"]) {
            self.pmt.orgnl_msg_id = Some(text.to_string());
        } else if tail(&["OrgnlPmtInfAndCxl", "OrgnlGrpInf", "OrgnlMsgNmId"]) {
            self.pmt.orgnl_msg_nm_id = Some(text.to_string());
        } else if tail(&["OrgnlPmtInfAndCxl", "CxlRsnInf", "Rsn", "Cd"])
            || tail(&["OrgnlPmtInfAndCxl", "CxlRsnInf", "Rsn", "Prtry"])
        {
            self.pmt.reason_code.get_or_insert_with(|| text.to_string());
        } else if tail(&["OrgnlPmtInfAndCxl", "CxlRsnInf", "AddtlInf"]) {
            self.pmt.reason_info.push(text.to_string());
        } else if tail(&["OrgnlPmtInfAndCxl", "CxlRsnInf", "Orgtr", "Nm"]) {
            self.pmt.reason_originator = Some(text.to_string());
        }
    }
}

enum Act {
    Eof,
    Tx,
    Push(String),
    Pop,
    CloseGroup,
    ClosePmtInf,
    Text(String),
    None,
}
