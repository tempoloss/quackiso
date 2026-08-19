//! The pieces every message family repeats: tag-name normalisation, the single
//! text path that makes `CDATA` content and not markup, subtree recording, and
//! the ISO 20022 leaf shapes that appear verbatim in camt.05x, pacs.008,
//! pacs.004, pain.001 and pain.002 — an amount with its currency, a party, an
//! account, an agent, a remittance block, a reason code.
//!
//! Nothing message-specific lives here. Each reader keeps its own grain, its own
//! carried context and its own row type; only the shapes that are identical
//! across families are shared. Five copies of one subtree recorder is how a
//! reader gets fixed in four places out of five.
//!
//! The case family is the one exception. Ten readers — the two cancellation
//! requests, the resolution and the seven investigation messages — parse an
//! identical `Assgnmt` block, seven of them an identical `Case` block, and four
//! an identical `Undrlyg` payment reference, so that context and its two
//! `capture_*` helpers live here instead of in ten copies. Grain and row type
//! still belong to the reader.
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

/// The text of a `Text` or `CDATA` event, trimmed, or `None` when the event is
/// neither or carries only whitespace. CDATA is literal content by definition,
/// so it is not unescaped.
pub fn event_text(ev: &Event) -> Result<Option<String>, Box<dyn Error>> {
    let t = match ev {
        Event::Text(e) => e.unescape()?.trim().to_string(),
        Event::CData(e) => String::from_utf8_lossy(e).trim().to_string(),
        _ => return Ok(None),
    };
    Ok((!t.is_empty()).then_some(t))
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

// ── the shapes every exception message repeats ───────────────────────────────

/// Reference to the original message a status, return or cancellation answers.
#[derive(Debug, Deserialize)]
pub struct OrgnlGrpInf {
    #[serde(rename = "OrgnlMsgId")]
    pub msg_id: Option<String>,
    #[serde(rename = "OrgnlMsgNmId")]
    pub msg_nm_id: Option<String>,
}

/// Why: a return reason, a status reason, or a cancellation reason. One shape
/// across pacs.004, pacs.002, pain.002 and camt.056 — plus the pre-2009
/// spellings (`RtrRsn`, `StsRsn`, `AddtlRtrRsnInf`, `RtrOrgtr`, `StsOrgtr`)
/// that are still in circulation.
#[derive(Debug, Deserialize)]
pub struct ReasonInfo {
    #[serde(rename = "Rsn")]
    pub rsn: Option<Reason>,
    #[serde(rename = "RtrRsn")]
    pub rtr_rsn: Option<Reason>,
    #[serde(rename = "StsRsn")]
    pub sts_rsn: Option<Reason>,
    #[serde(rename = "AddtlInf", default)]
    pub addtl_inf: Vec<String>,
    #[serde(rename = "AddtlRtrRsnInf", default)]
    pub addtl_rtr_rsn_inf: Vec<String>,
    #[serde(rename = "Orgtr")]
    pub orgtr: Option<Originator>,
    #[serde(rename = "RtrOrgtr")]
    pub rtr_orgtr: Option<Originator>,
    #[serde(rename = "StsOrgtr")]
    pub sts_orgtr: Option<Originator>,
}

impl ReasonInfo {
    pub fn code(&self) -> Option<String> {
        self.rsn
            .as_ref()
            .and_then(Reason::code)
            .or_else(|| self.rtr_rsn.as_ref().and_then(Reason::code))
            .or_else(|| self.sts_rsn.as_ref().and_then(Reason::code))
    }

    pub fn info(&self) -> impl Iterator<Item = &str> {
        self.addtl_inf
            .iter()
            .chain(self.addtl_rtr_rsn_inf.iter())
            .map(String::as_str)
    }

    pub fn originator(&self) -> Option<String> {
        self.orgtr
            .as_ref()
            .and_then(Originator::name)
            .or_else(|| self.rtr_orgtr.as_ref().and_then(Originator::name))
            .or_else(|| self.sts_orgtr.as_ref().and_then(Originator::name))
    }

    /// One reason out of a repeatable list: first code, all texts joined, first
    /// originator. Callers inherit a *whole* block from the group level or none
    /// of it — never a transaction's code next to the group's explanation.
    pub fn collapse(list: &[ReasonInfo]) -> (Option<String>, Option<String>, Option<String>) {
        let info: Vec<&str> = list.iter().flat_map(ReasonInfo::info).collect();
        (
            list.iter().find_map(ReasonInfo::code),
            (!info.is_empty()).then(|| info.join(" ")),
            list.iter().find_map(ReasonInfo::originator),
        )
    }
}

/// A copy of the original instruction, carried so the receiver can match a
/// status, return or cancellation without looking the payment up. This is one
/// shared ISO type (`OriginalTransactionReference`), so one struct serves all
/// four exception readers. Its sides are the ORIGINAL sides.
#[derive(Debug, Deserialize)]
pub struct OrgnlTxRef {
    #[serde(rename = "IntrBkSttlmAmt")]
    pub sttlm_amt: Option<Money>,
    #[serde(rename = "Amt")]
    pub amt: Option<AmtBlock>,
    #[serde(rename = "IntrBkSttlmDt")]
    pub sttlm_dt: Option<String>,
    #[serde(rename = "ReqdExctnDt")]
    pub reqd_exctn_dt: Option<DateOrText>,
    #[serde(rename = "Dbtr")]
    pub dbtr: Option<PartyName>,
    #[serde(rename = "DbtrAcct")]
    pub dbtr_acct: Option<AcctRef>,
    #[serde(rename = "DbtrAgt")]
    pub dbtr_agt: Option<Agent>,
    #[serde(rename = "Cdtr")]
    pub cdtr: Option<PartyName>,
    #[serde(rename = "CdtrAcct")]
    pub cdtr_acct: Option<AcctRef>,
    #[serde(rename = "CdtrAgt")]
    pub cdtr_agt: Option<Agent>,
    #[serde(rename = "RmtInf")]
    pub rmt_inf: Option<RmtInf>,
}

impl OrgnlTxRef {
    /// The original amount in whichever element the copy spelled it: interbank
    /// settlement first, then the pain-style instructed/equivalent wrapper.
    pub fn amount(&self) -> Result<(Option<i128>, Option<String>), String> {
        money(&[
            self.sttlm_amt.as_ref(),
            self.amt.as_ref().and_then(|a| a.instd.as_ref()),
            self.amt
                .as_ref()
                .and_then(|a| a.eqvt.as_ref())
                .and_then(|e| e.amt.as_ref()),
        ])
    }
}

// ── the shapes every case message repeats ────────────────────────────────────

/// The assignment: who is asking whom, and when. One per message in all ten
/// case messages — the cancellation requests, the resolution, and the seven
/// investigation messages.
#[derive(Debug, Default, Clone)]
pub struct AssignCtx {
    pub id: Option<String>,
    pub created: Option<String>,
    pub assigner: Option<String>,
    pub assignee: Option<String>,
}

/// Assignment-level leaves, by path tail. Returns true when the text belonged
/// to the assignment, so a reader's `capture` starts with this and falls
/// through to its own tails.
///
/// `Assgnr` and `Assgne` are each a choice of a party or an agent, and one real
/// message mixes the two. A BIC or a name sets the field; a clearing-system
/// member id or an organisation id only fills a gap, so a clearing id never
/// overwrites a BIC.
pub fn capture_assignment(ctx: &mut AssignCtx, path: &[String], text: &str) -> bool {
    let tail = |suffix: &[&str]| ends_with(path, suffix);

    if tail(&["Assgnmt", "Id"]) {
        ctx.id = Some(text.to_string());
    } else if tail(&["Assgnmt", "CreDtTm"]) {
        ctx.created = Some(text.to_string());
    } else if tail(&["Assgnr", "Agt", "FinInstnId", "BICFI"])
        || tail(&["Assgnr", "Agt", "FinInstnId", "BIC"])
        || tail(&["Assgnr", "Pty", "Nm"])
    {
        ctx.assigner = Some(text.to_string());
    } else if tail(&["Assgnr", "Agt", "FinInstnId", "ClrSysMmbId", "MmbId"])
        || tail(&["Assgnr", "Pty", "Id", "OrgId", "AnyBIC"])
        || tail(&["Assgnr", "Pty", "Id", "OrgId", "BICOrBEI"])
    {
        ctx.assigner.get_or_insert_with(|| text.to_string());
    } else if tail(&["Assgne", "Agt", "FinInstnId", "BICFI"])
        || tail(&["Assgne", "Agt", "FinInstnId", "BIC"])
        || tail(&["Assgne", "Pty", "Nm"])
    {
        ctx.assignee = Some(text.to_string());
    } else if tail(&["Assgne", "Agt", "FinInstnId", "ClrSysMmbId", "MmbId"])
        || tail(&["Assgne", "Pty", "Id", "OrgId", "AnyBIC"])
        || tail(&["Assgne", "Pty", "Id", "OrgId", "BICOrBEI"])
    {
        ctx.assignee.get_or_insert_with(|| text.to_string());
    } else if tail(&["Assgnmt", "Assgnr"]) {
        // The 2005 first editions name each side by a bare BIC, with no choice
        // element at all.
        ctx.assigner = Some(text.to_string());
    } else if tail(&["Assgnmt", "Assgne"]) {
        ctx.assignee = Some(text.to_string());
    } else {
        return false;
    }
    true
}

/// The investigation case a message belongs to.
#[derive(Debug, Deserialize)]
pub struct Case {
    #[serde(rename = "Id")]
    pub id: Option<String>,
}

/// The case at message level: its id and who opened it. The seven
/// investigation messages state this beside the assignment rather than inside
/// a transaction, so it is captured by path rather than deserialized.
#[derive(Debug, Default, Clone)]
pub struct CaseCtx {
    pub id: Option<String>,
    pub creator: Option<String>,
}

/// Case-level leaves, by path tail, on the same set/insert rule as
/// [`capture_assignment`]. `Cretr` is the same party choice as `Assgnr`, down
/// to the first edition's bare BIC.
pub fn capture_case(ctx: &mut CaseCtx, path: &[String], text: &str) -> bool {
    let tail = |suffix: &[&str]| ends_with(path, suffix);

    if tail(&["Case", "Id"]) {
        ctx.id = Some(text.to_string());
    } else if tail(&["Cretr", "Agt", "FinInstnId", "BICFI"])
        || tail(&["Cretr", "Agt", "FinInstnId", "BIC"])
        || tail(&["Cretr", "Pty", "Nm"])
        || tail(&["Case", "Cretr"])
    {
        ctx.creator = Some(text.to_string());
    } else if tail(&["Cretr", "Agt", "FinInstnId", "ClrSysMmbId", "MmbId"])
        || tail(&["Cretr", "Pty", "Id", "OrgId", "AnyBIC"])
        || tail(&["Cretr", "Pty", "Id", "OrgId", "BICOrBEI"])
    {
        ctx.creator.get_or_insert_with(|| text.to_string());
    } else {
        return false;
    }
    true
}

/// The payment a case is about. One ISO shape (`UnderlyingTransaction`) across
/// camt.027, camt.028, camt.037 and camt.087: the sender states it either as
/// the initiation it sent or as the interbank leg that settled, and the 2005
/// first editions state it inline with no arm at all.
///
/// Deserialized rather than captured by path: the amount carries its currency
/// in an attribute, which a text-only capture cannot see.
#[derive(Debug, Deserialize)]
pub struct UndrlygPmt {
    #[serde(rename = "Initn")]
    pub initn: Option<UndrlygInitn>,
    #[serde(rename = "IntrBk")]
    pub intr_bk: Option<UndrlygIntrBk>,
    #[serde(rename = "AssgnrInstrId")]
    pub assgnr_instr_id: Option<String>,
    #[serde(rename = "AssgneInstrId")]
    pub assgne_instr_id: Option<String>,
    #[serde(rename = "CcyAmt")]
    pub ccy_amt: Option<Money>,
    #[serde(rename = "ValDt")]
    pub val_dt: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UndrlygInitn {
    #[serde(rename = "OrgnlGrpInf")]
    pub orgnl_grp_inf: Option<OrgnlGrpInf>,
    #[serde(rename = "OrgnlInstrId")]
    pub orgnl_instr_id: Option<String>,
    #[serde(rename = "OrgnlEndToEndId")]
    pub orgnl_end_to_end_id: Option<String>,
    #[serde(rename = "OrgnlInstdAmt")]
    pub orgnl_instd_amt: Option<Money>,
    #[serde(rename = "ReqdExctnDt")]
    pub reqd_exctn_dt: Option<DateOrText>,
    /// A direct debit was to be collected, not executed.
    #[serde(rename = "ReqdColltnDt")]
    pub reqd_colltn_dt: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UndrlygIntrBk {
    #[serde(rename = "OrgnlGrpInf")]
    pub orgnl_grp_inf: Option<OrgnlGrpInf>,
    #[serde(rename = "OrgnlInstrId")]
    pub orgnl_instr_id: Option<String>,
    #[serde(rename = "OrgnlEndToEndId")]
    pub orgnl_end_to_end_id: Option<String>,
    #[serde(rename = "OrgnlIntrBkSttlmAmt")]
    pub orgnl_sttlm_amt: Option<Money>,
    #[serde(rename = "OrgnlIntrBkSttlmDt")]
    pub orgnl_sttlm_dt: Option<String>,
}

impl UndrlygPmt {
    pub fn msg_id(&self) -> Option<String> {
        self.grp_inf().and_then(|g| g.msg_id.clone())
    }

    pub fn msg_name_id(&self) -> Option<String> {
        self.grp_inf().and_then(|g| g.msg_nm_id.clone())
    }

    fn grp_inf(&self) -> Option<&OrgnlGrpInf> {
        self.initn
            .as_ref()
            .and_then(|i| i.orgnl_grp_inf.as_ref())
            .or_else(|| self.intr_bk.as_ref().and_then(|i| i.orgnl_grp_inf.as_ref()))
    }

    pub fn instr_id(&self) -> Option<String> {
        self.initn
            .as_ref()
            .and_then(|i| i.orgnl_instr_id.clone())
            .or_else(|| self.intr_bk.as_ref().and_then(|i| i.orgnl_instr_id.clone()))
            .or_else(|| self.assgnr_instr_id.clone())
            .or_else(|| self.assgne_instr_id.clone())
    }

    pub fn end_to_end_id(&self) -> Option<String> {
        self.initn
            .as_ref()
            .and_then(|i| i.orgnl_end_to_end_id.clone())
            .or_else(|| {
                self.intr_bk
                    .as_ref()
                    .and_then(|i| i.orgnl_end_to_end_id.clone())
            })
    }

    pub fn amount(&self) -> Result<(Option<i128>, Option<String>), String> {
        money(&[
            self.initn.as_ref().and_then(|i| i.orgnl_instd_amt.as_ref()),
            self.intr_bk
                .as_ref()
                .and_then(|i| i.orgnl_sttlm_amt.as_ref()),
            self.ccy_amt.as_ref(),
        ])
    }

    /// When the payment was to leave, on the initiation side.
    pub fn execution_date(&self) -> Option<String> {
        let initn = self.initn.as_ref()?;
        initn
            .reqd_exctn_dt
            .as_ref()
            .and_then(DateOrText::value)
            .or_else(|| initn.reqd_colltn_dt.clone())
    }

    /// When it settled between the banks; the first edition calls it `ValDt`.
    pub fn settlement_date(&self) -> Option<String> {
        self.intr_bk
            .as_ref()
            .and_then(|i| i.orgnl_sttlm_dt.clone())
            .or_else(|| self.val_dt.clone())
    }
}

/// The repeated free-text lines of a reason block as one column, or NULL when
/// there were none.
pub fn join(parts: &[String]) -> Option<String> {
    (!parts.is_empty()).then(|| parts.join(" "))
}

// ── the end of input ─────────────────────────────────────────────────────────

/// The innermost element still open, when input ran out inside one.
///
/// quick-xml raises `IllFormedError::MissingEndTag` only from `read_to_end`,
/// which no reader here calls: a `read_event_into` loop is handed `Event::Eof`
/// with the element stack still full and cannot tell a finished document from a
/// cut-off one. The path stack every reader already keeps is the whole check.
pub fn cut_short(path: &[String]) -> Option<&str> {
    path.last().map(String::as_str)
}

/// `read_event_into`, refusing an end of input that arrives inside an element.
/// Every reader reads through this, so a file that stops halfway cannot come
/// back as an empty result.
pub fn next_event<'b, R: BufRead>(
    reader: &mut Reader<R>,
    buf: &'b mut Vec<u8>,
    path: &[String],
    source: &str,
) -> Result<Event<'b>, Box<dyn Error>> {
    let ev = reader.read_event_into(buf)?;
    if matches!(ev, Event::Eof) {
        if let Some(open) = cut_short(path) {
            return Err(
                format!("{source}: not well-formed XML: end of input inside <{open}>").into(),
            );
        }
    }
    Ok(ev)
}
