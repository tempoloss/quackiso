//! pain.009 — Mandate Initiation Request. The creditor asking for the
//! authorisation a direct debit needs before any money can be pulled.
//!
//! Shape:
//!
//! ```text
//! MndtInitnReq
//!   GrpHdr             — message id, creation time, who initiated it
//!   Mndt (1..n)        — one mandate per record: the parties, the amount,
//!                        how often it may be collected
//! ```
//!
//! pain.008 carries `mandate_id`, `mandate_signed_on` and `sequence_type` on
//! every collection, so a collection could always be joined to a mandate id.
//! What was unreadable is the mandate itself — who registered it, against
//! which account, for how much and how often.
//!
//! There is **no signature date in the mandate block**: `DtOfSgntr` lives only
//! in pain.008's `MndtRltdInf`, so there is no `mandate_signed_on` column here
//! to be NULL in every file.
//!
//! This module owns the mandate shapes the other three mandate readers reuse:
//! `Mndt` is the same ISO type in the new state of a pain.010 amendment and in
//! the original mandate a pain.011 or pain.012 repeats, and `GrpCtx` is the
//! group header all four share.
//!
//! Grain: one row per `Mndt`.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::wire::{self, AcctRef, Agent, PartyName};

// ── serde model: the record subtree only ─────────────────────────────────────

/// The mandate (ISO `Mandate7`), as it appears in all four mandate messages.
#[derive(Debug, Deserialize)]
pub struct Mndt {
    #[serde(rename = "MndtId")]
    pub mndt_id: Option<String>,
    #[serde(rename = "MndtReqId")]
    pub mndt_req_id: Option<String>,
    #[serde(rename = "Ocrncs")]
    pub ocrncs: Option<Ocrncs>,
    #[serde(rename = "ColltnAmt")]
    pub colltn_amt: Option<wire::Money>,
    #[serde(rename = "Cdtr")]
    pub cdtr: Option<PartyName>,
    #[serde(rename = "CdtrAcct")]
    pub cdtr_acct: Option<AcctRef>,
    #[serde(rename = "CdtrAgt")]
    pub cdtr_agt: Option<Agent>,
    #[serde(rename = "Dbtr")]
    pub dbtr: Option<PartyName>,
    #[serde(rename = "DbtrAcct")]
    pub dbtr_acct: Option<AcctRef>,
    #[serde(rename = "DbtrAgt")]
    pub dbtr_agt: Option<Agent>,
    #[serde(rename = "UltmtDbtr")]
    pub ultmt_dbtr: Option<PartyName>,
    #[serde(rename = "RfrdDoc")]
    pub rfrd_doc: Option<RfrdDoc>,
}

