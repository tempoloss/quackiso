//! quackiso — query ISO 20022 financial messages as SQL in DuckDB.
//!
//! Nine streaming table functions:
//!
//! * `read_iso20022(path)` — cash management: camt.053 statements, camt.054
//!   notifications, camt.052 reports. One row per booked entry.
//! * `read_pacs008(path)` — FI-to-FI customer credit transfers (the ISO 20022
//!   replacement for SWIFT MT103). One row per transaction.
//! * `read_pacs004(path)` — payment returns: settled money coming back. One row
//!   per returned transaction, with the original amount beside the returned one.
//! * `read_pacs002(path)` — FI-to-FI payment status reports. One row per status
//!   statement, at batch or transaction level.
//! * `read_pain001(path)` — customer credit transfer initiation. One row per
//!   transaction, with the payer carried down from its `PmtInf` group.
//! * `read_pain002(path)` — customer payment status reports. One row per status
//!   statement, at whichever of the three levels the bank stated it.
//! * `read_pain008(path)` — direct debit initiation: the creditor pulls. One
//!   row per collection, with the collector carried down from its `PmtInf`
//!   group and the mandate beside the money.
//! * `read_camt056(path)` — payment cancellation requests. One row per
//!   cancellation statement; a whole-batch cancellation is a row too.
//! * `read_camt029(path)` — resolutions of investigation: the answer to a
//!   cancellation. One row per statement; most real files answer at message
//!   level only.
//!
//! `bind` only resolves the file list; parsing happens in `func`, which pulls the
//! next vector-sized batch on demand, so memory stays O(batch) regardless of file
//! size. Paths are local, and globs are expanded.
//!
//! Reading through DuckDB's own filesystem (`s3://`, `https://`) is deliberately
//! absent rather than half-working; `docs/adr/0002-no-remote-paths.md` records the
//! blocker and what it would take.

mod camt029;
mod camt056;
mod decimal;
mod model;
mod pacs002;
mod pacs004;
mod pacs008;
mod pain001;
mod pain002;
mod pain008;
mod stream;
mod temporal;
mod wire;

use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId},
    duckdb_entrypoint_c_api,
    vtab::{BindInfo, InitInfo, TableFunctionInfo, VTab},
    Connection, Result,
};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::{error::Error, fs::File, io::BufReader};

use camt029::{RoiRow, RoiStream};
use camt056::{CxlRow, CxlStream};
use model::Row;
use pacs002::{RptRow, RptStream};
use pacs004::{RtrRow, RtrStream};
use pacs008::{PacsRow, TxStream};
use pain001::{PainRow, PainStream};
use pain002::{StsRow, StsStream};
use pain008::{DdRow, DdStream};
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

