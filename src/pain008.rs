//! pain.008 — Customer Direct Debit Initiation. The pull half of the payments
//! world: the CREDITOR asks its bank to collect money from many debtors, the
//! mirror image of pain.001 where the debtor pushes money to many creditors.
//!
//! The mirror is structural: here the **creditor lives on the `PmtInf` group**
//! — the collector, its account, its agent, the collection date and its
//! creditor scheme id — and every `DrctDbtTxInf` names a different debtor to
//! charge. So the reader carries group context downward exactly as pain.001
//! does, with the sides flipped.
//!
//! What has no pain.001 counterpart is the **mandate**: the debtor's signed
//! authorisation (`MndtRltdInf`), which is what makes pulling money from
//! someone else's account legal. `mandate_id` and `mandate_signed_on` are
//! first-class columns, and `sequence_type` (FRST/RCUR/OOFF/FNAL) says where in
//! the mandate's life this collection sits.
//!
//! Grain: one row per `DrctDbtTxInf`.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::wire::{self, AcctRef, Agent, Money, PartyName, RmtInf};

// ── serde model: the transaction subtree only ────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct DrctDbtTxInf {
    #[serde(rename = "PmtId")]
    pub pmt_id: Option<PmtId>,
    /// Direct child here — pain.001 wraps it in `<Amt>`, pain.008 does not.
    #[serde(rename = "InstdAmt")]
    pub instd_amt: Option<Money>,
    #[serde(rename = "ChrgBr")]
    pub chrg_br: Option<String>,
    #[serde(rename = "PmtTpInf")]
    pub pmt_tp_inf: Option<PmtTpInf>,
    #[serde(rename = "DrctDbtTx")]
    pub drct_dbt_tx: Option<DrctDbtTx>,
    #[serde(rename = "Dbtr")]
    pub dbtr: Option<PartyName>,
    #[serde(rename = "DbtrAcct")]
    pub dbtr_acct: Option<AcctRef>,
    #[serde(rename = "DbtrAgt")]
    pub dbtr_agt: Option<Agent>,
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

#[derive(Debug, Deserialize)]
pub struct PmtTpInf {
    #[serde(rename = "SeqTp")]
    pub seq_tp: Option<String>,
}

/// The direct-debit specifics: the mandate, and sometimes a per-transaction
/// creditor scheme id.
#[derive(Debug, Deserialize)]
pub struct DrctDbtTx {
    #[serde(rename = "MndtRltdInf")]
    pub mndt: Option<MndtRltdInf>,
}

#[derive(Debug, Deserialize)]
pub struct MndtRltdInf {
    #[serde(rename = "MndtId")]
    pub mndt_id: Option<String>,
    #[serde(rename = "DtOfSgntr")]
    pub dt_of_sgntr: Option<String>,
}

// ── flattened row ────────────────────────────────────────────────────────────

