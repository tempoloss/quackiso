//! pacs.008 — FI-to-FI Customer Credit Transfer. The interbank instruction that
//! replaces SWIFT MT103. Structurally unrelated to camt.053: there is no
//! statement or booked entry, only a group header and a list of credit-transfer
//! transactions (`CdtTrfTxInf`), so it gets its own model and reader.
//!
//! Grain: one row per `CdtTrfTxInf`. Streams one transaction subtree at a time,
//! same constant-memory approach as the camt reader.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::{Reader, Writer};
use serde::Deserialize;

// ── serde model ───────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CdtTrfTxInf {
    #[serde(rename = "PmtId")]
    pub pmt_id: Option<PmtId>,
    /// Settled amount between the banks; present in most versions.
    #[serde(rename = "IntrBkSttlmAmt")]
    pub sttlm_amt: Option<Amt>,
    /// Amount as instructed by the debtor, when it differs from settlement.
    #[serde(rename = "InstdAmt")]
    pub instd_amt: Option<Amt>,
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

#[derive(Debug, Deserialize)]
pub struct Amt {
    #[serde(rename = "@Ccy")]
    pub ccy: Option<String>,
    #[serde(rename = "$text")]
    pub value: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PartyName {
    #[serde(rename = "Nm")]
    pub nm: Option<String>,
    /// Later versions wrap the party one level deeper, as in camt.053 v8.
    #[serde(rename = "Pty")]
    pub pty: Option<Inner>,
}

#[derive(Debug, Deserialize)]
pub struct Inner {
    #[serde(rename = "Nm")]
    pub nm: Option<String>,
}

impl PartyName {
    pub fn name(&self) -> Option<String> {
        self.nm
            .clone()
            .or_else(|| self.pty.as_ref().and_then(|p| p.nm.clone()))
    }
}

#[derive(Debug, Deserialize)]
pub struct AcctRef {
    #[serde(rename = "Id")]
    pub id: Option<AcctRefId>,
}

#[derive(Debug, Deserialize)]
pub struct AcctRefId {
    #[serde(rename = "IBAN")]
    pub iban: Option<String>,
    #[serde(rename = "Othr")]
    pub othr: Option<OtherId>,
}

#[derive(Debug, Deserialize)]
pub struct OtherId {
    #[serde(rename = "Id")]
    pub id: Option<String>,
}

impl AcctRefId {
    pub fn value(&self) -> Option<String> {
        self.iban
            .clone()
            .or_else(|| self.othr.as_ref().and_then(|o| o.id.clone()))
    }
}

/// Financial institution. The BIC element was renamed `BICFI` in later
/// versions; some messages identify the bank only by clearing-system id.
#[derive(Debug, Deserialize)]
pub struct Agent {
    #[serde(rename = "FinInstnId")]
    pub fin_instn_id: Option<FinInstnId>,
}

#[derive(Debug, Deserialize)]
pub struct FinInstnId {
    #[serde(rename = "BICFI")]
    pub bicfi: Option<String>,
    #[serde(rename = "BIC")]
    pub bic: Option<String>,
    #[serde(rename = "ClrSysMmbId")]
    pub clr: Option<ClrSysMmbId>,
    #[serde(rename = "Nm")]
    pub nm: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ClrSysMmbId {
    #[serde(rename = "MmbId")]
    pub mmb_id: Option<String>,
}

impl Agent {
    /// BIC if present (either spelling), else the clearing-system member id,
    /// else the institution name.
    pub fn id(&self) -> Option<String> {
        let f = self.fin_instn_id.as_ref()?;
        f.bicfi
            .clone()
            .or_else(|| f.bic.clone())
            .or_else(|| f.clr.as_ref().and_then(|c| c.mmb_id.clone()))
            .or_else(|| f.nm.clone())
    }
}

#[derive(Debug, Deserialize)]
pub struct RmtInf {
    #[serde(rename = "Ustrd", default)]
    pub ustrd: Vec<String>,
}

// ── flattened row ─────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct PacsRow {
    pub msg_id: Option<String>,
    pub instr_id: Option<String>,
    pub end_to_end_id: Option<String>,
    pub tx_id: Option<String>,
    pub uetr: Option<String>,
    pub amount: Option<f64>,
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

fn parse_amt(a: Option<&Amt>) -> (Option<f64>, Option<String>) {
    match a {
        Some(a) => (
            a.value.as_ref().and_then(|v| v.trim().parse::<f64>().ok()),
            a.ccy.clone(),
        ),
        None => (None, None),
    }
}

pub fn row_from_tx(tx: &CdtTrfTxInf, msg_id: &Option<String>, source: &str) -> PacsRow {
    // Settlement amount is what actually moved between the banks; fall back to
    // the instructed amount when a message carries only that.
    let (amount, currency) = {
        let (a, c) = parse_amt(tx.sttlm_amt.as_ref());
        if a.is_some() {
            (a, c)
        } else {
            parse_amt(tx.instd_amt.as_ref())
        }
    };
    let acct = |a: Option<&AcctRef>| a.and_then(|a| a.id.as_ref()).and_then(|i| i.value());

    PacsRow {
        msg_id: msg_id.clone(),
        instr_id: tx.pmt_id.as_ref().and_then(|p| p.instr_id.clone()),
        end_to_end_id: tx.pmt_id.as_ref().and_then(|p| p.end_to_end_id.clone()),
        tx_id: tx.pmt_id.as_ref().and_then(|p| p.tx_id.clone()),
        uetr: tx.pmt_id.as_ref().and_then(|p| p.uetr.clone()),
        amount,
        currency,
        settlement_date: tx.sttlm_dt.clone(),
        charge_bearer: tx.chrg_br.clone(),
        debtor_name: tx.dbtr.as_ref().and_then(|p| p.name()),
        debtor_account: acct(tx.dbtr_acct.as_ref()),
        debtor_agent_bic: tx.dbtr_agt.as_ref().and_then(|a| a.id()),
        creditor_name: tx.cdtr.as_ref().and_then(|p| p.name()),
        creditor_account: acct(tx.cdtr_acct.as_ref()),
        creditor_agent_bic: tx.cdtr_agt.as_ref().and_then(|a| a.id()),
        remittance_info: tx
            .rmt_inf
            .as_ref()
            .filter(|r| !r.ustrd.is_empty())
            .map(|r| r.ustrd.join(" ")),
        source_file: Some(source.to_string()),
    }
}

// ── streaming reader ──────────────────────────────────────────────────────────

pub struct TxStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    msg_id: Option<String>,
}

impl<R: BufRead> TxStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        TxStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            msg_id: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<PacsRow>, Box<dyn Error>> {
        loop {
            self.buf.clear();
            let action = match self.reader.read_event_into(&mut self.buf)? {
                Event::Eof => Act::Eof,
                Event::Start(e) => {
                    let name = local(e.name().as_ref());
                    if name == "CdtTrfTxInf" {
                        Act::Tx
                    } else {
                        Act::Push(name)
                    }
                }
                Event::End(_) => Act::Pop,
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
                Act::Eof => return Ok(None),
                Act::Tx => return Ok(Some(self.read_tx()?)),
                Act::Push(n) => self.path.push(n),
                Act::Pop => {
                    self.path.pop();
                }
                Act::Text(t) => {
                    // Only the group header's MsgId; a transaction's own ids are
                    // read from its subtree, which never enters `path`.
                    if self.path.len() >= 2
                        && self.path[self.path.len() - 1] == "MsgId"
                        && self.path[self.path.len() - 2] == "GrpHdr"
                    {
                        self.msg_id = Some(t);
                    }
                }
                Act::None => {}
            }
        }
    }

