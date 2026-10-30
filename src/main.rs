use cashflow::indicators::IndicatorHistory;
use cashflow::parser::parse_deals;
use cashflow::{calculate_portfolio, CalcConfig, DealSummary};
use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

const USAGE: &str = "\
cfcalc — расчёт платежей по телу и процентам

Использование:
  cfcalc [--deals FILE] [--indicator CODE=FILE]... [--deal-id 1,16,29]
         [--out FILE] [--summary FILE] [--threads N] [--act-act-as-365]

  --deals FILE          CSV сделок (`;`), по умолчанию data/deals.csv
  --indicator CODE=FILE история индикатора effective_date;rate_percent
                        (по умолчанию CBKEY1M=data/cbkey1m.csv)
  --deal-id LIST        считать только эти сделки
  --out FILE            CSV с потоками (по умолчанию stdout)
  --summary FILE        CSV с итогами по сделкам (тело, проценты, поток)
  --threads N           число потоков (по умолчанию — все ядра)
  --act-act-as-365      считать ACT/ACT как ACT/365 (как в эталонном Excel)
";

struct Args {
    deals: PathBuf,
    indicators: Vec<(String, PathBuf)>,
    deal_ids: Option<Vec<i64>>,
    out: Option<PathBuf>,
    summary: Option<PathBuf>,
    threads: usize,
    cfg: CalcConfig,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        deals: "data/deals.csv".into(),
        indicators: vec![],
        deal_ids: None,
        out: None,
        summary: None,
        threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        cfg: CalcConfig::default(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = |name: &str| it.next().ok_or(format!("{name} requires a value"));
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--deals" => a.deals = val("--deals")?.into(),
            "--indicator" => {
                let v = val("--indicator")?;
                let (k, p) = v.split_once('=').ok_or("--indicator expects CODE=FILE")?;
                a.indicators.push((k.to_string(), p.into()));
            }
            "--deal-id" => {
                let ids = val("--deal-id")?
                    .split(',')
                    .map(|s| s.trim().parse::<i64>().map_err(|_| format!("bad deal id {s:?}")))
                    .collect::<Result<Vec<_>, _>>()?;
                a.deal_ids = Some(ids);
            }
            "--out" => a.out = Some(val("--out")?.into()),
            "--summary" => a.summary = Some(val("--summary")?.into()),
            "--threads" => a.threads = val("--threads")?.parse().map_err(|_| "bad --threads")?,
            "--act-act-as-365" => a.cfg.act_act_as_365 = true,
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
        }
    }
    if a.indicators.is_empty() {
        a.indicators.push(("CBKEY1M".into(), "data/cbkey1m.csv".into()));
    }
    Ok(a)
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let t0 = Instant::now();

    let mut deals = parse_deals(&args.deals).map_err(|e| e.to_string())?;
    if let Some(ids) = &args.deal_ids {
        deals.retain(|d| ids.contains(&d.deal_id));
        if let Some(missing) = ids.iter().find(|id| !deals.iter().any(|d| d.deal_id == **id)) {
            return Err(format!("deal_id={missing} not found"));
        }
    }
    let mut hist = IndicatorHistory::new();
    for (code, path) in &args.indicators {
        hist.load_percent_csv(code, path).map_err(|e| e.to_string())?;
    }

    let t_calc = Instant::now();
    let load_time = t_calc.duration_since(t0);
    let results = calculate_portfolio(&deals, &hist, &args.cfg, args.threads);
    let calc_time = t_calc.elapsed();

    let mut buf = String::with_capacity(1 << 20);
    buf.push_str("deal_id;cashflow_date;currency;interest_period_start;interest_period_end;principal_cf;interest_cf;total_cf;balance_before;balance_after\n");

    let mut errors = 0;
    let mut summaries = Vec::new();
    for r in &results {
        match r {
            Err(e) => {
                errors += 1;
                eprintln!("ERROR {e}");
            }
            Ok(res) => {
                for c in &res.cashflows {
                    push_i64(&mut buf, c.deal_id);
                    buf.push(';');
                    push_date(&mut buf, c.cashflow_date);
                    buf.push(';');
                    buf.push_str(&c.currency);
                    buf.push(';');
                    push_date(&mut buf, c.interest_period_start);
                    buf.push(';');
                    push_date(&mut buf, c.interest_period_end);
                    for x in [c.principal_cf, c.interest_cf, c.total(), c.balance_before, c.balance_after] {
                        buf.push(';');
                        push_money(&mut buf, x);
                    }
                    buf.push('\n');
                }
                summaries.push(&res.summary);
            }
        }
    }
    match &args.out {
        Some(p) => std::fs::write(p, &buf).map_err(|e| format!("{}: {e}", p.display()))?,
        None => std::io::stdout().write_all(buf.as_bytes()).map_err(|e| e.to_string())?,
    }

    let mut s = String::from("deal_id;principal;interest;total_cf\n");
    let mut tot = DealSummary::default();
    for d in &summaries {
        let _ = writeln!(s, "{};{:.2};{:.2};{:.2}", d.deal_id, d.principal, d.interest, d.total_cf);
        tot.principal += d.principal;
        tot.interest += d.interest;
        tot.total_cf += d.total_cf;
    }
    let _ = writeln!(s, "TOTAL;{:.2};{:.2};{:.2}", tot.principal, tot.interest, tot.total_cf);
    match &args.summary {
        Some(p) => std::fs::write(p, &s).map_err(|e| format!("{}: {e}", p.display()))?,
        None if args.out.is_some() => print!("{s}"),
        None => {}
    }

    eprintln!(
        "deals: {}  ok: {}  errors: {}  load: {:.1} ms  calc: {:.1} ms  total: {:.1} ms  threads: {}",
        results.len(),
        results.len() - errors,
        errors,
        load_time.as_secs_f64() * 1e3,
        calc_time.as_secs_f64() * 1e3,
        t0.elapsed().as_secs_f64() * 1e3,
        args.threads
    );
    if errors > 0 {
        return Err(format!("{errors} deal(s) failed"));
    }
    Ok(())
}

fn push_u64(buf: &mut String, mut v: u64) {
    let mut tmp = [0u8; 20];
    let mut i = tmp.len();
    loop {
        i -= 1;
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    buf.push_str(std::str::from_utf8(&tmp[i..]).unwrap());
}

fn push_i64(buf: &mut String, v: i64) {
    if v < 0 {
        buf.push('-');
    }
    push_u64(buf, v.unsigned_abs());
}

fn push_money(buf: &mut String, v: f64) {
    let cents = (v * 100.0).round() as i64;
    if cents < 0 {
        buf.push('-');
    }
    let a = cents.unsigned_abs();
    push_u64(buf, a / 100);
    buf.push('.');
    buf.push((b'0' + (a % 100 / 10) as u8) as char);
    buf.push((b'0' + (a % 10) as u8) as char);
}

fn push_2(buf: &mut String, v: u32) {
    buf.push((b'0' + (v / 10) as u8) as char);
    buf.push((b'0' + (v % 10) as u8) as char);
}

fn push_date(buf: &mut String, d: cashflow::dates::Date) {
    use chrono::Datelike;
    let (y, m, dd) = (d.year(), d.month(), d.day());
    push_2(buf, (y / 100) as u32);
    push_2(buf, (y % 100) as u32);
    buf.push('-');
    push_2(buf, m);
    buf.push('-');
    push_2(buf, dd);
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}