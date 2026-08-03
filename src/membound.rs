//! The memory boundary, measured.
//!
//! `README.md` says a 1.7 GB statement reads in about 2 MB of resident memory.
//! That was a number someone once watched go by: the design notes admitted as
//! much — "nothing yet that measures the memory boundary". This module is the
//! measurement, run as a test, with the terms named.
//!
//! Three numbers, because "2 MB resident" on its own does not say which:
//!
//! * **peak live heap** — the high-water mark of the sum of live allocation
//!   sizes, taken by a tracking global allocator that exists only in test
//!   builds. Deterministic, and the number the bound is actually about.
//! * **peak RSS delta** — `VmHWM` after the parse, with the high-water mark
//!   reset through `/proc/self/clear_refs` immediately before it, minus the
//!   resident set at that moment. Physical pages this parse added. Linux only;
//!   nothing else exposes a resettable peak.
//! * **process peak RSS** — the whole test process, for scale.
//!
//! All three are a **standalone parser measurement**: this is the scan loop
//! `read_iso20022` runs — [`crate::pull_batch`] over an [`EntryStream`], one
//! vector of rows alive at a time — in a test binary with no DuckDB in the
//! process. None of it is an incremental-over-DuckDB figure, and DuckDB's own
//! resident set (tens of MB before a single row is read) is not in it.
//!
//! The bound is not independence from the input. It is
//! `O(VECTOR_SIZE × row + largest subtree)`: one output batch of at most 2048
//! flattened rows, plus the one `<Ntry>` subtree being copied and
//! deserialized. Both terms are exercised here — [`peak_follows_the_output_batch`]
//! widens the row, [`peak_follows_the_largest_subtree`] widens the subtree, and
//! both move the peak. What no input characteristic moves is file size:
//! [`peak_does_not_follow_file_size`] multiplies the statement by eight and the
//! peak does not budge, and [`the_documented_statement`] does it at 1.7 GB.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use crate::stream::EntryStream;
use crate::{pull_batch, spawn_workers, ScanState, Source, VECTOR_SIZE};

// ── what the parse is allowed to cost ────────────────────────────────────────

// The ceilings below are canaries, not budgets. Each sits a small multiple over
// a measured value, so an extra copy — here, or in quick-xml, or in serde —
// trips one. When one trips the fix is to find the copy. If the copy turns out
// to be justified, move the number and quote the new measurement in the commit;
// never round a ceiling up until the suite goes green.

/// Steady-state ceiling for a statement of ordinary entries, whatever its size.
/// One 2048-row batch of flattened rows dominates it: 1.23 MiB measured, from
/// 4,000 entries to three million. The ceiling leaves room for a different
/// allocator or a wider row, and none for a second batch.
const STEADY_HEAP_CEILING: usize = 4 << 20;

/// The same ceiling in resident pages, for the runs where the OS will say.
/// Higher than the heap ceiling because RSS carries the allocator's arenas, the
/// 64 KiB read buffer, and page granularity on top of the live bytes; the
/// 1.7 GB statement measures 2.04 MiB of it.
const STEADY_RSS_CEILING: usize = 8 << 20;

/// How much two peaks that should be equal are allowed to differ. Two runs of
/// the same loop over different-sized files allocate the same objects; this
/// covers allocator bookkeeping and the odd byte from a test running beside it.
const NOISE: usize = 256 << 10;

// ── the allocator the measurement reads ──────────────────────────────────────

/// The system allocator, plus a running total of live bytes and its high-water
/// mark. `mod membound` is `#[cfg(test)]`, so the shipped extension links no
/// allocator shim at all.
struct Tracking;

#[global_allocator]
static ALLOCATOR: Tracking = Tracking;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

#[inline]
fn grew(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            grew(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc_zeroed(layout);
        if !ptr.is_null() {
            grew(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    /// Forwarded to the system allocator rather than left to the trait's
    /// alloc-copy-dealloc default: a `Vec` that grows in place must be counted
    /// as a bigger allocation, not as a second live one.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = System.realloc(ptr, layout, new_size);
        if !moved.is_null() {
            match new_size.checked_sub(layout.size()) {
                Some(more) => grew(more),
                None => {
                    LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
                }
            }
        }
        moved
    }
}

// ── what the kernel says ─────────────────────────────────────────────────────

/// A `/proc/self/status` size field, in bytes.
#[cfg(target_os = "linux")]
fn status_field(name: &str) -> Option<usize> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with(name))?;
    let value = line.split_whitespace().nth(1)?;
    Some(value.parse::<usize>().ok()? * 1024)
}

