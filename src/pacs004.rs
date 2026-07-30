//! pacs.004 — Payment Return. Sent by an agent to the previous agent in the
//! payment chain to undo a settlement that already happened: a wrong account, a
//! closed account, or an accepted cancellation request.
//!
//! A return is not a payment. Every reference in it points at the message being
//! undone, so the columns are named `original_*` for the message that moved the
//! money and unprefixed for the return itself.
//!
//! Two shapes make this reader more than a copy of `pacs008`:
//!
//! * **The amount can shrink.** `RtrdIntrBkSttlmAmt` is what came back, which is
//!   the original settled amount *minus deducted charges*. Exposing only one
//!   amount would hide a partial return, so `amount` and `original_amount` are
//!   separate columns and a partial return is `amount < original_amount`.
//! * **The return chain swaps the sides.** In `RtrChain`, the debtor is the party
//!   giving the money back — the *original creditor*. Copying `RtrChain/Dbtr`
//!   into a column called `original_debtor_name` names the wrong party with full
//!   confidence, which is worse than a NULL. See `original_parties`.
//!
//! Grain: one row per `TxInf`.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::wire::{
    self, money, AcctRef, Agent, AmtBlock, Money, Originator, PartyName, Reason, RmtInf,
};

// ── serde model: the transaction subtree only ────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TxInf {
    #[serde(rename = "RtrId")]
    pub rtr_id: Option<String>,
    /// Present per transaction in the 2019+ versions; earlier messages carry it
    /// once for the whole group.
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
    /// What actually came back.
    #[serde(rename = "RtrdIntrBkSttlmAmt")]
    pub rtrd_sttlm_amt: Option<Money>,
    /// The returned amount before interbank charges, when the message says so.
    #[serde(rename = "RtrdInstdAmt")]
    pub rtrd_instd_amt: Option<Money>,
    #[serde(rename = "IntrBkSttlmDt")]
    pub sttlm_dt: Option<String>,
    #[serde(rename = "OrgnlIntrBkSttlmAmt")]
    pub orgnl_sttlm_amt: Option<Money>,
    #[serde(rename = "OrgnlIntrBkSttlmDt")]
    pub orgnl_sttlm_dt: Option<String>,
    #[serde(rename = "ChrgBr")]
    pub chrg_br: Option<String>,
    /// Repeatable: a chain of agents may each add a reason.
    #[serde(rename = "RtrRsnInf", default)]
    pub rsn_inf: Vec<RtrRsnInf>,
    #[serde(rename = "RtrChain")]
    pub rtr_chain: Option<RtrChain>,
    #[serde(rename = "OrgnlTxRef")]
    pub orgnl_tx_ref: Option<OrgnlTxRef>,
}

#[derive(Debug, Deserialize)]
pub struct OrgnlGrpInf {
    #[serde(rename = "OrgnlMsgId")]
    pub msg_id: Option<String>,
    #[serde(rename = "OrgnlMsgNmId")]
    pub msg_nm_id: Option<String>,
}

/// Why the money came back. The elements were renamed between versions: `Rsn`
/// was `RtrRsn`, `AddtlInf` was `AddtlRtrRsnInf`, `Orgtr` was `RtrOrgtr`. Both
/// spellings are still in circulation, so both are read.
#[derive(Debug, Deserialize)]
pub struct RtrRsnInf {
    #[serde(rename = "Rsn")]
    pub rsn: Option<Reason>,
    #[serde(rename = "RtrRsn")]
    pub rtr_rsn: Option<Reason>,
    #[serde(rename = "AddtlInf", default)]
    pub addtl_inf: Vec<String>,
    #[serde(rename = "AddtlRtrRsnInf", default)]
    pub addtl_rtr_rsn_inf: Vec<String>,
    #[serde(rename = "Orgtr")]
    pub orgtr: Option<Originator>,
    #[serde(rename = "RtrOrgtr")]
    pub rtr_orgtr: Option<Originator>,
}

impl RtrRsnInf {
    fn code(&self) -> Option<String> {
        self.rsn
            .as_ref()
            .and_then(Reason::code)
            .or_else(|| self.rtr_rsn.as_ref().and_then(Reason::code))
    }

