//! pacs.008 — FI-to-FI Customer Credit Transfer. The interbank instruction that
//! replaces SWIFT MT103. Structurally unrelated to camt.053: there is no
//! statement or booked entry, only a group header and a list of credit-transfer
//! transactions (`CdtTrfTxInf`), so it gets its own model and reader.
//!
//! Grain: one row per `CdtTrfTxInf`. Streams one transaction subtree at a time,
//! same constant-memory approach as the camt reader.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::wire::{self, money, AcctRef, Agent, Money, PartyName, RmtInf};

// ── serde model ───────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CdtTrfTxInf {
    #[serde(rename = "PmtId")]
    pub pmt_id: Option<PmtId>,
    /// Settled amount between the banks; present in most versions.
    #[serde(rename = "IntrBkSttlmAmt")]
    pub sttlm_amt: Option<Money>,
    /// Amount as instructed by the debtor, when it differs from settlement.
    #[serde(rename = "InstdAmt")]
    pub instd_amt: Option<Money>,
    #[serde(rename = "IntrBkSttlmDt")]
    pub sttlm_dt: Option<String>,
    #[serde(rename = "ChrgBr")]
    pub chrg_br: Option<String>,
    #[serde(rename = "Dbtr")]
    pub dbtr: Option<PartyName>,
    #[serde(rename = "Cdtr")]
    pub cdtr: Option<PartyName>,
    #[serde(rename = "DbtrAcct")]
    pub dbtr_acct: Option<AcctRef>,
    #[serde(rename = "CdtrAcct")]
    pub cdtr_acct: Option<AcctRef>,
    #[serde(rename = "DbtrAgt")]
    pub dbtr_agt: Option<Agent>,
    #[serde(rename = "CdtrAgt")]
    pub cdtr_agt: Option<Agent>,
    #[serde(rename = "RmtInf")]
    pub rmt_inf: Option<RmtInf>,
}

#[derive(Debug, Deserialize)]
pub struct PmtId {
    #[serde(rename = "InstrId")]
    pub instr_id: Option<String>,
    #[serde(rename = "EndToEndId")]
    pub end_to_end_id: Option<String>,
    #[serde(rename = "TxId")]
    pub tx_id: Option<String>,
    /// Mandatory from CBPR+ onwards; the payment's unique tracking reference.
    #[serde(rename = "UETR")]
    pub uetr: Option<String>,
}

// ── flattened row ─────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct PacsRow {
    pub msg_id: Option<String>,
    pub instr_id: Option<String>,
    pub end_to_end_id: Option<String>,
    pub tx_id: Option<String>,
    pub uetr: Option<String>,
    /// Exact amount scaled by `10^decimal::SCALE`; never a float.
    pub amount: Option<i128>,
    pub currency: Option<String>,
    pub settlement_date: Option<String>,
    pub charge_bearer: Option<String>,
    pub debtor_name: Option<String>,
    pub debtor_account: Option<String>,
    pub debtor_agent_bic: Option<String>,
    pub creditor_name: Option<String>,
    pub creditor_account: Option<String>,
    pub creditor_agent_bic: Option<String>,
    pub remittance_info: Option<String>,
    pub source_file: Option<String>,
}

pub fn row_from_tx(
    tx: &CdtTrfTxInf,
    msg_id: &Option<String>,
    source: &str,
) -> Result<PacsRow, String> {
    // Settlement amount is what actually moved between the banks; fall back to
    // the instructed amount when a message carries only that.
    let (amount, currency) = money(&[tx.sttlm_amt.as_ref(), tx.instd_amt.as_ref()])
        .map_err(|e| format!("{source}: {e}"))?;

    Ok(PacsRow {
        msg_id: msg_id.clone(),
        instr_id: tx.pmt_id.as_ref().and_then(|p| p.instr_id.clone()),
        end_to_end_id: tx.pmt_id.as_ref().and_then(|p| p.end_to_end_id.clone()),
        tx_id: tx.pmt_id.as_ref().and_then(|p| p.tx_id.clone()),
        uetr: tx.pmt_id.as_ref().and_then(|p| p.uetr.clone()),
        amount,
        currency,
        settlement_date: tx.sttlm_dt.clone(),
        charge_bearer: tx.chrg_br.clone(),
        debtor_name: tx.dbtr.as_ref().and_then(PartyName::name),
        debtor_account: tx.dbtr_acct.as_ref().and_then(AcctRef::value),
        debtor_agent_bic: tx.dbtr_agt.as_ref().and_then(Agent::id),
        creditor_name: tx.cdtr.as_ref().and_then(PartyName::name),
        creditor_account: tx.cdtr_acct.as_ref().and_then(AcctRef::value),
        creditor_agent_bic: tx.cdtr_agt.as_ref().and_then(Agent::id),
        remittance_info: tx.rmt_inf.as_ref().and_then(RmtInf::text),
        source_file: Some(source.to_string()),
    })
}

// ── streaming reader ──────────────────────────────────────────────────────────

pub struct TxStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    msg_id: Option<String>,
    /// SEPA credit transfers put the settlement date once on the group header
    /// rather than on every transaction, so it is carried down as a fallback.
    group_sttlm_dt: Option<String>,
    /// Seen anywhere in the file; only the EOF check reads it.
    saw_transfer: bool,
    /// `path.len()` at the innermost open container of this family.
    /// A `<CdtTrfTxInf>` outside it belongs to another message: pain.001 names
    /// its transaction element the same.
    in_transfer: Option<usize>,
}

impl<R: BufRead> TxStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        TxStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            msg_id: None,
            group_sttlm_dt: None,
            saw_transfer: false,
            in_transfer: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<PacsRow>, Box<dyn Error>> {
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
                    if name == "CdtTrfTxInf" && self.in_transfer.is_some() {
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
                    return if self.saw_transfer {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no <FIToFICstmrCdtTrf> found — is this a pacs.008 credit \
                             transfer?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Tx => {
                    let mut row = self.read_tx()?;
                    if row.settlement_date.is_none() {
                        row.settlement_date = self.group_sttlm_dt.clone();
                    }
                    return Ok(Some(row));
                }
                Act::Push(n) => {
                    if n == "FIToFICstmrCdtTrf" || n.starts_with("pacs.008.") {
                        self.saw_transfer = true;
                        self.in_transfer = Some(self.path.len());
                        self.msg_id = None;
                        self.group_sttlm_dt = None;
                    }
                    self.path.push(n);
                }
                Act::Pop => {
                    self.pop();
                }
                Act::Text(t) => {
                    // Group-header leaves only; a transaction's own fields are
                    // read from its subtree, which never enters `path`.
                    if wire::ends_with(&self.path, &["GrpHdr", "MsgId"]) {
                        self.msg_id = Some(t);
                    } else if wire::ends_with(&self.path, &["GrpHdr", "IntrBkSttlmDt"]) {
                        self.group_sttlm_dt = Some(t);
                    }
                }
                Act::None => {}
            }
        }
    }

    fn pop(&mut self) {
        self.path.pop();
        if self.in_transfer == Some(self.path.len()) {
            self.in_transfer = None;
        }
    }

    /// Record the current `<CdtTrfTxInf>` subtree and deserialize it.
    fn read_tx(&mut self) -> Result<PacsRow, Box<dyn Error>> {
        let xml =
            wire::record_subtree(&mut self.reader, &mut self.buf, "CdtTrfTxInf", &self.source)?;
        let tx: CdtTrfTxInf = quick_xml::de::from_str(&xml)?;
        Ok(row_from_tx(&tx, &self.msg_id, &self.source)?)
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
