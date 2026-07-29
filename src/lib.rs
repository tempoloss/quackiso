//! quackiso — query ISO 20022 financial messages as SQL in DuckDB.
//!
//! Two table functions, both streaming:
//!
//! * `read_iso20022(path)` — cash-management messages (camt.053 statements,
//!   camt.054 notifications, camt.052 reports). One row per booked entry.
//! * `read_pacs008(path)` — FI-to-FI customer credit transfers (the ISO 20022
//!   replacement for SWIFT MT103). One row per credit-transfer transaction.
//!
//! `bind` only resolves the file list; parsing happens inside `func`, which
//! pulls the next vector-sized batch on demand. Memory stays O(batch) no matter
//! how large the file is.

mod model;
mod pacs008;
mod stream;

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
use stream::EntryStream;

/// DuckDB's standard vector size. Rows are emitted in chunks of this many.
const VECTOR_SIZE: usize = 2048;

/// Expand a path or glob into a file list. Shared by both readers.
fn resolve_files(pattern: &str, fname: &str) -> Result<Vec<String>, Box<dyn Error>> {
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

/// Write one VARCHAR column from a batch, NULLing absent values.
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

/// Write one DOUBLE column. Values are filled through the raw slice first so
/// that borrow ends before the vector is touched again for NULLs.
fn write_double<T>(
    output: &mut DataChunkHandle,
    idx: usize,
    batch: &[T],
    get: impl Fn(&T) -> Option<f64>,
) {
    let mut v = output.flat_vector(idx);
    {
        let slice = unsafe { v.as_mut_slice::<f64>() };
        for (i, row) in batch.iter().enumerate() {
            if let Some(a) = get(row) {
                slice[i] = a;
            }
        }
    }
    for (i, row) in batch.iter().enumerate() {
        if get(row).is_none() {
            v.set_null(i);
        }
    }
}

// ── read_iso20022: camt.053 / camt.054 / camt.052 ─────────────────────────────

/// Dates stay VARCHAR (ISO strings) so `<Dt>` and `<DtTm>` both land without a
/// fragile parse; amount is f64 (exact below 2^53). Documented tradeoffs — a
/// DATE/DECIMAL mode is a later release.
const CAMT_COLUMNS: &[(&str, LogicalTypeId)] = &[
    ("msg_id", LogicalTypeId::Varchar),
    ("account_iban", LogicalTypeId::Varchar),
    ("statement_id", LogicalTypeId::Varchar),
    ("entry_ref", LogicalTypeId::Varchar),
    ("amount", LogicalTypeId::Double),
    ("currency", LogicalTypeId::Varchar),
    ("credit_debit", LogicalTypeId::Varchar),
    ("status", LogicalTypeId::Varchar),
    ("booking_date", LogicalTypeId::Varchar),
    ("value_date", LogicalTypeId::Varchar),
    ("bank_ref", LogicalTypeId::Varchar),
    ("end_to_end_id", LogicalTypeId::Varchar),
    ("counterparty_name", LogicalTypeId::Varchar),
    ("counterparty_iban", LogicalTypeId::Varchar),
    ("remittance_info", LogicalTypeId::Varchar),
    ("source_file", LogicalTypeId::Varchar),
];

#[repr(C)]
struct CamtBindData {
    files: Vec<String>,
}

struct CamtScan {
    idx: usize,
    cur: Option<EntryStream<BufReader<File>>>,
}

#[repr(C)]
struct CamtInitData {
    state: Mutex<CamtScan>,
}

struct ReadIso20022;

impl VTab for ReadIso20022 {
    type InitData = CamtInitData;
    type BindData = CamtBindData;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
        for (name, ty) in CAMT_COLUMNS {
            bind.add_result_column(name, LogicalTypeHandle::from(*ty));
        }
        let files = resolve_files(&bind.get_parameter(0).to_string(), "read_iso20022")?;
        Ok(CamtBindData { files })
    }

    fn init(_: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        Ok(CamtInitData {
            state: Mutex::new(CamtScan { idx: 0, cur: None }),
        })
    }

    fn func(func: &TableFunctionInfo<Self>, output: &mut DataChunkHandle) -> Result<(), Box<dyn Error>> {
        let files = &func.get_bind_data().files;
        let mut st = func.get_init_data().state.lock();

        let mut batch: Vec<Row> = Vec::with_capacity(VECTOR_SIZE);
        while batch.len() < VECTOR_SIZE {
            if st.cur.is_none() {
                if st.idx >= files.len() {
                    break;
                }
                let path = files[st.idx].clone();
                let file = File::open(&path)
                    .map_err(|e| format!("read_iso20022: cannot read {path}: {e}"))?;
                st.cur = Some(EntryStream::new(BufReader::new(file), &path));
            }
            match st.cur.as_mut().unwrap().next_row()? {
                Some(row) => batch.push(row),
                None => {
                    st.cur = None;
                    st.idx += 1;
                }
            }
        }

        write_double(output, 4, &batch, |r: &Row| r.amount);
        write_text(output, 0, &batch, |r: &Row| &r.msg_id);
        write_text(output, 1, &batch, |r: &Row| &r.account_iban);
        write_text(output, 2, &batch, |r: &Row| &r.statement_id);
        write_text(output, 3, &batch, |r: &Row| &r.entry_ref);
        write_text(output, 5, &batch, |r: &Row| &r.currency);
        write_text(output, 6, &batch, |r: &Row| &r.credit_debit);
        write_text(output, 7, &batch, |r: &Row| &r.status);
        write_text(output, 8, &batch, |r: &Row| &r.booking_date);
        write_text(output, 9, &batch, |r: &Row| &r.value_date);
        write_text(output, 10, &batch, |r: &Row| &r.bank_ref);
        write_text(output, 11, &batch, |r: &Row| &r.end_to_end_id);
        write_text(output, 12, &batch, |r: &Row| &r.counterparty_name);
        write_text(output, 13, &batch, |r: &Row| &r.counterparty_iban);
        write_text(output, 14, &batch, |r: &Row| &r.remittance_info);
        write_text(output, 15, &batch, |r: &Row| &r.source_file);
        output.set_len(batch.len());
        Ok(())
    }

    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![LogicalTypeHandle::from(LogicalTypeId::Varchar)])
    }
}

