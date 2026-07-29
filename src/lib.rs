//! quackiso — query ISO 20022 financial messages as SQL in DuckDB.
//!
//! v1 ships one table function, `read_iso20022(path)`, that flattens camt.053
//! bank statements into one row per booked entry. It mirrors the VTab pattern
//! from duckdb/extension-template-rs: `bind` declares the columns and does the
//! parsing, `func` streams the resulting rows in vector-sized chunks.

mod model;

use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId},
    duckdb_entrypoint_c_api,
    vtab::{BindInfo, InitInfo, TableFunctionInfo, VTab},
    Connection, Result,
};
use std::{
    error::Error,
    sync::atomic::{AtomicUsize, Ordering},
};

use model::{flatten, Document, Row};

/// DuckDB's standard vector size. Rows are emitted in chunks of this many.
const VECTOR_SIZE: usize = 2048;

/// Output columns, in order. `amount` is DOUBLE; everything else is VARCHAR.
/// v1 keeps dates as VARCHAR (ISO strings) so a `<Dt>` and a `<DtTm>` both land
/// without a fragile parse; cast in SQL if you want a real DATE. amount is f64
/// for ergonomics — fine below 2^53; an exact DECIMAL mode is a later release.
const COLUMNS: &[(&str, LogicalTypeId)] = &[
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
struct IsoBindData {
    rows: Vec<Row>,
}

#[repr(C)]
struct IsoInitData {
    cursor: AtomicUsize,
}

struct ReadIso20022;

impl VTab for ReadIso20022 {
    type InitData = IsoInitData;
    type BindData = IsoBindData;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
        for (name, ty) in COLUMNS {
            bind.add_result_column(name, LogicalTypeHandle::from(*ty));
        }

        let pattern = bind.get_parameter(0).to_string();
        let mut paths: Vec<_> = glob::glob(&pattern)
            .map_err(|e| format!("bad path pattern {pattern:?}: {e}"))?
            .filter_map(|p| p.ok())
            .collect();
        // A bare path that isn't a glob still counts as one file.
        if paths.is_empty() && std::path::Path::new(&pattern).is_file() {
            paths.push(pattern.clone().into());
        }
        if paths.is_empty() {
            return Err(format!("read_iso20022: no files matched {pattern:?}").into());
        }

        let mut rows = Vec::new();
        for path in paths {
            let name = path.display().to_string();
            let xml = std::fs::read_to_string(&path)
                .map_err(|e| format!("read_iso20022: cannot read {name}: {e}"))?;
            let doc: Document = quick_xml::de::from_str(&xml)
                .map_err(|e| format!("read_iso20022: {name} is not valid camt.053: {e}"))?;
            rows.extend(flatten(&doc, &name));
        }
        Ok(IsoBindData { rows })
    }

    fn init(_: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        Ok(IsoInitData {
            cursor: AtomicUsize::new(0),
        })
    }

    fn func(func: &TableFunctionInfo<Self>, output: &mut DataChunkHandle) -> Result<(), Box<dyn Error>> {
        let rows = &func.get_bind_data().rows;
        let total = rows.len();
        let start = func.get_init_data().cursor.load(Ordering::Relaxed);
        if start >= total {
            output.set_len(0);
            return Ok(());
        }
        let end = (start + VECTOR_SIZE).min(total);
        let batch = &rows[start..end];

        // amount (DOUBLE) — column 4
        {
            let mut v = output.flat_vector(4);
            let slice = v.as_mut_slice::<f64>();
            for (i, row) in batch.iter().enumerate() {
                match row.amount {
                    Some(a) => slice[i] = a,
                    None => v.set_null(i),
                }
            }
        }

        // every VARCHAR column, addressed by its extractor
        let text_cols: [(usize, fn(&Row) -> &Option<String>); 15] = [
            (0, |r| &r.msg_id),
            (1, |r| &r.account_iban),
            (2, |r| &r.statement_id),
            (3, |r| &r.entry_ref),
            (5, |r| &r.currency),
            (6, |r| &r.credit_debit),
            (7, |r| &r.status),
            (8, |r| &r.booking_date),
            (9, |r| &r.value_date),
            (10, |r| &r.bank_ref),
            (11, |r| &r.end_to_end_id),
            (12, |r| &r.counterparty_name),
            (13, |r| &r.counterparty_iban),
            (14, |r| &r.remittance_info),
            (15, |r| &r.source_file),
        ];
        for (idx, get) in text_cols {
            let mut v = output.flat_vector(idx);
            for (i, row) in batch.iter().enumerate() {
                match get(row) {
                    Some(s) => v.insert(i, s.as_str()),
                    None => v.set_null(i),
                }
            }
        }

        func.get_init_data().cursor.store(end, Ordering::Relaxed);
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
    Ok(())
}