/// Group-level context carried into every collection of a `PmtInf`: the
/// collector's side, plus the collection date and mandate scheme.
#[derive(Debug, Default, Clone)]
pub struct GroupCtx {
    pub msg_id: Option<String>,
    pub initiating_party: Option<String>,
    pub payment_info_id: Option<String>,
    pub payment_method: Option<String>,
    pub sequence_type: Option<String>,
    pub requested_collection_date: Option<String>,
    pub creditor_name: Option<String>,
    pub creditor_account: Option<String>,
    pub creditor_agent_bic: Option<String>,
    pub creditor_scheme_id: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct DdRow {
    pub msg_id: Option<String>,
    pub initiating_party: Option<String>,
    pub payment_info_id: Option<String>,
    pub payment_method: Option<String>,
    pub sequence_type: Option<String>,
    pub requested_collection_date: Option<String>,
    pub creditor_name: Option<String>,
    pub creditor_account: Option<String>,
    pub creditor_agent_bic: Option<String>,
    pub creditor_scheme_id: Option<String>,
    pub instr_id: Option<String>,
    pub end_to_end_id: Option<String>,
    pub uetr: Option<String>,
    /// Exact amount scaled by `10^decimal::SCALE`; never a float.
    pub amount: Option<i128>,
    pub currency: Option<String>,
    pub charge_bearer: Option<String>,
    pub mandate_id: Option<String>,
    pub mandate_signed_on: Option<String>,
    pub debtor_name: Option<String>,
    pub debtor_account: Option<String>,
    pub debtor_agent_bic: Option<String>,
    pub remittance_info: Option<String>,
    pub source_file: Option<String>,
}

pub fn row_from_tx(tx: &DrctDbtTxInf, ctx: &GroupCtx, source: &str) -> Result<DdRow, String> {
    let (amount, currency) =
        wire::money(&[tx.instd_amt.as_ref()]).map_err(|e| format!("{source}: {e}"))?;
    let mndt = tx.drct_dbt_tx.as_ref().and_then(|d| d.mndt.as_ref());

    Ok(DdRow {
        msg_id: ctx.msg_id.clone(),
        initiating_party: ctx.initiating_party.clone(),
        payment_info_id: ctx.payment_info_id.clone(),
        payment_method: ctx.payment_method.clone(),
        // where this collection sits in the mandate's life; a transaction may
        // restate it, overriding the group
        sequence_type: tx
            .pmt_tp_inf
            .as_ref()
            .and_then(|p| p.seq_tp.clone())
            .or_else(|| ctx.sequence_type.clone()),
        requested_collection_date: ctx.requested_collection_date.clone(),
        creditor_name: ctx.creditor_name.clone(),
        creditor_account: ctx.creditor_account.clone(),
        creditor_agent_bic: ctx.creditor_agent_bic.clone(),
        creditor_scheme_id: ctx.creditor_scheme_id.clone(),
        instr_id: tx.pmt_id.as_ref().and_then(|p| p.instr_id.clone()),
        end_to_end_id: tx.pmt_id.as_ref().and_then(|p| p.end_to_end_id.clone()),
        uetr: tx.pmt_id.as_ref().and_then(|p| p.uetr.clone()),
        amount,
        currency,
        // group-level fallback is applied by the reader, which knows the group
        charge_bearer: tx.chrg_br.clone(),
        mandate_id: mndt.and_then(|m| m.mndt_id.clone()),
        mandate_signed_on: mndt.and_then(|m| m.dt_of_sgntr.clone()),
        debtor_name: tx.dbtr.as_ref().and_then(PartyName::name),
        debtor_account: tx.dbtr_acct.as_ref().and_then(AcctRef::value),
        debtor_agent_bic: tx.dbtr_agt.as_ref().and_then(Agent::id),
        remittance_info: tx.rmt_inf.as_ref().and_then(RmtInf::text),
        source_file: Some(source.to_string()),
    })
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct DdStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    ctx: GroupCtx,
    group_chrg_br: Option<String>,
    /// Seen anywhere in the file; only the EOF check reads it.
    saw_initiation: bool,
    /// `path.len()` at the innermost open container of this family.
    /// A `<DrctDbtTxInf>` outside it belongs to another message: pacs.003
    /// names its transaction element the same.
    in_initiation: Option<usize>,
}

impl<R: BufRead> DdStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        DdStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            ctx: GroupCtx::default(),
            group_chrg_br: None,
            saw_initiation: false,
            in_initiation: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<DdRow>, Box<dyn Error>> {
        loop {
            self.buf.clear();
            let action = match self.reader.read_event_into(&mut self.buf)? {
                Event::Eof => Act::Eof,
                Event::Start(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if name == "DrctDbtTxInf" && self.in_initiation.is_some() {
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
                    return if self.saw_initiation {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no <CstmrDrctDbtInitn> found — is this a pain.008 direct \
                             debit initiation?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Tx => {
                    let xml =
                        wire::record_subtree(&mut self.reader, &mut self.buf, "DrctDbtTxInf")?;
                    let tx: DrctDbtTxInf = quick_xml::de::from_str(&xml)?;
                    let mut row = row_from_tx(&tx, &self.ctx, &self.source)?;
                    if row.charge_bearer.is_none() {
                        row.charge_bearer = self.group_chrg_br.clone();
                    }
                    return Ok(Some(row));
                }
                Act::Push(name) => {
                    if name == "CstmrDrctDbtInitn" || name.starts_with("pain.008.") {
                        self.saw_initiation = true;
                        self.in_initiation = Some(self.path.len());
                        self.ctx = GroupCtx::default();
                        self.group_chrg_br = None;
                    }
                    // a new payment group replaces the previous collector context
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
                    self.pop();
                }
                Act::Text(t) => self.capture(&t),
                Act::None => {}
            }
        }
    }

    fn pop(&mut self) {
        self.path.pop();
        if self.in_initiation == Some(self.path.len()) {
            self.in_initiation = None;
        }
    }

    /// Capture group-level leaves by path tail; transaction-internal elements
    /// live inside the `<DrctDbtTxInf>` subtree, which never enters `path`.
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
        } else if tail(&["PmtInf", "PmtTpInf", "SeqTp"]) {
            self.ctx.sequence_type = Some(text.to_string());
        } else if tail(&["PmtInf", "ReqdColltnDt"])
            || tail(&["ReqdColltnDt", "Dt"])
            || tail(&["ReqdColltnDt", "DtTm"])
        {
            self.ctx.requested_collection_date = Some(text.to_string());
        } else if tail(&["PmtInf", "ChrgBr"]) {
            self.group_chrg_br = Some(text.to_string());
        } else if tail(&["Cdtr", "Nm"]) || tail(&["Cdtr", "Pty", "Nm"]) {
            self.ctx.creditor_name = Some(text.to_string());
        } else if tail(&["CdtrAcct", "Id", "IBAN"]) {
            self.ctx.creditor_account = Some(text.to_string());
        } else if tail(&["CdtrAcct", "Id", "Othr", "Id"]) {
            self.ctx
                .creditor_account
                .get_or_insert_with(|| text.to_string());
        } else if tail(&["CdtrAgt", "FinInstnId", "BICFI"])
            || tail(&["CdtrAgt", "FinInstnId", "BIC"])
        {
            self.ctx.creditor_agent_bic = Some(text.to_string());
        } else if tail(&["CdtrAgt", "FinInstnId", "ClrSysMmbId", "MmbId"]) {
            self.ctx
                .creditor_agent_bic
                .get_or_insert_with(|| text.to_string());
        } else if tail(&["CdtrSchmeId", "Id", "PrvtId", "Othr", "Id"]) {
            // the SEPA creditor identifier the mandate is registered under
            self.ctx.creditor_scheme_id = Some(text.to_string());
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
