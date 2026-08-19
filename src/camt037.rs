//! camt.037 — Debit Authorisation Request. The bank asking its customer for
//! permission to take money back off the account, because the payment that put
//! it there is being cancelled.
//!
//! Shape:
//!
//! ```text
//! DbtAuthstnReq
//!   Assgnmt        — who asks whom
//!   Case           — the case this belongs to
//!   Undrlyg        — the payment being undone
//!   Dtl            — why (CxlRsn), and how much of it (AmtToDbt)
//! ```
//!
//! `amount_to_debit` is the point of the message and is **not** the original
//! amount: a bank that kept its charges asks for less than it paid out. Both
//! are columns, with their own currencies.
//!
//! The first edition (`camt.037.001.01`) names each party by a bare BIC, states
//! the payment inline instead of under `Initn`/`IntrBk`, and spells the reason
//! as a bare code; all three are handled where they are read, so that edition
//! is a row like any other.
//!
//! Grain: one row per message.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::wire::{self, AssignCtx, CaseCtx, Money, UndrlygPmt};

// ── serde model: the detail block ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct Dtl {
    #[serde(rename = "CxlRsn")]
    pub cxl_rsn: Option<CxlRsn>,
    #[serde(rename = "AmtToDbt")]
    pub amt_to_dbt: Option<Money>,
}

/// `<CxlRsn><Cd>CUST</Cd></CxlRsn>`, or the bare `<CxlRsn>CUST</CxlRsn>` of the
/// first edition.
#[derive(Debug, Deserialize)]
pub struct CxlRsn {
    #[serde(rename = "$text")]
    pub text: Option<String>,
    #[serde(rename = "Cd")]
    pub cd: Option<String>,
    #[serde(rename = "Prtry")]
    pub prtry: Option<String>,
}

impl CxlRsn {
    pub fn code(&self) -> Option<String> {
        self.cd
            .clone()
            .or_else(|| self.prtry.clone())
            .or_else(|| self.text.clone())
    }
}

// ── flattened row ────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct DbtReqRow {
    pub assignment_id: Option<String>,
    pub assignment_created: Option<String>,
    pub assigner: Option<String>,
    pub assignee: Option<String>,
    pub case_id: Option<String>,
    pub case_creator: Option<String>,
    pub original_instr_id: Option<String>,
    pub original_amount: Option<i128>,
    pub original_currency: Option<String>,
    pub original_execution_date: Option<String>,
    pub original_settlement_date: Option<String>,
    pub cancellation_reason: Option<String>,
    /// What is being asked for, which is at most the original.
    pub amount_to_debit: Option<i128>,
    pub debit_currency: Option<String>,
    pub source_file: Option<String>,
}

pub fn row_from_case(
    assign: &AssignCtx,
    case: &CaseCtx,
    undrlyg: Option<&UndrlygPmt>,
    dtl: Option<&Dtl>,
    source: &str,
) -> Result<DbtReqRow, String> {
    let at = |e: String| format!("{source}: {e}");
    let (original_amount, original_currency) = undrlyg
        .map(UndrlygPmt::amount)
        .transpose()
        .map_err(at)?
        .unwrap_or((None, None));
    let (amount_to_debit, debit_currency) =
        wire::money(&[dtl.and_then(|d| d.amt_to_dbt.as_ref())]).map_err(at)?;

    Ok(DbtReqRow {
        assignment_id: assign.id.clone(),
        assignment_created: assign.created.clone(),
        assigner: assign.assigner.clone(),
        assignee: assign.assignee.clone(),
        case_id: case.id.clone(),
        case_creator: case.creator.clone(),
        original_instr_id: undrlyg.and_then(UndrlygPmt::instr_id),
        original_amount,
        original_currency,
        original_execution_date: undrlyg.and_then(UndrlygPmt::execution_date),
        original_settlement_date: undrlyg.and_then(UndrlygPmt::settlement_date),
        cancellation_reason: dtl.and_then(|d| d.cxl_rsn.as_ref()).and_then(CxlRsn::code),
        amount_to_debit,
        debit_currency,
        source_file: Some(source.to_string()),
    })
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct DbtReqStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    assign: AssignCtx,
    case: CaseCtx,
    undrlyg: Option<UndrlygPmt>,
    dtl: Option<Dtl>,
    saw_request: bool,
    /// `path.len()` at the innermost open container of this family.
    in_request: Option<usize>,
}

impl<R: BufRead> DbtReqStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        DbtReqStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            assign: AssignCtx::default(),
            case: CaseCtx::default(),
            undrlyg: None,
            dtl: None,
            saw_request: false,
            in_request: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<DbtReqRow>, Box<dyn Error>> {
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
                    match name.as_ref() {
                        "Undrlyg" if self.in_request.is_some() => Act::Undrlyg,
                        "Dtl" if self.in_request.is_some() => Act::Dtl,
                        _ => Act::Push(name.into_owned()),
                    }
                }
                Event::End(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if self.in_request.is_some()
                        && (name == "DbtAuthstnReq" || name.starts_with("camt.037."))
                    {
                        Act::Close
                    } else {
                        Act::Pop
                    }
                }
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
                            "{}: no <DbtAuthstnReq> found — is this a camt.037 debit \
                             authorisation request?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Undrlyg => {
                    let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "Undrlyg")?;
                    self.undrlyg = Some(quick_xml::de::from_str(&xml)?);
                }
                Act::Dtl => {
                    let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "Dtl")?;
                    self.dtl = Some(quick_xml::de::from_str(&xml)?);
                }
                Act::Push(name) => {
                    if name == "DbtAuthstnReq" || name.starts_with("camt.037.") {
                        self.saw_request = true;
                        self.in_request = Some(self.path.len());
                        self.assign = AssignCtx::default();
                        self.case = CaseCtx::default();
                        self.undrlyg = None;
                        self.dtl = None;
                    }
                    self.path.push(name);
                }
                Act::Close => {
                    self.pop();
                    return Ok(Some(row_from_case(
                        &self.assign,
                        &self.case,
                        self.undrlyg.as_ref(),
                        self.dtl.as_ref(),
                        &self.source,
                    )?));
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
        if wire::capture_assignment(&mut self.assign, &self.path, text) {
            return;
        }
        wire::capture_case(&mut self.case, &self.path, text);
    }
}

enum Act {
    Eof,
    Undrlyg,
    Dtl,
    Push(String),
    Pop,
    Close,
    Text(String),
    None,
}
