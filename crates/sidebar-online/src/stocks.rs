//! Stock quotes from Yahoo Finance's chart API (what `yfinance` uses):
//! free, no key, delayed for some exchanges. Only the symbols are sent.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use sidebar_core::Source;
use sidebar_core::source::Quote;

use crate::http::{Http, encode};
use crate::poll;

const REFRESH: Duration = Duration::from_secs(5 * 60);
const RETRY: Duration = Duration::from_secs(60);
/// Seconds between intraday points.
const INTERVAL: i64 = 5 * 60;

/// Quotes in configured order. Symbols that have never loaded are left
/// out; the error is shown only if none have.
pub type QuotesReport = Result<Vec<Quote>, String>;

/// Starts polling quotes for `symbols` in the background.
pub fn yahoo_quotes(symbols: Vec<String>) -> Box<dyn Source<QuotesReport>> {
    let mut http: Option<Http> = None;
    // Last good quote per symbol, kept through failed refreshes.
    let mut last: HashMap<String, Quote> = HashMap::new();
    poll::spawn("stocks", REFRESH, RETRY, move || {
        let http = http.get_or_insert_with(Http::new);
        let mut first_error = None;
        for symbol in &symbols {
            let url = format!("https://query1.finance.yahoo.com/v8/finance/chart/{}?range=1d&interval=5m", encode(symbol));
            match http.get(&url).and_then(|json| parse_chart(&json, symbol, now())) {
                Ok(quote) => {
                    last.insert(symbol.clone(), quote);
                }
                Err(e) => {
                    first_error.get_or_insert(format!("{symbol}: {e}"));
                }
            }
        }
        let quotes: Vec<Quote> = symbols.iter().filter_map(|s| last.get(s).cloned()).collect();
        match first_error {
            Some(e) if quotes.is_empty() => Err(e),
            Some(e) => {
                eprintln!("stocks: {e}");
                Ok(quotes)
            }
            None => Ok(quotes),
        }
    })
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

#[derive(Deserialize)]
struct Chart {
    chart: ChartBody,
}

#[derive(Deserialize)]
struct ChartBody {
    #[serde(default)]
    result: Option<Vec<ChartResult>>,
    #[serde(default)]
    error: Option<ChartError>,
}

#[derive(Deserialize)]
struct ChartError {
    description: String,
}

#[derive(Deserialize)]
struct ChartResult {
    meta: Meta,
    #[serde(default)]
    indicators: Option<Indicators>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    #[serde(default)]
    currency: Option<String>,
    regular_market_price: f64,
    #[serde(default)]
    previous_close: Option<f64>,
    #[serde(default)]
    chart_previous_close: Option<f64>,
    #[serde(default)]
    current_trading_period: Option<TradingPeriods>,
}

#[derive(Deserialize)]
struct TradingPeriods {
    regular: Period,
}

#[derive(Deserialize)]
struct Period {
    start: i64,
    end: i64,
}

#[derive(Deserialize)]
struct Indicators {
    quote: Vec<QuoteSeries>,
}

#[derive(Deserialize)]
struct QuoteSeries {
    #[serde(default)]
    close: Vec<Option<f64>>,
}

/// A quote from a chart response; `now` (Unix seconds) decides whether the
/// market is open.
fn parse_chart(json: &str, symbol: &str, now: i64) -> Result<Quote, String> {
    let chart: Chart = serde_json::from_str(json).map_err(|e| format!("unexpected response: {e}"))?;
    if let Some(e) = chart.chart.error {
        return Err(e.description);
    }
    let r = chart.chart.result.and_then(|r| r.into_iter().next()).ok_or("no data")?;
    let m = r.meta;
    let previous_close = m.previous_close.or(m.chart_previous_close).ok_or("no previous close")?;
    let intraday = r
        .indicators
        .and_then(|i| i.quote.into_iter().next())
        .map(|q| q.close.into_iter().flatten().collect())
        .unwrap_or_default();
    let (session_points, market_open) = match m.current_trading_period {
        Some(TradingPeriods { regular: Period { start, end } }) if end > start => {
            (((end - start) / INTERVAL) as usize, (start..end).contains(&now))
        }
        _ => (0, false),
    };
    Ok(Quote {
        symbol: symbol.to_string(),
        currency: m.currency.unwrap_or_default(),
        price: m.regular_market_price,
        previous_close,
        intraday,
        session_points,
        market_open,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const AAPL: &str = r#"{"chart":{"result":[{"meta":{"currency":"USD","symbol":"AAPL","regularMarketPrice":54.495,
        "chartPreviousClose":58.27,"previousClose":58.27,
        "currentTradingPeriod":{"pre":{"start":1793869200,"end":1793889000},"regular":{"start":1793889000,"end":1793912400},
                                "post":{"start":1793912400,"end":1793926800}}},
        "timestamp":[1793889000,1793889300,1793889600],
        "indicators":{"quote":[{"close":[55.25,null,54.45]}]}}],"error":null}}"#;

    #[test]
    fn parses_quotes() {
        let q = parse_chart(AAPL, "AAPL", 1793900000).unwrap();
        assert_eq!((q.price, q.previous_close, q.currency.as_str()), (54.495, 58.27, "USD"));
        assert_eq!(q.intraday, [55.25, 54.45]);
        assert_eq!(q.session_points, 78);
        assert!(q.market_open);
        assert!((q.change_percent() - -6.479).abs() < 0.001);
        assert!(!parse_chart(AAPL, "AAPL", 1793912400).unwrap().market_open);
    }

    #[test]
    fn reports_errors() {
        let missing = r#"{"chart":{"result":null,"error":{"code":"Not Found","description":"No data found, symbol may be delisted"}}}"#;
        assert_eq!(parse_chart(missing, "NOPE", 0), Err("No data found, symbol may be delisted".into()));
        assert!(parse_chart("<html>", "X", 0).is_err());
        assert_eq!(encode("^GSPC"), "%5EGSPC");
    }
}
