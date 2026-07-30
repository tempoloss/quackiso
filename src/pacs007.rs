//! pacs.007 — FI-to-FI Payment Reversal. The twin of pacs.004 with the
//! direction flipped at the source: a return is the *receiver* sending money
//! back, a reversal is the *sender* taking a settled payment back — typically
//! a direct debit collected in error, undone by the bank that collected it.
//!
//! The vocabulary is the return's with `Rvsl` in place of `Rtr`: `RvslId`,
//! `RvsdIntrBkSttlmAmt`, `RvslRsnInf`. As in pacs.004, the reversed amount can
//! be smaller than the original when charges were kept, so both amounts are
//! columns. Unlike pacs.004 there is no `RtrChain`: the parties appear only in
//! the carried copy of the original, whose sides are the original sides.
//!
//! Grain: one row per `TxInf`.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::wire::{
    self, money, AcctRef, Agent, Money, OrgnlGrpInf, OrgnlTxRef, PartyName, ReasonInfo, RmtInf,
};

// ── serde model: the transaction subtree only ────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TxInf {
    #[serde(rename = "RvslId")]
    pub rvsl_id: Option<String>,
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
    /// What actually went back.
    #[serde(rename = "RvsdIntrBkSttlmAmt")]
    pub rvsd_sttlm_amt: Option<Money>,
    #[serde(rename = "RvsdInstdAmt")]
    pub rvsd_instd_amt: Option<Money>,
    #[serde(rename = "OrgnlIntrBkSttlmAmt")]
    pub orgnl_sttlm_amt: Option<Money>,
    #[serde(rename = "IntrBkSttlmDt")]
    pub sttlm_dt: Option<String>,
    #[serde(rename = "ChrgBr")]
    pub chrg_br: Option<String>,
    #[serde(rename = "RvslRsnInf", default)]
    pub rsn_inf: Vec<ReasonInfo>,
    #[serde(rename = "OrgnlTxRef")]
    pub orgnl_tx_ref: Option<OrgnlTxRef>,
}

// ── flattened row ────────────────────────────────────────────────────────────

/// Message-level context: pacs.007 states the original message reference once,
/// next to the transactions, and may state a settlement date for the whole
/// message.
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
pub struct RvslRow {
    pub msg_id: Option<String>,
    pub reversal_id: Option<String>,
    pub original_msg_id: Option<String>,
    pub original_msg_name_id: Option<String>,
    pub original_instr_id: Option<String>,
    pub original_end_to_end_id: Option<String>,
    pub original_tx_id: Option<String>,
    pub original_uetr: Option<String>,
    /// Reversed amount, scaled by `10^decimal::SCALE`; never a float.
    pub amount: Option<i128>,
    pub currency: Option<String>,
    pub original_amount: Option<i128>,
    pub original_currency: Option<String>,
    pub settlement_date: Option<String>,
    pub charge_bearer: Option<String>,
    pub reversal_reason_code: Option<String>,
    pub reversal_reason_info: Option<String>,
    pub reversal_originator: Option<String>,
    pub original_debtor_name: Option<String>,
    pub original_debtor_account: Option<String>,
    pub original_debtor_agent_bic: Option<String>,
    pub original_creditor_name: Option<String>,
    pub original_creditor_account: Option<String>,
    pub original_creditor_agent_bic: Option<String>,
    pub remittance_info: Option<String>,
    pub source_file: Option<String>,
}

