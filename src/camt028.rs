//! camt.028 — Additional Payment Information. The message that answers an
//! investigation with the detail it asked for: what the payment was for, and
//! which payment it was.
//!
//! Shape:
//!
//! ```text
//! AddtlPmtInf
//!   Assgnmt        — who answers whom
//!   Case           — the investigation being answered
//!   Undrlyg        — the payment, as an initiation or an interbank leg
//!   Inf            — the information itself; in every published sample the
//!                    remittance detail the other side was missing
//! ```
//!
//! The published samples name the payment by its instruction id and never by
//! the original message id, so there is no `original_msg_id` column to be NULL
//! in every file.
//!
//! Grain: one row per message.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::wire::{self, AssignCtx, CaseCtx, RmtInf, UndrlygPmt};

// ── serde model: the information block ───────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct Inf {
    #[serde(rename = "RmtInf")]
    pub rmt_inf: Option<RmtInf>,
}

// ── flattened row ────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct AddtlInfRow {
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
    pub remittance_info: Option<String>,
    pub source_file: Option<String>,
}

pub fn row_from_case(
    assign: &AssignCtx,
    case: &CaseCtx,
    undrlyg: Option<&UndrlygPmt>,
    inf: Option<&Inf>,
    source: &str,
) -> Result<AddtlInfRow, String> {
    let (original_amount, original_currency) = undrlyg
        .map(UndrlygPmt::amount)
        .transpose()
        .map_err(|e| format!("{source}: {e}"))?
        .unwrap_or((None, None));

    Ok(AddtlInfRow {
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
        remittance_info: inf.and_then(|i| i.rmt_inf.as_ref()).and_then(RmtInf::text),
        source_file: Some(source.to_string()),
    })
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct AddtlInfStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    assign: AssignCtx,
    case: CaseCtx,
    undrlyg: Option<UndrlygPmt>,
    inf: Option<Inf>,
    saw_info: bool,
    /// `path.len()` at the innermost open container of this family.
    in_info: Option<usize>,
}

impl<R: BufRead> AddtlInfStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        AddtlInfStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            assign: AssignCtx::default(),
            case: CaseCtx::default(),
            undrlyg: None,
            inf: None,
            saw_info: false,
            in_info: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<AddtlInfRow>, Box<dyn Error>> {
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
                        "Undrlyg" if self.in_info.is_some() => Act::Undrlyg,
                        "Inf" if self.in_info.is_some() => Act::Inf,
                        _ => Act::Push(name.into_owned()),
                    }
                }
                Event::End(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if self.in_info.is_some()
                        && (name == "AddtlPmtInf" || name.starts_with("camt.028."))
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
                    return if self.saw_info {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no <AddtlPmtInf> found — is this a camt.028 additional \
                             payment information?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Undrlyg => {
                    let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "Undrlyg")?;
                    self.undrlyg = Some(quick_xml::de::from_str(&xml)?);
                }
                Act::Inf => {
                    let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "Inf")?;
                    self.inf = Some(quick_xml::de::from_str(&xml)?);
                }
                Act::Push(name) => {
                    if name == "AddtlPmtInf" || name.starts_with("camt.028.") {
                        self.saw_info = true;
                        self.in_info = Some(self.path.len());
                        self.assign = AssignCtx::default();
                        self.case = CaseCtx::default();
                        self.undrlyg = None;
                        self.inf = None;
                    }
                    self.path.push(name);
                }
                Act::Close => {
                    self.pop();
                    return Ok(Some(row_from_case(
                        &self.assign,
                        &self.case,
                        self.undrlyg.as_ref(),
                        self.inf.as_ref(),
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
        if self.in_info == Some(self.path.len()) {
            self.in_info = None;
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
    Inf,
    Push(String),
    Pop,
    Close,
    Text(String),
    None,
}
