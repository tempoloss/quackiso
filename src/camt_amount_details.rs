//! `read_camt_amount_details` - every amount block inside an entry or transaction
//! `<AmtDtls>`, one row each.
//!
//! An entry can state the same money several times: instructed, settled,
//! counter-value, announced posting, plus the exchange rate and contract that
//! converted it. Before this grain, a cross-currency entry exposed one amount
//! in one currency and left the bank-applied rate unreachable.
//!
//! Grain: one row per amount block inside entry-level or transaction-level
//! `<AmtDtls>`. A present empty block still produces a row with nullable amount
//! facts, so callers can count the block and join back to the owning entry.
//!
//! Join keys: `source_file`, `statement_index`, `entry_index`, and for
//! transaction-level rows, `entry_details_index` and `transaction_index`.

use std::error::Error;
use std::io::BufRead;

use crate::camt::{StatementContext, StatementRecordStream};
use crate::model::{money, AmountCursor, AmountSite, Ntry, AMOUNT_PROPRIETARY};

#[derive(Debug, Default, Clone)]
pub struct AmountDetailRow {
    pub msg_id: Option<String>,
    pub statement_kind: Option<String>,
    pub statement_index: Option<i64>,
    pub statement_id: Option<String>,
    pub account_iban: Option<String>,
    pub entry_index: Option<i64>,
    pub entry_ref: Option<String>,
    pub entry_details_index: Option<i64>,
    pub transaction_index: Option<i64>,
    pub scope: Option<String>,
    pub amount_kind: Option<String>,
    pub amount_index: Option<i64>,
    pub proprietary_type: Option<String>,
    /// Exact amount scaled by `10^decimal::SCALE`; never a float.
    pub amount: Option<i128>,
    pub currency: Option<String>,
    pub exchange_source_currency: Option<String>,
    pub exchange_target_currency: Option<String>,
    pub exchange_unit_currency: Option<String>,
    /// A rate is not money; keep the XML lexical value.
    pub exchange_rate: Option<String>,
    pub exchange_contract_id: Option<String>,
    /// Raw `QtnDt`; the table writer parses it as a timestamp.
    pub exchange_quotation_time: Option<String>,
    pub source_file: Option<String>,
}

pub fn row_from_amount(
    ntry: &Ntry,
    ctx: &StatementContext,
    entry_index: i64,
    site: AmountSite<'_>,
    source: &str,
) -> Result<AmountDetailRow, String> {
    let detail = site.detail;
    let (amount, currency) = money(detail.amt.as_ref(), source)?;
    let exchange = detail.ccy_xchg.as_ref();

    Ok(AmountDetailRow {
        msg_id: ctx.msg_id.clone(),
        statement_kind: ctx.statement_kind.clone(),
        statement_index: Some(ctx.statement_index),
        statement_id: ctx.statement_id.clone(),
        account_iban: ctx.account_iban.clone(),
        entry_index: Some(entry_index),
        entry_ref: ntry.ntry_ref.clone(),
        entry_details_index: site.entry_details_index,
        transaction_index: site.transaction_index,
        scope: Some(site.scope.to_string()),
        amount_kind: Some(site.kind.to_string()),
        amount_index: Some(site.index),
        proprietary_type: match site.kind {
            AMOUNT_PROPRIETARY => detail.tp.clone(),
            _ => None,
        },
        amount,
        currency,
        exchange_source_currency: exchange.and_then(|x| x.src_ccy.clone()),
        exchange_target_currency: exchange.and_then(|x| x.trgt_ccy.clone()),
        exchange_unit_currency: exchange.and_then(|x| x.unit_ccy.clone()),
        exchange_rate: exchange.and_then(|x| x.xchg_rate.clone()),
        exchange_contract_id: exchange.and_then(|x| x.ctrct_id.clone()),
        exchange_quotation_time: exchange.and_then(|x| x.qtn_dt.clone()),
        source_file: Some(source.to_string()),
    })
}

/// The entry a cursor is walking. The stream owns one deserialized `<Ntry>` and
/// advances integers inside `AmountCursor`, with no row queue beside it.
struct OpenEntry {
    entry: Ntry,
    index: i64,
    ctx: StatementContext,
    cursor: AmountCursor,
}

pub struct AmountDetailStream<R: BufRead> {
    records: StatementRecordStream<R, Ntry>,
    open: Option<OpenEntry>,
}

impl<R: BufRead> AmountDetailStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        AmountDetailStream {
            records: StatementRecordStream::new(reader, source),
            open: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<AmountDetailRow>, Box<dyn Error>> {
        loop {
            if let Some(open) = self.open.as_mut() {
                if let Some(site) = open.cursor.next(&open.entry) {
                    return Ok(Some(row_from_amount(
                        &open.entry,
                        &open.ctx,
                        open.index,
                        site,
                        self.records.source(),
                    )?));
                }
                self.open = None;
            }
            let Some((index, entry)) = self.records.next_record()? else {
                return Ok(None);
            };
            self.open = Some(OpenEntry {
                entry,
                index,
                ctx: self.records.context().clone(),
                cursor: AmountCursor::default(),
            });
        }
    }
}
