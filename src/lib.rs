//! quackiso — query ISO 20022 financial messages as SQL in DuckDB.
//!
//! Three streaming table functions:
//!
//! * `read_iso20022(path)` — cash management: camt.053 statements, camt.054
//!   notifications, camt.052 reports. One row per booked entry.
//! * `read_pacs008(path)` — FI-to-FI customer credit transfers (the ISO 20022
//!   replacement for SWIFT MT103). One row per transaction.
//! * `read_pain001(path)` — customer credit transfer initiation. One row per
//!   transaction, with the payer carried down from its `PmtInf` group.
//!
//! `bind` only resolves the file list; parsing happens in `func`, which pulls the
//! next vector-sized batch on demand, so memory stays O(batch) regardless of file
//! size. Paths are local, and globs are expanded.
//!
//! Reading through DuckDB's own filesystem (`s3://`, `https://`) is deliberately
//! absent rather than half-working; `docs/adr/0002-no-remote-paths.md` records the
//! blocker and what it would take.

mod decimal;
mod model;
mod pacs008;
mod pain001;
mod stream;
mod temporal;

use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId},
    duckdb_entrypoint_c_api,
    vtab::{BindInfo, InitInfo, TableFunctionInfo, VTab},
    Connection, Result,
};
use parking_lot::Mutex;
use std::{error::Error, fs::File, io::BufReader};

use model::Row;
use pacs008::{PacsRow, TxStream};
use pain001::{PainRow, PainStream};
use stream::EntryStream;

/// DuckDB's standard vector size. Rows are emitted in chunks of this many.
const VECTOR_SIZE: usize = 2048;

/// Byte source for a scan. Buffered because the readers pull small XML events.
type Source = BufReader<File>;

