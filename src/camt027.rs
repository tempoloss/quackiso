//! camt.027 — Claim of Non-Receipt. The beneficiary's side saying the money
//! never arrived, which is where a payment investigation usually starts.
//!
//! Shape:
//!
//! ```text
//! ClmNonRct
//!   Assgnmt          — who asks whom, and when
//!   Case             — the investigation this belongs to, and who opened it
//!   Undrlyg          — the payment being chased, stated as the initiation
//!                      (Initn) or as the interbank leg (IntrBk)
//! ```
//!
//! Note the container: `ClmNonRct`, not `ClmNonRcpt` — the abbreviation drops
//! the p.
//!
//! A claim moves no money, so every monetary column is `original_*`: the
//! payment that is missing, not a payment this message makes. None of the three
//! blocks repeats, so the row is the message; it is emitted when the container
//! closes, because `Undrlyg` follows `Case` in document order.
//!
//! Grain: one row per message.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::wire::{self, AssignCtx, CaseCtx, UndrlygPmt};

// ── flattened row ────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct ClaimRow {
    pub assignment_id: Option<String>,
    pub assignment_created: Option<String>,
    pub assigner: Option<String>,
    pub assignee: Option<String>,
    pub case_id: Option<String>,
    pub case_creator: Option<String>,
    pub original_msg_id: Option<String>,
    pub original_msg_name_id: Option<String>,
    pub original_instr_id: Option<String>,
    /// The amount that did not arrive; scaled, never a float.
    pub original_amount: Option<i128>,
    pub original_currency: Option<String>,
    /// When it was to leave, on the initiation side.
    pub original_execution_date: Option<String>,
    /// When it settled, on the interbank side.
    pub original_settlement_date: Option<String>,
    pub source_file: Option<String>,
}

pub fn row_from_case(
    assign: &AssignCtx,
    case: &CaseCtx,
    undrlyg: Option<&UndrlygPmt>,
    source: &str,
) -> Result<ClaimRow, String> {
    let (original_amount, original_currency) = undrlyg
        .map(UndrlygPmt::amount)
        .transpose()
        .map_err(|e| format!("{source}: {e}"))?
        .unwrap_or((None, None));

    Ok(ClaimRow {
        assignment_id: assign.id.clone(),
        assignment_created: assign.created.clone(),
        assigner: assign.assigner.clone(),
        assignee: assign.assignee.clone(),
        case_id: case.id.clone(),
        case_creator: case.creator.clone(),
        original_msg_id: undrlyg.and_then(UndrlygPmt::msg_id),
        original_msg_name_id: undrlyg.and_then(UndrlygPmt::msg_name_id),
        original_instr_id: undrlyg.and_then(UndrlygPmt::instr_id),
        original_amount,
        original_currency,
        original_execution_date: undrlyg.and_then(UndrlygPmt::execution_date),
        original_settlement_date: undrlyg.and_then(UndrlygPmt::settlement_date),
        source_file: Some(source.to_string()),
    })
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct ClaimStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    assign: AssignCtx,
    case: CaseCtx,
    undrlyg: Option<UndrlygPmt>,
    saw_claim: bool,
    /// `path.len()` at the innermost open container of this family.
    in_claim: Option<usize>,
}

impl<R: BufRead> ClaimStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        ClaimStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            assign: AssignCtx::default(),
            case: CaseCtx::default(),
            undrlyg: None,
            saw_claim: false,
            in_claim: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<ClaimRow>, Box<dyn Error>> {
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
                    if name == "Undrlyg" && self.in_claim.is_some() {
                        Act::Undrlyg
                    } else {
                        Act::Push(name.into_owned())
                    }
                }
                Event::End(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if self.in_claim.is_some()
                        && (name == "ClmNonRct" || name.starts_with("camt.027."))
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
                    return if self.saw_claim {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no <ClmNonRct> found — is this a camt.027 claim of \
                             non-receipt?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Undrlyg => {
                    let xml = wire::record_subtree(
                        &mut self.reader,
                        &mut self.buf,
                        "Undrlyg",
                        &self.source,
                    )?;
                    self.undrlyg = Some(quick_xml::de::from_str(&xml)?);
                }
                Act::Push(name) => {
                    if name == "ClmNonRct" || name.starts_with("camt.027.") {
                        self.saw_claim = true;
                        self.in_claim = Some(self.path.len());
                        self.assign = AssignCtx::default();
                        self.case = CaseCtx::default();
                        self.undrlyg = None;
                    }
                    self.path.push(name);
                }
                Act::Close => {
                    self.pop();
                    return Ok(Some(row_from_case(
                        &self.assign,
                        &self.case,
                        self.undrlyg.as_ref(),
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
        if self.in_claim == Some(self.path.len()) {
            self.in_claim = None;
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
    Push(String),
    Pop,
    Close,
    Text(String),
    None,
}
