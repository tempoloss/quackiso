.PHONY: clean clean_all memory memory_full

PROJ_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

EXTENSION_NAME=quackiso

# duckdb-rs relies on unstable C API functionality, so binaries only work on
# TARGET_DUCKDB_VERSION (forwards compatibility is broken). Same constraint as
# the upstream Rust extension template.
USE_UNSTABLE_C_API=1
TARGET_DUCKDB_VERSION=v1.5.5

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

# The memory boundary. `memory` is the seven bounded-memory tests; `memory_full`
# writes a 1.7 GB statement of three million entries and parses it, which is the
# figure README.md quotes. See src/membound.rs, and scripts/measure_in_duckdb.py
# for the same statement measured inside a running DuckDB.
memory:
	cargo test --lib membound -- --nocapture

memory_full:
	cargo test --release --lib membound -- --ignored --nocapture

clean: clean_build clean_rust
clean_all: clean_configure clean
