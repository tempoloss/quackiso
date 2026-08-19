//! MT942 - Interim Transaction Report. An intraday account report with floor
//! limits, report time, entry totals and the entries known at report time.
//!
//! Grain: one row per `:61:` statement line.

use std::error::Error;
use std::io::BufRead;
use std::ops::Range;

use crate::mt::{self, Field, Fields, StatementLine};

/// Turns a field into the `path:line` prefix its errors carry.
type At<'a> = dyn Fn(&Field<'_>) -> String + 'a;

#[derive(Debug, Default, Clone)]
pub struct Mt942Row {
    pub direction: Option<String>,
    pub message_type: Option<String>,
    pub sender_bic: Option<String>,
    pub receiver_bic: Option<String>,
    pub uetr: Option<String>,
    pub validation_flag: Option<String>,
    pub mur: Option<String>,
    pub tx_ref: Option<String>,
    pub related_ref: Option<String>,
    pub account: Option<String>,
    pub account_bic: Option<String>,
    pub statement_number: Option<i64>,
    pub sequence_number: Option<i64>,
    pub floor_limit_debit: Option<i128>,
    pub floor_limit_debit_currency: Option<String>,
    pub floor_limit_credit: Option<i128>,
    pub floor_limit_credit_currency: Option<String>,
    pub report_datetime: Option<i64>,
    pub report_utc_offset: Option<String>,
    pub entry_index: Option<i64>,
    pub value_date: Option<i32>,
    pub entry_date: Option<i32>,
    pub credit_debit: Option<String>,
    pub funds_code: Option<String>,
    pub amount: Option<i128>,
    pub transaction_type: Option<String>,
    pub transaction_code: Option<String>,
    pub customer_ref: Option<String>,
    pub bank_ref: Option<String>,
    pub supplementary_details: Option<String>,
    pub narrative: Option<String>,
    pub statement_narrative: Option<String>,
    pub debit_entry_count: Option<i64>,
    pub debit_entry_currency: Option<String>,
    pub debit_entry_sum: Option<i128>,
    pub credit_entry_count: Option<i64>,
    pub credit_entry_currency: Option<String>,
    pub credit_entry_sum: Option<i128>,
    pub source_file: Option<String>,
}

#[derive(Debug, Default, Clone)]
struct Mt942Ctx {
    direction: Option<String>,
    message_type: Option<String>,
    sender_bic: Option<String>,
    receiver_bic: Option<String>,
    uetr: Option<String>,
    validation_flag: Option<String>,
    mur: Option<String>,
    tx_ref: Option<String>,
    related_ref: Option<String>,
    account: Option<String>,
    account_bic: Option<String>,
    statement_number: Option<i64>,
    sequence_number: Option<i64>,
    floor_limit_debit: Option<i128>,
    floor_limit_debit_currency: Option<String>,
    floor_limit_credit: Option<i128>,
    floor_limit_credit_currency: Option<String>,
    report_datetime: Option<i64>,
    report_utc_offset: Option<String>,
    statement_narrative: Option<String>,
    debit_entry_count: Option<i64>,
    debit_entry_currency: Option<String>,
    debit_entry_sum: Option<i128>,
    credit_entry_count: Option<i64>,
    credit_entry_currency: Option<String>,
    credit_entry_sum: Option<i128>,
}

/// The report being emitted, and how far its entry walk has got. Same
/// arrangement as `mt940::Statement`, for the same reason: the interim report
/// states its totals after the entries.
struct Report {
    msg: String,
    body: Range<usize>,
    body_line: usize,
    message_line: usize,
    ctx: Mt942Ctx,
    cursor: mt::EntryCursor,
    emitted: i64,
}

pub struct Mt942Stream<R: BufRead> {
    reader: mt::MtReader<R>,
    source: String,
    saw_my_type: bool,
    open: Option<Report>,
}

impl<R: BufRead> Mt942Stream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        Mt942Stream {
            reader: mt::MtReader::new(reader, source),
            source: source.to_string(),
            saw_my_type: false,
            open: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<Mt942Row>, Box<dyn Error>> {
        loop {
            if let Some(open) = &mut self.open {
                match open.cursor.next_site(&open.msg[open.body.clone()], "61") {
                    Some(site) => {
                        open.emitted += 1;
                        let index = open.emitted;
                        return Ok(Some(row_from_entry(open, index, &site, &self.source)?));
                    }
                    None => {
                        self.open = None;
                        continue;
                    }
                }
            }

            let Some(msg) = self.reader.next_message()? else {
                return if self.saw_my_type {
                    Ok(None)
                } else {
                    Err(format!(
                        "{}: no MT942 message found - is this a SWIFT MT942 file?",
                        self.source
                    )
                    .into())
                };
            };

            let message_line = self.reader.line();
            let (body, body_line) = mt::block_span(&msg, 4).unwrap_or((0..msg.len(), 1));
            let (fields, entries) = Fields::without_entries(&msg[body.clone()], "61");
            if !mt::claims(&msg, &fields, "942") {
                continue;
            }

            self.saw_my_type = true;
            let at = |field: &Field<'_>| {
                format!(
                    "{}:{}",
                    self.source,
                    mt::at(message_line, body_line, field.line)
                )
            };
            let ctx = ctx_from_message(&msg, &fields, &at)?;
            if entries == 0 {
                continue;
            }
            self.open = Some(Report {
                msg,
                body,
                body_line,
                message_line,
                ctx,
                cursor: mt::EntryCursor::default(),
                emitted: 0,
            });
        }
    }
}

