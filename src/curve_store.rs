use crate::curves::{Compounding, CurvePoint, CurveProvider, CurveSet, ZeroCurve};
use crate::dates::parse_date;
use crate::error::{Error, Result};
use rusqlite::{params, Connection};
use std::path::Path;

fn sql_error(error: rusqlite::Error) -> Error {
    Error::new(format!("curve database: {error}"))
}

fn compounding_name(value: Compounding) -> &'static str {
    match value {
        Compounding::Continuous => "continuous",
        Compounding::Annual => "annual",
    }
}

#[derive(Clone, Debug)]
pub struct LoadRun {
    pub id: i64,
    pub requested_date: Option<String>,
    pub mode: String,
    pub source: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub curves_loaded: i64,
    pub points_loaded: i64,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug)]
pub struct StoredCurve {
    pub id: i64,
    pub curve_type: String,
    pub curve_date: String,
    pub currency: String,
    pub scenario: String,
    pub compounding: String,
    pub source: String,
    pub loaded_at: String,
    pub point_count: i64,
}

pub struct CurveStore {
    connection: Connection,
}

impl CurveStore {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path).map_err(sql_error)?;
        let store = Self { connection };
        store.initialize()?;
        Ok(store)
    }

    pub fn initialize(&self) -> Result<()> {
        self.connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE IF NOT EXISTS curve_sets (
                     id INTEGER PRIMARY KEY,
                     curve_type TEXT NOT NULL,
                     curve_date TEXT NOT NULL,
                     currency TEXT NOT NULL,
                     scenario TEXT NOT NULL,
                     compounding TEXT NOT NULL,
                     source TEXT NOT NULL,
                     loaded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                     UNIQUE(curve_type, curve_date, currency, scenario)
                 );
                 CREATE TABLE IF NOT EXISTS curve_points (
                     curve_set_id INTEGER NOT NULL REFERENCES curve_sets(id) ON DELETE CASCADE,
                     tenor_days INTEGER NOT NULL,
                     zero_rate REAL NOT NULL,
                     discount_factor REAL NOT NULL,
                     PRIMARY KEY(curve_set_id, tenor_days)
                 );
                 CREATE TABLE IF NOT EXISTS curve_load_runs (
                     id INTEGER PRIMARY KEY,
                     requested_date TEXT,
                     mode TEXT NOT NULL,
                     source TEXT NOT NULL,
                     started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                     finished_at TEXT,
                     status TEXT NOT NULL,
                     curves_loaded INTEGER NOT NULL DEFAULT 0,
                     points_loaded INTEGER NOT NULL DEFAULT 0,
                     error_message TEXT
                 );",
            )
            .map_err(sql_error)
    }

    pub fn import_csv(
        &mut self,
        path: &Path,
        compounding: Compounding,
        mode: &str,
        source: &str,
        requested_date: Option<&str>,
    ) -> Result<i64> {
        let mode = mode.trim().to_ascii_uppercase();
        if !matches!(mode.as_str(), "MANUAL" | "AUTOMATIC") {
            return Err(Error::new("load mode must be MANUAL or AUTOMATIC"));
        }
        let run_id = self
            .connection
            .query_row(
                "INSERT INTO curve_load_runs(requested_date, mode, source, status)
                 VALUES (?1, ?2, ?3, 'RUNNING') RETURNING id",
                params![requested_date, mode, source],
                |row| row.get(0),
            )
            .map_err(sql_error)?;

        let result: Result<(i64, i64)> = (|| {
            let set = CurveSet::load_percent_csv(path, compounding)?;
            let transaction = self.connection.transaction().map_err(sql_error)?;
            let mut curve_count = 0_i64;
            let mut point_count = 0_i64;
            for curve in set.curves() {
                transaction
                    .execute(
                        "INSERT INTO curve_sets(curve_type, curve_date, currency, scenario, compounding, source)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                         ON CONFLICT(curve_type, curve_date, currency, scenario) DO UPDATE SET
                           compounding=excluded.compounding,
                           source=excluded.source,
                           loaded_at=CURRENT_TIMESTAMP",
                        params![
                            curve.curve_type,
                            curve.curve_date.to_string(),
                            curve.currency,
                            curve.scenario,
                            compounding_name(curve.compounding),
                            source,
                        ],
                    )
                    .map_err(sql_error)?;
                let curve_id: i64 = transaction
                    .query_row(
                        "SELECT id FROM curve_sets
                         WHERE curve_type=?1 AND curve_date=?2 AND currency=?3 AND scenario=?4",
                        params![
                            curve.curve_type,
                            curve.curve_date.to_string(),
                            curve.currency,
                            curve.scenario
                        ],
                        |row| row.get(0),
                    )
                    .map_err(sql_error)?;
                transaction
                    .execute("DELETE FROM curve_points WHERE curve_set_id=?1", [curve_id])
                    .map_err(sql_error)?;
                for point in &curve.points {
                    let time = point.tenor_days as f64 / 365.0;
                    let discount_factor = curve.discount_factor(time, point.zero_rate)?;
                    transaction
                        .execute(
                            "INSERT INTO curve_points(curve_set_id, tenor_days, zero_rate, discount_factor)
                             VALUES (?1, ?2, ?3, ?4)",
                            params![curve_id, point.tenor_days, point.zero_rate, discount_factor],
                        )
                        .map_err(sql_error)?;
                    point_count += 1;
                }
                curve_count += 1;
            }
            transaction.commit().map_err(sql_error)?;
            Ok((curve_count, point_count))
        })();

        match result {
            Ok((curves, points)) => {
                self.connection
                    .execute(
                        "UPDATE curve_load_runs SET status='SUCCESS', finished_at=CURRENT_TIMESTAMP,
                         curves_loaded=?1, points_loaded=?2 WHERE id=?3",
                        params![curves, points, run_id],
                    )
                    .map_err(sql_error)?;
                Ok(run_id)
            }
            Err(error) => {
                let _ = self.connection.execute(
                    "UPDATE curve_load_runs SET status='FAILED', finished_at=CURRENT_TIMESTAMP,
                     error_message=?1 WHERE id=?2",
                    params![error.to_string(), run_id],
                );
                Err(error)
            }
        }
    }

    pub fn load_curve_set(&self) -> Result<CurveSet> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, curve_type, curve_date, currency, scenario, compounding
                 FROM curve_sets ORDER BY curve_date, currency, scenario",
            )
            .map_err(sql_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(sql_error)?;
        let mut curves = Vec::new();
        for row in rows {
            let (id, curve_type, date, currency, scenario, compounding) = row.map_err(sql_error)?;
            let mut point_statement = self
                .connection
                .prepare(
                    "SELECT tenor_days, zero_rate FROM curve_points
                     WHERE curve_set_id=?1 ORDER BY tenor_days",
                )
                .map_err(sql_error)?;
            let points = point_statement
                .query_map([id], |row| {
                    Ok(CurvePoint {
                        tenor_days: row.get(0)?,
                        zero_rate: row.get(1)?,
                    })
                })
                .map_err(sql_error)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(sql_error)?;
            curves.push(ZeroCurve::new_named(
                curve_type,
                parse_date(&date)?,
                currency,
                scenario,
                points,
                Compounding::parse(&compounding)?,
            )?);
        }
        CurveSet::from_curves(curves)
    }

    pub fn history(&self, limit: usize) -> Result<Vec<LoadRun>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, requested_date, mode, source, started_at, finished_at, status,
                        curves_loaded, points_loaded, error_message
                 FROM curve_load_runs ORDER BY id DESC LIMIT ?1",
            )
            .map_err(sql_error)?;
        let history = statement
            .query_map([limit as i64], |row| {
                Ok(LoadRun {
                    id: row.get(0)?,
                    requested_date: row.get(1)?,
                    mode: row.get(2)?,
                    source: row.get(3)?,
                    started_at: row.get(4)?,
                    finished_at: row.get(5)?,
                    status: row.get(6)?,
                    curves_loaded: row.get(7)?,
                    points_loaded: row.get(8)?,
                    error_message: row.get(9)?,
                })
            })
            .map_err(sql_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sql_error)?;
        Ok(history)
    }

    pub fn curves(&self) -> Result<Vec<StoredCurve>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT c.id, c.curve_type, c.curve_date, c.currency, c.scenario,
                        c.compounding, c.source, c.loaded_at, COUNT(p.tenor_days)
                 FROM curve_sets c
                 LEFT JOIN curve_points p ON p.curve_set_id = c.id
                 GROUP BY c.id
                 ORDER BY c.curve_date, c.currency, c.scenario",
            )
            .map_err(sql_error)?;
        let curves = statement
            .query_map([], |row| {
                Ok(StoredCurve {
                    id: row.get(0)?,
                    curve_type: row.get(1)?,
                    curve_date: row.get(2)?,
                    currency: row.get(3)?,
                    scenario: row.get(4)?,
                    compounding: row.get(5)?,
                    source: row.get(6)?,
                    loaded_at: row.get(7)?,
                    point_count: row.get(8)?,
                })
            })
            .map_err(sql_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sql_error)?;
        Ok(curves)
    }
}

impl CurveProvider for CurveStore {
    fn get_curve(
        &self,
        curve_type: &str,
        currency: &str,
        report_date: crate::dates::Date,
        scenario: &str,
    ) -> Result<ZeroCurve> {
        self.load_curve_set()?
            .get_curve(curve_type, currency, report_date, scenario)
    }
}
