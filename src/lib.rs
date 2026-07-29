//! quackiso — query ISO 20022 financial messages as SQL in DuckDB.
//!
//! v1 ships one table function, `read_iso20022(path)`, that flattens camt.053
//! bank statements into one row per booked entry. `bind` only resolves the file
//! list; the actual parsing streams inside `func`, which pulls the next
//! vector-sized batch of entries on demand. Memory stays O(batch) no matter how
//! large the statement is.

mod model;
mod stream;

use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId},
    duckdb_entrypoint_c_api,
    vtab::{BindInfo, InitInfo, TableFunctionInfo, VTab},
    Connection, Result,
};
use std::{
    error::Error,
    fs::File,
    io::BufReader,
};
use parking_lot::Mutex;

use model::Row;
use stream::EntryStream;

/// DuckDB's standard vector size. Rows are emitted in chunks of this many.
const VECTOR_SIZE: usize = 2048;

/// Output columns, in order. `amount` is DOUBLE; everything else is VARCHAR.
/// Dates stay VARCHAR (ISO strings) so `<Dt>` and `<DtTm>` both land without a
/// fragile parse; amount is f64 (exact below 2^53). Both are documented
/// tradeoffs; a DATE/DECIMAL mode is a later release.
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
    files: Vec<String>,
}

/// Per-scan cursor: which file we are on and its open streaming reader. Behind a
/// Mutex because `func` only gets a shared reference to the init data.
struct ScanState {
    idx: usize,
    cur: Option<EntryStream<BufReader<File>>>,
}

#[repr(C)]
struct IsoInitData {
    state: Mutex<ScanState>,
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
        let mut files: Vec<String> = glob::glob(&pattern)
            .map_err(|e| format!("bad path pattern {pattern:?}: {e}"))?
            .filter_map(|p| p.ok())
            .map(|p| p.display().to_string())
            .collect();
        if files.is_empty() && std::path::Path::new(&pattern).is_file() {
            files.push(pattern.clone());
        }
        if files.is_empty() {
            return Err(format!("read_iso20022: no files matched {pattern:?}").into());
        }
        Ok(IsoBindData { files })
    }

    fn init(_: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        Ok(IsoInitData {
            state: Mutex::new(ScanState { idx: 0, cur: None }),
        })
    }

    fn func(func: &TableFunctionInfo<Self>, output: &mut DataChunkHandle) -> Result<(), Box<dyn Error>> {
        let files = &func.get_bind_data().files;
        let mut st = func.get_init_data().state.lock();

        // Pull up to one vector of rows, advancing across files as each drains.
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

        write_batch(&batch, output);
        output.set_len(batch.len());
        Ok(())
    }

    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![LogicalTypeHandle::from(LogicalTypeId::Varchar)])
    }
}

/// Write a batch of rows into the output chunk's column vectors.
fn write_batch(batch: &[Row], output: &mut DataChunkHandle) {
    // amount (DOUBLE) — column 4. Fill the slice first (that borrow ends before
    // we touch the vector again), then mark NULLs.
    {
        let mut v = output.flat_vector(4);
        {
            let slice = unsafe { v.as_mut_slice::<f64>() };
            for (i, row) in batch.iter().enumerate() {
                if let Some(a) = row.amount {
                    slice[i] = a;
                }
            }
        }
        for (i, row) in batch.iter().enumerate() {
            if row.amount.is_none() {
                v.set_null(i);
            }
        }
    }

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
}

#[duckdb_entrypoint_c_api]
pub unsafe fn extension_entrypoint(con: Connection) -> Result<(), Box<dyn Error>> {
    con.register_table_function::<ReadIso20022>("read_iso20022")?;
    Ok(())
}
