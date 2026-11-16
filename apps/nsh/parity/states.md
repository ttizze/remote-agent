# Conversation presence comparison

`components.toml` is the checklist derived from the official T3 Code conversation
source. It records the source revision, component locations, related renderers,
and every named child element, operation, and conditional display to check.

Run `scripts/dev-env.sh just nsh-parity --catalog` to render the complete list to
`target/nsh-parity/components.md` without building or launching either client.
The source links point to the recorded revision.

The same command joins `implementation.toml` to the inventory and writes
`target/nsh-parity/implementation.md`. It lists code availability in shared Rust
and in Mac, iPhone, and Windows UI, concrete missing elements, source evidence,
and the next implementation step. Every component must have exactly one
assessment; a missing assessment or evidence file fails generation. NSH/Win32
code belongs to the UI columns even though it is written in Rust. Code presence
does not establish runtime acceptance.

`map.toml` supplies the capture cases and element selectors. Its state count is
not the inventory of T3 components. A subset of captures cannot establish parity
for the whole checklist. Use the source-derived list when adding capture cases;
keep missing implementations and cases that cannot be captured visible.

`scripts/dev-env.sh just nsh-parity` runs every mapped capture case. The default
compares element presence. `--appearance` additionally checks visual metrics;
`--compare RUN` reuses captures and `--t3 RUN` recaptures the T3 side. The report
records comparison failures and capture failures separately, and never treats
missing selectors on both clients as a match.