fn ctx_from_message(
    msg: &str,
    fields: &Fields<'_>,
    at: &At<'_>,
) -> Result<Mt942Ctx, Box<dyn Error>> {
    let (account, account_bic) = fields
        .find("25")
        .map(|field| account(field.tag, &field.value))
        .unwrap_or((None, None));
    let (statement_number, sequence_number) = fields
        .value("28")
        .map(mt::statement_number)
        .unwrap_or((None, None));
    let (
        floor_limit_debit,
        floor_limit_debit_currency,
        floor_limit_credit,
        floor_limit_credit_currency,
    ) = floor_limits(fields, at)?;
    let (report_datetime, report_utc_offset) = report_time(fields, at)?;
    let (debit_entry_count, debit_entry_currency, debit_entry_sum) = total("90D", fields, at)?;
    let (credit_entry_count, credit_entry_currency, credit_entry_sum) = total("90C", fields, at)?;

    Ok(Mt942Ctx {
        direction: mt::direction(msg).map(str::to_string),
        message_type: mt::message_type(msg).map(str::to_string),
        sender_bic: mt::sender_bic(msg),
        receiver_bic: mt::receiver_bic(msg),
        uetr: mt::user_header_field(msg, "121").map(str::to_string),
        validation_flag: mt::user_header_field(msg, "119").map(str::to_string),
        mur: mt::user_header_field(msg, "108").map(str::to_string),
        tx_ref: fields.value("20").map(str::to_string),
        related_ref: fields.value("21").map(str::to_string),
        account,
        account_bic,
        statement_number,
        sequence_number,
        floor_limit_debit,
        floor_limit_debit_currency,
        floor_limit_credit,
        floor_limit_credit_currency,
        report_datetime,
        report_utc_offset,
        statement_narrative: statement_narrative(fields),
        debit_entry_count,
        debit_entry_currency,
        debit_entry_sum,
        credit_entry_count,
        credit_entry_currency,
        credit_entry_sum,
    })
}

/// One row from one entry region.
fn row_from_entry(
    open: &Report,
    index: i64,
    site: &mt::EntrySite,
    source: &str,
) -> Result<Mt942Row, Box<dyn Error>> {
    let body = &open.msg[open.body.clone()];
    let fields = Fields::parse(&body[site.bytes.clone()]);
    let where_ = format!(
        "{source}:{}",
        mt::at(open.message_line, open.body_line, site.line)
    );
    let Some(field) = fields.find("61") else {
        return Err(format!("{where_}: an entry region with no statement line").into());
    };
    let line = mt::statement_line(&field.value).map_err(|e| format!("{where_}: :61: {e}"))?;
    Ok(row(
        &open.ctx,
        &line,
        index,
        join_values(fields.all("86")),
        source,
    ))
}

fn row(
    ctx: &Mt942Ctx,
    line: &StatementLine,
    entry_index: i64,
    narrative: Option<String>,
    source: &str,
) -> Mt942Row {
    Mt942Row {
        direction: ctx.direction.clone(),
        message_type: ctx.message_type.clone(),
        sender_bic: ctx.sender_bic.clone(),
        receiver_bic: ctx.receiver_bic.clone(),
        uetr: ctx.uetr.clone(),
        validation_flag: ctx.validation_flag.clone(),
        mur: ctx.mur.clone(),
        tx_ref: ctx.tx_ref.clone(),
        related_ref: ctx.related_ref.clone(),
        account: ctx.account.clone(),
        account_bic: ctx.account_bic.clone(),
        statement_number: ctx.statement_number,
        sequence_number: ctx.sequence_number,
        floor_limit_debit: ctx.floor_limit_debit,
        floor_limit_debit_currency: ctx.floor_limit_debit_currency.clone(),
        floor_limit_credit: ctx.floor_limit_credit,
        floor_limit_credit_currency: ctx.floor_limit_credit_currency.clone(),
        report_datetime: ctx.report_datetime,
        report_utc_offset: ctx.report_utc_offset.clone(),
        entry_index: Some(entry_index),
        value_date: line.value_date,
        entry_date: line.entry_date,
        credit_debit: Some(line.credit_debit.clone()),
        funds_code: line.funds_code.clone(),
        amount: Some(line.amount),
        transaction_type: line.transaction_type.clone(),
        transaction_code: line.transaction_code.clone(),
        customer_ref: line.customer_ref.clone(),
        bank_ref: line.bank_ref.clone(),
        supplementary_details: line.supplementary.clone(),
        narrative,
        statement_narrative: ctx.statement_narrative.clone(),
        debit_entry_count: ctx.debit_entry_count,
        debit_entry_currency: ctx.debit_entry_currency.clone(),
        debit_entry_sum: ctx.debit_entry_sum,
        credit_entry_count: ctx.credit_entry_count,
        credit_entry_currency: ctx.credit_entry_currency.clone(),
        credit_entry_sum: ctx.credit_entry_sum,
        source_file: Some(source.to_string()),
    }
}