#[cfg(target_os = "linux")]
fn resident() -> Option<usize> {
    status_field("VmRSS:")
}

#[cfg(target_os = "linux")]
fn peak_resident() -> Option<usize> {
    status_field("VmHWM:")
}

/// Drop `VmHWM` back to the current `VmRSS` and report that baseline, so the
/// peak that follows belongs to one parse instead of to the whole process.
/// `5` is `CLEAR_REFS_MM_HIWATER_RSS`. A kernel that refuses gives `None`, and
/// the run reports heap only rather than a peak measured since process start.
#[cfg(target_os = "linux")]
fn reset_peak_resident() -> Option<usize> {
    std::fs::write("/proc/self/clear_refs", b"5\n").ok()?;
    resident()
}

#[cfg(not(target_os = "linux"))]
fn peak_resident() -> Option<usize> {
    None
}

#[cfg(not(target_os = "linux"))]
fn reset_peak_resident() -> Option<usize> {
    None
}

// ── one measured window ──────────────────────────────────────────────────────

struct Sample {
    /// Peak live heap the window added, in bytes.
    heap: usize,
    /// Peak resident set the window added, where the OS will say.
    rss: Option<usize>,
    /// Peak resident set of the whole test process, for scale.
    process: Option<usize>,
}

impl fmt::Display for Sample {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "peak live heap {}", mib(self.heap))?;
        match (self.rss, self.process) {
            (Some(rss), Some(process)) => {
                write!(f, ", peak RSS +{} (process peak {})", mib(rss), mib(process))
            }
            // Not a silent skip: the heap bound still holds, but the resident
            // half of the claim is only measurable where the peak can be reset.
            _ => write!(f, ", resident set not measured (needs Linux /proc/self/clear_refs)"),
        }
    }
}

/// The counters are process-wide and `cargo test` runs tests on parallel
/// threads, so a measurement that did not hold this would measure its
/// neighbours. Held across fixture generation too: writing a 24 MB file
/// allocates.
static MEASURING: Mutex<()> = Mutex::new(());

fn exclusive() -> MutexGuard<'static, ()> {
    MEASURING.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Run `body` and report what it cost. The guard is passed rather than taken
/// here because the fixtures have to be written under it as well.
fn measure<T>(_exclusive: &MutexGuard<'static, ()>, body: impl FnOnce() -> T) -> (T, Sample) {
    let heap_base = LIVE.load(Ordering::Relaxed);
    PEAK.store(heap_base, Ordering::Relaxed);
    let rss_base = reset_peak_resident();

    let out = body();

    let heap = PEAK.load(Ordering::Relaxed).saturating_sub(heap_base);
    let process = peak_resident();
    let rss = match (process, rss_base) {
        (Some(peak), Some(base)) => Some(peak.saturating_sub(base)),
        _ => None,
    };
    (out, Sample { heap, rss, process })
}

// ── the scan under measurement ───────────────────────────────────────────────

/// The sequential scan `read_iso20022` runs: `pull_batch` until it comes back
/// empty, one vector of rows alive at a time. DuckDB copies each batch into its
/// output chunk and drops it; here it is counted and dropped.
fn scan(files: &[String]) -> usize {
    let mut state = ScanState::<EntryStream<Source>>::new();
    let mut rows = 0;
    loop {
        let batch = pull_batch::<EntryStream<Source>>(files, &mut state, "read_iso20022")
            .expect("membound fixtures parse");
        if batch.is_empty() {
            return rows;
        }
        rows += batch.len();
    }
}

/// The parallel scan: workers claim files from the shared counter and hand
/// batches over the bounded channel, and the consumer drains them.
fn scan_parallel(files: Vec<String>, threads: usize) -> usize {
    let rx = spawn_workers::<EntryStream<Source>>(files, threads, "read_iso20022");
    let mut rows = 0;
    for batch in rx {
        rows += batch.expect("membound fixtures parse").len();
    }
    rows
}

// ── fixtures ─────────────────────────────────────────────────────────────────

