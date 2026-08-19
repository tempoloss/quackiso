//! pacs.009 — Financial Institution Credit Transfer. Banks moving money between
//! themselves: the ISO 20022 replacement for SWIFT MT202, and in its COV form
//! for MT202COV.
//!
//! The parties here are **financial institutions** — `Dbtr` and `Cdtr` are
//! `FinInstnId` blocks, not customer parties — so the columns say `debtor_fi`
//! and `creditor_fi`, and they resolve like any agent: BIC, else clearing
//! member id, else name.
//!
//! The COV variant is the interesting one. A cover payment settles between
//! banks the money of an underlying customer transfer that travelled as a
//! separate pacs.008 — and the `UndrlygCstmrCdtTrf` block names that underlying
//! customer debtor and creditor. MT202COV exists *because* hiding them enabled
//! money laundering; a reader that drops the block would reproduce exactly the
//! opacity the COV format was created to remove. The `underlying_*` columns
//! carry it.
//!
//! Grain: one row per `CdtTrfTxInf`.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::pacs008::PmtId;
use crate::wire::{self, money, AcctRef, Agent, Money, PartyName, RmtInf};

// ── serde model: the transaction subtree only ────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CdtTrfTxInf {
    #[serde(rename = "PmtId")]
    pub pmt_id: Option<PmtId>,
    #[serde(rename = "IntrBkSttlmAmt")]
    pub sttlm_amt: Option<Money>,
    #[serde(rename = "IntrBkSttlmDt")]
    pub sttlm_dt: Option<String>,
    /// A financial institution, not a customer.
    #[serde(rename = "Dbtr")]
    pub dbtr: Option<Agent>,
    #[serde(rename = "DbtrAcct")]
    pub dbtr_acct: Option<AcctRef>,
    #[serde(rename = "DbtrAgt")]
    pub dbtr_agt: Option<Agent>,
    #[serde(rename = "Cdtr")]
    pub cdtr: Option<Agent>,
    #[serde(rename = "CdtrAcct")]
    pub cdtr_acct: Option<AcctRef>,
    #[serde(rename = "CdtrAgt")]
    pub cdtr_agt: Option<Agent>,
    #[serde(rename = "UndrlygCstmrCdtTrf")]
    pub underlying: Option<Underlying>,
}

/// The customer transfer a cover payment settles: who the money is really for.
#[derive(Debug, Deserialize)]
pub struct Underlying {
    #[serde(rename = "Dbtr")]
    pub dbtr: Option<PartyName>,
    #[serde(rename = "DbtrAcct")]
    pub dbtr_acct: Option<AcctRef>,
    #[serde(rename = "Cdtr")]
    pub cdtr: Option<PartyName>,
    #[serde(rename = "CdtrAcct")]
    pub cdtr_acct: Option<AcctRef>,
    #[serde(rename = "RmtInf")]
    pub rmt_inf: Option<RmtInf>,
}

// ── flattened row ────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct FiRow {
    pub msg_id: Option<String>,
    pub instr_id: Option<String>,
    pub end_to_end_id: Option<String>,
    pub tx_id: Option<String>,
    pub uetr: Option<String>,
    /// Exact amount scaled by `10^decimal::SCALE`; never a float.
    pub amount: Option<i128>,
    pub currency: Option<String>,
    pub settlement_date: Option<String>,
    pub debtor_fi: Option<String>,
    pub debtor_account: Option<String>,
    pub debtor_agent_bic: Option<String>,
    pub creditor_fi: Option<String>,
    pub creditor_account: Option<String>,
    pub creditor_agent_bic: Option<String>,
    pub underlying_debtor_name: Option<String>,
    pub underlying_debtor_account: Option<String>,
    pub underlying_creditor_name: Option<String>,
    pub underlying_creditor_account: Option<String>,
    pub underlying_remittance_info: Option<String>,
    pub source_file: Option<String>,
}

