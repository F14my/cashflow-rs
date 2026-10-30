use cashflow::curve_store::CurveStore;
use cashflow::curves::Compounding;
use std::path::PathBuf;

const USAGE: &str = "\
curve_admin — хранилище и история загрузок кривых

Команды:
  curve_admin init --db FILE
  curve_admin import --db FILE --file FILE [--mode manual|automatic]
                     [--source NAME] [--requested-date YYYY-MM-DD]
                     [--compounding continuous|annual]
  curve_admin history --db FILE [--limit N]
  curve_admin curves --db FILE
";

fn value(args: &[String], name: &str) -> Result<String, String> {
    let index = args
        .iter()
        .position(|arg| arg == name)
        .ok_or_else(|| format!("{name} is required"))?;
    args.get(index + 1)
        .cloned()
        .ok_or_else(|| format!("{name} requires a value"))
}

fn optional_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        return Err(USAGE.into());
    };
    if matches!(command, "-h" | "--help") {
        print!("{USAGE}");
        return Ok(());
    }
    let db = PathBuf::from(value(&args, "--db")?);
    let mut store = CurveStore::open(&db).map_err(|error| error.to_string())?;
    match command {
        "init" => println!("initialized {}", db.display()),
        "import" => {
            let file = PathBuf::from(value(&args, "--file")?);
            let mode = optional_value(&args, "--mode").unwrap_or_else(|| "manual".into());
            let source = optional_value(&args, "--source").unwrap_or_else(|| "FILE".into());
            let requested_date = optional_value(&args, "--requested-date");
            let compounding =
                optional_value(&args, "--compounding").unwrap_or_else(|| "continuous".into());
            let compounding =
                Compounding::parse(&compounding).map_err(|error| error.to_string())?;
            let run_id = store
                .import_csv(
                    &file,
                    compounding,
                    &mode,
                    &source,
                    requested_date.as_deref(),
                )
                .map_err(|error| error.to_string())?;
            println!("load run {run_id}: SUCCESS");
        }
        "history" => {
            let limit = optional_value(&args, "--limit")
                .map(|value| {
                    value
                        .parse::<usize>()
                        .map_err(|_| "bad --limit".to_string())
                })
                .transpose()?
                .unwrap_or(20);
            println!(
                "id;requested_date;mode;source;started_at;finished_at;status;curves;points;error"
            );
            for run in store.history(limit).map_err(|error| error.to_string())? {
                println!(
                    "{};{};{};{};{};{};{};{};{};{}",
                    run.id,
                    run.requested_date.as_deref().unwrap_or(""),
                    run.mode,
                    run.source,
                    run.started_at,
                    run.finished_at.as_deref().unwrap_or(""),
                    run.status,
                    run.curves_loaded,
                    run.points_loaded,
                    run.error_message.as_deref().unwrap_or("")
                );
            }
        }
        "curves" => {
            println!(
                "id;curve_type;curve_date;currency;scenario;compounding;source;loaded_at;points"
            );
            for curve in store.curves().map_err(|error| error.to_string())? {
                println!(
                    "{};{};{};{};{};{};{};{};{}",
                    curve.id,
                    curve.curve_type,
                    curve.curve_date,
                    curve.currency,
                    curve.scenario,
                    curve.compounding,
                    curve.source,
                    curve.loaded_at,
                    curve.point_count
                );
            }
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
