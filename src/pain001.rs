//! pain.001 — Customer Credit Transfer Initiation. What a corporate sends its
//! own bank to ask for payments, as opposed to pacs.008 which is what banks send
//! each other.
//!
//! The shape differs in one structural way that matters: the **debtor lives on
//! the `PmtInf` group**, not on the transaction. One `PmtInf` carries the payer,
//! the debit account, the execution date and the payment method, then holds many
//! `CdtTrfTxInf` children that each name a different creditor. So the reader
//! carries group context downward, the same way the camt reader carries
//! statement context.
//!
//! Grain: one row per `CdtTrfTxInf`.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::{Reader, Writer};
use serde::Deserialize;

use crate::decimal;

// ── serde model: the transaction subtree only ────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CdtTrfTxInf {
    #[serde(rename = "PmtId")]
    pub pmt_id: Option<PmtId>,
    #[serde(rename = "Amt")]
    pub amt: Option<AmtBlock>,
    #[serde(rename = "ChrgBr")]
    pub chrg_br: Option<String>,
    #[serde(rename = "Cdtr")]
    pub cdtr: Option<PartyName>,
    #[serde(rename = "CdtrAcct")]
    pub cdtr_acct: Option<AcctRef>,
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
    #[serde(rename = "UETR")]
    pub uetr: Option<String>,
}

/// pain.001 wraps the amount: `<Amt><InstdAmt Ccy="EUR">…</InstdAmt></Amt>`.
/// Equivalent-amount instructions carry `EqvtAmt/Amt` instead.
#[derive(Debug, Deserialize)]
pub struct AmtBlock {
    #[serde(rename = "InstdAmt")]
    pub instd: Option<Money>,
    #[serde(rename = "EqvtAmt")]
    pub eqvt: Option<EqvtAmt>,
}

#[derive(Debug, Deserialize)]
pub struct EqvtAmt {
    #[serde(rename = "Amt")]
    pub amt: Option<Money>,
}

