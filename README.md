# cashflow-rs

Калькулятор строит будущие платежи по основному долгу и процентам, а затем дисконтирует их по внешней бескупонной кривой. Расчёт выполняется отдельно по каждой сделке, но вход читается чанками, сделки внутри чанка считаются параллельно, а результат сразу записывается в CSV.

## Архитектура

Код разложен по слоям чистой архитектуры, зависимости направлены внутрь:

```text
src/
  domain/          модель сделки, графики, ставки, кривые, дисконтирование — чистые расчёты без I/O
  application/     use case оценки портфеля по сценариям (ValuationService) и порты (DiscountCurves)
  infrastructure/  адаптеры: CSV сделок/кривых/индикаторов, SQLite-хранилище кривых
  interfaces/http/ HTTP API на axum: DTO и роутер
  main.rs          CLI cfcalc
  bin/cfcalc_api.rs  HTTP-сервер
  bin/curve_admin.rs администрирование SQLite-хранилища
```

График платежей сделки строится один раз и дисконтируется по каждой кривой сценария, так что
оценка N сценариев не перестраивает потоки. Сделки считаются параллельно.

## HTTP API

```bash
cargo run --release --bin cfcalc-api          # слушает 0.0.0.0:8080
```

| Метод | Путь | Назначение |
| --- | --- | --- |
| GET | `/api/v1/health` | живость процесса |
| POST | `/api/v1/valuations` | потоки, PV и NPV портфеля по набору сценарных кривых |

Тело `POST /api/v1/valuations`:

```json
{
  "deals": [{"report_dttm": "2022-08-31", "deal_id": 1, "discriminator": "ANN", "...": "колонки deals.csv"}],
  "curves": [
    {"scenario": "BASE", "curve_date": "2022-08-31", "currency": "RUB", "compounding": "continuous",
     "points": [{"tenor_days": 30, "zero_rate_percent": 8.1}, {"tenor_days": 365, "zero_rate_percent": 8.6}]},
    {"scenario": "PARALLEL_UP", "curve_date": "2022-08-31", "currency": "RUB",
     "points": [{"tenor_days": 30, "zero_rate_percent": 12.1}]}
  ],
  "scenarios": ["BASE", "PARALLEL_UP"],
  "indicators": [{"code": "CBKEY1M", "points": [{"effective_date": "2022-07-25", "rate_percent": 8}]}],
  "act_act_as_365": false,
  "include_cashflows": false
}
```

Ответ содержит итоги по сценариям (`scenarios[].total.npv`) и по сделкам
(`deals[].scenarios[].summary`, при `include_cashflows` — ещё и потоки). Ошибка отдельной сделки
не валит запрос: она возвращается в `deals[].scenarios[].error` и учитывается в `failed`.
Некорректный запрос — `422 {"code":"validation_error","message":...}`.

Переменные окружения сервера: `CFCALC_ADDR`, `CFCALC_INDICATORS` (`CODE=FILE[,CODE=FILE]`),
`CFCALC_THREADS`. Образ собирается `docker build -t cashflow-rs .`.

## Денежный поток

Поддерживаются схемы погашения `SIMPLE`, `RGM` и `ANN`, явные графики погашения, фиксированные и плавающие ставки, основная и дополнительная процентные ноги, `ACT/365` и `ACT/ACT ISDA`.

Для плавающей ставки эффективная годовая ставка равна:

```text
interest_rate + coefficient * indicator
```

После этого применяются `floor` и `cap`. Историческая фиксация индикатора выбирается на дату репрайсинга с учётом `lag`. Для будущей фиксации источник индикатора возвращает последнее значение, известное на `report_date`. Если у FLOAT-ставки нет ни кода индикатора, ни `base_indicator_rate`, сделка завершается ошибкой: скрытого значения по умолчанию нет.

### Субсидии

Дополнительная процентная нога (`sub_*`) — субсидия поверх ставки клиента, например
компенсация по льготным кредитам. Её проценты идут отдельным потоком `subsidy_cf`; проценты
клиента остаются в `interest_cf`. В NPV входят оба потока.

### Досрочные погашения

Код `behavior_model_cd` сделки задаёт модель поведения, политика досрочных погашений
сопоставляет ей годовую ставку CPR (`PREPAYMENT_MORTGAGE` 12 %, `PREPAYMENT_CONSUMER` 20 %,
`PREPAYMENT_AUTO` 15 %, `PREPAYMENT_MR8_CRED` 10 %, `PREPAYMENT_MR8_DEP` 8 %; `CPR_15` или
`CPR:0.15` задают ставку прямо). Договорной график масштабируется на долю непогашенного долга
`S(t) = (1 − CPR)^(t/365)`: на каждой дате графика клиент платит плановые суммы с оставшегося
остатка, а часть остатка гасит досрочно (`prepayment_cf`). Для аннуитета это «срок прежний,
платёж меньше». Плановые и досрочные погашения вместе равны остатку долга; прокатка портфеля
на следующую дату учитывает уже погашенное.

Включение: `--prepayment standard` (или `CODE=CPR,...`) в CLI, поле
`prepayment: {enabled, rates}` в запросах API, `CFCALC_PREPAYMENT` — значение сервера по
умолчанию (`off`).

### Кривые из сервиса кривых

С `CFCALC_CURVES_URL` калькулятор сам берёт базу и шесть сценариев IRRBB у curve-parser
(последний торговый день не позже даты): `curve_date` вместо `curves` в наборе портфельной
оценки, `curves_from_service: true` в `/api/v1/valuations`, `GET /api/v1/curves/{date}` —
кривые в формате калькулятора.

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