fn account(tag: &str, value: &str) -> (Option<String>, Option<String>) {
    let lines: Vec<&str> = value
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if tag == "25P" {
        (
            lines.first().and_then(|value| some_text(value)),
            lines.get(1).and_then(|value| some_text(value)),
        )
    } else {
        (some_text(value), None)
    }
}

fn floor_limits(fields: &Fields<'_>, at: &At<'_>) -> Result<mt::FloorLimits, Box<dyn Error>> {
    let mut limits = fields.iter().filter(|field| field.tag == "34F");
    let Some(first) = limits.next() else {
        return Ok((None, None, None, None));
    };
    let (first_currency, first_mark, first_amount) =
        floor_limit(&first.value).map_err(|e| format!("{}: {e}", at(first)))?;

    if let Some(second) = limits.next() {
        let (second_currency, _, second_amount) =
            floor_limit(&second.value).map_err(|e| format!("{}: {e}", at(second)))?;
        return Ok((
            Some(first_amount),
            Some(first_currency),
            Some(second_amount),
            Some(second_currency),
        ));
    }

    match first_mark.as_deref() {
        Some("D") => Ok((Some(first_amount), Some(first_currency), None, None)),
        Some("C") => Ok((None, None, Some(first_amount), Some(first_currency))),
        _ => Ok((
            Some(first_amount),
            Some(first_currency.clone()),
            Some(first_amount),
            Some(first_currency),
        )),
    }
}

fn floor_limit(value: &str) -> Result<(String, Option<String>, i128), String> {
    let s = value.trim();
    if !s.is_ascii() || s.len() < 4 {
        return Err(format!(":34F: not a floor limit: {value:?}"));
    }
    let currency = &s[..3];
    if !currency.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Err(format!(":34F: not a currency: {currency:?}"));
    }
    let rest = &s[3..];
    let (mark, amount_text) = match rest.as_bytes().first() {
        Some(b'D') | Some(b'C') => (Some(rest[..1].to_string()), &rest[1..]),
        _ => (None, rest),
    };
    if amount_text.is_empty() {
        return Err(format!(":34F: not an amount: {value:?}"));
    }
    let amount = mt::amount(amount_text).map_err(|e| format!(":34F: {e}"))?;
    Ok((currency.to_string(), mark, amount))
}

fn report_time(
    fields: &Fields<'_>,
    at: &At<'_>,
) -> Result<(Option<i64>, Option<String>), Box<dyn Error>> {
    match fields.find("13D") {
        Some(field) => {
            let (micros, offset) =
                mt::datetime13d(&field.value).map_err(|e| format!("{}: :13D: {e}", at(field)))?;
            Ok((micros, Some(offset)))
        }
        None => Ok((None, None)),
    }
}

fn total(
    tag: &str,
    fields: &Fields<'_>,
    at: &At<'_>,
) -> Result<mt::CountCcyAmount, Box<dyn Error>> {
    match fields.find(tag) {
        Some(field) => {
            let (count, currency, amount) = mt::count_ccy_amount(&field.value)
                .map_err(|e| format!("{}: :{tag}: {e}", at(field)))?;
            Ok((count, Some(currency), Some(amount)))
        }
        None => Ok((None, None, None)),
    }
}

fn statement_narrative(fields: &Fields<'_>) -> Option<String> {
    let last_total = fields
        .iter()
        .enumerate()
        .filter(|(_, field)| matches!(field.tag, "90D" | "90C"))
        .map(|(index, _)| index)
        .max()?;
    join_values(
        fields
            .iter()
            .skip(last_total + 1)
            .filter(|field| field.tag == "86")
            .map(|field| field.value.as_str()),
    )
}

fn join_values<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut values = values
        .into_iter()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let first = values.next()?;
    let mut out = first.to_string();
    for value in values {
        out.push('\n');
        out.push_str(value);
    }
    Some(out)
}

fn some_text(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}
