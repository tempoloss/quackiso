//! pacs.003 — FI-to-FI Customer Direct Debit. The interbank leg of a direct
//! debit: what the creditor's bank sends the debtor's bank to actually collect
//! the money a pain.008 asked for. pacs.008 is to pain.001 what pacs.003 is to
//! pain.008.
//!
//! Unlike pacs.009, both parties here are customers again — the creditor
//! collecting and the debtor being charged — and the mandate travels with the
//! collection, because the debtor's bank is entitled to check it before letting
//! money leave the account.
//!
//! Two fields live on the group header in real files and are carried down:
//! the interbank settlement date, and the mandate sequence type (`SeqTp`) —
//! a batch is typically all-FRST or all-RCUR, so the wire states it once.
//!
//! Grain: one row per `DrctDbtTxInf`.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::pacs008::PmtId;
use crate::pain008::{DrctDbtTx, PmtTpInf};
use crate::wire::{self, money, AcctRef, Agent, Money, PartyName, RmtInf};

// ── serde model: the transaction subtree only ────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct DrctDbtTxInf {
    #[serde(rename = "PmtId")]
    pub pmt_id: Option<PmtId>,
    #[serde(rename = "PmtTpInf")]
    pub pmt_tp_inf: Option<PmtTpInf>,
    /// What settles between the banks; the instructed amount is the fallback.
    #[serde(rename = "IntrBkSttlmAmt")]
    pub sttlm_amt: Option<Money>,
    #[serde(rename = "InstdAmt")]
    pub instd_amt: Option<Money>,
    #[serde(rename = "IntrBkSttlmDt")]
    pub sttlm_dt: Option<String>,
    #[serde(rename = "ReqdColltnDt")]
    pub reqd_colltn_dt: Option<String>,
    #[serde(rename = "ChrgBr")]
    pub chrg_br: Option<String>,
    #[serde(rename = "DrctDbtTx")]
    pub drct_dbt_tx: Option<DrctDbtTx>,
    #[serde(rename = "Cdtr")]
    pub cdtr: Option<PartyName>,
    #[serde(rename = "CdtrAcct")]
    pub cdtr_acct: Option<AcctRef>,
    #[serde(rename = "CdtrAgt")]
    pub cdtr_agt: Option<Agent>,
    #[serde(rename = "Dbtr")]
    pub dbtr: Option<PartyName>,
    #[serde(rename = "DbtrAcct")]
    pub dbtr_acct: Option<AcctRef>,
    #[serde(rename = "DbtrAgt")]
    pub dbtr_agt: Option<Agent>,
    #[serde(rename = "RmtInf")]
    pub rmt_inf: Option<RmtInf>,
}

// ── flattened row ────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct DdiRow {
    pub msg_id: Option<String>,
    pub instr_id: Option<String>,
    pub end_to_end_id: Option<String>,
    pub tx_id: Option<String>,
    pub uetr: Option<String>,
    /// Exact amount scaled by `10^decimal::SCALE`; never a float.
    pub amount: Option<i128>,
    pub currency: Option<String>,
    pub settlement_date: Option<String>,
    pub requested_collection_date: Option<String>,
    pub sequence_type: Option<String>,
    pub charge_bearer: Option<String>,
    pub mandate_id: Option<String>,
    pub mandate_signed_on: Option<String>,
    pub creditor_name: Option<String>,
    pub creditor_account: Option<String>,
    pub creditor_agent_bic: Option<String>,
    pub debtor_name: Option<String>,
    pub debtor_account: Option<String>,
    pub debtor_agent_bic: Option<String>,
    pub remittance_info: Option<String>,
    pub source_file: Option<String>,
}

