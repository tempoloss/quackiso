# 2. No `s3://` or `https://` paths in v1

Status: accepted

## Context

The obvious next feature after local files is
`read_iso20022('s3://bucket/statements/*.xml')`. DuckDB already resolves remote
URIs through its `VirtualFileSystem`, honouring the secrets and settings in the
session, so the extension should not need its own HTTP client.

The C extension API does expose that filesystem:

```
duckdb_client_context_get_file_system(context) -> duckdb_file_system
duckdb_file_system_open(fs, path, options, out_handle) -> duckdb_state
duckdb_file_handle_read(handle, buffer, size) -> i64
```

An implementation was written against it: a `Read` adapter over
`duckdb_file_handle`, with the filesystem taken from a client context obtained in
the extension entrypoint (the only place a `duckdb_database` handle is available)
and cached for later scans.

## What actually blocks it

That design fails, and the failure is not a detail of the caching:

```
Invalid Input Error: read_iso20022: cannot open http://127.0.0.1:8999/x.xml:
TransactionContext::ActiveTransaction called without active transaction
```

Opening a remote file resolves secrets, which requires a client context with an
**active transaction**. The extension's own private connection has none. Only the
context of the executing query does, and it is reachable in exactly one way:

```
duckdb_table_function_get_client_context(duckdb_function_info, &out_context)
```

`duckdb_function_info` is the argument DuckDB passes to a table function's scan
callback. With `duckdb-rs` 1.10505.0 it cannot be reached from an implementation
of the safe `VTab` trait:

* `TableFunctionInfo` stores its `ptr: duckdb_function_info` privately and exposes
  only `get_bind_data`, `get_init_data`, `get_extra_info` and `set_error`;
* `DataChunkHandle::new_unowned` — the only way to wrap the output chunk DuckDB
  hands the callback — is `pub(crate)`.

So using the query's context means not using the safe wrapper at all: bind, init
and scan would each become hand-written `extern "C"` callbacks, and the whole
vector-writing layer (string assignment, validity masks, chunk sizing) would be
reimplemented in `unsafe` against the raw C API.

## Decision

Ship local paths and globs. Reject a URI at bind time with a message that names
this document, instead of failing later inside a scan.

Two alternatives were considered and rejected:

* **A bundled HTTP client.** Creates a second source of truth for credentials
  next to DuckDB's secret manager, and still cannot speak S3 auth.
* **Keeping a transaction open on a private connection** to satisfy the secret
  manager. An open read transaction pins a snapshot and blocks checkpointing —
  loading this extension would quietly degrade the user's database.

Hand-rolling ~200 lines of table-function ABI in `unsafe` is a poor trade in a
tool that computes money, where parsing correctness is the property people rely
on. Downloading a statement first is a small inconvenience; a memory-safety bug
in a financial parser is not.

## Consequences

Remote reads stay open, with a known route: reimplement the fifteen table functions
against the raw C API and take the filesystem from
`duckdb_table_function_get_client_context`. That is a self-contained change and
should be its own release, with the local test corpus as its regression net.

If `duckdb-rs` later exposes either the raw `duckdb_function_info` or a public
unowned `DataChunkHandle`, this becomes a small change instead of a rewrite.