#[derive(Debug, Deserialize)]
pub struct Money {
    #[serde(rename = "@Ccy")]
    pub ccy: Option<String>,
    #[serde(rename = "$text")]
    pub value: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PartyName {
    #[serde(rename = "Nm")]
    pub nm: Option<String>,
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
    #[serde(rename = "Strd", default)]
    pub strd: Vec<Strd>,
}

#[derive(Debug, Deserialize)]
pub struct Strd {
    #[serde(rename = "CdtrRefInf")]
    pub cdtr_ref_inf: Option<CdtrRefInf>,
}

#[derive(Debug, Deserialize)]
pub struct CdtrRefInf {
    #[serde(rename = "Ref")]
    pub reference: Option<String>,
}

// ── flattened row ────────────────────────────────────────────────────────────

/// Group-level context carried into every transaction of a `PmtInf`.
#[derive(Debug, Default, Clone)]
pub struct GroupCtx {
    pub msg_id: Option<String>,
    pub initiating_party: Option<String>,
    pub payment_info_id: Option<String>,
    pub payment_method: Option<String>,
    pub requested_execution_date: Option<String>,
    pub debtor_name: Option<String>,
    pub debtor_account: Option<String>,
    pub debtor_agent_bic: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct PainRow {
    pub msg_id: Option<String>,
    pub initiating_party: Option<String>,
    pub payment_info_id: Option<String>,
    pub payment_method: Option<String>,
    pub requested_execution_date: Option<String>,
    pub debtor_name: Option<String>,
    pub debtor_account: Option<String>,
    pub debtor_agent_bic: Option<String>,
    pub instr_id: Option<String>,
    pub end_to_end_id: Option<String>,
    /// Exact amount scaled by `10^decimal::SCALE`; never a float.
    pub amount: Option<i128>,
    pub currency: Option<String>,
    pub charge_bearer: Option<String>,
    pub creditor_name: Option<String>,
    pub creditor_account: Option<String>,
    pub creditor_agent_bic: Option<String>,
    pub remittance_info: Option<String>,
    pub source_file: Option<String>,
}

/// An amount and its currency, exact. `Err` on malformed input rather than a
/// NULL that would silently drop out of a `SUM`.
fn money(m: Option<&Money>) -> Result<(Option<i128>, Option<String>), String> {
    match m {
        Some(m) => Ok((decimal::scaled_opt(m.value.as_ref())?, m.ccy.clone())),
        None => Ok((None, None)),
    }
}

pub fn row_from_tx(tx: &CdtTrfTxInf, ctx: &GroupCtx, source: &str) -> Result<PainRow, String> {
    // instructed amount, else the equivalent-amount form
    let (amount, currency) = {
        let (a, c) = money(tx.amt.as_ref().and_then(|a| a.instd.as_ref()))
            .map_err(|e| format!("{source}: {e}"))?;
        if a.is_some() {
            (a, c)
        } else {
            money(
                tx.amt
                    .as_ref()
                    .and_then(|a| a.eqvt.as_ref())
                    .and_then(|e| e.amt.as_ref()),
            )
            .map_err(|e| format!("{source}: {e}"))?
        }
    };
    let remittance = tx.rmt_inf.as_ref().and_then(|r| {
        if !r.ustrd.is_empty() {
            Some(r.ustrd.join(" "))
        } else {
            r.strd
                .iter()
                .find_map(|s| s.cdtr_ref_inf.as_ref().and_then(|c| c.reference.clone()))
        }
    });

    Ok(PainRow {
        msg_id: ctx.msg_id.clone(),
        initiating_party: ctx.initiating_party.clone(),
        payment_info_id: ctx.payment_info_id.clone(),
        payment_method: ctx.payment_method.clone(),
        requested_execution_date: ctx.requested_execution_date.clone(),
        debtor_name: ctx.debtor_name.clone(),
        debtor_account: ctx.debtor_account.clone(),
        debtor_agent_bic: ctx.debtor_agent_bic.clone(),
        instr_id: tx.pmt_id.as_ref().and_then(|p| p.instr_id.clone()),
        end_to_end_id: tx.pmt_id.as_ref().and_then(|p| p.end_to_end_id.clone()),
        amount,
        currency,
        // group-level fallback is applied by the reader, which knows the group
        charge_bearer: tx.chrg_br.clone(),
        creditor_name: tx.cdtr.as_ref().and_then(|p| p.name()),
        creditor_account: tx
            .cdtr_acct
            .as_ref()
            .and_then(|a| a.id.as_ref())
            .and_then(|i| i.value()),
        creditor_agent_bic: tx.cdtr_agt.as_ref().and_then(|a| a.id()),
        remittance_info: remittance,
        source_file: Some(source.to_string()),
    })
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct PainStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    ctx: GroupCtx,
    /// group-level charge bearer, when the file puts it on PmtInf
    group_chrg_br: Option<String>,
}

impl<R: BufRead> PainStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        PainStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            ctx: GroupCtx::default(),
            group_chrg_br: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<PainRow>, Box<dyn Error>> {
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
                Act::Tx => {
                    let mut row = self.read_tx()?;
                    if row.charge_bearer.is_none() {
                        row.charge_bearer = self.group_chrg_br.clone();
                    }
                    return Ok(Some(row));
                }
                Act::Push(name) => {
                    // a new payment group replaces the previous debtor context
                    if name == "PmtInf" {
                        let msg_id = self.ctx.msg_id.clone();
                        let initg = self.ctx.initiating_party.clone();
                        self.ctx = GroupCtx {
                            msg_id,
                            initiating_party: initg,
                            ..Default::default()
                        };
                        self.group_chrg_br = None;
                    }
                    self.path.push(name);
                }
                Act::Pop => {
                    self.path.pop();
                }
                Act::Text(t) => self.capture(&t),
                Act::None => {}
            }
        }
    }

    /// Capture group-level leaves by path tail. Transaction-internal elements
    /// live inside the `<CdtTrfTxInf>` subtree, which never enters `path`, so
    /// these tails cannot collide with a creditor's name or account.
    fn capture(&mut self, text: &str) {
        let p = &self.path;
        let tail = |suffix: &[&str]| -> bool {
            p.len() >= suffix.len()
                && p[p.len() - suffix.len()..]
                    .iter()
                    .zip(suffix)
                    .all(|(a, b)| a == b)
        };

        if tail(&["GrpHdr", "MsgId"]) {
            self.ctx.msg_id = Some(text.to_string());
        } else if tail(&["GrpHdr", "InitgPty", "Nm"]) {
            self.ctx.initiating_party = Some(text.to_string());
        } else if tail(&["PmtInf", "PmtInfId"]) {
            self.ctx.payment_info_id = Some(text.to_string());
        } else if tail(&["PmtInf", "PmtMtd"]) {
            self.ctx.payment_method = Some(text.to_string());
        } else if tail(&["PmtInf", "ReqdExctnDt"])
            || tail(&["ReqdExctnDt", "Dt"])
            || tail(&["ReqdExctnDt", "DtTm"])
        {
            // .03 has it inline; later versions wrap it as either
            // <ReqdExctnDt><Dt>…</Dt></> or <ReqdExctnDt><DtTm>…</DtTm></>
            self.ctx.requested_execution_date = Some(text.to_string());
        } else if tail(&["PmtInf", "ChrgBr"]) {
            self.group_chrg_br = Some(text.to_string());
        } else if tail(&["Dbtr", "Nm"]) || tail(&["Dbtr", "Pty", "Nm"]) {
            self.ctx.debtor_name = Some(text.to_string());
        } else if tail(&["DbtrAcct", "Id", "IBAN"]) {
            self.ctx.debtor_account = Some(text.to_string());
        } else if tail(&["DbtrAcct", "Id", "Othr", "Id"]) {
            if self.ctx.debtor_account.is_none() {
                self.ctx.debtor_account = Some(text.to_string());
            }
        } else if tail(&["DbtrAgt", "FinInstnId", "BICFI"])
            || tail(&["DbtrAgt", "FinInstnId", "BIC"])
        {
            self.ctx.debtor_agent_bic = Some(text.to_string());
        } else if tail(&["DbtrAgt", "FinInstnId", "ClrSysMmbId", "MmbId"]) {
            if self.ctx.debtor_agent_bic.is_none() {
                self.ctx.debtor_agent_bic = Some(text.to_string());
            }
        }
    }

    /// Record the `<CdtTrfTxInf>` subtree, normalising prefixed tag names so a
    /// message like `<pain:CdtTrfTxInf>` still closes the synthetic root.
    fn read_tx(&mut self) -> Result<PainRow, Box<dyn Error>> {
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
        Ok(row_from_tx(&tx, &self.ctx, &self.source)?)
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
