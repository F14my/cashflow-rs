use cashflow::curve_store::CurveStore;
use cashflow::curves::{Compounding, CurveSet};
use cashflow::discounting::discount_cashflow;
use cashflow::indicators::IndicatorHistory;
use cashflow::parser::for_each_deal_chunk;
use cashflow::{calculate_portfolio, CalcConfig, DealSummary};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::Instant;

const USAGE: &str = "\
cfcalc — расчёт и дисконтирование денежных потоков

Использование:
  cfcalc (--discount-curve FILE | --curve-db FILE) [--deals FILE]
         [--curve-type NAME] [--scenario NAME] [--indicator CODE=FILE]...
         [--discount-compounding continuous|annual] [--deal-id 1,16,29]
         [--out FILE] [--summary FILE] [--threads N] [--chunk-size N]
         [--act-act-as-365]

  --deals FILE          CSV сделок (`;`), по умолчанию data/deals.csv
  --indicator CODE=FILE история индикатора effective_date;rate_percent
                        (по умолчанию CBKEY1M=data/cbkey1m.csv)
  --discount-curve FILE узлы curve_date;currency;tenor_days;zero_rate_percent
  --curve-db FILE        SQLite-хранилище, подготовленное curve_admin
  --curve-type NAME      тип кривой (по умолчанию G_CURVE)
  --scenario NAME        сценарий (по умолчанию BASE)
  --discount-compounding continuous или annual (по умолчанию continuous)
  --deal-id LIST        считать только эти сделки
  --out FILE            CSV с потоками (по умолчанию stdout)
  --summary FILE        CSV с итогами по сделкам
  --threads N           число потоков (по умолчанию — все ядра)
  --chunk-size N        сделок в одном чанке (по умолчанию 10000)
  --act-act-as-365      считать ACT/ACT как ACT/365 (как в эталонном Excel)
";

struct Args {
    deals: PathBuf,
    indicators: Vec<(String, PathBuf)>,
    discount_curve: Option<PathBuf>,
    curve_db: Option<PathBuf>,
    curve_type: String,
    scenario: String,
    compounding: Compounding,
    deal_ids: Option<HashSet<i64>>,
    out: Option<PathBuf>,
    summary: Option<PathBuf>,
    threads: usize,
    chunk_size: usize,
    cfg: CalcConfig,
}

fn parse_args() -> Result<Args, String> {
    let mut deals = PathBuf::from("data/deals.csv");
    let mut indicators = Vec::new();
    let mut discount_curve = None;
    let mut curve_db = None;
    let mut curve_type = String::from("G_CURVE");
    let mut scenario = String::from("BASE");
    let mut compounding = Compounding::Continuous;
    let mut deal_ids = None;
    let mut out = None;
    let mut summary = None;
    let mut threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let mut chunk_size = 10_000;
    let mut cfg = CalcConfig::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = |name: &str| it.next().ok_or(format!("{name} requires a value"));
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--deals" => deals = val("--deals")?.into(),
            "--indicator" => {
                let value = val("--indicator")?;
                let (code, path) = value
                    .split_once('=')
                    .ok_or("--indicator expects CODE=FILE")?;
                indicators.push((code.to_string(), path.into()));
            }
            "--discount-curve" => discount_curve = Some(val("--discount-curve")?.into()),
            "--curve-db" => curve_db = Some(val("--curve-db")?.into()),
            "--curve-type" => curve_type = val("--curve-type")?,
            "--scenario" => scenario = val("--scenario")?,
            "--discount-compounding" => {
                compounding = Compounding::parse(&val("--discount-compounding")?)
                    .map_err(|e| e.to_string())?
            }
            "--deal-id" => {
                let ids = val("--deal-id")?
                    .split(',')
                    .map(|s| {
                        s.trim()
                            .parse::<i64>()
                            .map_err(|_| format!("bad deal id {s:?}"))
                    })
                    .collect::<Result<HashSet<_>, _>>()?;
                deal_ids = Some(ids);
            }
            "--out" => out = Some(val("--out")?.into()),
            "--summary" => summary = Some(val("--summary")?.into()),
            "--threads" => threads = val("--threads")?.parse().map_err(|_| "bad --threads")?,
            "--chunk-size" => {
                chunk_size = val("--chunk-size")?
                    .parse()
                    .map_err(|_| "bad --chunk-size")?
            }
            "--act-act-as-365" => cfg.act_act_as_365 = true,
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
        }
    }
    if threads == 0 || chunk_size == 0 {
        return Err("--threads and --chunk-size must be greater than zero".into());
    }
    if indicators.is_empty() {
        indicators.push(("CBKEY1M".into(), "data/cbkey1m.csv".into()));
    }
    if discount_curve.is_some() == curve_db.is_some() {
        return Err(format!(
            "specify exactly one of --discount-curve or --curve-db\n\n{USAGE}"
        ));
    }
    Ok(Args {
        deals,
        indicators,
        discount_curve,
        curve_db,
        curve_type,
        scenario,
        compounding,
        deal_ids,
        out,
        summary,
        threads,
        chunk_size,
        cfg,
    })
}

