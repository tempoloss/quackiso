# Downloads

Weekly installs of `quackiso` from the DuckDB community-extension repository,
against every community extension in the same week. Source:
`https://community-extensions.duckdb.org/download-stats-weekly/2026/<week>.json`
(one object, extension name → downloads that ISO week) and
`https://community-extensions.duckdb.org/downloads-last-week.json` for the
rolling week.

"Per 100k" is quackiso's share of all community-extension downloads that week,
so it reads interest with platform-wide swings divided out.

| Week (2026) | Ending | quackiso | Rank | Median extension | All extensions | Per 100k |
|---|---|---:|---|---:|---:|---:|
| 32 | 08-09 | 173 | 274 / 297 | 823 | 943,690 | 18.3 |
| 33 | 08-16 | 292 | 261 / 307 | 775 | 964,313 | 30.3 |
| 34 | 08-23 | 337 | 265 / 318 | 806 | 1,036,077 | 32.5 |
| 35 | 08-30 | 334 | 256 / 319 | 660 | 1,053,774 | 31.7 |
| 36 | 09-06 | 390 | 248 / 325 | 722 | 1,018,802 | 38.3 |
| 37 | 09-13 | 458 | 257 / 328 | 793 | 1,076,473 | 42.5 |
| 38 | 09-20 | 528 | 262 / 336 | 817 | 1,112,037 | 47.5 |
| 39 | 09-27 | 640 | 266 / 343 | 933 | 1,109,948 | 57.7 |
| 40 | 10-04 | 744 | 269 / 350 | 1,073 | 1,273,104 | 58.4 |
| 41 | 10-11 | 438 | 280 / 357 | 842 | 1,185,249 | 37.0 |
| 42 | 10-18 | 247 | 316 / 363 | 848 | 1,240,149 | 19.9 |
| 43 | 10-25 | 2,029 | 308 / 388 | 7,760 | 4,238,465 | 47.9 |
| rolling | 10-27 | 3,065 | 309 / 392 | 10,925 | 5,515,342 | 55.6 |

## 2026-10-27: first 3,000-install week

Week 43 is the DuckDB 2.0.0 release (21 October 2026): every user, container
and CI job moving to a new DuckDB re-installs its extensions, and the median
extension went up 8.8x. quackiso went up 8.2x, so the raw jump to 2,029 and then
3,065 is the platform, not the project. The share column is the signal: it fell
from 58 per 100k in week 40 to 20 in week 42, and is back to 56, the September
peak. Every one of those installs was a v1.5.5 build: on this date quackiso has
no build for DuckDB 1.5.6 or 2.0.0, so `INSTALL quackiso FROM community` fails
on the release that caused the wave.
