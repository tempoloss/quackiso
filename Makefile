.PHONY: clean clean_all memory memory_full sweep

PROJ_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

EXTENSION_NAME=quackiso

# DuckDB 2.0 refuses the exact-version (C_STRUCT_UNSTABLE) binaries duckdb-rs's
# loadable-extension feature produced up to 1.5: such an extension must export
# `quackiso_init_c_api_v2`, which only the duckdb-neo crate emits ("must use the
# entrypoint from duckdb_extension_v2.h"). The v1 C API the `duckdb` crate wraps
# is stable from v1.5.6 on (libduckdb-sys marks every function it uses
# "stable: v1.5.6"), and DuckDB 2.0 serves it for an extension that asks for it
# by that version. So the build is a stable-ABI one: TARGET_DUCKDB_VERSION is the
# C API version the extension needs, not the DuckDB it is built for -- the macro
# requests it (DUCKDB_EXTENSION_MIN_DUCKDB_VERSION in rust.Makefile) and the
# metadata footer records it -- and one binary loads into v1.5.6 and v2.0.x alike.
USE_UNSTABLE_C_API=0
TARGET_DUCKDB_VERSION=v1.5.6

# The pip DuckDB that venv installs is what loads the extension in `make test`
# and in scripts/*.py. The base Makefile installs the latest release for a
# stable-ABI extension unless DUCKDB_TEST_VERSION is set, and a job that forgot to
# set it broke the day DuckDB shipped a release that moved under it (the Sweep
# runs of 2026-09-29 and 2026-10-06, when v1.5.6 arrived). So the pin lives here,
# once, and is the DuckDB the extension is released for; DUCKDB_TEST_VERSION=main
# in the environment still overrides it, to try an upcoming release.
DUCKDB_TEST_VERSION ?= 2.0.0

all: configure debug

# Makefiles vendored from DuckDB via the extension-ci-tools submodule.
include extension-ci-tools/makefiles/c_api_extensions/base.Makefile
include extension-ci-tools/makefiles/c_api_extensions/rust.Makefile

configure: venv platform extension_version

debug: build_extension_library_debug build_extension_with_metadata_debug
release: build_extension_library_release build_extension_with_metadata_release

test: test_debug
test_debug: test_extension_debug
test_release: test_extension_release

# The memory boundary. `memory` is the eight bounded-memory tests; `memory_full`
# writes a 1.7 GB statement of three million entries and parses it, which is the
# figure docs/streaming.md quotes. See src/membound.rs, and scripts/measure_in_duckdb.py
# for the same statement measured inside a running DuckDB.
memory:
	cargo test --lib membound -- --nocapture

memory_full:
	cargo test --release --lib membound -- --ignored --nocapture

# The foreign-corpus sweep: other projects' ISO 20022 samples through every reader,
# failing on a crash, a changed outcome, or zero rows with no error. Needs `make debug`
# first, and a network for --fetch. See scripts/sweep_foreign_corpora.py.
sweep:
	configure/venv/bin/python3 scripts/sweep_foreign_corpora.py --fetch
	cd tools/mxgen && cargo run --release -- \
	  --scenarios ../../target/foreign-corpus/static/mx-message-3.1.4/test_scenarios \
	  --out ../../target/foreign-corpus/generated
	cd tools/mtgen && cargo run --release -- \
	  --scenarios ../../target/foreign-corpus/static/swift-mt-message-3.1.5/test_scenarios \
	  --out ../../target/foreign-corpus/generated
	configure/venv/bin/python3 scripts/sweep_foreign_corpora.py

clean: clean_build clean_rust
clean_all: clean_configure clean
