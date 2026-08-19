//! pain.014 - Creditor Payment Activation Request Status Report.
//!
//! Reports pain.013 status at group, payment-info and transaction levels.
//!
//! Grain: one row per status statement: `OrgnlGrpInfAndSts`, `OrgnlPmtInfAndSts` or `TxInfAndSts`.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::decimal;
use crate::wire::{self, AcctRef, DateOrText, OrgnlTxRef, PartyName, ReasonInfo, RmtInf};

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
    #[serde(rename = "StsRsnInf", default)]
    pub rsn_inf: Vec<ReasonInfo>,
    #[serde(rename = "AccptncDtTm")]
    pub accptnc_dt_tm: Option<String>,
    #[serde(rename = "OrgnlTxRef")]
    pub orgnl_tx_ref: Option<OrgnlTxRef>,
}

pub const LEVEL_GROUP: &str = "GROUP";
pub const LEVEL_PAYMENT_INFO: &str = "PAYMENT_INFO";
pub const LEVEL_TRANSACTION: &str = "TRANSACTION";

#[derive(Debug, Default, Clone)]
pub struct MsgCtx {
    pub msg_id: Option<String>,
    pub initiating_party: Option<String>,
    pub original_msg_id: Option<String>,
    pub original_msg_name_id: Option<String>,
}

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
pub struct ActvtnStsRow {
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
    pub original_number_of_txs: Option<String>,
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
    pub remittance_info: Option<String>,
    pub acceptance_date_time: Option<String>,
    pub source_file: Option<String>,
}

fn join(parts: &[String]) -> Option<String> {
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn row_from_status(
    level: &str,
    sts: &StsCtx,
    msg: &MsgCtx,
    source: &str,
) -> Result<ActvtnStsRow, String> {
    Ok(ActvtnStsRow {
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
) -> Result<ActvtnStsRow, String> {
    let orgnl = tx.orgnl_tx_ref.as_ref();
    let (amount, currency) = orgnl
        .map(OrgnlTxRef::amount)
        .transpose()
        .map_err(|e| format!("{source}: {e}"))?
        .unwrap_or((None, None));

    let (reason_code, reason_info, reason_originator) = if tx.rsn_inf.is_empty() {
        (
            pmt.reason_code.clone(),
            join(&pmt.reason_info),
            pmt.reason_originator.clone(),
        )
    } else {
        ReasonInfo::collapse(&tx.rsn_inf)
    };

    Ok(ActvtnStsRow {
        msg_id: msg.msg_id.clone(),
        initiating_party: msg.initiating_party.clone(),
        original_msg_id: msg.original_msg_id.clone(),
        original_msg_name_id: msg.original_msg_name_id.clone(),
        status_level: Some(LEVEL_TRANSACTION.to_string()),
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

pub struct ActvtnStsStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    msg: MsgCtx,
    grp: StsCtx,
    pmt: StsCtx,
    saw_report: bool,
    in_report: Option<usize>,
    saw_status: bool,
}

impl<R: BufRead> ActvtnStsStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        ActvtnStsStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            msg: MsgCtx::default(),
            grp: StsCtx::default(),
            pmt: StsCtx::default(),
            saw_report: false,
            in_report: None,
            saw_status: false,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<ActvtnStsRow>, Box<dyn Error>> {
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
                    if name == "TxInfAndSts" && self.in_report.is_some() {
                        Act::Tx
                    } else {
                        Act::Push(name.into_owned())
                    }
                }
                Event::End(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if name == "OrgnlGrpInfAndSts" && self.in_report.is_some() {
                        Act::CloseGroup
                    } else if name == "OrgnlPmtInfAndSts" && self.in_report.is_some() {
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
                    return if self.saw_status {
                        Ok(None)
                    } else if self.saw_report {
                        Err(format!(
                            "{}: no <OrgnlGrpInfAndSts> found - is this a pain.014 creditor payment activation request status report?",
                            self.source
                        )
                        .into())
                    } else {
                        Err(format!(
                            "{}: no <CdtrPmtActvtnReqStsRpt> found - is this a pain.014 creditor payment activation request status report?",
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
                    if name == "CdtrPmtActvtnReqStsRpt" || name.starts_with("pain.014.") {
                        self.saw_report = true;
                        self.in_report = Some(self.path.len());
                        self.msg = MsgCtx::default();
                        self.grp = StsCtx::default();
                        self.pmt = StsCtx::default();
                    }
                    if name == "OrgnlPmtInfAndSts" && self.in_report.is_some() {
                        self.pmt = StsCtx::default();
                    }
                    self.path.push(name);
                }
                Act::CloseGroup => {
                    self.pop();
                    self.saw_status = true;
                    let row = row_from_status(LEVEL_GROUP, &self.grp, &self.msg, &self.source)?;
                    self.grp = StsCtx::default();
                    return Ok(Some(row));
                }
                Act::ClosePmtInf => {
                    self.saw_status = true;
                    self.pop();
                    let row =
                        row_from_status(LEVEL_PAYMENT_INFO, &self.pmt, &self.msg, &self.source)?;
                    self.pmt = StsCtx::default();
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
        if self.in_report == Some(self.path.len()) {
            self.in_report = None;
        }
    }

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