    fn info(&self) -> impl Iterator<Item = &str> {
        self.addtl_inf
            .iter()
            .chain(self.addtl_rtr_rsn_inf.iter())
            .map(String::as_str)
    }

    fn originator(&self) -> Option<String> {
        self.orgtr
            .as_ref()
            .and_then(Originator::name)
            .or_else(|| self.rtr_orgtr.as_ref().and_then(Originator::name))
    }
}

/// The parties of the **return**, not of the payment: its debtor is whoever is
/// giving the money back.
#[derive(Debug, Deserialize)]
pub struct RtrChain {
    #[serde(rename = "Dbtr")]
    pub dbtr: Option<PartyName>,
    #[serde(rename = "DbtrAcct")]
    pub dbtr_acct: Option<AcctRef>,
    #[serde(rename = "Cdtr")]
    pub cdtr: Option<PartyName>,
    #[serde(rename = "CdtrAcct")]
    pub cdtr_acct: Option<AcctRef>,
}

/// A copy of the original instruction, carried so the receiver can match the
/// return without looking the payment up. Its sides are the original sides.
#[derive(Debug, Deserialize)]
pub struct OrgnlTxRef {
    #[serde(rename = "IntrBkSttlmAmt")]
    pub sttlm_amt: Option<Money>,
    #[serde(rename = "Amt")]
    pub amt: Option<AmtBlock>,
    #[serde(rename = "IntrBkSttlmDt")]
    pub sttlm_dt: Option<String>,
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

// ── flattened row ────────────────────────────────────────────────────────────

/// Group-level context carried into every transaction of the message. The 2019+
/// versions repeat `OrgnlGrpInf` inside each `TxInf`; the earlier ones state it
/// once, next to the transactions, and a reader that only looks inside `TxInf`
/// returns NULL for every original reference in those files.
#[derive(Debug, Default, Clone)]
pub struct GroupCtx {
    pub msg_id: Option<String>,
    pub sttlm_dt: Option<String>,
    pub orgnl_msg_id: Option<String>,
    pub orgnl_msg_nm_id: Option<String>,
    pub reason_code: Option<String>,
    pub reason_info: Vec<String>,
    pub reason_originator: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct RtrRow {
    pub msg_id: Option<String>,
    pub return_id: Option<String>,
    pub original_msg_id: Option<String>,
    pub original_msg_name_id: Option<String>,
    pub original_instr_id: Option<String>,
    pub original_end_to_end_id: Option<String>,
    pub original_tx_id: Option<String>,
    pub original_uetr: Option<String>,
    /// Returned amount, scaled by `10^decimal::SCALE`; never a float.
    pub amount: Option<i128>,
    pub currency: Option<String>,
    /// What the payment settled for. Larger than `amount` when charges were
    /// deducted from the return.
    pub original_amount: Option<i128>,
    pub original_currency: Option<String>,
    pub settlement_date: Option<String>,
    pub original_settlement_date: Option<String>,
    pub charge_bearer: Option<String>,
    pub return_reason_code: Option<String>,
    pub return_reason_info: Option<String>,
    pub return_originator: Option<String>,
    pub original_debtor_name: Option<String>,
    pub original_debtor_account: Option<String>,
    pub original_debtor_agent_bic: Option<String>,
    pub original_creditor_name: Option<String>,
    pub original_creditor_account: Option<String>,
    pub original_creditor_agent_bic: Option<String>,
    pub remittance_info: Option<String>,
    pub source_file: Option<String>,
}

/// Resolve the two sides of the *original* payment.
///
/// `OrgnlTxRef` states them directly. When it is absent, `RtrChain` is the only
/// place the names appear — and its sides are the sides of the return, so they
/// are read **crossed**: the party debited for the return is the party that was
/// paid by the original transfer.
///
/// This is not a guess. The SIX interbank example set ships both halves of one
/// transaction: `RTGS_pacs_008_sample_CSTPMT_basic` pays `Uhrengrosshandel
/// Müller` (`Dbtr`) to `Horlogerie du Joux` (`Cdtr`), and
/// `RTGS_pacs_004_sample_FOCR`, which returns that exact UETR, lists `Horlogerie
/// du Joux` under `RtrChain/Dbtr`. `testdata/pacs004_return_chain.xml` and
/// `testdata/pacs008_returned_original.xml` mirror that pair, and the test joins
/// them on the UETR to hold the crossing in place.
fn original_parties(
    orgnl: Option<&OrgnlTxRef>,
    chain: Option<&RtrChain>,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let debtor_name = orgnl
        .and_then(|r| r.dbtr.as_ref())
        .and_then(PartyName::name)
        .or_else(|| {
            chain
                .and_then(|c| c.cdtr.as_ref())
                .and_then(PartyName::name)
        });
    let debtor_account = orgnl
        .and_then(|r| r.dbtr_acct.as_ref())
        .and_then(AcctRef::value)
        .or_else(|| {
            chain
                .and_then(|c| c.cdtr_acct.as_ref())
                .and_then(AcctRef::value)
        });
    let creditor_name = orgnl
        .and_then(|r| r.cdtr.as_ref())
        .and_then(PartyName::name)
        .or_else(|| {
            chain
                .and_then(|c| c.dbtr.as_ref())
                .and_then(PartyName::name)
        });
    let creditor_account = orgnl
        .and_then(|r| r.cdtr_acct.as_ref())
        .and_then(AcctRef::value)
        .or_else(|| {
            chain
                .and_then(|c| c.dbtr_acct.as_ref())
                .and_then(AcctRef::value)
        });
    (debtor_name, debtor_account, creditor_name, creditor_account)
}

pub fn row_from_tx(tx: &TxInf, ctx: &GroupCtx, source: &str) -> Result<RtrRow, String> {
    let orgnl = tx.orgnl_tx_ref.as_ref();
    let at = |e: String| format!("{source}: {e}");

    // Returned first, then the instructed form of the return; both describe the
    // return itself, so whichever is present is this row's amount.
    let (amount, currency) =
        money(&[tx.rtrd_sttlm_amt.as_ref(), tx.rtrd_instd_amt.as_ref()]).map_err(at)?;
    // The original amount can be stated on the transaction or inside the copy of
    // the original instruction, and the copy may use the pain-style wrapper.
    let (original_amount, original_currency) = money(&[
        tx.orgnl_sttlm_amt.as_ref(),
        orgnl.and_then(|r| r.sttlm_amt.as_ref()),
        orgnl
            .and_then(|r| r.amt.as_ref())
            .and_then(|a| a.instd.as_ref()),
        orgnl
            .and_then(|r| r.amt.as_ref())
            .and_then(|a| a.eqvt.as_ref())
            .and_then(|e| e.amt.as_ref()),
    ])
    .map_err(at)?;

    let (
        original_debtor_name,
        original_debtor_account,
        original_creditor_name,
        original_creditor_account,
    ) = original_parties(orgnl, tx.rtr_chain.as_ref());

    // A reason is inherited as a whole block or not at all. Filling a missing
    // explanation from the group while keeping the transaction's own code would
    // print a code next to text that explains a different reason — a sentence
    // nobody in the payment chain wrote.
    let (return_reason_code, return_reason_info, return_originator) = if tx.rsn_inf.is_empty() {
        (
            ctx.reason_code.clone(),
            (!ctx.reason_info.is_empty()).then(|| ctx.reason_info.join(" ")),
            ctx.reason_originator.clone(),
        )
    } else {
        let info: Vec<&str> = tx.rsn_inf.iter().flat_map(RtrRsnInf::info).collect();
        (
            tx.rsn_inf.iter().find_map(RtrRsnInf::code),
            (!info.is_empty()).then(|| info.join(" ")),
            tx.rsn_inf.iter().find_map(RtrRsnInf::originator),
        )
    };

    Ok(RtrRow {
        msg_id: ctx.msg_id.clone(),
        return_id: tx.rtr_id.clone(),
        original_msg_id: tx
            .orgnl_grp_inf
            .as_ref()
            .and_then(|g| g.msg_id.clone())
            .or_else(|| ctx.orgnl_msg_id.clone()),
        original_msg_name_id: tx
            .orgnl_grp_inf
            .as_ref()
            .and_then(|g| g.msg_nm_id.clone())
            .or_else(|| ctx.orgnl_msg_nm_id.clone()),
        original_instr_id: tx.orgnl_instr_id.clone(),
        original_end_to_end_id: tx.orgnl_end_to_end_id.clone(),
        original_tx_id: tx.orgnl_tx_id.clone(),
        original_uetr: tx.orgnl_uetr.clone(),
        amount,
        currency,
        original_amount,
        original_currency,
        settlement_date: tx.sttlm_dt.clone().or_else(|| ctx.sttlm_dt.clone()),
        original_settlement_date: tx
            .orgnl_sttlm_dt
            .clone()
            .or_else(|| orgnl.and_then(|r| r.sttlm_dt.clone())),
        charge_bearer: tx.chrg_br.clone(),
        return_reason_code,
        return_reason_info,
        return_originator,
        original_debtor_name,
        original_debtor_account,
        original_debtor_agent_bic: orgnl.and_then(|r| r.dbtr_agt.as_ref()).and_then(Agent::id),
        original_creditor_name,
        original_creditor_account,
        original_creditor_agent_bic: orgnl.and_then(|r| r.cdtr_agt.as_ref()).and_then(Agent::id),
        remittance_info: orgnl
            .and_then(|r| r.rmt_inf.as_ref())
            .and_then(RmtInf::text),
        source_file: Some(source.to_string()),
    })
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct RtrStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    ctx: GroupCtx,
    /// Whether anything in this file identified it as a payment return. A file
    /// with no `<TxInf>` yields no rows, and an empty result with no error reads
    /// exactly like a message that returned nothing.
    saw_tx: bool,
}

impl<R: BufRead> RtrStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        RtrStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            ctx: GroupCtx::default(),
            saw_tx: false,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<RtrRow>, Box<dyn Error>> {
        loop {
            self.buf.clear();
            let action = match self.reader.read_event_into(&mut self.buf)? {
                Event::Eof => Act::Eof,
                Event::Start(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if name == "TxInf" {
                        Act::Tx
                    } else {
                        Act::Push(name.into_owned())
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
                Act::Eof => {
                    return if self.saw_tx {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no <TxInf> found — is this a pacs.004 payment return?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Tx => {
                    self.saw_tx = true;
                    let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "TxInf")?;
                    let tx: TxInf = quick_xml::de::from_str(&xml)?;
                    return Ok(Some(row_from_tx(&tx, &self.ctx, &self.source)?));
                }
                Act::Push(name) => self.path.push(name),
                Act::Pop => {
                    self.path.pop();
                }
                Act::Text(t) => self.capture(&t),
                Act::None => {}
            }
        }
    }

    /// Capture the message-level leaves by path tail. The per-transaction copies
    /// of the same elements live inside the `<TxInf>` subtree, which never enters
    /// `path`, so these tails only ever see group-level values.
    fn capture(&mut self, text: &str) {
        let p = &self.path;
        let tail = |suffix: &[&str]| wire::ends_with(p, suffix);
        if tail(&["GrpHdr", "MsgId"]) {
            self.ctx.msg_id = Some(text.to_string());
        } else if tail(&["GrpHdr", "IntrBkSttlmDt"]) {
            // SEPA states the settlement date once for the whole message.
            self.ctx.sttlm_dt = Some(text.to_string());
        } else if tail(&["OrgnlGrpInf", "OrgnlMsgId"]) {
            self.ctx.orgnl_msg_id = Some(text.to_string());
        } else if tail(&["OrgnlGrpInf", "OrgnlMsgNmId"]) {
            self.ctx.orgnl_msg_nm_id = Some(text.to_string());
        } else if tail(&["RtrRsnInf", "Rsn", "Cd"])
            || tail(&["RtrRsnInf", "Rsn", "Prtry"])
            || tail(&["RtrRsnInf", "RtrRsn", "Cd"])
            || tail(&["RtrRsnInf", "RtrRsn", "Prtry"])
        {
            if self.ctx.reason_code.is_none() {
                self.ctx.reason_code = Some(text.to_string());
            }
        } else if tail(&["RtrRsnInf", "AddtlInf"]) || tail(&["RtrRsnInf", "AddtlRtrRsnInf"]) {
            self.ctx.reason_info.push(text.to_string());
        } else if tail(&["RtrRsnInf", "Orgtr", "Nm"]) || tail(&["RtrRsnInf", "RtrOrgtr", "Nm"]) {
            self.ctx.reason_originator = Some(text.to_string());
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
