//! `read_camt_remittance` - every supported remittance text leaf under a
//! camt.052/.053/.054 transaction, one row each.
//!
//! `read_iso20022` used to have one `remittance_info` column filled from the
//! first remittance slot that had text, with repeated values joined by spaces.
//! A transaction stating two invoice numbers in two `<Ustrd>` leaves came back
//! as one string nobody could split back into invoices, and one carrying free
//! text beside a structured creditor reference reported only the free text.
//! This is the lossless replacement, and it is why the entry column is now NULL
//! whenever there is more than one leaf.
//!
//! Grain: one row per non-empty supported text leaf under `TxDtls/RmtInf`.
//! `RfrdDocInf`, `TaxRmt` and other structured remittance objects are outside
//! this slice.

use std::error::Error;
use std::io::BufRead;

use crate::camt::{StatementContext, StatementRecordStream};
use crate::model::{Ntry, RemittanceCursor, RemittanceSite};

#[derive(Debug, Default, Clone)]
pub struct RemittanceRow {
    pub msg_id: Option<String>,
    pub statement_kind: Option<String>,
    pub statement_index: Option<i64>,
    pub statement_id: Option<String>,
    pub account_iban: Option<String>,
    pub entry_index: Option<i64>,
    pub entry_ref: Option<String>,
    pub entry_details_index: Option<i64>,
    pub transaction_index: Option<i64>,
    pub remittance_index: Option<i64>,
    /// The owning `<Strd>` ordinal for structured leaves; NULL for `Ustrd`.
    pub structured_index: Option<i64>,
    pub slot: Option<String>,
    pub text: Option<String>,
    pub source_file: Option<String>,
}

/// A remittance row reads text leaves and nothing else, so unlike the other
/// four camt row constructors it has no amount to refuse and no `Result`.
pub fn row_from_remittance(
    ntry: &Ntry,
    ctx: &StatementContext,
    entry_index: i64,
    site: RemittanceSite<'_>,
    source: &str,
) -> RemittanceRow {
    RemittanceRow {
        msg_id: ctx.msg_id.clone(),
        statement_kind: ctx.statement_kind.clone(),
        statement_index: Some(ctx.statement_index),
        statement_id: ctx.statement_id.clone(),
        account_iban: ctx.account_iban.clone(),
        entry_index: Some(entry_index),
        entry_ref: ntry.ntry_ref.clone(),
        entry_details_index: Some(site.entry_details_index),
        transaction_index: Some(site.transaction_index),
        remittance_index: Some(site.index),
        structured_index: site.leaf.structured_index,
        slot: Some(site.leaf.slot.to_string()),
        text: Some(site.leaf.text.to_string()),
        source_file: Some(source.to_string()),
    }
}

/// The entry a remittance cursor is walking. The stream owns one deserialized
/// `<Ntry>` at a time plus the cursor that walks its transaction leaves.
struct OpenEntry {
    entry: Ntry,
    index: i64,
    ctx: StatementContext,
    cursor: RemittanceCursor,
}

pub struct RemittanceStream<R: BufRead> {
    records: StatementRecordStream<R, Ntry>,
    open: Option<OpenEntry>,
}

impl<R: BufRead> RemittanceStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        RemittanceStream {
            records: StatementRecordStream::new(reader, source),
            open: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<RemittanceRow>, Box<dyn Error>> {
        loop {
            if let Some(open) = self.open.as_mut() {
                if let Some(site) = open.cursor.next(&open.entry) {
                    return Ok(Some(row_from_remittance(
                        &open.entry,
                        &open.ctx,
                        open.index,
                        site,
                        self.records.source(),
                    )));
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
                cursor: RemittanceCursor::default(),
            });
        }
    }
}