pub fn row_from_tx(
    tx: &DrctDbtTxInf,
    msg_id: &Option<String>,
    source: &str,
) -> Result<DdiRow, String> {
    let (amount, currency) = money(&[tx.sttlm_amt.as_ref(), tx.instd_amt.as_ref()])
        .map_err(|e| format!("{source}: {e}"))?;
    let mndt = tx.drct_dbt_tx.as_ref().and_then(|d| d.mndt.as_ref());

    Ok(DdiRow {
        msg_id: msg_id.clone(),
        instr_id: tx.pmt_id.as_ref().and_then(|p| p.instr_id.clone()),
        end_to_end_id: tx.pmt_id.as_ref().and_then(|p| p.end_to_end_id.clone()),
        tx_id: tx.pmt_id.as_ref().and_then(|p| p.tx_id.clone()),
        uetr: tx.pmt_id.as_ref().and_then(|p| p.uetr.clone()),
        amount,
        currency,
        settlement_date: tx.sttlm_dt.clone(),
        requested_collection_date: tx.reqd_colltn_dt.clone(),
        // group-level fallback is applied by the reader, which knows the group
        sequence_type: tx.pmt_tp_inf.as_ref().and_then(|p| p.seq_tp.clone()),
        charge_bearer: tx.chrg_br.clone(),
        mandate_id: mndt.and_then(|m| m.mndt_id.clone()),
        mandate_signed_on: mndt.and_then(|m| m.dt_of_sgntr.clone()),
        creditor_name: tx.cdtr.as_ref().and_then(PartyName::name),
        creditor_account: tx.cdtr_acct.as_ref().and_then(AcctRef::value),
        creditor_agent_bic: tx.cdtr_agt.as_ref().and_then(Agent::id),
        debtor_name: tx.dbtr.as_ref().and_then(PartyName::name),
        debtor_account: tx.dbtr_acct.as_ref().and_then(AcctRef::value),
        debtor_agent_bic: tx.dbtr_agt.as_ref().and_then(Agent::id),
        remittance_info: tx.rmt_inf.as_ref().and_then(RmtInf::text),
        source_file: Some(source.to_string()),
    })
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct DdiStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    msg_id: Option<String>,
    group_sttlm_dt: Option<String>,
    group_seq_tp: Option<String>,
    /// Seen anywhere in the file; only the EOF check reads it.
    saw_debit: bool,
    /// `path.len()` at the innermost open container of this family.
    /// A `<DrctDbtTxInf>` outside it belongs to another message: pain.008
    /// names its transaction element the same.
    in_debit: Option<usize>,
}

impl<R: BufRead> DdiStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        DdiStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            msg_id: None,
            group_sttlm_dt: None,
            group_seq_tp: None,
            saw_debit: false,
            in_debit: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<DdiRow>, Box<dyn Error>> {
        loop {
            self.buf.clear();
            let action = match self.reader.read_event_into(&mut self.buf)? {
                Event::Eof => Act::Eof,
                Event::Start(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if name == "DrctDbtTxInf" && self.in_debit.is_some() {
                        Act::Tx
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
                    return if self.saw_debit {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no <FIToFICstmrDrctDbt> found — is this a pacs.003 direct \
                             debit?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Tx => {
                    let xml =
                        wire::record_subtree(&mut self.reader, &mut self.buf, "DrctDbtTxInf")?;
                    let tx: DrctDbtTxInf = quick_xml::de::from_str(&xml)?;
                    let mut row = row_from_tx(&tx, &self.msg_id, &self.source)?;
                    if row.settlement_date.is_none() {
                        row.settlement_date = self.group_sttlm_dt.clone();
                    }
                    if row.sequence_type.is_none() {
                        row.sequence_type = self.group_seq_tp.clone();
                    }
                    return Ok(Some(row));
                }
                Act::Push(name) => {
                    if name == "FIToFICstmrDrctDbt" || name.starts_with("pacs.003.") {
                        self.saw_debit = true;
                        self.in_debit = Some(self.path.len());
                        self.msg_id = None;
                        self.group_sttlm_dt = None;
                        self.group_seq_tp = None;
                    }
                    self.path.push(name);
                }
                Act::Pop => {
                    self.pop();
                }
                Act::Text(t) => {
                    if wire::ends_with(&self.path, &["GrpHdr", "MsgId"]) {
                        self.msg_id = Some(t);
                    } else if wire::ends_with(&self.path, &["GrpHdr", "IntrBkSttlmDt"]) {
                        self.group_sttlm_dt = Some(t);
                    } else if wire::ends_with(&self.path, &["GrpHdr", "PmtTpInf", "SeqTp"]) {
                        self.group_seq_tp = Some(t);
                    }
                }
                Act::None => {}
            }
        }
    }

    fn pop(&mut self) {
        self.path.pop();
        if self.in_debit == Some(self.path.len()) {
            self.in_debit = None;
        }
    }
}

enum Act {
    Eof,
    Tx,
    Push(String),
    Pop,
    Text(String),
    None,
}