fn output_writer(path: &Option<PathBuf>) -> Result<Box<dyn Write>, String> {
    match path {
        Some(path) => std::fs::File::create(path)
            .map(|file| Box::new(BufWriter::new(file)) as Box<dyn Write>)
            .map_err(|e| format!("{}: {e}", path.display())),
        None => Ok(Box::new(BufWriter::new(std::io::stdout()))),
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let started = Instant::now();
    let mut indicators = IndicatorHistory::new();
    for (code, path) in &args.indicators {
        indicators
            .load_percent_csv(code, path)
            .map_err(|e| e.to_string())?;
    }
    let curves = match (&args.discount_curve, &args.curve_db) {
        (Some(path), None) => CurveSet::load_percent_csv(path, args.compounding),
        (None, Some(path)) => CurveStore::open(path).and_then(|store| store.load_curve_set()),
        _ => unreachable!(),
    }
    .map_err(|e| e.to_string())?;

    let mut output = output_writer(&args.out)?;
    output
        .write_all(b"deal_id;cashflow_date;currency;interest_period_start;interest_period_end;principal_cf;interest_cf;total_cf;balance_before;balance_after;curve_date;discount_time;zero_rate;discount_factor;discounted_principal_cf;discounted_interest_cf;discounted_total_cf\n")
        .map_err(|e| e.to_string())?;
    let mut summary_output = args
        .summary
        .as_ref()
        .map(|path| {
            std::fs::File::create(path)
                .map(BufWriter::new)
                .map_err(|e| format!("{}: {e}", path.display()))
        })
        .transpose()?;
    if let Some(writer) = summary_output.as_mut() {
        writer
            .write_all(b"deal_id;principal;interest;total_cf;pv_principal;pv_interest;npv\n")
            .map_err(|e| e.to_string())?;
    }

    let mut found = HashSet::new();
    let mut total = DealSummary::default();
    let mut deal_count = 0usize;
    let mut ok_count = 0usize;
    let mut errors = 0usize;
    let mut line = String::with_capacity(512);

    for_each_deal_chunk(&args.deals, args.chunk_size, |mut deals| {
        if let Some(wanted) = &args.deal_ids {
            deals.retain(|deal| {
                let keep = wanted.contains(&deal.deal_id);
                if keep {
                    found.insert(deal.deal_id);
                }
                keep
            });
        }
        deal_count += deals.len();
        let results = calculate_portfolio(&deals, &indicators, &args.cfg, args.threads);
        for (result, deal) in results.into_iter().zip(&deals) {
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    errors += 1;
                    eprintln!("ERROR {error}");
                    continue;
                }
            };
            let curve = match curves.curve_for(
                &args.curve_type,
                &deal.currency,
                deal.report_date,
                &args.scenario,
            ) {
                Ok(curve) => curve,
                Err(error) => {
                    errors += 1;
                    eprintln!("ERROR deal_id={}: {error}", deal.deal_id);
                    continue;
                }
            };
            let discounted = match result
                .cashflows
                .into_iter()
                .map(|cashflow| discount_cashflow(cashflow, deal.report_date, curve))
                .collect::<cashflow::Result<Vec<_>>>()
            {
                Ok(value) => value,
                Err(error) => {
                    errors += 1;
                    eprintln!("ERROR deal_id={}: {error}", deal.deal_id);
                    continue;
                }
            };
            let mut summary = result.summary;
            for discounted_cf in discounted {
                summary.pv_principal += discounted_cf.discounted_principal_cf;
                summary.pv_interest += discounted_cf.discounted_interest_cf;
                summary.npv += discounted_cf.discounted_total_cf;
                line.clear();
                write_cashflow(&mut line, &discounted_cf);
                output
                    .write_all(line.as_bytes())
                    .map_err(|e| cashflow::Error::new(e.to_string()))?;
            }
            if let Some(writer) = summary_output.as_mut() {
                line.clear();
                write_summary(&mut line, &summary);
                writer
                    .write_all(line.as_bytes())
                    .map_err(|e| cashflow::Error::new(e.to_string()))?;
            }
            total.principal += summary.principal;
            total.interest += summary.interest;
            total.total_cf += summary.total_cf;
            total.pv_principal += summary.pv_principal;
            total.pv_interest += summary.pv_interest;
            total.npv += summary.npv;
            ok_count += 1;
        }
        output
            .flush()
            .map_err(|e| cashflow::Error::new(e.to_string()))?;
        Ok(())
    })
    .map_err(|e| e.to_string())?;

    if let Some(wanted) = &args.deal_ids {
        let mut missing: Vec<_> = wanted.difference(&found).copied().collect();
        if !missing.is_empty() {
            missing.sort_unstable();
            return Err(format!("deal_id values not found: {missing:?}"));
        }
    }
    if let Some(writer) = summary_output.as_mut() {
        line.clear();
        line.push_str("TOTAL;");
        write_summary_values(&mut line, &total);
        line.push('\n');
        writer
            .write_all(line.as_bytes())
            .map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
    }
    output.flush().map_err(|e| e.to_string())?;
    if args.out.is_some() && args.summary.is_none() {
        println!(
            "TOTAL principal={:.2} interest={:.2} total_cf={:.2} pv_principal={:.2} pv_interest={:.2} npv={:.2}",
            total.principal, total.interest, total.total_cf, total.pv_principal, total.pv_interest, total.npv
        );
    }
    eprintln!(
        "deals: {deal_count}  ok: {ok_count}  errors: {errors}  total: {:.1} ms  threads: {}  chunk: {}",
        started.elapsed().as_secs_f64() * 1e3,
        args.threads,
        args.chunk_size
    );
    if errors > 0 {
        return Err(format!("{errors} deal(s) failed"));
    }
    Ok(())
}

