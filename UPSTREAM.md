# Upstream provenance and maintenance

This repository derives directly from the public [LastExceed/azo](https://github.com/LastExceed/azo) history at commit [`46a8473910414fc610fa46f672172de3ee769ef5`](https://github.com/LastExceed/azo/commit/46a8473910414fc610fa46f672172de3ee769ef5). It is independently maintained by DnucleusZ. The upstream MIT notices and original authorship remain intact.

The upstream baseline includes the ASIO layout corrections following tag `azo/v0.5.2`. Its package versions are `azo 0.5.2` and `azo-sys 0.3.2`; those version labels alone do not identify the later layout changes. Downstream packages use `0.6.0-dev.1` and an exact Git revision.

The raw interface, structures, registry metadata and Windows binding generation retain their upstream origins. Downstream changes restrict the target to Windows x64 MSVC, correct callback mutability and optionality, distinguish ordinary and future success codes, and add audited layout and raw-value tests. The external SDK test contains the test-side invocation protocol only; SDK headers and the native oracle are not distributed here.

The exclusive owner, initialized response checks and registration-retention contracts are downstream work developed with agent assistance. Relevant downstream regression fixtures were developed with agent assistance and reused after source review. They validate observable driver behavior and ownership boundaries rather than implementation layout.

The public API supports one control owner, required device queries, three typed IO-format operations and explicit registration/start/stop lifetime. The upstream Host/Proxy implementation and registry-publishing workflows are removed from this product scope. Changes are reviewed as direct APIs; no legacy API aliases or hidden fallback paths are provided.

Future upstream updates require a new fixed source revision and review of ABI, return-code, ownership and callback changes. Tests and measurements retain their actual source and build identities. Software tests, external SDK round trips and physical device observations are recorded separately.