// ── read_pacs008: FI-to-FI customer credit transfer ───────────────────────────

const PACS_COLUMNS: &[(&str, LogicalTypeId)] = &[
    ("msg_id", LogicalTypeId::Varchar),
    ("instr_id", LogicalTypeId::Varchar),
    ("end_to_end_id", LogicalTypeId::Varchar),
    ("tx_id", LogicalTypeId::Varchar),
    ("uetr", LogicalTypeId::Varchar),
    ("amount", LogicalTypeId::Double),
    ("currency", LogicalTypeId::Varchar),
    ("settlement_date", LogicalTypeId::Varchar),
    ("charge_bearer", LogicalTypeId::Varchar),
    ("debtor_name", LogicalTypeId::Varchar),
    ("debtor_account", LogicalTypeId::Varchar),
    ("debtor_agent_bic", LogicalTypeId::Varchar),
    ("creditor_name", LogicalTypeId::Varchar),
    ("creditor_account", LogicalTypeId::Varchar),
    ("creditor_agent_bic", LogicalTypeId::Varchar),
    ("remittance_info", LogicalTypeId::Varchar),
    ("source_file", LogicalTypeId::Varchar),
];

#[repr(C)]
struct PacsBindData {
    files: Vec<String>,
}

struct PacsScan {
    idx: usize,
    cur: Option<TxStream<BufReader<File>>>,
}

#[repr(C)]
struct PacsInitData {
    state: Mutex<PacsScan>,
}

struct ReadPacs008;

impl VTab for ReadPacs008 {
    type InitData = PacsInitData;
    type BindData = PacsBindData;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
        for (name, ty) in PACS_COLUMNS {
            bind.add_result_column(name, LogicalTypeHandle::from(*ty));
        }
        let files = resolve_files(&bind.get_parameter(0).to_string(), "read_pacs008")?;
        Ok(PacsBindData { files })
    }

    fn init(_: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        Ok(PacsInitData {
            state: Mutex::new(PacsScan { idx: 0, cur: None }),
        })
    }

    fn func(func: &TableFunctionInfo<Self>, output: &mut DataChunkHandle) -> Result<(), Box<dyn Error>> {
        let files = &func.get_bind_data().files;
        let mut st = func.get_init_data().state.lock();

        let mut batch: Vec<PacsRow> = Vec::with_capacity(VECTOR_SIZE);
        while batch.len() < VECTOR_SIZE {
            if st.cur.is_none() {
                if st.idx >= files.len() {
                    break;
                }
                let path = files[st.idx].clone();
                let file = File::open(&path)
                    .map_err(|e| format!("read_pacs008: cannot read {path}: {e}"))?;
                st.cur = Some(TxStream::new(BufReader::new(file), &path));
            }
            match st.cur.as_mut().unwrap().next_row()? {
                Some(row) => batch.push(row),
                None => {
                    st.cur = None;
                    st.idx += 1;
                }
            }
        }

        write_double(output, 5, &batch, |r: &PacsRow| r.amount);
        write_text(output, 0, &batch, |r: &PacsRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &PacsRow| &r.instr_id);
        write_text(output, 2, &batch, |r: &PacsRow| &r.end_to_end_id);
        write_text(output, 3, &batch, |r: &PacsRow| &r.tx_id);
        write_text(output, 4, &batch, |r: &PacsRow| &r.uetr);
        write_text(output, 6, &batch, |r: &PacsRow| &r.currency);
        write_text(output, 7, &batch, |r: &PacsRow| &r.settlement_date);
        write_text(output, 8, &batch, |r: &PacsRow| &r.charge_bearer);
        write_text(output, 9, &batch, |r: &PacsRow| &r.debtor_name);
        write_text(output, 10, &batch, |r: &PacsRow| &r.debtor_account);
        write_text(output, 11, &batch, |r: &PacsRow| &r.debtor_agent_bic);
        write_text(output, 12, &batch, |r: &PacsRow| &r.creditor_name);
        write_text(output, 13, &batch, |r: &PacsRow| &r.creditor_account);
        write_text(output, 14, &batch, |r: &PacsRow| &r.creditor_agent_bic);
        write_text(output, 15, &batch, |r: &PacsRow| &r.remittance_info);
        write_text(output, 16, &batch, |r: &PacsRow| &r.source_file);
        output.set_len(batch.len());
        Ok(())
    }

    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![LogicalTypeHandle::from(LogicalTypeId::Varchar)])
    }
}

#[duckdb_entrypoint_c_api]
pub unsafe fn extension_entrypoint(con: Connection) -> Result<(), Box<dyn Error>> {
    con.register_table_function::<ReadIso20022>("read_iso20022")?;
    con.register_table_function::<ReadPacs008>("read_pacs008")?;
    Ok(())
}
