//! camt.087 — Request to Modify Payment. Not "cancel it" but "send it
//! differently": a new amount, new remittance information, a different
//! reimbursement agent.
//!
//! Shape:
//!
//! ```text
//! ReqToModfyPmt
//!   Assgnmt        — who asks whom
//!   Case           — the case this belongs to
//!   Undrlyg        — the payment as sent
//!   Mod            — what it should become
//! ```
//!
//! The original and the modification sit side by side, so the difference is a
//! subtraction in SQL rather than a second query. `Mod` states its amount as
//! `IntrBkSttlmAmt` on the interbank side and as `Amt/InstdAmt` on the pain
//! side; both feed `modified_amount`.
//!
//! Grain: one row per message.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::wire::{self, AmtBlock, AssignCtx, CaseCtx, Money, RmtInf, UndrlygPmt};

// ── serde model: the modification block ──────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct Mod {
    #[serde(rename = "IntrBkSttlmAmt")]
    pub sttlm_amt: Option<Money>,
    #[serde(rename = "Amt")]
    pub amt: Option<AmtBlock>,
    #[serde(rename = "RmtInf")]
    pub rmt_inf: Option<RmtInf>,
}

impl Mod {
    pub fn amount(&self) -> Result<(Option<i128>, Option<String>), String> {
        wire::money(&[
            self.sttlm_amt.as_ref(),
            self.amt.as_ref().and_then(|a| a.instd.as_ref()),
        ])
    }
}

// ── flattened row ────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct ModfyRow {
    pub assignment_id: Option<String>,
    pub assignment_created: Option<String>,
    pub assigner: Option<String>,
    pub assignee: Option<String>,
    pub case_id: Option<String>,
    pub case_creator: Option<String>,
    pub original_msg_id: Option<String>,
    pub original_msg_name_id: Option<String>,
    pub original_instr_id: Option<String>,
    pub original_end_to_end_id: Option<String>,
    pub original_amount: Option<i128>,
    pub original_currency: Option<String>,
    pub original_execution_date: Option<String>,
    pub original_settlement_date: Option<String>,
    /// What the sender asks the payment to become.
    pub modified_amount: Option<i128>,
    pub modified_currency: Option<String>,
    pub modified_remittance_info: Option<String>,
    pub source_file: Option<String>,
}

pub fn row_from_case(
    assign: &AssignCtx,
    case: &CaseCtx,
    undrlyg: Option<&UndrlygPmt>,
    modification: Option<&Mod>,
    source: &str,
) -> Result<ModfyRow, String> {
    let at = |e: String| format!("{source}: {e}");
    let (original_amount, original_currency) = undrlyg
        .map(UndrlygPmt::amount)
        .transpose()
        .map_err(at)?
        .unwrap_or((None, None));
    let (modified_amount, modified_currency) = modification
        .map(Mod::amount)
        .transpose()
        .map_err(at)?
        .unwrap_or((None, None));

    Ok(ModfyRow {
        assignment_id: assign.id.clone(),
        assignment_created: assign.created.clone(),
        assigner: assign.assigner.clone(),
        assignee: assign.assignee.clone(),
        case_id: case.id.clone(),
        case_creator: case.creator.clone(),
        original_msg_id: undrlyg.and_then(UndrlygPmt::msg_id),
        original_msg_name_id: undrlyg.and_then(UndrlygPmt::msg_name_id),
        original_instr_id: undrlyg.and_then(UndrlygPmt::instr_id),
        original_end_to_end_id: undrlyg.and_then(UndrlygPmt::end_to_end_id),
        original_amount,
        original_currency,
        original_execution_date: undrlyg.and_then(UndrlygPmt::execution_date),
        original_settlement_date: undrlyg.and_then(UndrlygPmt::settlement_date),
        modified_amount,
        modified_currency,
        modified_remittance_info: modification
            .and_then(|m| m.rmt_inf.as_ref())
            .and_then(RmtInf::text),
        source_file: Some(source.to_string()),
    })
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct ModfyStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    assign: AssignCtx,
    case: CaseCtx,
    undrlyg: Option<UndrlygPmt>,
    modification: Option<Mod>,
    saw_request: bool,
    /// `path.len()` at the innermost open container of this family.
    in_request: Option<usize>,
}

impl<R: BufRead> ModfyStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        ModfyStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            assign: AssignCtx::default(),
            case: CaseCtx::default(),
            undrlyg: None,
            modification: None,
            saw_request: false,
            in_request: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<ModfyRow>, Box<dyn Error>> {
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
                        "Mod" if self.in_request.is_some() => Act::Mod,
                        _ => Act::Push(name.into_owned()),
                    }
                }
                Event::End(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if self.in_request.is_some()
                        && (name == "ReqToModfyPmt" || name.starts_with("camt.087."))
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
                            "{}: no <ReqToModfyPmt> found — is this a camt.087 request to \
                             modify a payment?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Undrlyg => {
                    let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "Undrlyg")?;
                    self.undrlyg = Some(quick_xml::de::from_str(&xml)?);
                }
                Act::Mod => {
                    let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "Mod")?;
                    self.modification = Some(quick_xml::de::from_str(&xml)?);
                }
                Act::Push(name) => {
                    if name == "ReqToModfyPmt" || name.starts_with("camt.087.") {
                        self.saw_request = true;
                        self.in_request = Some(self.path.len());
                        self.assign = AssignCtx::default();
                        self.case = CaseCtx::default();
                        self.undrlyg = None;
                        self.modification = None;
                    }
                    self.path.push(name);
                }
                Act::Close => {
                    self.pop();
                    return Ok(Some(row_from_case(
                        &self.assign,
                        &self.case,
                        self.undrlyg.as_ref(),
                        self.modification.as_ref(),
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
    Mod,
    Push(String),
    Pop,
    Close,
    Text(String),
    None,
}
