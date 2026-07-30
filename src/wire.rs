//! The pieces every message family repeats: tag-name normalisation, subtree
//! recording, and the ISO 20022 leaf shapes that appear verbatim in camt.05x,
//! pacs.008, pacs.004, pain.001 and pain.002 — an amount with its currency, a
//! party, an account, an agent, a remittance block, a reason code.
//!
//! Nothing message-specific lives here. Each reader keeps its own grain, its own
//! carried context and its own row type; only the shapes that are identical
//! across families are shared. Five copies of one subtree recorder is how a
//! reader gets fixed in four places out of five.
//!
//! camt.05x keeps its own party and amount structs in `model`: a statement entry
//! resolves a counterparty across both sides and the "ultimate" parties, which
//! the payment families have no equivalent of.

use std::borrow::Cow;
use std::error::Error;
use std::io::BufRead;

use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::{Reader, Writer};
use serde::Deserialize;

use crate::decimal;

// ── tag names and paths ──────────────────────────────────────────────────────

/// The local part of a tag name: `urn2:CdtTrfTxInf` becomes `CdtTrfTxInf`.
///
/// Borrowed whenever the bytes are valid UTF-8, which is every real message. A
/// reader asks this once per element, so a 1.7 GB statement asks it tens of
/// millions of times; returning `String` would allocate for every one of them.
pub fn local(name: &[u8]) -> Cow<'_, str> {
    match String::from_utf8_lossy(name) {
        Cow::Borrowed(s) => Cow::Borrowed(after_colon(s)),
        Cow::Owned(s) => Cow::Owned(after_colon(&s).to_string()),
    }
}

fn after_colon(s: &str) -> &str {
    match s.rsplit_once(':') {
        Some((_, local)) => local,
        None => s,
    }
}

/// True when `path` ends with `suffix`.
///
/// Readers capture group-level leaves by path tail. A short tail is safe because
/// transaction subtrees are consumed whole and never enter the path, so
/// `["Dbtr", "Nm"]` cannot match a name that belongs to one transaction.
pub fn ends_with(path: &[String], suffix: &[&str]) -> bool {
    path.len() >= suffix.len()
        && path[path.len() - suffix.len()..]
            .iter()
            .zip(suffix)
            .all(|(a, b)| a == b)
}

/// Copy the subtree whose `<tag>` start event was just consumed and return it as
/// standalone XML, ready for the deserializer.
///
/// Tag names are rewritten to their local part while copying. A message that
/// uses a namespace prefix (`<urn2:Ntry>`, `<pacs:TxInf>`) would otherwise
/// produce a subtree whose synthetic unprefixed root never matches its own
/// closing tag, and serde rejects it as ill-formed. Attributes are copied
/// verbatim: an amount carries its currency there.
///
/// Nesting of `tag` inside itself is counted rather than assumed impossible.
pub fn record_subtree<R: BufRead>(
    reader: &mut Reader<R>,
    buf: &mut Vec<u8>,
    tag: &str,
) -> Result<String, Box<dyn Error>> {
    let mut w = Writer::new(Vec::new());
    w.write_event(Event::Start(BytesStart::new(tag)))?;
    let mut depth = 1usize;
    loop {
        buf.clear();
        match reader.read_event_into(buf)? {
            Event::Eof => return Err(format!("unexpected EOF inside <{tag}>").into()),
            Event::Start(e) => {
                let qname = e.name();
                let name = local(qname.as_ref());
                if name == tag {
                    depth += 1;
                }
                let mut s = BytesStart::new(name.as_ref());
                for a in e.attributes().flatten() {
                    s.push_attribute(a);
                }
                w.write_event(Event::Start(s))?;
            }
            Event::Empty(e) => {
                let qname = e.name();
                let name = local(qname.as_ref());
                let mut s = BytesStart::new(name.as_ref());
                for a in e.attributes().flatten() {
                    s.push_attribute(a);
                }
                w.write_event(Event::Empty(s))?;
            }
            Event::End(e) => {
                let qname = e.name();
                let name = local(qname.as_ref());
                if name == tag {
                    depth -= 1;
                }
                w.write_event(Event::End(BytesEnd::new(name.as_ref())))?;
                if depth == 0 {
                    return Ok(String::from_utf8(w.into_inner())?);
                }
            }
            other => {
                w.write_event(other)?;
            }
        }
    }
}

// ── amounts ──────────────────────────────────────────────────────────────────

/// `<Amt Ccy="EUR">100.00</Amt>` — a currency attribute plus text content.
#[derive(Debug, Deserialize)]
pub struct Money {
    #[serde(rename = "@Ccy")]
    pub ccy: Option<String>,
    #[serde(rename = "$text")]
    pub value: Option<String>,
}

/// The first candidate that carries a value, as an exact scaled integer with the
/// currency that belongs to *that* candidate.
///
/// One message spells its amount in one of several elements (settled, instructed,
/// equivalent), and each element carries its own `Ccy`. Reading the value from
/// one and the currency from another is how a total ends up labelled with the
/// wrong currency.
///
/// A malformed amount is an error, never a NULL: a NULL disappears from a `SUM`
/// and hands back a plausible wrong total.
pub fn money(candidates: &[Option<&Money>]) -> Result<(Option<i128>, Option<String>), String> {
    for c in candidates.iter().flatten() {
        if let Some(scaled) = decimal::scaled_opt(c.value.as_ref())? {
            return Ok((Some(scaled), c.ccy.clone()));
        }
    }
    Ok((None, None))
}