pub fn row_from_tx(tx: &TxInf, ctx: &GroupCtx, source: &str) -> Result<RvslRow, String> {
    let orgnl = tx.orgnl_tx_ref.as_ref();
    let at = |e: String| format!("{source}: {e}");

    let (amount, currency) =
        money(&[tx.rvsd_sttlm_amt.as_ref(), tx.rvsd_instd_amt.as_ref()]).map_err(at)?;
    let (original_amount, original_currency) = {
        let own = money(&[tx.orgnl_sttlm_amt.as_ref()]).map_err(at)?;
        if own.0.is_some() {
            own
        } else {
            orgnl
                .map(OrgnlTxRef::amount)
                .transpose()
                .map_err(|e| format!("{source}: {e}"))?
                .unwrap_or((None, None))
        }
    };

    // Whole-block reason inheritance, as everywhere in this crate.
    let (reversal_reason_code, reversal_reason_info, reversal_originator) = if tx.rsn_inf.is_empty()
    {
        (
            ctx.reason_code.clone(),
            (!ctx.reason_info.is_empty()).then(|| ctx.reason_info.join(" ")),
            ctx.reason_originator.clone(),
        )
    } else {
        ReasonInfo::collapse(&tx.rsn_inf)
    };

    Ok(RvslRow {
        msg_id: ctx.msg_id.clone(),
        reversal_id: tx.rvsl_id.clone(),
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
        charge_bearer: tx.chrg_br.clone(),
        reversal_reason_code,
        reversal_reason_info,
        reversal_originator,
        original_debtor_name: orgnl
            .and_then(|r| r.dbtr.as_ref())
            .and_then(PartyName::name),
        original_debtor_account: orgnl
            .and_then(|r| r.dbtr_acct.as_ref())
            .and_then(AcctRef::value),
        original_debtor_agent_bic: orgnl.and_then(|r| r.dbtr_agt.as_ref()).and_then(Agent::id),
        original_creditor_name: orgnl
            .and_then(|r| r.cdtr.as_ref())
            .and_then(PartyName::name),
        original_creditor_account: orgnl
            .and_then(|r| r.cdtr_acct.as_ref())
            .and_then(AcctRef::value),
        original_creditor_agent_bic: orgnl.and_then(|r| r.cdtr_agt.as_ref()).and_then(Agent::id),
        remittance_info: orgnl
            .and_then(|r| r.rmt_inf.as_ref())
            .and_then(RmtInf::text),
        source_file: Some(source.to_string()),
    })
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct RvslStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    ctx: GroupCtx,
    /// Whether the message's own container (`<FIToFIPmtRvsl>`, or the versioned
    /// name of the first editions) was seen. `<TxInf>` alone is not identity.
    in_reversal: bool,
}

impl<R: BufRead> RvslStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        RvslStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            ctx: GroupCtx::default(),
            in_reversal: false,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<RvslRow>, Box<dyn Error>> {
        loop {
            self.buf.clear();
            let action = match self.reader.read_event_into(&mut self.buf)? {
                Event::Eof => Act::Eof,
                Event::Start(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if name == "TxInf" && self.in_reversal {
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
                    return if self.in_reversal {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no <FIToFIPmtRvsl> found — is this a pacs.007 payment \
                             reversal?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Tx => {
                    let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "TxInf")?;
                    let tx: TxInf = quick_xml::de::from_str(&xml)?;
                    return Ok(Some(row_from_tx(&tx, &self.ctx, &self.source)?));
                }
                Act::Push(name) => {
                    if name == "FIToFIPmtRvsl" || name.starts_with("pacs.007.") {
                        self.in_reversal = true;
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

    /// Capture message-level leaves; per-transaction copies live inside the
    /// `<TxInf>` subtree, which never enters `path`.
    fn capture(&mut self, text: &str) {
        let tail = |suffix: &[&str]| wire::ends_with(&self.path, suffix);
        if tail(&["GrpHdr", "MsgId"]) {
            self.ctx.msg_id = Some(text.to_string());
        } else if tail(&["GrpHdr", "IntrBkSttlmDt"]) {
            self.ctx.sttlm_dt = Some(text.to_string());
        } else if tail(&["OrgnlGrpInf", "OrgnlMsgId"]) {
            self.ctx.orgnl_msg_id = Some(text.to_string());
        } else if tail(&["OrgnlGrpInf", "OrgnlMsgNmId"]) {
            self.ctx.orgnl_msg_nm_id = Some(text.to_string());
        } else if tail(&["RvslRsnInf", "Rsn", "Cd"]) || tail(&["RvslRsnInf", "Rsn", "Prtry"]) {
            self.ctx.reason_code.get_or_insert_with(|| text.to_string());
        } else if tail(&["RvslRsnInf", "AddtlInf"]) {
            self.ctx.reason_info.push(text.to_string());
        } else if tail(&["RvslRsnInf", "Orgtr", "Nm"]) {
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
