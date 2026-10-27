//! `read_camt_statements` - one row per camt.052 `Rpt`, camt.053 `Stmt` or
//! camt.054 `Ntfctn`: what the statement says about itself.
//!
//! The other camt readers are at the grain of something inside a statement, so
//! the statement's own header had nowhere to go. MT940 has `statement_number`
//! and `sequence_number` and camt had no equivalent, which left no way in SQL
//! to ask whether the statement for Tuesday arrived, or arrived twice.
//!
//! Grain: one row per statement, emitted when its container closes - the entry
//! and balance counts are only known then. The entries and balances themselves
//! are consumed and counted, not returned, so the walk is as cheap as
//! `read_camt_balances`. ADR 0012.
//!
//! Header text stays text until a row is built: a malformed
//! `<ElctrncSeqNb>` fails this reader and only this one, because no other
//! reader returns it.

use std::error::Error;
use std::io::BufRead;

use crate::camt::{StatementClose, StatementContext, StatementRecordStream};
use crate::decimal;

#[derive(Debug, Default, Clone)]
pub struct StatementRow {
    pub msg_id: Option<String>,
    pub message_created: Option<String>,
    pub message_page: Option<i64>,
    /// As the wire spelled it, like `reversal_indicator`: `true`, `false`, `1`.
    pub message_last_page: Option<String>,
    pub statement_kind: Option<String>,
    pub statement_index: Option<i64>,
    pub statement_id: Option<String>,
    pub electronic_sequence: Option<i64>,
    pub legal_sequence: Option<i64>,
    pub statement_created: Option<String>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub copy_duplicate: Option<String>,
    pub page_number: Option<i64>,
    pub last_page: Option<String>,
    pub account_iban: Option<String>,
    pub account_currency: Option<String>,
    pub account_owner_name: Option<String>,
    pub account_servicer_bic: Option<String>,
    pub summary_entries: Option<i64>,
    /// Exact amounts scaled by `10^decimal::SCALE`; never a float.
    pub summary_sum: Option<i128>,
    pub summary_net_amount: Option<i128>,
    pub summary_net_credit_debit: Option<String>,
    pub summary_credit_entries: Option<i64>,
    pub summary_credit_sum: Option<i128>,
    pub summary_debit_entries: Option<i64>,
    pub summary_debit_sum: Option<i128>,
    pub entry_count: Option<i64>,
    pub balance_count: Option<i64>,
    pub source_file: Option<String>,
}

/// A count the wire states, or the text that was not one. A count that does not
/// parse is no count, and reading it as absent would let a short statement
/// look like one that never claimed a total.
fn whole(text: &Option<String>, elem: &str, source: &str) -> Result<Option<i64>, String> {
    text.as_deref()
        .map(|t| {
            t.trim()
                .parse::<i64>()
                .map_err(|_| format!("{source}: <{elem}> {t:?} is not a whole number"))
        })
        .transpose()
}

/// A sum the wire states, through the parser every amount goes through.
fn sum(text: &Option<String>, elem: &str, source: &str) -> Result<Option<i128>, String> {
    text.as_deref()
        .map(|t| decimal::scaled(t).map_err(|e| format!("{source}: <{elem}> {e}")))
        .transpose()
}

pub fn row_from_statement(ctx: &StatementContext, source: &str) -> Result<StatementRow, String> {
    Ok(StatementRow {
        msg_id: ctx.msg_id.clone(),
        message_created: ctx.message_created.clone(),
        message_page: whole(&ctx.message_page, "PgNb", source)?,
        message_last_page: ctx.message_last_page.clone(),
        statement_kind: ctx.statement_kind.clone(),
        statement_index: Some(ctx.statement_index),
        statement_id: ctx.statement_id.clone(),
        electronic_sequence: whole(&ctx.electronic_sequence, "ElctrncSeqNb", source)?,
        legal_sequence: whole(&ctx.legal_sequence, "LglSeqNb", source)?,
        statement_created: ctx.statement_created.clone(),
        from_date: ctx.from_date.clone(),
        to_date: ctx.to_date.clone(),
        copy_duplicate: ctx.copy_duplicate.clone(),
        page_number: whole(&ctx.page_number, "PgNb", source)?,
        last_page: ctx.last_page.clone(),
        account_iban: ctx.account_iban.clone(),
        account_currency: ctx.account_currency.clone(),
        account_owner_name: ctx.account_owner_name.clone(),
        account_servicer_bic: ctx.account_servicer_bic.clone(),
        summary_entries: whole(&ctx.summary_entries, "NbOfNtries", source)?,
        summary_sum: sum(&ctx.summary_sum, "Sum", source)?,
        summary_net_amount: sum(&ctx.summary_net_amount, "Amt", source)?,
        summary_net_credit_debit: ctx.summary_net_credit_debit.clone(),
        summary_credit_entries: whole(&ctx.summary_credit_entries, "NbOfNtries", source)?,
        summary_credit_sum: sum(&ctx.summary_credit_sum, "Sum", source)?,
        summary_debit_entries: whole(&ctx.summary_debit_entries, "NbOfNtries", source)?,
        summary_debit_sum: sum(&ctx.summary_debit_sum, "Sum", source)?,
        entry_count: Some(ctx.entry_count),
        balance_count: Some(ctx.balance_count),
        source_file: Some(source.to_string()),
    })
}

pub struct StatementsStream<R: BufRead> {
    records: StatementRecordStream<R, StatementClose>,
}

impl<R: BufRead> StatementsStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        StatementsStream {
            records: StatementRecordStream::new(reader, source),
        }
    }

    pub fn next_row(&mut self) -> Result<Option<StatementRow>, Box<dyn Error>> {
        let Some(_) = self.records.next_record()? else {
            return Ok(None);
        };
        Ok(Some(row_from_statement(
            self.records.context(),
            self.records.source(),
        )?))
    }
}
