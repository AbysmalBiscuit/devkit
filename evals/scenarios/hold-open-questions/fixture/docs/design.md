# Report exporter

The exporter reads finished reports and writes them as CSV for the finance team.

## Behaviour

- It runs nightly and exports every report finished since the last run.
- It records the time of its last run, so a missed night is caught up on the next.

## Configuration

The exporter needs its output directory and its schedule configured.

## Open questions

- Config format: TOML or YAML?
- Where do logs go?
