.PHONY: clean clean_all memory memory_full sweep

PROJ_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

EXTENSION_NAME=quackiso

# duckdb-rs relies on unstable C API functionality, so binaries only work on
# TARGET_DUCKDB_VERSION (forwards compatibility is broken). Same constraint as
# the upstream Rust extension template.
USE_UNSTABLE_C_API=1
TARGET_DUCKDB_VERSION=v1.5.5

# The pip DuckDB that venv installs is what loads the extension in `make test`
# and in scripts/*.py, and it refuses any build made for another version. The
# base Makefile installs the latest release unless DUCKDB_TEST_VERSION is set,
# so a job that forgot to set it broke the day DuckDB shipped v1.5.6 (the Sweep
# runs of 2026-09-29 and 2026-10-06: "built specifically for DuckDB version
# 'v1.5.5' ... this version of DuckDB is 'v1.5.6'"). Derive the pin here, once,
# so no workflow can drift from the target above; DUCKDB_TEST_VERSION=main in
# the environment still overrides it, to try an upcoming release.
DUCKDB_TEST_VERSION ?= $(patsubst v%,%,$(TARGET_DUCKDB_VERSION))

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
# figure README.md quotes. See src/membound.rs, and scripts/measure_in_duckdb.py
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
