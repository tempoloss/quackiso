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

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::wire::{self, AcctRef, Agent, AmtBlock, PartyName, RmtInf};

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
    /// Follows the payment into the pacs.008 that settles it and the pacs.004
    /// that returns it. Mandatory from the 2019 versions onwards.
    pub uetr: Option<String>,
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

pub fn row_from_tx(tx: &CdtTrfTxInf, ctx: &GroupCtx, source: &str) -> Result<PainRow, String> {
    // instructed amount, else the equivalent-amount form
    let (amount, currency) = match tx.amt.as_ref() {
        Some(block) => block.value().map_err(|e| format!("{source}: {e}"))?,
        None => (None, None),
    };

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
        uetr: tx.pmt_id.as_ref().and_then(|p| p.uetr.clone()),
        amount,
        currency,
        // group-level fallback is applied by the reader, which knows the group
        charge_bearer: tx.chrg_br.clone(),
        creditor_name: tx.cdtr.as_ref().and_then(PartyName::name),
        creditor_account: tx.cdtr_acct.as_ref().and_then(AcctRef::value),
        creditor_agent_bic: tx.cdtr_agt.as_ref().and_then(Agent::id),
        remittance_info: tx.rmt_inf.as_ref().and_then(RmtInf::text),
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
    /// Whether the message's own container (`<CstmrCdtTrfInitn>`, or the
    /// versioned name of the first editions) was seen. `<CdtTrfTxInf>` alone is
    /// not identity: pacs.008 names its transaction element the same.
    in_initiation: bool,
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
            in_initiation: false,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<PainRow>, Box<dyn Error>> {
        loop {
            self.buf.clear();
            let action = match self.reader.read_event_into(&mut self.buf)? {
                Event::Eof => Act::Eof,
                Event::Start(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if name == "CdtTrfTxInf" && self.in_initiation {
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
                    return if self.in_initiation {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no <CstmrCdtTrfInitn> found — is this a pain.001 payment \
                             initiation?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Tx => {
                    let mut row = self.read_tx()?;
                    if row.charge_bearer.is_none() {
                        row.charge_bearer = self.group_chrg_br.clone();
                    }
                    return Ok(Some(row));
                }
                Act::Push(name) => {
                    if name == "CstmrCdtTrfInitn" || name.starts_with("pain.001.") {
                        self.in_initiation = true;
                    }
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
        let tail = |suffix: &[&str]| wire::ends_with(p, suffix);

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
            // a proprietary account number identifies the payer only when no
            // IBAN did; an IBAN already seen for this group wins
            self.ctx
                .debtor_account
                .get_or_insert_with(|| text.to_string());
        } else if tail(&["DbtrAgt", "FinInstnId", "BICFI"])
            || tail(&["DbtrAgt", "FinInstnId", "BIC"])
        {
            self.ctx.debtor_agent_bic = Some(text.to_string());
        } else if tail(&["DbtrAgt", "FinInstnId", "ClrSysMmbId", "MmbId"]) {
            // likewise: a clearing-system member id only when no BIC did
            self.ctx
                .debtor_agent_bic
                .get_or_insert_with(|| text.to_string());
        }
    }

    /// Record the `<CdtTrfTxInf>` subtree and deserialize it.
    fn read_tx(&mut self) -> Result<PainRow, Box<dyn Error>> {
        let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "CdtTrfTxInf")?;
        let tx: CdtTrfTxInf = quick_xml::de::from_str(&xml)?;
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
