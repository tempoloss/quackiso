# 13. Function help is a table function

Status: accepted

Decided with the 1.9.0 release, with the change that added
`quackiso_functions()`. It answers issue #11, which asked that `duckdb_functions()`
describe the functions this extension registers.

## Context

quackiso registers forty-three functions, and a person or an agent that has
loaded it can learn their names and nothing else from the catalog:

```sql
SELECT function_name, description, examples, function_type
FROM duckdb_functions() WHERE function_name = 'read_pacs008';
```

```text
read_pacs008    NULL    []    table
```

`description` and `examples` are columns of `duckdb_functions()`, and DuckDB's own
functions fill them (`range` reports a sentence and `range(2, 5, 3)`). An
extension's table function cannot, because of what the C API it is registered
through offers. `libduckdb-sys` 1.20000.0, the version `duckdb-rs` pins, lists the
setters of a table function in `bindgen_bundled_version_loadable.rs`:
`duckdb_table_function_set_name`, `_add_parameter`, `_add_named_parameter`,
`_set_extra_info`, `_set_bind`, `_set_init`, `_set_local_init`, `_set_function` and
`_supports_projection_pushdown`. There is no description, no example and no name
for a positional parameter.

The two catalog routes left are closed as well. `COMMENT ON FUNCTION` on a loaded
quackiso function fails on DuckDB 2.0.0, because it looks for a macro:

```text
Catalog Error: Macro Function with name read_pacs008 does not exist!
```

and on a built-in it fails with `Can not comment on System Catalog entries`. A
macro takes a comment and a description, and a macro is an entry in the catalog of
the database the extension is loaded into: `CREATE MACRO m1(x) AS x+1` in a file
database is still there after the file is reopened, listed by `duckdb_functions()`
with `internal = false`. The connection an extension gets in its entrypoint is a
connection to that database, so a macro created over it is written into the user's
catalog and persisted with it.

## Decision

A table function, `quackiso_functions()`. It takes no argument and returns one row
per function the extension registers, itself included:

```text
function_name   VARCHAR   the name to call
description     VARCHAR   one sentence
example         VARCHAR   one runnable SELECT
```

The rows are the constant `catalog::FUNCTIONS` in `src/catalog.rs`. A description
is the opening sentence of the function's section in `docs/reference.md`, or its
row in `docs/functions.md` when the section opens with a column list. An example
is a single `SELECT * FROM <function>('<path>')` with a generic path in the style
of `community-extension/description.yml` (`statements/*.xml`, `payments/*.xml`,
`mt/*.txt`); `quackiso_functions` takes none. The function is written by hand and
not with `table_function!`, which generates a path, a file list and a worker
count, and this function has no file.

The catalogue is kept honest by two checks:

- `cargo test` compares the names in `FUNCTIONS` with the names
  `extension_entrypoint` registers, read off its source, so a function added to
  one and not the other fails before anything is built;
- `scripts/check_hello_world.py` loads the built extension and requires the set of
  names LOAD adds to `duckdb_functions()` to equal the set `quackiso_functions()`
  lists, then runs every `example` through the same fixture routing it applies to
  the descriptor's examples and requires rows. A stale example, a path that no
  fixture serves and a function with no row each fail the build with the name in
  the message.

## Alternatives rejected

**One macro per function carrying the comment.** The only catalog entry that
takes a description today. It writes into the catalog of whoever loads the
extension, forty-three entries that outlive the session in a file database and
that the user did not ask for, and each one a second function beside the real
one: DuckDB 2.0.0 accepts `CREATE MACRO read_pacs008(x) AS x` with the extension
loaded, so the help would be a scalar macro of the same name as a table function.
Help that has to be removed from a database it was written into is worse than no
help.

**Waiting for the C API.** `duckdb-rs` exposes what the C API has, and the C API
has not got this. Until it has, a person or an agent calling
`duckdb_functions()` finds a name and nothing to read, and the issue stays a wish.
A function that answers now and is replaced later costs one table.

**The documentation alone.** `docs/functions.md` and `docs/reference.md` are right
and are not where a session that has only the extension installed is looking.
The point of the table is that it is in the same place as the function it
describes, and that a build fails when it drifts, which a page cannot do.

## Consequences

`quackiso_functions()` is the forty-third function, and the count in the
descriptor, `docs/functions.md` and the README moves with it. Adding a function
now takes a row in `catalog::FUNCTIONS` as well as the registration, and the
build says which of the two is missing.

The text of a description is a second copy of a sentence in the documentation. A
copy can drift, and nothing checks the prose against the pages; the check is that
the name, the example and the row exist, which is what a caller meets first. The
sentence was chosen to be the one the page opens with so that the two are edited
together.

When `duckdb-rs` exposes a description and an example for a table function, the
same `FUNCTIONS` table feeds them at registration, and `quackiso_functions()`
stays: it lists every function in one query, which a column on
`duckdb_functions()` read per name does not, and callers that already use it keep
working.
