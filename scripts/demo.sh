#!/usr/bin/env bash
set -euo pipefail

mkdir -p out
rm -f out/demo_curves.db out/demo_cashflows.csv out/demo_summary.csv

cargo run --release --bin curve_admin -- init --db out/demo_curves.db
cargo run --release --bin curve_admin -- import \
  --db out/demo_curves.db \
  --file data/demo_curves.csv \
  --mode manual \
  --source DEMO_FILE \
  --requested-date 2022-08-29
cargo run --release --bin curve_admin -- import \
  --db out/demo_curves.db \
  --file data/demo_curves.csv \
  --mode automatic \
  --source DEMO_SCHEDULER \
  --requested-date 2022-08-30
cargo run --release --bin curve_admin -- import \
  --db out/demo_curves.db \
  --file data/demo_curves.csv \
  --mode automatic \
  --source DEMO_SCHEDULER \
  --requested-date 2022-08-31
cargo run --release --bin curve_admin -- history --db out/demo_curves.db
cargo run --release --bin curve_admin -- curves --db out/demo_curves.db

cargo run --release --bin cfcalc -- \
  --deals data/deals.csv \
  --indicator CBKEY1M=data/cbkey1m.csv \
  --curve-db out/demo_curves.db \
  --curve-type G_CURVE \
  --scenario BASE \
  --act-act-as-365 \
  --out out/demo_cashflows.csv \
  --summary out/demo_summary.csv

echo "Created out/demo_curves.db, out/demo_cashflows.csv and out/demo_summary.csv"