impl RowStream for RtrStream<Source> {
    type Row = RtrRow;
    fn open(source: Source, name: &str) -> Self {
        RtrStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<RtrRow>, Box<dyn Error>> {
        RtrStream::next_row(self)
    }
}

impl RowStream for StsStream<Source> {
    type Row = StsRow;
    fn open(source: Source, name: &str) -> Self {
        StsStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<StsRow>, Box<dyn Error>> {
        StsStream::next_row(self)
    }
}

impl RowStream for RptStream<Source> {
    type Row = RptRow;
    fn open(source: Source, name: &str) -> Self {
        RptStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<RptRow>, Box<dyn Error>> {
        RptStream::next_row(self)
    }
}

impl RowStream for CxlStream<Source> {
    type Row = CxlRow;
    fn open(source: Source, name: &str) -> Self {
        CxlStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<CxlRow>, Box<dyn Error>> {
        CxlStream::next_row(self)
    }
}

impl RowStream for DdStream<Source> {
    type Row = DdRow;
    fn open(source: Source, name: &str) -> Self {
        DdStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<DdRow>, Box<dyn Error>> {
        DdStream::next_row(self)
    }
}

impl RowStream for RoiStream<Source> {
    type Row = RoiRow;
    fn open(source: Source, name: &str) -> Self {
        RoiStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<RoiRow>, Box<dyn Error>> {
        RoiStream::next_row(self)
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

// ── parallel scan ────────────────────────────────────────────────────────────

/// Where a multi-file scan is. Chosen on the first `func` call, because only
/// then are both the file count and the `threads` argument in hand.
enum Scan<S: RowStream> {
    Pending,
    Sequential(ScanState<S>),
    Parallel(mpsc::Receiver<std::result::Result<Vec<S::Row>, String>>),
}

/// How many worker threads a scan gets. An explicit `threads := n` wins
/// (anything below 1 means sequential); the default is one thread per file,
/// capped at the machine's parallelism. One file is always sequential — XML
/// has no safe split points, so a single document cannot be divided.
fn effective_threads(requested: Option<i64>, nfiles: usize) -> usize {
    let auto = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    match requested {
        Some(n) if n >= 1 => (n as usize).min(nfiles),
        Some(_) => 1,
        None => auto.min(nfiles),
    }
}

/// File-level parallelism. The unit of work is the whole file: workers claim
/// the next unparsed file from a shared counter, parse it into vector-sized
/// batches, and hand the batches over a bounded channel.
///
/// Bounded, so memory stays O(threads × batch) no matter how many files the
/// glob matched — the same discipline as the sequential scan, multiplied by
/// the worker count and the channel capacity. Rows of one file stay in file
/// order; files interleave nondeterministically, which is what `source_file`
/// is for. A `LIMIT` that stops the scan drops the receiver, every following
/// `send` fails, and the workers exit instead of parsing the rest of the glob.
///
/// Errors cross the channel as strings and abort the scan at the batch where
/// they surfaced, exactly as in the sequential path: a malformed amount in any
/// file still fails the whole query rather than dropping out of a `SUM`.
fn spawn_workers<S>(
    files: Vec<String>,
    threads: usize,
    fname: &'static str,
) -> mpsc::Receiver<std::result::Result<Vec<S::Row>, String>>
where
    S: RowStream + 'static,
    S::Row: Send + 'static,
{
    let (tx, rx) = mpsc::sync_channel(threads * 2);
    let files = Arc::new(files);
    let next = Arc::new(AtomicUsize::new(0));
    for _ in 0..threads {
        let tx = tx.clone();
        let files = Arc::clone(&files);
        let next = Arc::clone(&next);
        std::thread::spawn(move || loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(path) = files.get(i) else { return };
            let source = match open_source(path) {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.send(Err(format!("{fname}: {e}")));
                    return;
                }
            };
            let mut stream = S::open(source, path);
            let mut batch = Vec::with_capacity(VECTOR_SIZE);
            loop {
                match stream.next_row() {
                    Ok(Some(row)) => {
                        batch.push(row);
                        if batch.len() == VECTOR_SIZE {
                            if tx.send(Ok(std::mem::take(&mut batch))).is_err() {
                                return; // scan stopped early (LIMIT, error)
                            }
                            batch.reserve(VECTOR_SIZE);
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                }
            }
            if !batch.is_empty() && tx.send(Ok(batch)).is_err() {
                return;
            }
        });
    }
    // Drop the template sender: the channel disconnects when the last worker
    // finishes, which is how the scan knows it is done.
    drop(tx);
    rx
}

/// The next batch of rows, deciding sequential-vs-parallel on first call.
fn next_batch<S>(
    files: &[String],
    threads: Option<i64>,
    scan: &mut Scan<S>,
    fname: &'static str,
) -> std::result::Result<Vec<S::Row>, Box<dyn Error>>
where
    S: RowStream + 'static,
    S::Row: Send + 'static,
{
    if matches!(scan, Scan::Pending) {
        let t = effective_threads(threads, files.len());
        *scan = if t <= 1 {
            Scan::Sequential(ScanState::new())
        } else {
            Scan::Parallel(spawn_workers::<S>(files.to_vec(), t, fname))
        };
    }
    match scan {
        Scan::Sequential(st) => pull_batch(files, st, fname),
        Scan::Parallel(rx) => match rx.recv() {
            Ok(Ok(batch)) => Ok(batch),
            Ok(Err(e)) => Err(e.into()),
            Err(_) => Ok(Vec::new()), // every worker done
        },
        Scan::Pending => unreachable!(),
    }
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
    (i > 1
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+'))
    .then_some(scheme)
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

/// Files resolved at bind time, plus the requested worker count. Shared by all
/// nine functions.
#[repr(C)]
struct FileList {
    files: Vec<String>,
    threads: Option<i64>,
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
            state: Mutex<Scan<$stream>>,
        }

        struct $vtab;

        impl VTab for $vtab {
            type InitData = $init;
            type BindData = FileList;

            fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
                declare(bind, $columns);
                Ok(FileList {
                    files: resolve_files(&bind.get_parameter(0).to_string(), $sql_name)?,
                    threads: bind.get_named_parameter("threads").map(|v| v.to_int64()),
                })
            }

            fn init(_: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
                Ok($init {
                    state: Mutex::new(Scan::Pending),
                })
            }

            fn func(
                func: &TableFunctionInfo<Self>,
                $output: &mut DataChunkHandle,
            ) -> Result<(), Box<dyn Error>> {
                let bind_data = func.get_bind_data();
                let mut st = func.get_init_data().state.lock();
                let $batch: Vec<$row> =
                    next_batch(&bind_data.files, bind_data.threads, &mut st, $sql_name)?;
                // The lock only guards the scan cursor, not the writing below.
                drop(st);
                $write
                $output.set_len($batch.len());
                Ok(())
            }

            fn parameters() -> Option<Vec<LogicalTypeHandle>> {
                Some(vec![LogicalTypeHandle::from(LogicalTypeId::Varchar)])
            }

            fn named_parameters() -> Option<Vec<(String, LogicalTypeHandle)>> {
                Some(vec![(
                    "threads".to_string(),
                    LogicalTypeHandle::from(LogicalTypeId::Bigint),
                )])
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
    // The tracking reference that follows one payment across message families:
    // the same UETR appears on the pacs.008 that settles it and on the pacs.004
    // that returns it, which is what makes those readers joinable.
    ("uetr", Col::Text),
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
        write_decimal(output, 11, &batch, |r: &PainRow| r.amount);
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
        write_text(output, 10, &batch, |r: &PainRow| &r.uetr);
        write_text(output, 12, &batch, |r: &PainRow| &r.currency);
        write_text(output, 13, &batch, |r: &PainRow| &r.charge_bearer);
        write_text(output, 14, &batch, |r: &PainRow| &r.creditor_name);
        write_text(output, 15, &batch, |r: &PainRow| &r.creditor_account);
        write_text(output, 16, &batch, |r: &PainRow| &r.creditor_agent_bic);
        write_text(output, 17, &batch, |r: &PainRow| &r.remittance_info);
        write_text(output, 18, &batch, |r: &PainRow| &r.source_file);
    }
}

// ── read_pacs004 ─────────────────────────────────────────────────────────────

const RTR_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("return_id", Col::Text),
    ("original_msg_id", Col::Text),
    ("original_msg_name_id", Col::Text),
    ("original_instr_id", Col::Text),
    ("original_end_to_end_id", Col::Text),
    ("original_tx_id", Col::Text),
    ("original_uetr", Col::Text),
    // What came back, and what the payment had settled for. Equal on a full
    // return; `amount < original_amount` is a return with charges deducted.
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("original_amount", Col::Money),
    ("original_currency", Col::Text),
    ("settlement_date", Col::Date),
    ("original_settlement_date", Col::Date),
    ("charge_bearer", Col::Text),
    ("return_reason_code", Col::Text),
    ("return_reason_info", Col::Text),
    ("return_originator", Col::Text),
    ("original_debtor_name", Col::Text),
    ("original_debtor_account", Col::Text),
    ("original_debtor_agent_bic", Col::Text),
    ("original_creditor_name", Col::Text),
    ("original_creditor_account", Col::Text),
    ("original_creditor_agent_bic", Col::Text),
    ("remittance_info", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPacs004, RtrInit, RtrStream<Source>, RtrRow,
    name = "read_pacs004",
    columns = RTR_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 8, &batch, |r: &RtrRow| r.amount);
        write_decimal(output, 10, &batch, |r: &RtrRow| r.original_amount);
        write_date(output, 12, &batch, |r: &RtrRow| {
            r.settlement_date.as_deref().and_then(temporal::date_days)
        });
        write_date(output, 13, &batch, |r: &RtrRow| {
            r.original_settlement_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &RtrRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &RtrRow| &r.return_id);
        write_text(output, 2, &batch, |r: &RtrRow| &r.original_msg_id);
        write_text(output, 3, &batch, |r: &RtrRow| &r.original_msg_name_id);
        write_text(output, 4, &batch, |r: &RtrRow| &r.original_instr_id);
        write_text(output, 5, &batch, |r: &RtrRow| &r.original_end_to_end_id);
        write_text(output, 6, &batch, |r: &RtrRow| &r.original_tx_id);
        write_text(output, 7, &batch, |r: &RtrRow| &r.original_uetr);
        write_text(output, 9, &batch, |r: &RtrRow| &r.currency);
        write_text(output, 11, &batch, |r: &RtrRow| &r.original_currency);
        write_text(output, 14, &batch, |r: &RtrRow| &r.charge_bearer);
        write_text(output, 15, &batch, |r: &RtrRow| &r.return_reason_code);
        write_text(output, 16, &batch, |r: &RtrRow| &r.return_reason_info);
        write_text(output, 17, &batch, |r: &RtrRow| &r.return_originator);
        write_text(output, 18, &batch, |r: &RtrRow| &r.original_debtor_name);
        write_text(output, 19, &batch, |r: &RtrRow| &r.original_debtor_account);
        write_text(output, 20, &batch, |r: &RtrRow| &r.original_debtor_agent_bic);
        write_text(output, 21, &batch, |r: &RtrRow| &r.original_creditor_name);
        write_text(output, 22, &batch, |r: &RtrRow| &r.original_creditor_account);
        write_text(output, 23, &batch, |r: &RtrRow| &r.original_creditor_agent_bic);
        write_text(output, 24, &batch, |r: &RtrRow| &r.remittance_info);
        write_text(output, 25, &batch, |r: &RtrRow| &r.source_file);
    }
}

// ── read_pain002 ─────────────────────────────────────────────────────────────

const STS_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("initiating_party", Col::Text),
    ("original_msg_id", Col::Text),
    ("original_msg_name_id", Col::Text),
    // Which level of the report this row states: GROUP, PAYMENT_INFO or
    // TRANSACTION. Only TRANSACTION rows carry an amount.
    ("status_level", Col::Text),
    ("original_payment_info_id", Col::Text),
    ("status_id", Col::Text),
    ("status", Col::Text),
    ("reason_code", Col::Text),
    ("reason_info", Col::Text),
    ("reason_originator", Col::Text),
    // A count, not an amount: kept as the wire spelled it.
    ("original_number_of_txs", Col::Text),
    ("original_control_sum", Col::Money),
    ("original_instr_id", Col::Text),
    ("original_end_to_end_id", Col::Text),
    ("original_uetr", Col::Text),
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("requested_execution_date", Col::Date),
    ("debtor_name", Col::Text),
    ("debtor_account", Col::Text),
    ("creditor_name", Col::Text),
    ("creditor_account", Col::Text),
    ("remittance_info", Col::Text),
    ("acceptance_date_time", Col::Stamp),
    ("source_file", Col::Text),
];

table_function! {
    ReadPain002, StsInit, StsStream<Source>, StsRow,
    name = "read_pain002",
    columns = STS_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 12, &batch, |r: &StsRow| r.original_control_sum);
        write_decimal(output, 16, &batch, |r: &StsRow| r.amount);
        write_date(output, 18, &batch, |r: &StsRow| {
            r.requested_execution_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_timestamp(output, 24, &batch, |r: &StsRow| {
            r.acceptance_date_time.as_deref().and_then(temporal::ts_micros)
        });
        write_text(output, 0, &batch, |r: &StsRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &StsRow| &r.initiating_party);
        write_text(output, 2, &batch, |r: &StsRow| &r.original_msg_id);
        write_text(output, 3, &batch, |r: &StsRow| &r.original_msg_name_id);
        write_text(output, 4, &batch, |r: &StsRow| &r.status_level);
        write_text(output, 5, &batch, |r: &StsRow| &r.original_payment_info_id);
        write_text(output, 6, &batch, |r: &StsRow| &r.status_id);
        write_text(output, 7, &batch, |r: &StsRow| &r.status);
        write_text(output, 8, &batch, |r: &StsRow| &r.reason_code);
        write_text(output, 9, &batch, |r: &StsRow| &r.reason_info);
        write_text(output, 10, &batch, |r: &StsRow| &r.reason_originator);
        write_text(output, 11, &batch, |r: &StsRow| &r.original_number_of_txs);
        write_text(output, 13, &batch, |r: &StsRow| &r.original_instr_id);
        write_text(output, 14, &batch, |r: &StsRow| &r.original_end_to_end_id);
        write_text(output, 15, &batch, |r: &StsRow| &r.original_uetr);
        write_text(output, 17, &batch, |r: &StsRow| &r.currency);
        write_text(output, 19, &batch, |r: &StsRow| &r.debtor_name);
        write_text(output, 20, &batch, |r: &StsRow| &r.debtor_account);
        write_text(output, 21, &batch, |r: &StsRow| &r.creditor_name);
        write_text(output, 22, &batch, |r: &StsRow| &r.creditor_account);
        write_text(output, 23, &batch, |r: &StsRow| &r.remittance_info);
        write_text(output, 25, &batch, |r: &StsRow| &r.source_file);
    }
}

// ── read_pacs002 ─────────────────────────────────────────────────────────────

const RPT_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    // Who is reporting to whom; per-transaction agents override the group pair.
    ("instructing_agent_bic", Col::Text),
    ("instructed_agent_bic", Col::Text),
    // GROUP or TRANSACTION; the group block is optional in pacs.002, so a file
    // may contain only transaction rows.
    ("status_level", Col::Text),
    ("status_id", Col::Text),
    ("status", Col::Text),
    ("reason_code", Col::Text),
    ("reason_info", Col::Text),
    ("reason_originator", Col::Text),
    ("original_msg_id", Col::Text),
    ("original_msg_name_id", Col::Text),
    ("original_instr_id", Col::Text),
    ("original_end_to_end_id", Col::Text),
    ("original_tx_id", Col::Text),
    ("original_uetr", Col::Text),
    ("acceptance_date_time", Col::Stamp),
    ("original_amount", Col::Money),
    ("original_currency", Col::Text),
    ("original_settlement_date", Col::Date),
    ("original_debtor_name", Col::Text),
    ("original_creditor_name", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPacs002, RptInit, RptStream<Source>, RptRow,
    name = "read_pacs002",
    columns = RPT_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 16, &batch, |r: &RptRow| r.original_amount);
        write_timestamp(output, 15, &batch, |r: &RptRow| {
            r.acceptance_date_time.as_deref().and_then(temporal::ts_micros)
        });
        write_date(output, 18, &batch, |r: &RptRow| {
            r.original_settlement_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &RptRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &RptRow| &r.instructing_agent_bic);
        write_text(output, 2, &batch, |r: &RptRow| &r.instructed_agent_bic);
        write_text(output, 3, &batch, |r: &RptRow| &r.status_level);
        write_text(output, 4, &batch, |r: &RptRow| &r.status_id);
        write_text(output, 5, &batch, |r: &RptRow| &r.status);
        write_text(output, 6, &batch, |r: &RptRow| &r.reason_code);
        write_text(output, 7, &batch, |r: &RptRow| &r.reason_info);
        write_text(output, 8, &batch, |r: &RptRow| &r.reason_originator);
        write_text(output, 9, &batch, |r: &RptRow| &r.original_msg_id);
        write_text(output, 10, &batch, |r: &RptRow| &r.original_msg_name_id);
        write_text(output, 11, &batch, |r: &RptRow| &r.original_instr_id);
        write_text(output, 12, &batch, |r: &RptRow| &r.original_end_to_end_id);
        write_text(output, 13, &batch, |r: &RptRow| &r.original_tx_id);
        write_text(output, 14, &batch, |r: &RptRow| &r.original_uetr);
        write_text(output, 17, &batch, |r: &RptRow| &r.original_currency);
        write_text(output, 19, &batch, |r: &RptRow| &r.original_debtor_name);
        write_text(output, 20, &batch, |r: &RptRow| &r.original_creditor_name);
        write_text(output, 21, &batch, |r: &RptRow| &r.source_file);
    }
}

// ── read_camt056 ─────────────────────────────────────────────────────────────

const CXL_COLUMNS: &[(&str, Col)] = &[
    ("assignment_id", Col::Text),
    ("assignment_created", Col::Stamp),
    ("assigner", Col::Text),
    ("assignee", Col::Text),
    // GROUP (a whole underlying batch, possibly GrpCxl) or TRANSACTION.
    ("scope", Col::Text),
    ("cancellation_id", Col::Text),
    ("case_id", Col::Text),
    // As the wire spelled it; "true" means the whole batch is to be cancelled.
    ("group_cancellation", Col::Text),
    ("original_number_of_txs", Col::Text),
    ("original_msg_id", Col::Text),
    ("original_msg_name_id", Col::Text),
    ("original_instr_id", Col::Text),
    ("original_end_to_end_id", Col::Text),
    ("original_tx_id", Col::Text),
    ("original_uetr", Col::Text),
    // A cancellation moves no money: there is no `amount`, only the original's.
    ("original_amount", Col::Money),
    ("original_currency", Col::Text),
    ("original_settlement_date", Col::Date),
    ("cancellation_reason_code", Col::Text),
    ("cancellation_reason_info", Col::Text),
    ("cancellation_originator", Col::Text),
    ("original_debtor_name", Col::Text),
    ("original_debtor_account", Col::Text),
    ("original_creditor_name", Col::Text),
    ("original_creditor_account", Col::Text),
    ("remittance_info", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadCamt056, CxlInit, CxlStream<Source>, CxlRow,
    name = "read_camt056",
    columns = CXL_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 15, &batch, |r: &CxlRow| r.original_amount);
        write_timestamp(output, 1, &batch, |r: &CxlRow| {
            r.assignment_created.as_deref().and_then(temporal::ts_micros)
        });
        write_date(output, 17, &batch, |r: &CxlRow| {
            r.original_settlement_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &CxlRow| &r.assignment_id);
        write_text(output, 2, &batch, |r: &CxlRow| &r.assigner);
        write_text(output, 3, &batch, |r: &CxlRow| &r.assignee);
        write_text(output, 4, &batch, |r: &CxlRow| &r.scope);
        write_text(output, 5, &batch, |r: &CxlRow| &r.cancellation_id);
        write_text(output, 6, &batch, |r: &CxlRow| &r.case_id);
        write_text(output, 7, &batch, |r: &CxlRow| &r.group_cancellation);
        write_text(output, 8, &batch, |r: &CxlRow| &r.original_number_of_txs);
        write_text(output, 9, &batch, |r: &CxlRow| &r.original_msg_id);
        write_text(output, 10, &batch, |r: &CxlRow| &r.original_msg_name_id);
        write_text(output, 11, &batch, |r: &CxlRow| &r.original_instr_id);
        write_text(output, 12, &batch, |r: &CxlRow| &r.original_end_to_end_id);
        write_text(output, 13, &batch, |r: &CxlRow| &r.original_tx_id);
        write_text(output, 14, &batch, |r: &CxlRow| &r.original_uetr);
        write_text(output, 16, &batch, |r: &CxlRow| &r.original_currency);
        write_text(output, 18, &batch, |r: &CxlRow| &r.cancellation_reason_code);
        write_text(output, 19, &batch, |r: &CxlRow| &r.cancellation_reason_info);
        write_text(output, 20, &batch, |r: &CxlRow| &r.cancellation_originator);
        write_text(output, 21, &batch, |r: &CxlRow| &r.original_debtor_name);
        write_text(output, 22, &batch, |r: &CxlRow| &r.original_debtor_account);
        write_text(output, 23, &batch, |r: &CxlRow| &r.original_creditor_name);
        write_text(output, 24, &batch, |r: &CxlRow| &r.original_creditor_account);
        write_text(output, 25, &batch, |r: &CxlRow| &r.remittance_info);
        write_text(output, 26, &batch, |r: &CxlRow| &r.source_file);
    }
}

// ── read_pain008 ─────────────────────────────────────────────────────────────

const DD_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("initiating_party", Col::Text),
    ("payment_info_id", Col::Text),
    ("payment_method", Col::Text),
    // FRST/RCUR/OOFF/FNAL — where this collection sits in the mandate's life.
    ("sequence_type", Col::Text),
    ("requested_collection_date", Col::Date),
    // The collector: pain.008 puts the CREDITOR on the payment group and one
    // debtor per transaction — pain.001 mirrored.
    ("creditor_name", Col::Text),
    ("creditor_account", Col::Text),
    ("creditor_agent_bic", Col::Text),
    ("creditor_scheme_id", Col::Text),
    ("instr_id", Col::Text),
    ("end_to_end_id", Col::Text),
    ("uetr", Col::Text),
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("charge_bearer", Col::Text),
    // The debtor's signed authorisation — what makes the pull legal.
    ("mandate_id", Col::Text),
    ("mandate_signed_on", Col::Date),
    ("debtor_name", Col::Text),
    ("debtor_account", Col::Text),
    ("debtor_agent_bic", Col::Text),
    ("remittance_info", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPain008, DdInit, DdStream<Source>, DdRow,
    name = "read_pain008",
    columns = DD_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 13, &batch, |r: &DdRow| r.amount);
        write_date(output, 5, &batch, |r: &DdRow| {
            r.requested_collection_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_date(output, 17, &batch, |r: &DdRow| {
            r.mandate_signed_on.as_deref().and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &DdRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &DdRow| &r.initiating_party);
        write_text(output, 2, &batch, |r: &DdRow| &r.payment_info_id);
        write_text(output, 3, &batch, |r: &DdRow| &r.payment_method);
        write_text(output, 4, &batch, |r: &DdRow| &r.sequence_type);
        write_text(output, 6, &batch, |r: &DdRow| &r.creditor_name);
        write_text(output, 7, &batch, |r: &DdRow| &r.creditor_account);
        write_text(output, 8, &batch, |r: &DdRow| &r.creditor_agent_bic);
        write_text(output, 9, &batch, |r: &DdRow| &r.creditor_scheme_id);
        write_text(output, 10, &batch, |r: &DdRow| &r.instr_id);
        write_text(output, 11, &batch, |r: &DdRow| &r.end_to_end_id);
        write_text(output, 12, &batch, |r: &DdRow| &r.uetr);
        write_text(output, 14, &batch, |r: &DdRow| &r.currency);
        write_text(output, 15, &batch, |r: &DdRow| &r.charge_bearer);
        write_text(output, 16, &batch, |r: &DdRow| &r.mandate_id);
        write_text(output, 18, &batch, |r: &DdRow| &r.debtor_name);
        write_text(output, 19, &batch, |r: &DdRow| &r.debtor_account);
        write_text(output, 20, &batch, |r: &DdRow| &r.debtor_agent_bic);
        write_text(output, 21, &batch, |r: &DdRow| &r.remittance_info);
        write_text(output, 22, &batch, |r: &DdRow| &r.source_file);
    }
}

// ── read_camt029 ─────────────────────────────────────────────────────────────

const ROI_COLUMNS: &[(&str, Col)] = &[
    ("assignment_id", Col::Text),
    ("assignment_created", Col::Stamp),
    ("assigner", Col::Text),
    ("assignee", Col::Text),
    // RESOLUTION (the message-level answer), GROUP, or TRANSACTION. Most real
    // camt.029 files answer at message level only.
    ("scope", Col::Text),
    // CNCL cancelled, RJCR cancellation rejected, … — on the RESOLUTION row.
    ("resolution_status", Col::Text),
    ("case_id", Col::Text),
    ("cancellation_status_id", Col::Text),
    ("cancellation_status", Col::Text),
    ("reason_code", Col::Text),
    ("reason_info", Col::Text),
    ("reason_originator", Col::Text),
    ("original_msg_id", Col::Text),
    ("original_msg_name_id", Col::Text),
    ("original_instr_id", Col::Text),
    ("original_end_to_end_id", Col::Text),
    ("original_tx_id", Col::Text),
    ("original_uetr", Col::Text),
    ("original_amount", Col::Money),
    ("original_currency", Col::Text),
    ("original_settlement_date", Col::Date),
    ("original_debtor_name", Col::Text),
    ("original_creditor_name", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadCamt029, RoiInit, RoiStream<Source>, RoiRow,
    name = "read_camt029",
    columns = ROI_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 18, &batch, |r: &RoiRow| r.original_amount);
        write_timestamp(output, 1, &batch, |r: &RoiRow| {
            r.assignment_created.as_deref().and_then(temporal::ts_micros)
        });
        write_date(output, 20, &batch, |r: &RoiRow| {
            r.original_settlement_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &RoiRow| &r.assignment_id);
        write_text(output, 2, &batch, |r: &RoiRow| &r.assigner);
        write_text(output, 3, &batch, |r: &RoiRow| &r.assignee);
        write_text(output, 4, &batch, |r: &RoiRow| &r.scope);
        write_text(output, 5, &batch, |r: &RoiRow| &r.resolution_status);
        write_text(output, 6, &batch, |r: &RoiRow| &r.case_id);
        write_text(output, 7, &batch, |r: &RoiRow| &r.cancellation_status_id);
        write_text(output, 8, &batch, |r: &RoiRow| &r.cancellation_status);
        write_text(output, 9, &batch, |r: &RoiRow| &r.reason_code);
        write_text(output, 10, &batch, |r: &RoiRow| &r.reason_info);
        write_text(output, 11, &batch, |r: &RoiRow| &r.reason_originator);
        write_text(output, 12, &batch, |r: &RoiRow| &r.original_msg_id);
        write_text(output, 13, &batch, |r: &RoiRow| &r.original_msg_name_id);
        write_text(output, 14, &batch, |r: &RoiRow| &r.original_instr_id);
        write_text(output, 15, &batch, |r: &RoiRow| &r.original_end_to_end_id);
        write_text(output, 16, &batch, |r: &RoiRow| &r.original_tx_id);
        write_text(output, 17, &batch, |r: &RoiRow| &r.original_uetr);
        write_text(output, 19, &batch, |r: &RoiRow| &r.original_currency);
        write_text(output, 21, &batch, |r: &RoiRow| &r.original_debtor_name);
        write_text(output, 22, &batch, |r: &RoiRow| &r.original_creditor_name);
        write_text(output, 23, &batch, |r: &RoiRow| &r.source_file);
    }
}

#[duckdb_entrypoint_c_api]
pub unsafe fn extension_entrypoint(con: Connection) -> Result<(), Box<dyn Error>> {
    con.register_table_function::<ReadIso20022>("read_iso20022")?;
    con.register_table_function::<ReadPacs008>("read_pacs008")?;
    con.register_table_function::<ReadPacs004>("read_pacs004")?;
    con.register_table_function::<ReadPacs002>("read_pacs002")?;
    con.register_table_function::<ReadPain001>("read_pain001")?;
    con.register_table_function::<ReadPain002>("read_pain002")?;
    con.register_table_function::<ReadPain008>("read_pain008")?;
    con.register_table_function::<ReadCamt056>("read_camt056")?;
    con.register_table_function::<ReadCamt029>("read_camt029")?;
    Ok(())
}
