//! pain.013 - Creditor Payment Activation Request.
//!
//! The debtor and payment terms live on `PmtInf`; each `CdtTrfTx` names the creditor.
//!
//! Grain: one row per `CdtTrfTx`.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::pacs008::PmtId;
use crate::wire::{self, AcctRef, Agent, AmtBlock, DateOrText, PartyName, RmtInf};

#[derive(Debug, Deserialize)]
pub struct CdtTrfTx {
    #[serde(rename = "PmtId")]
    pub pmt_id: Option<PmtId>,
    #[serde(rename = "ReqdExctnDt")]
    pub reqd_exctn_dt: Option<DateOrText>,
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

#[derive(Debug, Default, Clone)]
pub struct GroupCtx {
    pub msg_id: Option<String>,
    pub initiating_party: Option<String>,
    pub payment_info_id: Option<String>,
    pub payment_method: Option<String>,
    pub requested_execution_date: Option<String>,
    pub expiry_date: Option<String>,
    pub debtor_name: Option<String>,
    pub debtor_account: Option<String>,
    pub debtor_agent_bic: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct ActvtnRow {
    pub msg_id: Option<String>,
    pub initiating_party: Option<String>,
    pub payment_info_id: Option<String>,
    pub payment_method: Option<String>,
    pub requested_execution_date: Option<String>,
    pub expiry_date: Option<String>,
    pub debtor_name: Option<String>,
    pub debtor_account: Option<String>,
    pub debtor_agent_bic: Option<String>,
    pub instr_id: Option<String>,
    pub end_to_end_id: Option<String>,
    pub uetr: Option<String>,
    pub amount: Option<i128>,
    pub currency: Option<String>,
    pub charge_bearer: Option<String>,
    pub creditor_name: Option<String>,
    pub creditor_account: Option<String>,
    pub creditor_agent_bic: Option<String>,
    pub remittance_info: Option<String>,
    pub source_file: Option<String>,
}

pub fn row_from_tx(tx: &CdtTrfTx, ctx: &GroupCtx, source: &str) -> Result<ActvtnRow, String> {
    let (amount, currency) = match tx.amt.as_ref() {
        Some(block) => block.value().map_err(|e| format!("{source}: {e}"))?,
        None => (None, None),
    };

    Ok(ActvtnRow {
        msg_id: ctx.msg_id.clone(),
        initiating_party: ctx.initiating_party.clone(),
        payment_info_id: ctx.payment_info_id.clone(),
        payment_method: ctx.payment_method.clone(),
        requested_execution_date: tx
            .reqd_exctn_dt
            .as_ref()
            .and_then(DateOrText::value)
            .or_else(|| ctx.requested_execution_date.clone()),
        expiry_date: ctx.expiry_date.clone(),
        debtor_name: ctx.debtor_name.clone(),
        debtor_account: ctx.debtor_account.clone(),
        debtor_agent_bic: ctx.debtor_agent_bic.clone(),
        instr_id: tx.pmt_id.as_ref().and_then(|p| p.instr_id.clone()),
        end_to_end_id: tx.pmt_id.as_ref().and_then(|p| p.end_to_end_id.clone()),
        uetr: tx.pmt_id.as_ref().and_then(|p| p.uetr.clone()),
        amount,
        currency,
        charge_bearer: tx.chrg_br.clone(),
        creditor_name: tx.cdtr.as_ref().and_then(PartyName::name),
        creditor_account: tx.cdtr_acct.as_ref().and_then(AcctRef::value),
        creditor_agent_bic: tx.cdtr_agt.as_ref().and_then(Agent::id),
        remittance_info: tx.rmt_inf.as_ref().and_then(RmtInf::text),
        source_file: Some(source.to_string()),
    })
}

pub struct ActvtnStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    ctx: GroupCtx,
    group_chrg_br: Option<String>,
    saw_activation: bool,
    in_activation: Option<usize>,
}

impl<R: BufRead> ActvtnStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        ActvtnStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            ctx: GroupCtx::default(),
            group_chrg_br: None,
            saw_activation: false,
            in_activation: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<ActvtnRow>, Box<dyn Error>> {
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
                    if name == "CdtTrfTx" && self.in_activation.is_some() {
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
                    return if self.saw_activation {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no <CdtrPmtActvtnReq> found - is this a pain.013 creditor payment activation request?",
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
                    if name == "CdtrPmtActvtnReq" || name.starts_with("pain.013.") {
                        self.saw_activation = true;
                        self.in_activation = Some(self.path.len());
                        self.ctx = GroupCtx::default();
                        self.group_chrg_br = None;
                    }
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
        if self.in_activation == Some(self.path.len()) {
            self.in_activation = None;
        }
    }

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
            self.ctx.requested_execution_date = Some(text.to_string());
        } else if tail(&["PmtInf", "XpryDt"])
            || tail(&["XpryDt", "Dt"])
            || tail(&["XpryDt", "DtTm"])
        {
            self.ctx.expiry_date = Some(text.to_string());
        } else if tail(&["PmtInf", "ChrgBr"]) {
            self.group_chrg_br = Some(text.to_string());
        } else if tail(&["Dbtr", "Nm"]) || tail(&["Dbtr", "Pty", "Nm"]) {
            self.ctx.debtor_name = Some(text.to_string());
        } else if tail(&["DbtrAcct", "Id", "IBAN"]) {
            self.ctx.debtor_account = Some(text.to_string());
        } else if tail(&["DbtrAcct", "Id", "Othr", "Id"]) {
            self.ctx
                .debtor_account
                .get_or_insert_with(|| text.to_string());
        } else if tail(&["DbtrAgt", "FinInstnId", "BICFI"])
            || tail(&["DbtrAgt", "FinInstnId", "BIC"])
        {
            self.ctx.debtor_agent_bic = Some(text.to_string());
        } else if tail(&["DbtrAgt", "FinInstnId", "ClrSysMmbId", "MmbId"])
            || tail(&["DbtrAgt", "FinInstnId", "Nm"])
        {
            self.ctx
                .debtor_agent_bic
                .get_or_insert_with(|| text.to_string());
        }
    }

    fn read_tx(&mut self) -> Result<ActvtnRow, Box<dyn Error>> {
        let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "CdtTrfTx")?;
        let tx: CdtTrfTx = quick_xml::de::from_str(&xml)?;
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