fn open_source(path: &str) -> Result<Source, Box<dyn Error>> {
    let file = File::open(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    Ok(BufReader::with_capacity(64 * 1024, file))
}

// ── shared scan machinery ────────────────────────────────────────────────────

/// A streaming reader over one file, yielding flattened rows.
trait RowStream: Sized {
    type Row;
    fn open(source: Source, name: &str) -> Self;
    fn next_row(&mut self) -> Result<Option<Self::Row>, Box<dyn Error>>;
}

impl RowStream for EntryStream<Source> {
    type Row = Row;
    fn open(source: Source, name: &str) -> Self {
        EntryStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<Row>, Box<dyn Error>> {
        EntryStream::next_row(self)
    }
}

impl RowStream for TxStream<Source> {
    type Row = PacsRow;
    fn open(source: Source, name: &str) -> Self {
        TxStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<PacsRow>, Box<dyn Error>> {
        TxStream::next_row(self)
    }
}

impl RowStream for PainStream<Source> {
    type Row = PainRow;
    fn open(source: Source, name: &str) -> Self {
        PainStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<PainRow>, Box<dyn Error>> {
        PainStream::next_row(self)
    }
}

/// Where a scan is: which file, and its open reader.
struct ScanState<S> {
    idx: usize,
    cur: Option<S>,
}

impl<S> ScanState<S> {
    fn new() -> Self {
        ScanState { idx: 0, cur: None }
    }
}

/// Pull up to one vector of rows, advancing across files as each drains.
fn pull_batch<S: RowStream>(
    files: &[String],
    st: &mut ScanState<S>,
    fname: &str,
) -> Result<Vec<S::Row>, Box<dyn Error>> {
    let mut batch = Vec::with_capacity(VECTOR_SIZE);
    while batch.len() < VECTOR_SIZE {
        if st.cur.is_none() {
            if st.idx >= files.len() {
                break;
            }
            let path = files[st.idx].clone();
            let source = open_source(&path).map_err(|e| format!("{fname}: {e}"))?;
            st.cur = Some(S::open(source, &path));
        }
        match st.cur.as_mut().unwrap().next_row()? {
            Some(row) => batch.push(row),
            None => {
                st.cur = None;
                st.idx += 1;
            }
        }
    }
    Ok(batch)
}

/// Expand a path or glob into a file list.
fn resolve_files(pattern: &str, fname: &str) -> Result<Vec<String>, Box<dyn Error>> {
    if let Some(scheme) = remote_scheme(pattern) {
        return Err(format!(
            "{fname}: {scheme}:// paths are not supported; read a local file \
             (see docs/adr/0002-no-remote-paths.md)"
        )
        .into());
    }
    let mut files: Vec<String> = glob::glob(pattern)
        .map_err(|e| format!("bad path pattern {pattern:?}: {e}"))?
        .filter_map(|p| p.ok())
        .map(|p| p.display().to_string())
        .collect();
    if files.is_empty() && std::path::Path::new(pattern).is_file() {
        files.push(pattern.to_string());
    }
    if files.is_empty() {
        return Err(format!("{fname}: no files matched {pattern:?}").into());
    }
    Ok(files)
}

/// The URI scheme of a path, when it has one. A Windows drive letter (`C:/…`) is
/// not a URI, hence the length check.
fn remote_scheme(path: &str) -> Option<&str> {
    let i = path.find("://")?;
    let scheme = &path[..i];
    (i > 1 && scheme.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+')).then_some(scheme)
}

// ── column declaration and writers ───────────────────────────────────────────

/// Column kinds this extension emits. Amounts are `DECIMAL`, never `DOUBLE`:
/// see `decimal`. Dates keep their wire precision: `TIMESTAMP` where the corpus
/// mixes date-only and date-time values, `DATE` where the schema says date.
#[derive(Clone, Copy)]
enum Col {
    Text,
    Date,
    Stamp,
    Money,
}

impl Col {
    fn handle(self) -> LogicalTypeHandle {
        match self {
            Col::Text => LogicalTypeHandle::from(LogicalTypeId::Varchar),
            Col::Date => LogicalTypeHandle::from(LogicalTypeId::Date),
            Col::Stamp => LogicalTypeHandle::from(LogicalTypeId::Timestamp),
            Col::Money => LogicalTypeHandle::decimal(decimal::WIDTH, decimal::SCALE),
        }
    }
}

fn declare(bind: &BindInfo, columns: &[(&str, Col)]) {
    for (name, col) in columns {
        bind.add_result_column(name, col.handle());
    }
}

fn write_text<T>(
    output: &mut DataChunkHandle,
    idx: usize,
    batch: &[T],
    get: impl Fn(&T) -> &Option<String>,
) {
    let mut v = output.flat_vector(idx);
    for (i, row) in batch.iter().enumerate() {
        match get(row) {
            Some(s) => v.insert(i, s.as_str()),
            None => v.set_null(i),
        }
    }
}

/// Write a fixed-width numeric column. Values go through the raw slice in an
/// inner scope so the borrow ends before the vector is touched again for NULLs.
macro_rules! write_numeric {
    ($name:ident, $ty:ty) => {
        fn $name<T>(
            output: &mut DataChunkHandle,
            idx: usize,
            batch: &[T],
            get: impl Fn(&T) -> Option<$ty>,
        ) {
            let mut v = output.flat_vector(idx);
            {
                let slice = unsafe { v.as_mut_slice::<$ty>() };
                for (i, row) in batch.iter().enumerate() {
                    if let Some(x) = get(row) {
                        slice[i] = x;
                    }
                }
            }
            for (i, row) in batch.iter().enumerate() {
                if get(row).is_none() {
                    v.set_null(i);
                }
            }
        }
    };
}

write_numeric!(write_date, i32);
write_numeric!(write_timestamp, i64);
// DECIMAL(38,5) is physically INT128.
write_numeric!(write_decimal, i128);

/// Files resolved at bind time. Shared by all three functions.
#[repr(C)]
struct FileList {
    files: Vec<String>,
}

/// Generates the boilerplate every table function repeats: bind resolves the
/// file list and declares columns, init opens a scan, `parameters` takes one
/// path. Only the column writing differs, so only that is spelled out.
macro_rules! table_function {
    (
        $vtab:ident, $init:ident, $stream:ty, $row:ty,
        name = $sql_name:literal,
        columns = $columns:expr,
        write = |$output:ident, $batch:ident| $write:block
    ) => {
        #[repr(C)]
        struct $init {
            state: Mutex<ScanState<$stream>>,
        }

        struct $vtab;

        impl VTab for $vtab {
            type InitData = $init;
            type BindData = FileList;

            fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
                declare(bind, $columns);
                Ok(FileList {
                    files: resolve_files(&bind.get_parameter(0).to_string(), $sql_name)?,
                })
            }

            fn init(_: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
                Ok($init {
                    state: Mutex::new(ScanState::new()),
                })
            }

            fn func(
                func: &TableFunctionInfo<Self>,
                $output: &mut DataChunkHandle,
            ) -> Result<(), Box<dyn Error>> {
                let files = &func.get_bind_data().files;
                let mut st = func.get_init_data().state.lock();
                let $batch: Vec<$row> = pull_batch(files, &mut st, $sql_name)?;
                // The lock only guards the scan cursor, not the writing below.
                drop(st);
                $write
                $output.set_len($batch.len());
                Ok(())
            }

            fn parameters() -> Option<Vec<LogicalTypeHandle>> {
                Some(vec![LogicalTypeHandle::from(LogicalTypeId::Varchar)])
            }
        }
    };
}

// ── read_iso20022: camt.053 / camt.054 / camt.052 ────────────────────────────

const CAMT_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("account_iban", Col::Text),
    ("statement_id", Col::Text),
    ("entry_ref", Col::Text),
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("credit_debit", Col::Text),
    ("status", Col::Text),
    ("booking_date", Col::Stamp),
    ("value_date", Col::Stamp),
    ("bank_ref", Col::Text),
    ("end_to_end_id", Col::Text),
    ("counterparty_name", Col::Text),
    ("counterparty_iban", Col::Text),
    ("remittance_info", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadIso20022, CamtInit, EntryStream<Source>, Row,
    name = "read_iso20022",
    columns = CAMT_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 4, &batch, |r: &Row| r.amount);
        write_timestamp(output, 8, &batch, |r: &Row| {
            r.booking_date.as_deref().and_then(temporal::ts_micros)
        });
        write_timestamp(output, 9, &batch, |r: &Row| {
            r.value_date.as_deref().and_then(temporal::ts_micros)
        });
        write_text(output, 0, &batch, |r: &Row| &r.msg_id);
        write_text(output, 1, &batch, |r: &Row| &r.account_iban);
        write_text(output, 2, &batch, |r: &Row| &r.statement_id);
        write_text(output, 3, &batch, |r: &Row| &r.entry_ref);
        write_text(output, 5, &batch, |r: &Row| &r.currency);
        write_text(output, 6, &batch, |r: &Row| &r.credit_debit);
        write_text(output, 7, &batch, |r: &Row| &r.status);
        write_text(output, 10, &batch, |r: &Row| &r.bank_ref);
        write_text(output, 11, &batch, |r: &Row| &r.end_to_end_id);
        write_text(output, 12, &batch, |r: &Row| &r.counterparty_name);
        write_text(output, 13, &batch, |r: &Row| &r.counterparty_iban);
        write_text(output, 14, &batch, |r: &Row| &r.remittance_info);
        write_text(output, 15, &batch, |r: &Row| &r.source_file);
    }
}

// ── read_pacs008 ─────────────────────────────────────────────────────────────

const PACS_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("instr_id", Col::Text),
    ("end_to_end_id", Col::Text),
    ("tx_id", Col::Text),
    ("uetr", Col::Text),
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("settlement_date", Col::Date),
    ("charge_bearer", Col::Text),
    ("debtor_name", Col::Text),
    ("debtor_account", Col::Text),
    ("debtor_agent_bic", Col::Text),
    ("creditor_name", Col::Text),
    ("creditor_account", Col::Text),
    ("creditor_agent_bic", Col::Text),
    ("remittance_info", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPacs008, PacsInit, TxStream<Source>, PacsRow,
    name = "read_pacs008",
    columns = PACS_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 5, &batch, |r: &PacsRow| r.amount);
        write_date(output, 7, &batch, |r: &PacsRow| {
            r.settlement_date.as_deref().and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &PacsRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &PacsRow| &r.instr_id);
        write_text(output, 2, &batch, |r: &PacsRow| &r.end_to_end_id);
        write_text(output, 3, &batch, |r: &PacsRow| &r.tx_id);
        write_text(output, 4, &batch, |r: &PacsRow| &r.uetr);
        write_text(output, 6, &batch, |r: &PacsRow| &r.currency);
        write_text(output, 8, &batch, |r: &PacsRow| &r.charge_bearer);
        write_text(output, 9, &batch, |r: &PacsRow| &r.debtor_name);
        write_text(output, 10, &batch, |r: &PacsRow| &r.debtor_account);
        write_text(output, 11, &batch, |r: &PacsRow| &r.debtor_agent_bic);
        write_text(output, 12, &batch, |r: &PacsRow| &r.creditor_name);
        write_text(output, 13, &batch, |r: &PacsRow| &r.creditor_account);
        write_text(output, 14, &batch, |r: &PacsRow| &r.creditor_agent_bic);
        write_text(output, 15, &batch, |r: &PacsRow| &r.remittance_info);
        write_text(output, 16, &batch, |r: &PacsRow| &r.source_file);
    }
}

// ── read_pain001 ─────────────────────────────────────────────────────────────

const PAIN_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("initiating_party", Col::Text),
    ("payment_info_id", Col::Text),
    ("payment_method", Col::Text),
    ("requested_execution_date", Col::Date),
    ("debtor_name", Col::Text),
    ("debtor_account", Col::Text),
    ("debtor_agent_bic", Col::Text),
    ("instr_id", Col::Text),
    ("end_to_end_id", Col::Text),
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("charge_bearer", Col::Text),
    ("creditor_name", Col::Text),
    ("creditor_account", Col::Text),
    ("creditor_agent_bic", Col::Text),
    ("remittance_info", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPain001, PainInit, PainStream<Source>, PainRow,
    name = "read_pain001",
    columns = PAIN_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 10, &batch, |r: &PainRow| r.amount);
        write_date(output, 4, &batch, |r: &PainRow| {
            r.requested_execution_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &PainRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &PainRow| &r.initiating_party);
        write_text(output, 2, &batch, |r: &PainRow| &r.payment_info_id);
        write_text(output, 3, &batch, |r: &PainRow| &r.payment_method);
        write_text(output, 5, &batch, |r: &PainRow| &r.debtor_name);
        write_text(output, 6, &batch, |r: &PainRow| &r.debtor_account);
        write_text(output, 7, &batch, |r: &PainRow| &r.debtor_agent_bic);
        write_text(output, 8, &batch, |r: &PainRow| &r.instr_id);
        write_text(output, 9, &batch, |r: &PainRow| &r.end_to_end_id);
        write_text(output, 11, &batch, |r: &PainRow| &r.currency);
        write_text(output, 12, &batch, |r: &PainRow| &r.charge_bearer);
        write_text(output, 13, &batch, |r: &PainRow| &r.creditor_name);
        write_text(output, 14, &batch, |r: &PainRow| &r.creditor_account);
        write_text(output, 15, &batch, |r: &PainRow| &r.creditor_agent_bic);
        write_text(output, 16, &batch, |r: &PainRow| &r.remittance_info);
        write_text(output, 17, &batch, |r: &PainRow| &r.source_file);
    }
}

#[duckdb_entrypoint_c_api]
pub unsafe fn extension_entrypoint(con: Connection) -> Result<(), Box<dyn Error>> {
    con.register_table_function::<ReadIso20022>("read_iso20022")?;
    con.register_table_function::<ReadPacs008>("read_pacs008")?;
    con.register_table_function::<ReadPain001>("read_pain001")?;
    Ok(())
}
