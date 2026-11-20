//! HTTP server for the calculator.
//!
//! Environment:
//! - `CFCALC_ADDR`       listen address (default `0.0.0.0:8080`);
//! - `CFCALC_INDICATORS` default indicator histories, `CODE=FILE[,CODE=FILE]`
//!   (default `CBKEY1M=data/cbkey1m.csv`);
//! - `CFCALC_THREADS`    worker threads per request (default: all cores).

use cashflow::domain::indicators::IndicatorHistory;
use cashflow::infrastructure::csv_indicators::load_indicator_csv;
use cashflow::interfaces::http::{router, AppState};
use std::path::Path;
use std::sync::Arc;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.into())
}

#[tokio::main]
async fn main() {
    let addr = env_or("CFCALC_ADDR", "0.0.0.0:8080");
    let spec = env_or("CFCALC_INDICATORS", "CBKEY1M=data/cbkey1m.csv");
    let threads = std::env::var("CFCALC_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        });

    let mut indicators = IndicatorHistory::new();
    for item in spec.split(',').filter(|s| !s.trim().is_empty()) {
        let Some((code, path)) = item.split_once('=') else {
            eprintln!("CFCALC_INDICATORS expects CODE=FILE, got {item:?}");
            std::process::exit(2);
        };
        if let Err(e) = load_indicator_csv(&mut indicators, code.trim(), Path::new(path.trim())) {
            eprintln!("cannot load indicator {code}: {e}");
            std::process::exit(2);
        }
    }

    let app = router(AppState {
        indicators: Arc::new(indicators),
        threads,
    });
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot bind {addr}: {e}");
            std::process::exit(2);
        }
    };
    eprintln!("cfcalc-api listening on {addr} (threads={threads})");
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
    {
        eprintln!("server error: {e}");
        std::process::exit(1);
    }
}
