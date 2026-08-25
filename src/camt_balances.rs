//! `read_camt_balances` - every direct `<Bal>` of a camt.052 report or camt.053
//! statement, one row each.
//!
//! `testdata/camt053_empty_statement.xml` carries two balances and no entries.
//! `read_iso20022` truthfully returned zero rows for it, but no function exposed
//! the balances, so the account's closing position was unreachable from SQL
//! while the file stated it plainly.
//!
//! Grain: one row per direct statement balance. Schema-valid camt.054
//! notifications carry no `<Bal>`; the tolerant statement walk still reports one
//! if a national notification puts it at the same direct level.

use std::error::Error;
use std::io::BufRead;

use crate::camt::{StatementContext, StatementRecordStream};
use crate::model::{money, Balance};

#[derive(Debug, Default, Clone)]
pub struct BalanceRow {
    pub msg_id: Option<String>,
    pub statement_kind: Option<String>,
    pub statement_index: Option<i64>,
    pub statement_id: Option<String>,
    pub account_iban: Option<String>,
    pub account_currency: Option<String>,
    pub balance_index: Option<i64>,
    pub balance_type: Option<String>,
    pub balance_type_scheme: Option<String>,
    pub balance_subtype: Option<String>,
    pub balance_subtype_scheme: Option<String>,
    /// Exact amount scaled by `10^decimal::SCALE`; never a float.
    pub amount: Option<i128>,
    pub currency: Option<String>,
    pub credit_debit: Option<String>,
    pub balance_date: Option<String>,
    pub source_file: Option<String>,
}

pub fn row_from_balance(
    balance: &Balance,
    ctx: &StatementContext,
    balance_index: i64,
    source: &str,
) -> Result<BalanceRow, String> {
    let (balance_type, balance_type_scheme) = match balance.kind() {
        Some((value, scheme)) => (Some(value), Some(scheme.to_string())),
        None => (None, None),
    };
    let (balance_subtype, balance_subtype_scheme) = match balance.subkind() {
        Some((value, scheme)) => (Some(value), Some(scheme.to_string())),
        None => (None, None),
    };
    let (amount, currency) = money(balance.amt.as_ref(), source)?;

    Ok(BalanceRow {
        msg_id: ctx.msg_id.clone(),
        statement_kind: ctx.statement_kind.clone(),
        statement_index: Some(ctx.statement_index),
        statement_id: ctx.statement_id.clone(),
        account_iban: ctx.account_iban.clone(),
        account_currency: ctx.account_currency.clone(),
        balance_index: Some(balance_index),
        balance_type,
        balance_type_scheme,
        balance_subtype,
        balance_subtype_scheme,
        amount,
        currency,
        credit_debit: balance.cdt_dbt_ind.clone(),
        balance_date: balance.dt.as_ref().and_then(|d| d.value()),
        source_file: Some(source.to_string()),
    })
}

pub struct BalanceStream<R: BufRead> {
    records: StatementRecordStream<R, Balance>,
}

impl<R: BufRead> BalanceStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        BalanceStream {
            records: StatementRecordStream::new(reader, source),
        }
    }

    pub fn next_row(&mut self) -> Result<Option<BalanceRow>, Box<dyn Error>> {
        let Some((index, balance)) = self.records.next_record()? else {
            return Ok(None);
        };
        Ok(Some(row_from_balance(
            &balance,
            self.records.context(),
            index,
            self.records.source(),
        )?))
    }
}
