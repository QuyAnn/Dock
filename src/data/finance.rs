//! Public, no-key endpoints. Each worker schedules with Instant so latency cannot cause drift.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, LazyLock};
use chrono::{DateTime, Utc};
use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub struct Finance {
    pub btc: Option<f64>,
    pub btc_at: Option<DateTime<Utc>>,
    pub rates: BTreeMap<String, f64>,
    pub rates_at: Option<DateTime<Utc>>,
    pub btc_error: bool,
    pub rates_error: bool,
}
static DATA: LazyLock<Mutex<Arc<Finance>>> = LazyLock::new(|| Mutex::new(Arc::new(Finance::default())));
pub fn snapshot() -> Arc<Finance> { DATA.lock().unwrap_or_else(|e| e.into_inner()).clone() }
fn positive(v: f64) -> Option<f64> { (v.is_finite() && v > 0.0).then_some(v) }
pub fn parse_btc(v: &Value) -> Option<f64> { positive(v.pointer("/data/amount")?.as_str()?.parse().ok()?) }
pub fn parse_rates(v: &Value) -> Option<BTreeMap<String,f64>> {
    if v.get("result")?.as_str()? != "success" || v.get("base_code")?.as_str()? != "USD" { return None; }
    let rates: BTreeMap<_,_> = v.get("rates")?.as_object()?.iter()
        .filter_map(|(k,v)| Some((k.clone(), positive(v.as_f64()?)?))).collect();
    (rates.get("USD") == Some(&1.0) && rates.contains_key("VND") && rates.contains_key("EUR")).then_some(rates)
}
pub fn convert(rates: &BTreeMap<String,f64>, amount: f64, from: &str, to: &str) -> Option<f64> {
    if !amount.is_finite() || amount < 0.0 { return None; }
    let result = amount / positive(*rates.get(&from.to_ascii_uppercase())?)? * positive(*rates.get(&to.to_ascii_uppercase())?)?;
    result.is_finite().then_some(result)
}
pub fn refresh_btc() {
    let value = super::http::agent().get("https://api.coinbase.com/v2/prices/BTC-USD/spot").call().ok()
        .and_then(|r| r.into_json::<Value>().ok()).and_then(|v| parse_btc(&v));
    let mut guard = DATA.lock().unwrap_or_else(|e| e.into_inner());
    let d = Arc::make_mut(&mut guard); d.btc_error = value.is_none();
    if let Some(v) = value { d.btc = Some(v); d.btc_at = Some(Utc::now()); }
}
pub fn refresh_rates() {
    let value = super::http::agent().get("https://open.er-api.com/v6/latest/USD").call().ok()
        .and_then(|r| r.into_json::<Value>().ok()).and_then(|v| parse_rates(&v));
    let mut guard = DATA.lock().unwrap_or_else(|e| e.into_inner());
    let d = Arc::make_mut(&mut guard); d.rates_error = value.is_none();
    if let Some(v) = value { d.rates = v; d.rates_at = Some(Utc::now()); }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_prices_and_rates() {
        assert_eq!(parse_btc(&serde_json::json!({"data":{"amount":"123.50"}})),Some(123.5));
        for price in ["NaN","inf","0","-5"] { assert!(parse_btc(&serde_json::json!({"data":{"amount":price}})).is_none()); }
        assert!(parse_rates(&serde_json::json!({"result":"error"})).is_none());
    }
    #[test]
    fn cross_currency_and_invalid_amount() {
        let r = BTreeMap::from([("USD".into(),1.0),("VND".into(),25000.0),("EUR".into(),0.9)]);
        assert_eq!(convert(&r,2.0,"usd","VND"),Some(50000.0));
        assert!((convert(&r,0.9,"EUR","VND").unwrap() - 25000.0).abs() < 0.001);
        assert_eq!(convert(&r,1.0,"XXX","USD"),None);
        assert_eq!(convert(&r,f64::NAN,"USD","USD"),None);
    }
}
