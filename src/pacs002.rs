//! pacs.002 — FI-to-FI Payment Status Report. What one bank tells another about
//! an instruction it received: accepted, rejected, pending, and why. The
//! interbank sibling of pain.002, and the same two design problems minus one
//! level — there is no payment-info tier here, only the batch and the
//! transaction:
//!
//! ```text
//! FIToFIPmtStsRpt
//!   GrpHdr                       — who is telling whom (InstgAgt/InstdAgt)
//!   OrgnlGrpInfAndSts   GrpSts   — the whole original batch   (0..n!)
//!   TxInfAndSts         TxSts    — one original transaction   (0..n)
//! ```
//!
//! Two shapes differ from pain.002 and matter:
//!
//! * **The group block is optional.** 9 of the 22 real pacs.002 files in the
//!   corpus have no `OrgnlGrpInfAndSts` at all — CBPR+-era messages reference
//!   the original inside each transaction instead. So the message-identity
//!   guard is on the `FIToFIPmtStsRpt` container, not on the group block.
//! * **The whole message repeats.** pacs.002.001.03 files exist with several
//!   complete `FIToFIPmtStsRpt` blocks in one `Document`, each with its own
//!   `GrpHdr`. All carried context resets at each message, or the second
//!   message's transactions would answer under the first message's ids.
//! * **So does the group block.** `OrgnlGrpInfAndSts` is `0..n`: one report may
//!   answer about several original messages. The group context resets at each
//!   block, or the second block reports the first one's status, reason code and
//!   every `AddtlInf` in the file joined together.
//!
//! Grain: one row per status statement, like pain.002. `status_level` is
//! `GROUP` or `TRANSACTION`; only transaction rows carry an amount.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::wire::{self, Agent, OrgnlGrpInf, OrgnlTxRef, PartyName, ReasonInfo};

// ── serde model: the transaction subtree only ────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TxInfAndSts {
    #[serde(rename = "StsId")]
    pub sts_id: Option<String>,
    /// CBPR+-era messages reference the original message here, per transaction.
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
    #[serde(rename = "TxSts")]
    pub tx_sts: Option<String>,
    #[serde(rename = "StsRsnInf", default)]
    pub rsn_inf: Vec<ReasonInfo>,
    #[serde(rename = "AccptncDtTm")]
    pub accptnc_dt_tm: Option<String>,
    /// Some clearing systems restate the agents per transaction; they override
    /// the pair on the group header.
    #[serde(rename = "InstgAgt")]
    pub instg_agt: Option<Agent>,
    #[serde(rename = "InstdAgt")]
    pub instd_agt: Option<Agent>,
    #[serde(rename = "OrgnlTxRef")]
    pub orgnl_tx_ref: Option<OrgnlTxRef>,
}

// ── flattened row ────────────────────────────────────────────────────────────

pub const LEVEL_GROUP: &str = "GROUP";
pub const LEVEL_TRANSACTION: &str = "TRANSACTION";

/// Message-level context: the reporting pair and the original message the
/// report answers. Reset at every `FIToFIPmtStsRpt`.
#[derive(Debug, Default, Clone)]
pub struct MsgCtx {
    pub msg_id: Option<String>,
    pub instg_bic: Option<String>,
    pub instd_bic: Option<String>,
    pub orgnl_msg_id: Option<String>,
    pub orgnl_msg_nm_id: Option<String>,
}

