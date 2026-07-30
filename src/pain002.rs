//! pain.002 — Customer Payment Status Report. The bank's answer to a pain.001:
//! accepted, rejected, pending, and why.
//!
//! A status report states its status at **three levels**, and this is the whole
//! design problem:
//!
//! ```text
//! OrgnlGrpInfAndSts     GrpSts     — the whole batch
//!   OrgnlPmtInfAndSts   PmtInfSts  — one payment group of the batch
//!     TxInfAndSts       TxSts      — one transaction
//! ```
//!
//! Only the group level is mandatory. A bank that rejects a file outright sends
//! one `GrpSts` and *no transactions at all* — `nivaes-002-bus1.xml` in the
//! corpus says `ACCP` over three transactions and details none of them. A reader
//! whose grain is the transaction returns **zero rows** for that file: the
//! message says "your batch was accepted", SQL says nothing at all, and nothing
//! fails.
//!
//! So the grain is one row per *status statement*, and `status_level` says which
//! level the row came from. Nothing is dropped, and nothing is double-counted
//! either: only transaction rows carry an amount, so `SUM(amount)` is unaffected
//! by the coarser rows. Filter with `WHERE status_level = 'TRANSACTION'` when the
//! transaction grain is what you want.
//!
//! `NbOfTxsPerSts` (a count per status code) is deliberately not read: it is a
//! third grain again, and a count of rejected transactions belongs in a `GROUP BY`
//! over the rows this reader already returns.
//!
//! pain.002.001.01 is **not** supported: it predates this structure entirely
//! (`OrgnlGrpRefInfAndSts`, `OrgnlPmtInf/OrgnlTxRefInfAndSts`) and is rejected by
//! name rather than half-read.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::decimal;
use crate::wire::{self, AcctRef, DateOrText, OrgnlTxRef, PartyName, ReasonInfo, RmtInf};

// ── serde model: the transaction subtree only ────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TxInfAndSts {
    #[serde(rename = "StsId")]
    pub sts_id: Option<String>,
    #[serde(rename = "OrgnlInstrId")]
    pub orgnl_instr_id: Option<String>,
    #[serde(rename = "OrgnlEndToEndId")]
    pub orgnl_end_to_end_id: Option<String>,
    #[serde(rename = "OrgnlUETR")]
    pub orgnl_uetr: Option<String>,
    #[serde(rename = "TxSts")]
    pub tx_sts: Option<String>,
    /// The pre-2019 spellings (`StsRsn`, `StsOrgtr`) are read too.
    #[serde(rename = "StsRsnInf", default)]
    pub rsn_inf: Vec<ReasonInfo>,
    /// When the bank took the instruction on. Date *and* time, unlike the
    /// execution date.
    #[serde(rename = "AccptncDtTm")]
    pub accptnc_dt_tm: Option<String>,
    #[serde(rename = "OrgnlTxRef")]
    pub orgnl_tx_ref: Option<OrgnlTxRef>,
}

// ── flattened row ────────────────────────────────────────────────────────────

/// Which level of the report a row states the status of.
pub const LEVEL_GROUP: &str = "GROUP";
pub const LEVEL_PAYMENT_INFO: &str = "PAYMENT_INFO";
pub const LEVEL_TRANSACTION: &str = "TRANSACTION";

/// Message-level context. Outlives the group element it is read from, because
/// transaction rows further down still need to name the message they answer.
#[derive(Debug, Default, Clone)]
pub struct MsgCtx {
    pub msg_id: Option<String>,
    pub initiating_party: Option<String>,
    pub original_msg_id: Option<String>,
    pub original_msg_name_id: Option<String>,
}