pub fn row_from_tx(
    tx: &CdtTrfTxInf,
    msg_id: &Option<String>,
    source: &str,
) -> Result<FiRow, String> {
    let (amount, currency) =
        money(&[tx.sttlm_amt.as_ref()]).map_err(|e| format!("{source}: {e}"))?;
    let u = tx.underlying.as_ref();

    Ok(FiRow {
        msg_id: msg_id.clone(),
        instr_id: tx.pmt_id.as_ref().and_then(|p| p.instr_id.clone()),
        end_to_end_id: tx.pmt_id.as_ref().and_then(|p| p.end_to_end_id.clone()),
        tx_id: tx.pmt_id.as_ref().and_then(|p| p.tx_id.clone()),
        uetr: tx.pmt_id.as_ref().and_then(|p| p.uetr.clone()),
        amount,
        currency,
        settlement_date: tx.sttlm_dt.clone(),
        debtor_fi: tx.dbtr.as_ref().and_then(Agent::id),
        debtor_account: tx.dbtr_acct.as_ref().and_then(AcctRef::value),
        debtor_agent_bic: tx.dbtr_agt.as_ref().and_then(Agent::id),
        creditor_fi: tx.cdtr.as_ref().and_then(Agent::id),
        creditor_account: tx.cdtr_acct.as_ref().and_then(AcctRef::value),
        creditor_agent_bic: tx.cdtr_agt.as_ref().and_then(Agent::id),
        underlying_debtor_name: u.and_then(|x| x.dbtr.as_ref()).and_then(PartyName::name),
        underlying_debtor_account: u
            .and_then(|x| x.dbtr_acct.as_ref())
            .and_then(AcctRef::value),
        underlying_creditor_name: u.and_then(|x| x.cdtr.as_ref()).and_then(PartyName::name),
        underlying_creditor_account: u
            .and_then(|x| x.cdtr_acct.as_ref())
            .and_then(AcctRef::value),
        underlying_remittance_info: u.and_then(|x| x.rmt_inf.as_ref()).and_then(RmtInf::text),
        source_file: Some(source.to_string()),
    })
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct FiStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    msg_id: Option<String>,
    /// Some clearing systems state the settlement date once on the group.
    group_sttlm_dt: Option<String>,
    /// Seen anywhere in the file; only the EOF check reads it.
    saw_transfer: bool,
    /// `path.len()` at the innermost open container of this family.
    /// A `<CdtTrfTxInf>` outside it belongs to another message: pacs.008 and
    /// pain.001 name their transaction element the same.
    in_transfer: Option<usize>,
}

impl<R: BufRead> FiStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        FiStream {
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

    pub fn next_row(&mut self) -> Result<Option<FiRow>, Box<dyn Error>> {
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
                            "{}: no <FICdtTrf> found — is this a pacs.009 financial \
                             institution transfer?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Tx => {
                    let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "CdtTrfTxInf")?;
                    let tx: CdtTrfTxInf = quick_xml::de::from_str(&xml)?;
                    let mut row = row_from_tx(&tx, &self.msg_id, &self.source)?;
                    if row.settlement_date.is_none() {
                        row.settlement_date = self.group_sttlm_dt.clone();
                    }
                    return Ok(Some(row));
                }
                Act::Push(name) => {
                    // FICdtTrf since .04; FinInstnCdtTrf in the .02/.03 era;
                    // FinInstToFinInstCdtTrf in some sandbox schemas; the
                    // versioned message name in the first editions.
                    if name == "FICdtTrf"
                        || name == "FinInstnCdtTrf"
                        || name == "FinInstToFinInstCdtTrf"
                        || name.starts_with("pacs.009.")
                    {
                        self.saw_transfer = true;
                        self.in_transfer = Some(self.path.len());
                        self.msg_id = None;
                        self.group_sttlm_dt = None;
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
}

enum Act {
    Eof,
    Tx,
    Push(String),
    Pop,
    Text(String),
    None,
}