/// A group-level status and its reason, while its block is open — and after it
/// closes, the reason transactions without their own block inherit.
#[derive(Debug, Default, Clone)]
pub struct GrpCtx {
    pub status: Option<String>,
    pub reason_code: Option<String>,
    pub reason_info: Vec<String>,
    pub reason_originator: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct RptRow {
    pub msg_id: Option<String>,
    pub instructing_agent_bic: Option<String>,
    pub instructed_agent_bic: Option<String>,
    pub status_level: Option<String>,
    pub status_id: Option<String>,
    pub status: Option<String>,
    pub reason_code: Option<String>,
    pub reason_info: Option<String>,
    pub reason_originator: Option<String>,
    pub original_msg_id: Option<String>,
    pub original_msg_name_id: Option<String>,
    pub original_instr_id: Option<String>,
    pub original_end_to_end_id: Option<String>,
    pub original_tx_id: Option<String>,
    pub original_uetr: Option<String>,
    pub acceptance_date_time: Option<String>,
    /// From the copy of the original instruction; scaled, never a float.
    pub original_amount: Option<i128>,
    pub original_currency: Option<String>,
    pub original_settlement_date: Option<String>,
    pub original_debtor_name: Option<String>,
    pub original_creditor_name: Option<String>,
    pub source_file: Option<String>,
}

fn join(parts: &[String]) -> Option<String> {
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn row_from_group(grp: &GrpCtx, msg: &MsgCtx, source: &str) -> RptRow {
    RptRow {
        msg_id: msg.msg_id.clone(),
        instructing_agent_bic: msg.instg_bic.clone(),
        instructed_agent_bic: msg.instd_bic.clone(),
        status_level: Some(LEVEL_GROUP.to_string()),
        status: grp.status.clone(),
        reason_code: grp.reason_code.clone(),
        reason_info: join(&grp.reason_info),
        reason_originator: grp.reason_originator.clone(),
        original_msg_id: msg.orgnl_msg_id.clone(),
        original_msg_name_id: msg.orgnl_msg_nm_id.clone(),
        source_file: Some(source.to_string()),
        ..Default::default()
    }
}

pub fn row_from_tx(
    tx: &TxInfAndSts,
    msg: &MsgCtx,
    grp: &GrpCtx,
    source: &str,
) -> Result<RptRow, String> {
    let orgnl = tx.orgnl_tx_ref.as_ref();
    let (original_amount, original_currency) = orgnl
        .map(OrgnlTxRef::amount)
        .transpose()
        .map_err(|e| format!("{source}: {e}"))?
        .unwrap_or((None, None));

    // Whole-block inheritance, as everywhere in this crate: a transaction's own
    // code must never sit next to the group's explanation.
    let (reason_code, reason_info, reason_originator) = if tx.rsn_inf.is_empty() {
        (
            grp.reason_code.clone(),
            join(&grp.reason_info),
            grp.reason_originator.clone(),
        )
    } else {
        ReasonInfo::collapse(&tx.rsn_inf)
    };

    Ok(RptRow {
        msg_id: msg.msg_id.clone(),
        instructing_agent_bic: tx
            .instg_agt
            .as_ref()
            .and_then(Agent::id)
            .or_else(|| msg.instg_bic.clone()),
        instructed_agent_bic: tx
            .instd_agt
            .as_ref()
            .and_then(Agent::id)
            .or_else(|| msg.instd_bic.clone()),
        status_level: Some(LEVEL_TRANSACTION.to_string()),
        status_id: tx.sts_id.clone(),
        status: tx.tx_sts.clone(),
        reason_code,
        reason_info,
        reason_originator,
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
        acceptance_date_time: tx.accptnc_dt_tm.clone(),
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

pub struct RptStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    msg: MsgCtx,
    grp: GrpCtx,
    /// Whether a `FIToFIPmtStsRpt` container was seen at all.
    saw_report: bool,
    /// `path.len()` at the innermost open container of this family.
    /// A `<TxInfAndSts>` outside it belongs to another message: pain.002 names
    /// its transaction element the same.
    in_report: Option<usize>,
}

impl<R: BufRead> RptStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        RptStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            msg: MsgCtx::default(),
            grp: GrpCtx::default(),
            saw_report: false,
            in_report: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<RptRow>, Box<dyn Error>> {
        loop {
            self.buf.clear();
            let action = match self.reader.read_event_into(&mut self.buf)? {
                Event::Eof => Act::Eof,
                Event::Start(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if name == "TxInfAndSts" && self.in_report.is_some() {
                        Act::Tx
                    } else {
                        Act::Push(name.into_owned())
                    }
                }
                Event::End(e) => {
                    let qname = e.name();
                    if wire::local(qname.as_ref()) == "OrgnlGrpInfAndSts"
                        && self.in_report.is_some()
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
                    return if self.saw_report {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no <FIToFIPmtStsRpt> found — is this a pacs.002 status \
                             report?",
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
                    // One Document may hold several complete reports; nothing
                    // may leak from one into the next.
                    if name == "FIToFIPmtStsRpt" || name.starts_with("pacs.002.") {
                        self.saw_report = true;
                        self.in_report = Some(self.path.len());
                        self.msg = MsgCtx::default();
                        self.grp = GrpCtx::default();
                    }
                    // Each group block answers about its own original message.
                    // Gated like the close that emits its row: a block belonging
                    // to another message in the same envelope must not clear the
                    // reason this report's transactions still inherit.
                    if name == "OrgnlGrpInfAndSts" && self.in_report.is_some() {
                        self.grp = GrpCtx::default();
                    }
                    self.path.push(name);
                }
                Act::CloseGroup => {
                    self.pop();
                    // The group block is complete: emit its row. Its reason is
                    // KEPT for transactions without their own block — unlike the
                    // ids above, a reason stated once for the batch belongs to
                    // every transaction of the batch.
                    return Ok(Some(row_from_group(&self.grp, &self.msg, &self.source)));
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
        if self.in_report == Some(self.path.len()) {
            self.in_report = None;
        }
    }

    /// Capture message- and group-level leaves by path tail. Transaction copies
    /// live inside the `<TxInfAndSts>` subtree, which never enters `path`.
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
        } else if tail(&["OrgnlGrpInfAndSts", "OrgnlMsgId"]) {
            self.msg.orgnl_msg_id = Some(text.to_string());
        } else if tail(&["OrgnlGrpInfAndSts", "OrgnlMsgNmId"]) {
            self.msg.orgnl_msg_nm_id = Some(text.to_string());
        } else if tail(&["OrgnlGrpInfAndSts", "GrpSts"]) {
            self.grp.status = Some(text.to_string());
        } else if tail(&["OrgnlGrpInfAndSts", "StsRsnInf", "Rsn", "Cd"])
            || tail(&["OrgnlGrpInfAndSts", "StsRsnInf", "Rsn", "Prtry"])
        {
            self.grp.reason_code.get_or_insert_with(|| text.to_string());
        } else if tail(&["OrgnlGrpInfAndSts", "StsRsnInf", "AddtlInf"]) {
            self.grp.reason_info.push(text.to_string());
        } else if tail(&["OrgnlGrpInfAndSts", "StsRsnInf", "Orgtr", "Nm"]) {
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