/// pain.001 and pain.002 wrap the instructed amount:
/// `<Amt><InstdAmt Ccy="EUR">…</InstdAmt></Amt>`. An instruction priced in
/// another currency carries `EqvtAmt/Amt` instead.
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

impl AmtBlock {
    pub fn value(&self) -> Result<(Option<i128>, Option<String>), String> {
        money(&[
            self.instd.as_ref(),
            self.eqvt.as_ref().and_then(|e| e.amt.as_ref()),
        ])
    }
}

// ── parties, accounts, agents ────────────────────────────────────────────────

/// A party: `<Cdtr><Nm>…</Nm></Cdtr>`, or one level deeper as
/// `<Cdtr><Pty><Nm>…</Nm></Pty></Cdtr>` from the 2019 versions onwards.
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

/// An account reference. IBAN when there is one, else the proprietary account
/// number under `Othr/Id` — a US or in-house account has no IBAN at all.
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

impl AcctRef {
    pub fn value(&self) -> Option<String> {
        let id = self.id.as_ref()?;
        id.iban
            .clone()
            .or_else(|| id.othr.as_ref().and_then(|o| o.id.clone()))
    }
}

/// A financial institution. The BIC element was renamed `BICFI` in the 2019
/// versions, and some messages identify the bank only by clearing-system member
/// id or by name.
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
    /// BIC in either spelling, else the clearing-system member id, else the
    /// institution name. One column, best available identifier.
    pub fn id(&self) -> Option<String> {
        let f = self.fin_instn_id.as_ref()?;
        f.bicfi
            .clone()
            .or_else(|| f.bic.clone())
            .or_else(|| f.clr.as_ref().and_then(|c| c.mmb_id.clone()))
            .or_else(|| f.nm.clone())
    }
}

// ── remittance, reasons, dates ───────────────────────────────────────────────

/// What the payment is for. Free text when the sender wrote any, else the
/// structured creditor reference: corporate messages often carry only that.
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
    #[serde(rename = "AddtlRmtInf", default)]
    pub addtl: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct CdtrRefInf {
    #[serde(rename = "Ref")]
    pub reference: Option<String>,
}

impl RmtInf {
    pub fn text(&self) -> Option<String> {
        if !self.ustrd.is_empty() {
            return Some(self.ustrd.join(" "));
        }
        self.strd
            .iter()
            .find_map(|s| s.cdtr_ref_inf.as_ref().and_then(|c| c.reference.clone()))
            .or_else(|| {
                let addtl: Vec<&str> = self
                    .strd
                    .iter()
                    .flat_map(|s| s.addtl.iter().map(String::as_str))
                    .collect();
                (!addtl.is_empty()).then(|| addtl.join(" "))
            })
    }
}

/// `<Rsn><Cd>AC04</Cd></Rsn>`, or `<Rsn><Prtry>…</Prtry></Rsn>` when the reason
/// is outside the published code list.
#[derive(Debug, Deserialize)]
pub struct Reason {
    #[serde(rename = "Cd")]
    pub cd: Option<String>,
    #[serde(rename = "Prtry")]
    pub prtry: Option<String>,
}

impl Reason {
    pub fn code(&self) -> Option<String> {
        self.cd.clone().or_else(|| self.prtry.clone())
    }
}

/// Who asked for the return, or who reported the status: a name, else whatever
/// identifier the message used instead.
#[derive(Debug, Deserialize)]
pub struct Originator {
    #[serde(rename = "Nm")]
    pub nm: Option<String>,
    #[serde(rename = "Id")]
    pub id: Option<OrgIdWrapper>,
}

#[derive(Debug, Deserialize)]
pub struct OrgIdWrapper {
    #[serde(rename = "OrgId")]
    pub org_id: Option<OrgId>,
}

#[derive(Debug, Deserialize)]
pub struct OrgId {
    /// `BICOrBEI` in the pre-2019 versions, `AnyBIC` after.
    #[serde(rename = "AnyBIC")]
    pub any_bic: Option<String>,
    #[serde(rename = "BICOrBEI")]
    pub bic_or_bei: Option<String>,
    #[serde(rename = "LEI")]
    pub lei: Option<String>,
    #[serde(rename = "Othr")]
    pub othr: Option<OtherId>,
}

impl Originator {
    pub fn name(&self) -> Option<String> {
        if let Some(nm) = &self.nm {
            return Some(nm.clone());
        }
        let org = self.id.as_ref()?.org_id.as_ref()?;
        org.any_bic
            .clone()
            .or_else(|| org.bic_or_bei.clone())
            .or_else(|| org.lei.clone())
            .or_else(|| org.othr.as_ref().and_then(|o| o.id.clone()))
    }
}

/// A date that may be bare text, or wrapped as `<Dt>` or `<DtTm>`. The pain
/// family moved `ReqdExctnDt` from the first form to the second in 2019 and both
/// are in circulation.
#[derive(Debug, Deserialize)]
pub struct DateOrText {
    #[serde(rename = "$text")]
    pub text: Option<String>,
    #[serde(rename = "Dt")]
    pub dt: Option<String>,
    #[serde(rename = "DtTm")]
    pub dt_tm: Option<String>,
}

impl DateOrText {
    pub fn value(&self) -> Option<String> {
        self.dt
            .clone()
            .or_else(|| self.dt_tm.clone())
            .or_else(|| self.text.clone())
    }
}
