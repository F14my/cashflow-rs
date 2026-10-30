# cashflow-rs

Калькулятор строит будущие платежи по основному долгу и процентам, а затем дисконтирует их по внешней бескупонной кривой. Расчёт выполняется отдельно по каждой сделке, но вход читается чанками, сделки внутри чанка считаются параллельно, а результат сразу записывается в CSV.

## Денежный поток

Поддерживаются схемы погашения `SIMPLE`, `RGM` и `ANN`, явные графики погашения, фиксированные и плавающие ставки, основная и дополнительная процентные ноги, `ACT/365` и `ACT/ACT ISDA`.

Для плавающей ставки эффективная годовая ставка равна:

```text
interest_rate + coefficient * indicator
```

После этого применяются `floor` и `cap`. Историческая фиксация индикатора выбирается на дату репрайсинга с учётом `lag`. Для будущей фиксации источник индикатора возвращает последнее значение, известное на `report_date`. Если у FLOAT-ставки нет ни кода индикатора, ни `base_indicator_rate`, сделка завершается ошибкой: скрытого значения по умолчанию нет.

## Дисконтирование

Ключевая ставка используется для прогнозирования процентов FLOAT-сделки и не является ставкой дисконтирования. Дисконтирование выполняется по отдельной бескупонной кривой без сдвига.

Формат файла кривых:

```csv
curve_date;currency;tenor_days;zero_rate_percent
2022-08-31;RUB;30;7.82
2022-08-31;RUB;365;8.15
2022-08-31;RUB;730;8.32
```

В одном файле могут находиться разные даты и валюты. Для сделки выбирается кривая её валюты с максимальной `curve_date`, не превышающей `report_date`. Если подходящей кривой нет, сделка завершается ошибкой.

Между узлами применяется линейная интерполяция zero rate. До первого и после последнего узла используется ближайшая крайняя ставка. Время до платежа рассчитывается независимо от day count сделки:

```text
t = (cashflow_date - report_date) / 365
```

Поддерживаются два соглашения:

```text
continuous: DF = exp(-r * t)
annual:     DF = (1 + r)^(-t)
```

Соглашение обязано соответствовать источнику кривой. Для G-кривой, заданной параметрами, сначала необходимо получить узлы zero rate по официальной формуле источника; текущий загрузчик принимает готовые узлы.

Приведённые стоимости:

```text
discounted_principal_cf = principal_cf * DF
discounted_interest_cf  = interest_cf * DF
discounted_total_cf     = discounted_principal_cf + discounted_interest_cf
NPV                     = sum(discounted_total_cf)
```

## Запуск

```bash
cargo run --release -- \
  --deals data/deals.csv \
  --indicator CBKEY1M=data/cbkey1m.csv \
  --discount-curve data/rub_zero_curve.csv \
  --discount-compounding continuous \
  --out out/cashflows.csv \
  --summary out/summary.csv
```

Нужно указать ровно один источник кривых: `--discount-curve` или `--curve-db`. Основные дополнительные параметры:

- `--deal-id 1,16,29` — ограничить набор сделок;
- `--threads N` — число рабочих потоков;
- `--chunk-size N` — размер чанка, по умолчанию 10 000;
- `--act-act-as-365` — режим совместимости с эталонным Excel.

## SQLite-хранилище и история загрузок

Для демонстрации второго спринта предусмотрен `curve_admin`. Он создаёт таблицы `curve_sets`, `curve_points` и `curve_load_runs`. В узлах сохраняются zero rate и рассчитанный discount factor. Набор кривых идентифицируется типом, датой, валютой и сценарием.

```bash
cargo run --release --bin curve_admin -- init --db out/curves.db

cargo run --release --bin curve_admin -- import \
  --db out/curves.db \
  --file data/demo_curves.csv \
  --mode manual \
  --source DEMO_FILE \
  --requested-date 2022-08-31

cargo run --release --bin curve_admin -- history --db out/curves.db
cargo run --release --bin curve_admin -- curves --db out/curves.db
```

Режим импорта может быть `manual` или `automatic`; оба записываются в историю вместе со статусом, временем, количеством кривых и точек и сообщением об ошибке. Повторный импорт того же набора обновляет его атомарно, а не создаёт дубликаты.

Расчёт из базы:

```bash
cargo run --release --bin cfcalc -- \
  --deals data/deals.csv \
  --curve-db out/curves.db \
  --curve-type G_CURVE \
  --scenario BASE \
  --out out/cashflows.csv \
  --summary out/summary.csv
```

Полный сценарий защиты запускается одной командой:

```bash
./scripts/demo_defense.sh
```

Файл `data/demo_curves.csv` содержит синтетические данные только для демонстрации механики. Его значения нельзя использовать как рыночные.

Выход с потоками содержит исходные суммы, фактическую дату кривой, время, zero rate, discount factor и дисконтированные суммы. Summary содержит `principal`, `interest`, `total_cf`, `pv_principal`, `pv_interest` и `npv`.

## Проверка

```bash
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
```

Для финансовой приёмки дополнительно нужны реальные данные бескупонной кривой и контрольные PV/NPV из внешней системы. Синтетическая кривая проверяет алгоритм и инварианты, но не подтверждает соответствие конкретной методике публикации G-кривой.
