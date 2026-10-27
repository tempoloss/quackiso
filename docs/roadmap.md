# Roadmap

- Remote paths, once the blocker in ADR 0002 is resolved.
- Archive members, now that an archive is named instead of misparsed. A member
  reader needs a grain of its own: `source_file` would have to say which member
  of which archive, and a glob over archives would have to say both. That is a
  design, not an extension of an existing function.
- MT address lines as reader columns. `audit_addresses` reads them, so the parse
  exists; what is missing is `read_mt103` exposing the lines it drops beside the
  name it keeps. The audit answers the migration question without them, which is
  why this is a roadmap item and not a defect.
- Within-file parallelism is **not** on the roadmap: XML has no safe split
  points, so the parallel unit is the file, and that is already built.
