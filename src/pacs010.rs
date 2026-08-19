//! pacs.010 - Financial Institution Direct Debit. One bank collecting from
//! another bank's account by direct debit: the FI counterpart of pacs.003, where
//! both sides are institutions rather than a creditor and its customer.
//!
//! The mid level is `CdtInstr`, the credit instruction. One instruction names
//! the collecting bank once - the creditor, its account and its agent - then
//! holds many `DrctDbtTxInf` children that each name a different debtor bank. So
//! the reader carries that creditor context downward, the same way the pain.001
//! reader carries its payment group.
//!
//! Grain: one row per DrctDbtTxInf.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::pacs008::PmtId;
use crate::wire::{self, money, AcctRef, Agent, Money, Reason, RmtInf};

// ── serde model: the transaction subtree only ────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct DrctDbtTxInf {
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
    #[serde(rename = "Purp")]
    pub purp: Option<Reason>,
    #[serde(rename = "RmtInf")]
    pub rmt_inf: Option<RmtInf>,
}

// ── flattened row ────────────────────────────────────────────────────────────

/// Credit-instruction context carried into every transaction beneath it.
#[derive(Debug, Default, Clone)]
pub struct CdtInstrCtx {
    pub msg_id: Option<String>,
    pub credit_instruction_id: Option<String>,
    pub instructing_agent_bic: Option<String>,
    pub instructed_agent_bic: Option<String>,
    pub creditor_fi: Option<String>,
    pub creditor_account: Option<String>,
    pub creditor_agent_bic: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct FiDdRow {
    pub msg_id: Option<String>,
    pub credit_instruction_id: Option<String>,
    pub instructing_agent_bic: Option<String>,
    pub instructed_agent_bic: Option<String>,
    pub creditor_fi: Option<String>,
    pub creditor_account: Option<String>,
    pub creditor_agent_bic: Option<String>,
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
    pub purpose: Option<String>,
    pub remittance_info: Option<String>,
    pub source_file: Option<String>,
}

pub fn row_from_tx(tx: &DrctDbtTxInf, ctx: &CdtInstrCtx, source: &str) -> Result<FiDdRow, String> {
    let (amount, currency) =
        money(&[tx.sttlm_amt.as_ref()]).map_err(|e| format!("{source}: {e}"))?;

    Ok(FiDdRow {
        msg_id: ctx.msg_id.clone(),
        credit_instruction_id: ctx.credit_instruction_id.clone(),
        instructing_agent_bic: ctx.instructing_agent_bic.clone(),
        instructed_agent_bic: ctx.instructed_agent_bic.clone(),
        creditor_fi: ctx.creditor_fi.clone(),
        creditor_account: ctx.creditor_account.clone(),
        creditor_agent_bic: ctx.creditor_agent_bic.clone(),
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
        purpose: tx.purp.as_ref().and_then(Reason::code),
        remittance_info: tx.rmt_inf.as_ref().and_then(RmtInf::text),
        source_file: Some(source.to_string()),
    })
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct FiDdStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    ctx: CdtInstrCtx,
    /// Seen anywhere in the file; only the EOF check reads it.
    saw_direct_debit: bool,
    /// `path.len()` at the innermost open container of this family.
    /// A `<DrctDbtTxInf>` outside it belongs to another message: pacs.003 and
    /// pain.008 name their transaction element the same.
    in_direct_debit: Option<usize>,
}

impl<R: BufRead> FiDdStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        FiDdStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            ctx: CdtInstrCtx::default(),
            saw_direct_debit: false,
            in_direct_debit: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<FiDdRow>, Box<dyn Error>> {
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
                    if name == "DrctDbtTxInf" && self.in_direct_debit.is_some() {
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
                    return if self.saw_direct_debit {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no <FIDrctDbt> found - is this a pacs.010 financial \
                             institution direct debit?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Tx => return Ok(Some(self.read_tx()?)),
                Act::Push(name) => {
                    if name == "FIDrctDbt" || name.starts_with("pacs.010.") {
                        self.saw_direct_debit = true;
                        self.in_direct_debit = Some(self.path.len());
                        self.ctx = CdtInstrCtx::default();
                    }
                    // a new credit instruction replaces the previous creditor
                    if name == "CdtInstr" {
                        let msg_id = self.ctx.msg_id.clone();
                        self.ctx = CdtInstrCtx {
                            msg_id,
                            ..Default::default()
                        };
                    }
                    self.path.push(name);
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
        if self.in_direct_debit == Some(self.path.len()) {
            self.in_direct_debit = None;
        }
    }

    /// Capture instruction-level leaves by path tail. Transaction-internal
    /// elements live inside the `<DrctDbtTxInf>` subtree, which never enters
    /// `path`, so these tails cannot collide with a debtor bank's own agent.
    fn capture(&mut self, text: &str) {
        let p = &self.path;
        let tail = |suffix: &[&str]| wire::ends_with(p, suffix);

        if tail(&["GrpHdr", "MsgId"]) {
            self.ctx.msg_id = Some(text.to_string());
        } else if tail(&["CdtInstr", "CdtId"]) {
            self.ctx.credit_instruction_id = Some(text.to_string());
        } else if tail(&["InstgAgt", "FinInstnId", "BICFI"])
            || tail(&["InstgAgt", "FinInstnId", "BIC"])
        {
            self.ctx.instructing_agent_bic = Some(text.to_string());
        } else if tail(&["InstgAgt", "FinInstnId", "ClrSysMmbId", "MmbId"]) {
            // a clearing-system member id identifies the agent only when no BIC did
            self.ctx
                .instructing_agent_bic
                .get_or_insert_with(|| text.to_string());
        } else if tail(&["InstdAgt", "FinInstnId", "BICFI"])
            || tail(&["InstdAgt", "FinInstnId", "BIC"])
        {
            self.ctx.instructed_agent_bic = Some(text.to_string());
        } else if tail(&["InstdAgt", "FinInstnId", "ClrSysMmbId", "MmbId"]) {
            self.ctx
                .instructed_agent_bic
                .get_or_insert_with(|| text.to_string());
        } else if tail(&["CdtrAgt", "FinInstnId", "BICFI"])
            || tail(&["CdtrAgt", "FinInstnId", "BIC"])
        {
            self.ctx.creditor_agent_bic = Some(text.to_string());
        } else if tail(&["CdtrAgt", "FinInstnId", "ClrSysMmbId", "MmbId"]) {
            self.ctx
                .creditor_agent_bic
                .get_or_insert_with(|| text.to_string());
        } else if tail(&["Cdtr", "FinInstnId", "BICFI"]) || tail(&["Cdtr", "FinInstnId", "BIC"]) {
            self.ctx.creditor_fi = Some(text.to_string());
        } else if tail(&["Cdtr", "FinInstnId", "ClrSysMmbId", "MmbId"])
            || tail(&["Cdtr", "FinInstnId", "Nm"])
        {
            // BIC, else clearing member id, else name: the order `wire::Agent::id`
            // resolves, applied here where the leaves arrive one at a time
            self.ctx.creditor_fi.get_or_insert_with(|| text.to_string());
        } else if tail(&["CdtrAcct", "Id", "IBAN"]) {
            self.ctx.creditor_account = Some(text.to_string());
        } else if tail(&["CdtrAcct", "Id", "Othr", "Id"]) {
            self.ctx
                .creditor_account
                .get_or_insert_with(|| text.to_string());
        }
    }

    /// Record the `<DrctDbtTxInf>` subtree and deserialize it.
    fn read_tx(&mut self) -> Result<FiDdRow, Box<dyn Error>> {
        let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "DrctDbtTxInf")?;
        let tx: DrctDbtTxInf = quick_xml::de::from_str(&xml)?;
        Ok(row_from_tx(&tx, &self.ctx, &self.source)?)
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