    /// Record the current `<CdtTrfTxInf>` subtree and deserialize it. Tag names
    /// are rewritten to their local part while copying: CBPR+ and several
    /// vendors ship prefixed messages (`<urn2:...>`, `<Doc:...>`), which would
    /// otherwise close a synthetic unprefixed root and be rejected as
    /// ill-formed. Attributes are preserved — amounts carry `Ccy` there.
    fn read_tx(&mut self) -> Result<PacsRow, Box<dyn Error>> {
        let mut w = Writer::new(Vec::new());
        w.write_event(Event::Start(BytesStart::new("CdtTrfTxInf")))?;
        let mut depth = 1;
        loop {
            self.buf.clear();
            let ev = self.reader.read_event_into(&mut self.buf)?;
            match ev {
                Event::Eof => return Err("unexpected EOF inside <CdtTrfTxInf>".into()),
                Event::Start(e) => {
                    let name = local(e.name().as_ref());
                    if name == "CdtTrfTxInf" {
                        depth += 1;
                    }
                    let mut s = BytesStart::new(name);
                    for a in e.attributes().flatten() {
                        s.push_attribute(a);
                    }
                    w.write_event(Event::Start(s))?;
                }
                Event::Empty(e) => {
                    let mut s = BytesStart::new(local(e.name().as_ref()));
                    for a in e.attributes().flatten() {
                        s.push_attribute(a);
                    }
                    w.write_event(Event::Empty(s))?;
                }
                Event::End(e) => {
                    let name = local(e.name().as_ref());
                    if name == "CdtTrfTxInf" {
                        depth -= 1;
                    }
                    w.write_event(Event::End(BytesEnd::new(name)))?;
                    if depth == 0 {
                        break;
                    }
                }
                other => {
                    w.write_event(other)?;
                }
            }
        }
        let xml = String::from_utf8(w.into_inner())?;
        let tx: CdtTrfTxInf = quick_xml::de::from_str(&xml)?;
        Ok(row_from_tx(&tx, &self.msg_id, &self.source))
    }
}

fn local(name: &[u8]) -> String {
    let s = String::from_utf8_lossy(name);
    match s.rsplit_once(':') {
        Some((_, l)) => l.to_string(),
        None => s.into_owned(),
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