/// A generated statement on disk — up to 1.7 GB of it — removed when the test
/// drops it, unless the caller asked to keep it.
struct Fixture {
    path: PathBuf,
    entries: usize,
    bytes: u64,
    keep: bool,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl Fixture {
    fn arg(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

/// Set `QUACKISO_MEMBOUND_KEEP=<dir>` to write the fixtures there, under a
/// stable name, and leave them behind. That is how the same bytes reach
/// `scripts/measure_in_duckdb.py`: one generator, two measurements, no second
/// definition of what "the documented statement" is.
fn fixture_path(tag: &str) -> (PathBuf, bool) {
    match std::env::var_os("QUACKISO_MEMBOUND_KEEP") {
        Some(dir) => (
            PathBuf::from(dir).join(format!("quackiso-membound-{tag}.xml")),
            true,
        ),
        None => (
            std::env::temp_dir().join(format!(
                "quackiso-membound-{tag}-{}.xml",
                std::process::id()
            )),
            false,
        ),
    }
}

const HEAD: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
    "<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:camt.053.001.02\">\n",
    "  <BkToCstmrStmt>\n",
    "    <GrpHdr><MsgId>MEMBOUND-MSG</MsgId><CreDtTm>2026-07-01T00:00:00</CreDtTm></GrpHdr>\n",
    "    <Stmt>\n",
    "      <Id>MEMBOUND-STMT</Id>\n",
    "      <Acct><Id><IBAN>DE89370400440532013000</IBAN></Id></Acct>\n",
);

const TAIL: &str = concat!("    </Stmt>\n", "  </BkToCstmrStmt>\n", "</Document>\n");

/// The remittance text of an ordinary entry: 28 bytes, which puts the whole
/// entry at 576 and three million of them at 1.7 GB — the shape behind the
/// documented pair.
fn ordinary(i: usize) -> String {
    format!("Invoice {i:012} settled")
}

/// The same text stretched to `width` bytes, for the runs that widen a row or a
/// subtree on purpose.
fn padded(i: usize, width: usize) -> String {
    let base = format!("{} ", ordinary(i));
    let mut text = base.repeat(width / base.len() + 1);
    text.truncate(width);
    text
}

/// Wrap whatever `body` writes in a camt.053 document and record what it cost
/// on disk.
fn fixture(tag: &str, entries: usize, body: impl FnOnce(&mut BufWriter<File>)) -> Fixture {
    let (path, keep) = fixture_path(tag);
    let file = File::create(&path).expect("membound fixture is writable");
    let mut out = BufWriter::with_capacity(1 << 20, file);
    out.write_all(HEAD.as_bytes()).expect("fixture head");
    body(&mut out);
    out.write_all(TAIL.as_bytes()).expect("fixture tail");
    out.flush().expect("fixture flush");
    drop(out);

    let bytes = std::fs::metadata(&path).expect("fixture exists").len();
    Fixture {
        path,
        entries,
        bytes,
        keep,
    }
}

/// Write a camt.053 statement of `entries` booked entries. `remittance` decides
/// how much unstructured text each one carries, which is how the subtree and
/// batch terms of the bound get moved independently of the file size.
fn statement(tag: &str, entries: usize, remittance: &dyn Fn(usize) -> String) -> Fixture {
    fixture(tag, entries, |out| {
        for i in 0..entries {
            let whole = (i % 900_000) + 100;
            let cents = i % 100;
            let remit = remittance(i);
            write!(
                out,
                r#"    <Ntry>
      <NtryRef>NTRY-{i:012}</NtryRef>
      <Amt Ccy="EUR">{whole}.{cents:02}</Amt>
      <CdtDbtInd>DBIT</CdtDbtInd>
      <Sts><Cd>BOOK</Cd></Sts>
      <BookgDt><Dt>2026-07-01</Dt></BookgDt>
      <ValDt><Dt>2026-07-02</Dt></ValDt>
      <NtryDtls><TxDtls>
        <Refs><EndToEndId>E2E-{i:012}</EndToEndId><TxId>TX-{i:012}</TxId></Refs>
        <RltdPties><Dbtr><Nm>Debtor {i:09}</Nm></Dbtr><Cdtr><Nm>Creditor {i:09}</Nm></Cdtr></RltdPties>
        <RmtInf><Ustrd>{remit}</Ustrd></RmtInf>
      </TxDtls></NtryDtls>
    </Ntry>
"#
            )
            .expect("fixture entry");
        }
    })
}

/// Corpus files whose entries are copied verbatim into the real-shape fixture.
/// All four carry a default namespace, so their `<Ntry>` subtrees are
/// well-formed inside any camt document; between them they cover `<Sts>BOOK</Sts>`
/// and `<Sts><Cd>`, five-fraction-digit amounts, the camt.054 notification
/// shape, and the camt.053.001.08 `Pty/Nm` nesting.
const CORPUS_SHAPES: [&str; 4] = [
    "camt053_sample.xml",
    "camt053_decimal_sample.xml",
    "camt053_v8_sample.xml",
    "camt054_sample.xml",
];

/// Everything outside an XML comment. The corpus files explain themselves in
/// comments, and those comments quote the tags they are about — `<Ntry>` among
/// them — so a harvester that reads them takes a "subtree" that starts inside
/// prose and ends in another element.
fn without_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("<!--") {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        match after.find("-->") {
            Some(close) => rest = &after[close + "-->".len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Every `<Ntry>` those files hold, verbatim — indentation, element order and
/// all. Generated entries are uniform by construction, which is exactly what a
/// memory bound should not be measured on alone.
fn real_entries() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    let mut entries = Vec::new();
    for name in CORPUS_SHAPES {
        let path = dir.join(name);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let text = without_comments(&text);
        let mut rest = text.as_str();
        while let Some(start) = rest.find("<Ntry>") {
            let from = &rest[start..];
            let end = from
                .find("</Ntry>")
                .unwrap_or_else(|| panic!("{name}: an entry never closes"))
                + "</Ntry>".len();
            entries.push(format!("    {}\n", &from[..end]));
            rest = &from[end..];
        }
    }
    assert!(
        entries.len() >= 10,
        "the corpus shapes yielded only {} entries; a fixture changed shape",
        entries.len()
    );
    entries
}

/// A statement of `entries` entries, cycled through the real shapes.
fn corpus_statement(tag: &str, entries: usize) -> Fixture {
    let shapes = real_entries();
    fixture(tag, entries, |out| {
        for i in 0..entries {
            out.write_all(shapes[i % shapes.len()].as_bytes())
                .expect("fixture entry");
        }
    })
}

// ── reporting ────────────────────────────────────────────────────────────────

fn mib(bytes: usize) -> String {
    format!("{:.2} MiB", bytes as f64 / (1 << 20) as f64)
}

fn on_disk(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.2} GB", bytes as f64 / 1e9)
    } else {
        format!("{:.1} MB", bytes as f64 / 1e6)
    }
}

/// One line per measurement, so `--nocapture` prints a table rather than a
/// verdict. The claim in the README is only worth what these lines say.
fn report(label: &str, fixture: &Fixture, rows: usize, sample: &Sample) {
    println!(
        "[membound] {label}: {rows} rows, {} entries, {} on disk -> {sample}",
        fixture.entries,
        on_disk(fixture.bytes),
    );
    if fixture.keep {
        println!("[membound] kept at {}", fixture.path.display());
    }
}

// ── the tests ────────────────────────────────────────────────────────────────

/// Eight times the file, the same peak. This is the property the README claims
/// and the one the parallel and batch terms are measured against.
#[test]
fn peak_does_not_follow_file_size() {
    let lock = exclusive();
    let small = statement("small", 4_000, &ordinary);
    let large = statement("large", 32_000, &ordinary);

    let (small_rows, small_peak) = measure(&lock, || scan(&[small.arg()]));
    let (large_rows, large_peak) = measure(&lock, || scan(&[large.arg()]));
    report("4k entries", &small, small_rows, &small_peak);
    report("32k entries", &large, large_rows, &large_peak);

    assert_eq!(small_rows, small.entries, "the fixture must actually parse");
    assert_eq!(large_rows, large.entries, "the fixture must actually parse");
    assert!(
        large.bytes > small.bytes * 7,
        "the large fixture is meant to be ~8x the small one: {} vs {} bytes",
        large.bytes,
        small.bytes
    );

    assert!(
        large_peak.heap <= small_peak.heap + NOISE,
        "peak follows file size: {} for {} bytes against {} for {} bytes",
        mib(large_peak.heap),
        large.bytes,
        mib(small_peak.heap),
        small.bytes
    );
    assert!(
        large_peak.heap <= STEADY_HEAP_CEILING,
        "steady-state heap {} is over the {} ceiling",
        mib(large_peak.heap),
        mib(STEADY_HEAP_CEILING)
    );
    if let Some(rss) = large_peak.rss {
        assert!(
            rss <= STEADY_RSS_CEILING,
            "steady-state RSS {} is over the {} ceiling",
            mib(rss),
            mib(STEADY_RSS_CEILING)
        );
    }
}

/// The first term of the bound: 2048 rows are alive at once, so a row carrying
/// 4 KiB of remittance text costs 2048 × 4 KiB more than a narrow one. Memory
/// is bounded, not independent of what the rows contain.
#[test]
fn peak_follows_the_output_batch() {
    const WIDE: usize = 4 << 10;
    let lock = exclusive();
    let narrow = statement("narrow", 6_000, &ordinary);
    let wide = statement("wide", 6_000, &|i| padded(i, WIDE));

    let (narrow_rows, narrow_peak) = measure(&lock, || scan(&[narrow.arg()]));
    let (wide_rows, wide_peak) = measure(&lock, || scan(&[wide.arg()]));
    report("narrow rows", &narrow, narrow_rows, &narrow_peak);
    report("4 KiB rows", &wide, wide_rows, &wide_peak);

    assert_eq!(narrow_rows, narrow.entries);
    assert_eq!(wide_rows, wide.entries);

    let batch = VECTOR_SIZE * WIDE;
    let grew = wide_peak.heap.saturating_sub(narrow_peak.heap);
    assert!(
        grew >= batch / 2,
        "a batch of 4 KiB rows should cost about {}, it cost {}",
        mib(batch),
        mib(grew)
    );
    assert!(
        grew <= batch * 3,
        "a batch of 4 KiB rows cost {}, more than three times the {} the batch holds",
        mib(grew),
        mib(batch)
    );
}

/// The second term, and the one nothing in the docs admitted to: the largest
/// single subtree. An entry is copied out of the event stream, deserialized,
/// and flattened into a row, so a fat `<Ntry>` is live several times over while
/// that happens. The peak tracks it — quadruple the subtree, quadruple the
/// peak — which is why "independent of the input" is the wrong claim and
/// `O(batch + largest subtree)` is the right one.
#[test]
fn peak_follows_the_largest_subtree() {
    const SMALL: usize = 4 << 20;
    const LARGE: usize = 16 << 20;
    let lock = exclusive();

    let mut peaks = Vec::new();
    for (tag, huge) in [("subtree4", SMALL), ("subtree16", LARGE)] {
        let fixture = statement(tag, 200, &|i| {
            if i == 0 {
                padded(i, huge)
            } else {
                ordinary(i)
            }
        });
        let (rows, peak) = measure(&lock, || scan(&[fixture.arg()]));
        report(&format!("one {} entry", mib(huge)), &fixture, rows, &peak);
        assert_eq!(rows, fixture.entries);
        assert!(
            peak.heap >= huge,
            "the subtree is copied and deserialized, so the peak cannot be under its {}: {}",
            mib(huge),
            mib(peak.heap)
        );
        // Copy buffer (doubling as it grows), the deserialized string, and the
        // row's own copy, all live at once. Measured at ~6x; the ceiling leaves
        // room for one more copy and none for a second subtree.
        assert!(
            peak.heap <= huge * 8,
            "a {} subtree cost {}, more than eight copies of itself",
            mib(huge),
            mib(peak.heap)
        );
        peaks.push(peak.heap);
    }

    // Four times the subtree, about four times the peak: the dependence is
    // linear in the subtree, not in the file, which is 200 entries either way.
    let (small_peak, large_peak) = (peaks[0], peaks[1]);
    assert!(
        large_peak >= small_peak * 3,
        "quadrupling the subtree moved the peak only from {} to {}",
        mib(small_peak),
        mib(large_peak)
    );
}

/// The parallel scan multiplies the bound by the worker count and the channel
/// depth, not by the size of the glob: three times the corpus, same workers,
/// same peak.
#[test]
fn parallel_peak_follows_threads_not_corpus() {
    const THREADS: usize = 8;
    let lock = exclusive();
    let fixtures: Vec<Fixture> = (0..24)
        .map(|n| statement(&format!("par{n}"), 2_500, &ordinary))
        .collect();
    let eight: Vec<String> = fixtures[..THREADS].iter().map(Fixture::arg).collect();
    let all: Vec<String> = fixtures.iter().map(Fixture::arg).collect();
    let corpus: u64 = fixtures.iter().map(|f| f.bytes).sum();

    let (eight_rows, eight_peak) = measure(&lock, || scan_parallel(eight, THREADS));
    let (all_rows, all_peak) = measure(&lock, || scan_parallel(all, THREADS));
    println!(
        "[membound] 8 files, {THREADS} workers: {eight_rows} rows -> {eight_peak}\n\
         [membound] 24 files ({}), {THREADS} workers: {all_rows} rows -> {all_peak}",
        on_disk(corpus)
    );

    assert_eq!(eight_rows, THREADS * 2_500);
    assert_eq!(all_rows, 24 * 2_500);

    // A batch in flight per worker, twice that queued in the bounded channel,
    // and one in the consumer's hand: the glob's length is not in the formula.
    let inflight = THREADS * 3 + 1;
    assert!(
        all_peak.heap <= eight_peak.heap + inflight * STEADY_HEAP_CEILING / 4,
        "tripling the corpus moved the peak from {} to {}",
        mib(eight_peak.heap),
        mib(all_peak.heap)
    );
    assert!(
        all_peak.heap <= inflight * STEADY_HEAP_CEILING,
        "parallel peak {} is over the {} × batch ceiling",
        mib(all_peak.heap),
        inflight
    );
}

/// Every other fixture here is generated, and generated entries are uniform by
/// construction — one shape, one width, one element order. This one cycles
/// verbatim `<Ntry>` subtrees out of the real corpus, so the bound is measured
/// on the shapes the readers were actually written against.
#[test]
fn peak_is_bounded_on_real_entry_shapes() {
    let lock = exclusive();
    let fixture = corpus_statement("corpus", 20_000);

    let (rows, peak) = measure(&lock, || scan(&[fixture.arg()]));
    report("real corpus shapes", &fixture, rows, &peak);

    assert_eq!(rows, fixture.entries, "the fixture must actually parse");
    assert!(
        peak.heap <= STEADY_HEAP_CEILING,
        "real entry shapes cost {}, over the {} ceiling that generated ones stay under",
        mib(peak.heap),
        mib(STEADY_HEAP_CEILING)
    );
}

/// The documented statement, reproduced: three million entries, 1.7 GB on disk.
/// Ignored by default — it writes 1.7 GB to the temp directory and takes
/// minutes in a debug build:
///
/// ```text
/// cargo test --release membound -- --ignored --nocapture
/// ```
///
/// `QUACKISO_MEMBOUND_ENTRIES` scales the statement down for a run that has to
/// fit somewhere smaller — CI, where the point is that the measurement runs at
/// all. The 1.7 GB shape check applies only at the documented count, because
/// only that count is what README.md quotes.
#[test]
#[ignore = "writes a 1.7 GB fixture; run it deliberately"]
fn the_documented_statement() {
    const DOCUMENTED: usize = 3_000_000;
    let entries = match std::env::var("QUACKISO_MEMBOUND_ENTRIES") {
        Ok(text) => text
            .trim()
            .parse()
            .expect("QUACKISO_MEMBOUND_ENTRIES is an entry count"),
        Err(_) => DOCUMENTED,
    };

    let lock = exclusive();
    let fixture = statement("documented", entries, &ordinary);
    if entries == DOCUMENTED {
        assert!(
            (1_600_000_000..1_800_000_000).contains(&fixture.bytes),
            "the fixture is meant to be the documented 1.7 GB shape, it is {} bytes",
            fixture.bytes
        );
    }

    let (rows, peak) = measure(&lock, || scan(&[fixture.arg()]));
    report("the documented statement", &fixture, rows, &peak);

    assert_eq!(rows, entries);
    assert!(
        peak.heap <= STEADY_HEAP_CEILING,
        "{} cost {} of live heap, over the {} ceiling",
        on_disk(fixture.bytes),
        mib(peak.heap),
        mib(STEADY_HEAP_CEILING)
    );
    if let Some(rss) = peak.rss {
        assert!(
            rss <= STEADY_RSS_CEILING,
            "{} cost {} of resident memory, over the {} ceiling",
            on_disk(fixture.bytes),
            mib(rss),
            mib(STEADY_RSS_CEILING)
        );
    }
}