fn write_cashflow(line: &mut String, discounted: &cashflow::DiscountedCashFlow) {
    let cf = &discounted.cashflow;
    let _ = writeln!(
        line,
        "{};{};{};{};{};{:.2};{:.2};{:.2};{:.2};{:.2};{};{:.10};{:.10};{:.12};{:.2};{:.2};{:.2}",
        cf.deal_id,
        cf.cashflow_date,
        cf.currency,
        cf.interest_period_start,
        cf.interest_period_end,
        cf.principal_cf,
        cf.interest_cf,
        cf.total(),
        cf.balance_before,
        cf.balance_after,
        discounted.curve_date,
        discounted.discount_time,
        discounted.zero_rate,
        discounted.discount_factor,
        discounted.discounted_principal_cf,
        discounted.discounted_interest_cf,
        discounted.discounted_total_cf
    );
}

fn write_summary(line: &mut String, summary: &DealSummary) {
    let _ = write!(line, "{};", summary.deal_id);
    write_summary_values(line, summary);
    line.push('\n');
}

fn write_summary_values(line: &mut String, summary: &DealSummary) {
    let _ = write!(
        line,
        "{:.2};{:.2};{:.2};{:.2};{:.2};{:.2}",
        summary.principal,
        summary.interest,
        summary.total_cf,
        summary.pv_principal,
        summary.pv_interest,
        summary.npv
    );
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