/// When and how often the creditor may collect.
#[derive(Debug, Deserialize)]
pub struct Ocrncs {
    #[serde(rename = "SeqTp")]
    pub seq_tp: Option<String>,
    #[serde(rename = "Frqcy")]
    pub frqcy: Option<Frqcy>,
    /// A validity window, which some mandates state instead of naming the
    /// first and final collection dates.
    #[serde(rename = "Drtn")]
    pub drtn: Option<Drtn>,
    #[serde(rename = "FrstColltnDt")]
    pub frst_colltn_dt: Option<String>,
    #[serde(rename = "FnlColltnDt")]
    pub fnl_colltn_dt: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Frqcy {
    #[serde(rename = "Tp")]
    pub tp: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Drtn {
    #[serde(rename = "FrDt")]
    pub fr_dt: Option<String>,
    #[serde(rename = "ToDt")]
    pub to_dt: Option<String>,
}

/// The contract or invoice the mandate was signed against.
#[derive(Debug, Deserialize)]
pub struct RfrdDoc {
    #[serde(rename = "Nb")]
    pub nb: Option<String>,
}

impl Mndt {
    pub fn sequence_type(&self) -> Option<String> {
        self.ocrncs.as_ref().and_then(|o| o.seq_tp.clone())
    }

    pub fn frequency(&self) -> Option<String> {
        self.ocrncs
            .as_ref()
            .and_then(|o| o.frqcy.as_ref())
            .and_then(|f| f.tp.clone())
    }

    pub fn first_collection_date(&self) -> Option<String> {
        let o = self.ocrncs.as_ref()?;
        o.frst_colltn_dt
            .clone()
            .or_else(|| o.drtn.as_ref().and_then(|d| d.fr_dt.clone()))
    }

    pub fn final_collection_date(&self) -> Option<String> {
        let o = self.ocrncs.as_ref()?;
        o.fnl_colltn_dt
            .clone()
            .or_else(|| o.drtn.as_ref().and_then(|d| d.to_dt.clone()))
    }

    pub fn amount(&self) -> Result<(Option<i128>, Option<String>), String> {
        wire::money(&[self.colltn_amt.as_ref()])
    }

    pub fn creditor_name(&self) -> Option<String> {
        self.cdtr.as_ref().and_then(PartyName::name)
    }

    pub fn creditor_account(&self) -> Option<String> {
        self.cdtr_acct.as_ref().and_then(AcctRef::value)
    }

    pub fn creditor_agent_bic(&self) -> Option<String> {
        self.cdtr_agt.as_ref().and_then(Agent::id)
    }

    pub fn debtor_name(&self) -> Option<String> {
        self.dbtr.as_ref().and_then(PartyName::name)
    }

    pub fn debtor_account(&self) -> Option<String> {
        self.dbtr_acct.as_ref().and_then(AcctRef::value)
    }

    pub fn debtor_agent_bic(&self) -> Option<String> {
        self.dbtr_agt.as_ref().and_then(Agent::id)
    }

    pub fn ultimate_debtor_name(&self) -> Option<String> {
        self.ultmt_dbtr.as_ref().and_then(PartyName::name)
    }

    pub fn referred_document_number(&self) -> Option<String> {
        self.rfrd_doc.as_ref().and_then(|d| d.nb.clone())
    }
}

// ── flattened row ────────────────────────────────────────────────────────────

/// The group header of a mandate message. Identical in all four, so all four
/// carry this and fill it with [`capture_group_header`].
#[derive(Debug, Default, Clone)]
pub struct GrpCtx {
    pub msg_id: Option<String>,
    pub created: Option<String>,
    pub initiating_party: Option<String>,
    pub instructing_agent_bic: Option<String>,
    pub instructed_agent_bic: Option<String>,
}

/// Group-header leaves, by path tail. Returns true when the text belonged to
/// the header, so a reader's `capture` starts with this and falls through to
/// its own tails.
///
/// A BIC sets the agent; a clearing-system member id only fills a gap, so a
/// clearing id never overwrites a BIC.
pub fn capture_group_header(ctx: &mut GrpCtx, path: &[String], text: &str) -> bool {
    let tail = |suffix: &[&str]| wire::ends_with(path, suffix);

    if tail(&["GrpHdr", "MsgId"]) {
        ctx.msg_id = Some(text.to_string());
    } else if tail(&["GrpHdr", "CreDtTm"]) {
        ctx.created = Some(text.to_string());
    } else if tail(&["InitgPty", "Nm"]) {
        ctx.initiating_party = Some(text.to_string());
    } else if tail(&["InstgAgt", "FinInstnId", "BICFI"]) || tail(&["InstgAgt", "FinInstnId", "BIC"])
    {
        ctx.instructing_agent_bic = Some(text.to_string());
    } else if tail(&["InstgAgt", "FinInstnId", "ClrSysMmbId", "MmbId"]) {
        ctx.instructing_agent_bic
            .get_or_insert_with(|| text.to_string());
    } else if tail(&["InstdAgt", "FinInstnId", "BICFI"]) || tail(&["InstdAgt", "FinInstnId", "BIC"])
    {
        ctx.instructed_agent_bic = Some(text.to_string());
    } else if tail(&["InstdAgt", "FinInstnId", "ClrSysMmbId", "MmbId"]) {
        ctx.instructed_agent_bic
            .get_or_insert_with(|| text.to_string());
    } else {
        return false;
    }
    true
}

/// One mandate. The initiation request states no agents of its own in any
/// published example, so the instructing pair the amendment and cancellation
/// readers expose has no column here.
#[derive(Debug, Default, Clone)]
pub struct MndtRow {
    pub msg_id: Option<String>,
    pub created: Option<String>,
    pub initiating_party: Option<String>,
    pub mandate_id: Option<String>,
    /// The id the *request* is filed under, which a mandate that has not been
    /// registered yet is the only identifier for.
    pub mandate_request_id: Option<String>,
    pub sequence_type: Option<String>,
    pub frequency: Option<String>,
    pub first_collection_date: Option<String>,
    pub final_collection_date: Option<String>,
    /// The fixed amount of each collection, scaled; never a float.
    pub collection_amount: Option<i128>,
    pub currency: Option<String>,
    pub creditor_name: Option<String>,
    pub creditor_account: Option<String>,
    pub creditor_agent_bic: Option<String>,
    pub debtor_name: Option<String>,
    pub debtor_account: Option<String>,
    pub debtor_agent_bic: Option<String>,
    pub ultimate_debtor_name: Option<String>,
    pub referred_document_number: Option<String>,
    pub source_file: Option<String>,
}

pub fn row_from_mandate(m: &Mndt, ctx: &GrpCtx, source: &str) -> Result<MndtRow, String> {
    let (collection_amount, currency) = m.amount().map_err(|e| format!("{source}: {e}"))?;

    Ok(MndtRow {
        msg_id: ctx.msg_id.clone(),
        created: ctx.created.clone(),
        initiating_party: ctx.initiating_party.clone(),
        mandate_id: m.mndt_id.clone(),
        mandate_request_id: m.mndt_req_id.clone(),
        sequence_type: m.sequence_type(),
        frequency: m.frequency(),
        first_collection_date: m.first_collection_date(),
        final_collection_date: m.final_collection_date(),
        collection_amount,
        currency,
        creditor_name: m.creditor_name(),
        creditor_account: m.creditor_account(),
        creditor_agent_bic: m.creditor_agent_bic(),
        debtor_name: m.debtor_name(),
        debtor_account: m.debtor_account(),
        debtor_agent_bic: m.debtor_agent_bic(),
        ultimate_debtor_name: m.ultimate_debtor_name(),
        referred_document_number: m.referred_document_number(),
        source_file: Some(source.to_string()),
    })
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct MndtStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    ctx: GrpCtx,
    saw_request: bool,
    /// `path.len()` at the innermost open container of this family. A `<Mndt>`
    /// outside it belongs to another message: pain.010 states the new mandate
    /// with the same element name.
    in_request: Option<usize>,
}

impl<R: BufRead> MndtStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        MndtStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            ctx: GrpCtx::default(),
            saw_request: false,
            in_request: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<MndtRow>, Box<dyn Error>> {
        loop {
            self.buf.clear();
            let action = match self.reader.read_event_into(&mut self.buf)? {
                Event::Eof => Act::Eof,
                Event::Start(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if name == "Mndt" && self.in_request.is_some() {
                        Act::Mandate
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
                    return if self.saw_request {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no <MndtInitnReq> found — is this a pain.009 mandate \
                             initiation request?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Mandate => {
                    let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "Mndt")?;
                    let m: Mndt = quick_xml::de::from_str(&xml)?;
                    return Ok(Some(row_from_mandate(&m, &self.ctx, &self.source)?));
                }
                Act::Push(name) => {
                    if name == "MndtInitnReq" || name.starts_with("pain.009.") {
                        self.saw_request = true;
                        self.in_request = Some(self.path.len());
                        self.ctx = GrpCtx::default();
                    }
                    self.path.push(name);
                }
                Act::Pop => self.pop(),
                Act::Text(t) => self.capture(&t),
                Act::None => {}
            }
        }
    }

    fn pop(&mut self) {
        self.path.pop();
        if self.in_request == Some(self.path.len()) {
            self.in_request = None;
        }
    }

    fn capture(&mut self, text: &str) {
        capture_group_header(&mut self.ctx, &self.path, text);
    }
}

enum Act {
    Eof,
    Mandate,
    Push(String),
    Pop,
    Text(String),
    None,
}