/// A status plus its reason, at whichever level it was stated.
#[derive(Debug, Default, Clone)]
pub struct StsCtx {
    pub id: Option<String>,
    pub status: Option<String>,
    pub reason_code: Option<String>,
    pub reason_info: Vec<String>,
    pub reason_originator: Option<String>,
    pub number_of_txs: Option<String>,
    pub control_sum: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct StsRow {
    pub msg_id: Option<String>,
    pub initiating_party: Option<String>,
    pub original_msg_id: Option<String>,
    pub original_msg_name_id: Option<String>,
    pub status_level: Option<String>,
    pub original_payment_info_id: Option<String>,
    pub status_id: Option<String>,
    pub status: Option<String>,
    pub reason_code: Option<String>,
    pub reason_info: Option<String>,
    pub reason_originator: Option<String>,
    /// As spelled on the wire; a count, not an amount, so it is not parsed.
    pub original_number_of_txs: Option<String>,
    /// Scaled by `10^decimal::SCALE`, like every amount here.
    pub original_control_sum: Option<i128>,
    pub original_instr_id: Option<String>,
    pub original_end_to_end_id: Option<String>,
    pub original_uetr: Option<String>,
    pub amount: Option<i128>,
    pub currency: Option<String>,
    pub requested_execution_date: Option<String>,
    pub debtor_name: Option<String>,
    pub debtor_account: Option<String>,
    pub creditor_name: Option<String>,
    pub creditor_account: Option<String>,
    /// What the rejected or accepted payment was for, as the bank echoed it.
    pub remittance_info: Option<String>,
    pub acceptance_date_time: Option<String>,
    pub source_file: Option<String>,
}

fn join(parts: &[String]) -> Option<String> {
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// One row for a group-level or payment-group-level status. Both carry a status,
/// a reason and the original control totals, and neither carries an amount.
fn row_from_status(
    level: &str,
    sts: &StsCtx,
    msg: &MsgCtx,
    source: &str,
) -> Result<StsRow, String> {
    Ok(StsRow {
        msg_id: msg.msg_id.clone(),
        initiating_party: msg.initiating_party.clone(),
        original_msg_id: msg.original_msg_id.clone(),
        original_msg_name_id: msg.original_msg_name_id.clone(),
        status_level: Some(level.to_string()),
        original_payment_info_id: sts.id.clone(),
        status: sts.status.clone(),
        reason_code: sts.reason_code.clone(),
        reason_info: join(&sts.reason_info),
        reason_originator: sts.reason_originator.clone(),
        original_number_of_txs: sts.number_of_txs.clone(),
        original_control_sum: decimal::scaled_opt(sts.control_sum.as_ref())
            .map_err(|e| format!("{source}: control sum: {e}"))?,
        source_file: Some(source.to_string()),
        ..Default::default()
    })
}

pub fn row_from_tx(
    tx: &TxInfAndSts,
    msg: &MsgCtx,
    pmt: &StsCtx,
    source: &str,
) -> Result<StsRow, String> {
    let orgnl = tx.orgnl_tx_ref.as_ref();
    let (amount, currency) = orgnl
        .map(OrgnlTxRef::amount)
        .transpose()
        .map_err(|e| format!("{source}: {e}"))?
        .unwrap_or((None, None));

    // Inherited as a whole block or not at all: a transaction's own code next to
    // the payment group's explanation would describe a reason nobody stated.
    let (reason_code, reason_info, reason_originator) = if tx.rsn_inf.is_empty() {
        (
            pmt.reason_code.clone(),
            join(&pmt.reason_info),
            pmt.reason_originator.clone(),
        )
    } else {
        ReasonInfo::collapse(&tx.rsn_inf)
    };

    Ok(StsRow {
        msg_id: msg.msg_id.clone(),
        initiating_party: msg.initiating_party.clone(),
        original_msg_id: msg.original_msg_id.clone(),
        original_msg_name_id: msg.original_msg_name_id.clone(),
        status_level: Some(LEVEL_TRANSACTION.to_string()),
        // The payment group a transaction sits in, so a rejected transaction can
        // be traced back to the pain.001 group that asked for it. Early versions
        // put transactions outside any group; then this is NULL, not the id of
        // whichever group happened to come before.
        original_payment_info_id: pmt.id.clone(),
        status_id: tx.sts_id.clone(),
        status: tx.tx_sts.clone(),
        reason_code,
        reason_info,
        reason_originator,
        original_number_of_txs: None,
        original_control_sum: None,
        original_instr_id: tx.orgnl_instr_id.clone(),
        original_end_to_end_id: tx.orgnl_end_to_end_id.clone(),
        original_uetr: tx.orgnl_uetr.clone(),
        amount,
        currency,
        requested_execution_date: orgnl
            .and_then(|r| r.reqd_exctn_dt.as_ref())
            .and_then(DateOrText::value),
        debtor_name: orgnl
            .and_then(|r| r.dbtr.as_ref())
            .and_then(PartyName::name),
        debtor_account: orgnl
            .and_then(|r| r.dbtr_acct.as_ref())
            .and_then(AcctRef::value),
        creditor_name: orgnl
            .and_then(|r| r.cdtr.as_ref())
            .and_then(PartyName::name),
        creditor_account: orgnl
            .and_then(|r| r.cdtr_acct.as_ref())
            .and_then(AcctRef::value),
        remittance_info: orgnl
            .and_then(|r| r.rmt_inf.as_ref())
            .and_then(RmtInf::text),
        acceptance_date_time: tx.accptnc_dt_tm.clone(),
        source_file: Some(source.to_string()),
    })
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct StsStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    msg: MsgCtx,
    grp: StsCtx,
    pmt: StsCtx,
    /// Whether the message's own container (`<CstmrPmtStsRpt>`, or the versioned
    /// name of the early editions) was seen. `<TxInfAndSts>` alone is not
    /// identity: pacs.002 names its transaction element the same.
    in_report: bool,
    /// Whether a status element was seen. pain.002.001.01 passes the container
    /// check and nothing else — its vocabulary is different — and must fail by
    /// name rather than read to zero rows.
    saw_status: bool,
}

impl<R: BufRead> StsStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        StsStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            msg: MsgCtx::default(),
            grp: StsCtx::default(),
            pmt: StsCtx::default(),
            in_report: false,
            saw_status: false,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<StsRow>, Box<dyn Error>> {
        loop {
            self.buf.clear();
            let action = match self.reader.read_event_into(&mut self.buf)? {
                Event::Eof => Act::Eof,
                Event::Start(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if name == "TxInfAndSts" && self.in_report {
                        Act::Tx
                    } else {
                        Act::Push(name.into_owned())
                    }
                }
                Event::End(e) => {
                    // A status element is complete only at its closing tag: its
                    // own status and reason are children, and the transactions it
                    // may contain come after them.
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if name == "OrgnlGrpInfAndSts" && self.in_report {
                        Act::CloseGroup
                    } else if name == "OrgnlPmtInfAndSts" && self.in_report {
                        Act::ClosePmtInf
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
                    return if self.saw_status {
                        Ok(None)
                    } else if self.in_report {
                        Err(format!(
                            "{}: no <OrgnlGrpInfAndSts> found — pain.002.001.01 is a \
                             different structure and is not supported",
                            self.source
                        )
                        .into())
                    } else {
                        Err(format!(
                            "{}: no <CstmrPmtStsRpt> found — is this a pain.002 status \
                             report?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Tx => {
                    self.saw_status = true;
                    let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "TxInfAndSts")?;
                    let tx: TxInfAndSts = quick_xml::de::from_str(&xml)?;
                    return Ok(Some(row_from_tx(&tx, &self.msg, &self.pmt, &self.source)?));
                }
                Act::Push(name) => {
                    if name == "CstmrPmtStsRpt" || name.starts_with("pain.002.") {
                        self.in_report = true;
                    }
                    if name == "OrgnlPmtInfAndSts" {
                        self.pmt = StsCtx::default();
                    }
                    self.path.push(name);
                }
                Act::CloseGroup => {
                    self.path.pop();
                    self.saw_status = true;
                    let row = row_from_status(LEVEL_GROUP, &self.grp, &self.msg, &self.source)?;
                    self.grp = StsCtx::default();
                    return Ok(Some(row));
                }
                Act::ClosePmtInf => {
                    self.path.pop();
                    let row =
                        row_from_status(LEVEL_PAYMENT_INFO, &self.pmt, &self.msg, &self.source)?;
                    // Cleared on close as well as on open: a version that puts
                    // transactions outside any payment group would otherwise
                    // inherit the id of the last group that closed.
                    self.pmt = StsCtx::default();
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

    /// Capture the group-level and payment-group-level leaves by path tail. Each
    /// tail names its container, because the two levels spell their reason blocks
    /// identically and only the container tells them apart.
    fn capture(&mut self, text: &str) {
        let p = &self.path;
        let tail = |suffix: &[&str]| wire::ends_with(p, suffix);

        if tail(&["GrpHdr", "MsgId"]) {
            self.msg.msg_id = Some(text.to_string());
        } else if tail(&["GrpHdr", "InitgPty", "Nm"]) {
            self.msg.initiating_party = Some(text.to_string());
        } else if tail(&["OrgnlGrpInfAndSts", "OrgnlMsgId"]) {
            self.msg.original_msg_id = Some(text.to_string());
        } else if tail(&["OrgnlGrpInfAndSts", "OrgnlMsgNmId"]) {
            self.msg.original_msg_name_id = Some(text.to_string());
        } else if tail(&["OrgnlGrpInfAndSts", "GrpSts"]) {
            self.grp.status = Some(text.to_string());
        } else if tail(&["OrgnlGrpInfAndSts", "OrgnlNbOfTxs"]) {
            self.grp.number_of_txs = Some(text.to_string());
        } else if tail(&["OrgnlGrpInfAndSts", "OrgnlCtrlSum"]) {
            self.grp.control_sum = Some(text.to_string());
        } else if tail(&["OrgnlPmtInfAndSts", "OrgnlPmtInfId"]) {
            self.pmt.id = Some(text.to_string());
        } else if tail(&["OrgnlPmtInfAndSts", "PmtInfSts"]) {
            self.pmt.status = Some(text.to_string());
        } else if tail(&["OrgnlPmtInfAndSts", "OrgnlNbOfTxs"]) {
            self.pmt.number_of_txs = Some(text.to_string());
        } else if tail(&["OrgnlPmtInfAndSts", "OrgnlCtrlSum"]) {
            self.pmt.control_sum = Some(text.to_string());
        } else if let (Some(level), Some(kind)) = (reason_leaf(p), reason_kind(p)) {
            let sts = match level {
                Level::Group => &mut self.grp,
                Level::PmtInf => &mut self.pmt,
            };
            match kind {
                ReasonKind::Code => {
                    if sts.reason_code.is_none() {
                        sts.reason_code = Some(text.to_string());
                    }
                }
                ReasonKind::Info => sts.reason_info.push(text.to_string()),
                ReasonKind::Originator => sts.reason_originator = Some(text.to_string()),
            }
        }
    }
}

enum Level {
    Group,
    PmtInf,
}

enum ReasonKind {
    Code,
    Info,
    Originator,
}

/// Which level a `StsRsnInf` leaf belongs to: the containing element is the only
/// thing that distinguishes a batch-wide reason from one payment group's reason.
fn reason_leaf(path: &[String]) -> Option<Level> {
    let i = path.iter().rposition(|e| e == "StsRsnInf")?;
    match path.get(i.checked_sub(1)?)?.as_str() {
        "OrgnlGrpInfAndSts" => Some(Level::Group),
        "OrgnlPmtInfAndSts" => Some(Level::PmtInf),
        _ => None,
    }
}

fn reason_kind(path: &[String]) -> Option<ReasonKind> {
    let last = path.last()?.as_str();
    let parent = path.get(path.len().checked_sub(2)?)?.as_str();
    match (parent, last) {
        ("Rsn" | "StsRsn", "Cd" | "Prtry") => Some(ReasonKind::Code),
        ("StsRsnInf", "AddtlInf") => Some(ReasonKind::Info),
        ("Orgtr" | "StsOrgtr", "Nm") => Some(ReasonKind::Originator),
        _ => None,
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
