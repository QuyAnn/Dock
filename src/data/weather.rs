//! Weather from Open-Meteo (free, no API key). Refresh every ~30 minutes from the caller.
use std::sync::{Mutex, PoisonError};

use serde_json::Value;

/// Same default as the old app's settings.
pub const DEFAULT_LOCATION: &str = "Hanoi";

#[derive(Clone, Debug)]
pub struct Current {
    pub temp: f32,
    pub feels: f32,
    /// WMO weather code.
    pub code: u32,
    pub humidity: f32,
    pub wind: f32,
}

#[derive(Clone, Debug)]
pub struct Day {
    pub date: String,
    pub max: f32,
    pub min: f32,
    pub code: u32,
}

#[derive(Clone, Debug)]
pub struct Weather {
    pub location: String,
    pub current: Current,
    pub daily: Vec<Day>,
    pub hourly: Vec<Hour>,
    pub sunrise: Option<i64>,
    pub sunset: Option<i64>,
    pub utc_offset_seconds: i32,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Debug)]
pub struct Hour { pub at: i64, pub temp: f32, pub code: u32 }

#[derive(Clone, Debug)]
struct Place {
    lat: f64,
    lon: f64,
    name: String,
}

/// Geocoding results by query string, so each name is looked up once.
static GEO_CACHE: Mutex<Vec<(String, Place)>> = Mutex::new(Vec::new());

/// Weather for the default location. None on any network or parse failure.
pub fn refresh_weather() -> Option<Weather> {
    refresh_weather_at(DEFAULT_LOCATION)
}

/// Weather for a place name or a "lat,lon" pair. None on any network or parse failure.
pub fn refresh_weather_at(location: &str) -> Option<Weather> {
    let place = resolve(location)?;
    let (lat, lon) = (place.lat.to_string(), place.lon.to_string());
    let j = get_json(
        "https://api.open-meteo.com/v1/forecast",
        &[
            ("latitude", lat.as_str()),
            ("longitude", lon.as_str()),
            ("current", "temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,wind_speed_10m"),
            ("daily", "weather_code,temperature_2m_max,temperature_2m_min,sunrise,sunset"),
            ("hourly", "temperature_2m,weather_code"),
            ("timezone", "auto"),
            ("forecast_days", "5"),
        ],
    )?;

    parse_weather(&j, place.name)
}

fn parse_weather(j: &Value, location: String) -> Option<Weather> {
    let c = j.get("current")?;
    let current = Current {
        temp: num(c.get("temperature_2m"))?,
        feels: num(c.get("apparent_temperature"))?,
        code: code(c.get("weather_code"))?,
        humidity: num(c.get("relative_humidity_2m"))?,
        wind: num(c.get("wind_speed_10m"))?,
    };

    let d = j.get("daily")?;
    let daily = d
        .get("time")?
        .as_array()?
        .iter()
        .take(5)
        .enumerate()
        .filter_map(|(i, date)| {
            Some(Day {
                date: date.as_str()?.to_owned(),
                max: num(d.get("temperature_2m_max")?.get(i))?,
                min: num(d.get("temperature_2m_min")?.get(i))?,
                code: code(d.get("weather_code")?.get(i))?,
            })
        })
        .collect();

    let utc_offset_seconds = j.get("utc_offset_seconds").and_then(Value::as_i64).unwrap_or(0) as i32;
    let timestamp = |v: &Value| -> Option<i64> {
        Some(chrono::NaiveDateTime::parse_from_str(v.as_str()?, "%Y-%m-%dT%H:%M").ok()?.and_utc().timestamp() - utc_offset_seconds as i64)
    };
    let hourly = j.get("hourly").and_then(|h| {
        Some(h.get("time")?.as_array()?.iter().take(5 * 24).enumerate().filter_map(|(i,time)| {
            Some(Hour { at: timestamp(time)?, temp: num(h.get("temperature_2m")?.get(i))?, code: code(h.get("weather_code")?.get(i))? })
        }).collect())
    }).unwrap_or_default();
    Some(Weather { location, current, daily, hourly,
        sunrise: d.get("sunrise").and_then(|a| a.get(0)).and_then(timestamp),
        sunset: d.get("sunset").and_then(|a| a.get(0)).and_then(timestamp),
        utc_offset_seconds, fetched_at: chrono::Utc::now() })
}

#[cfg(test)]
mod forecast_tests {
    use super::*;
    #[test]
    fn parses_forecast_in_location_timezone_and_skips_null_hours() {
        let j = serde_json::json!({"utc_offset_seconds":25200,
            "current":{"temperature_2m":28,"apparent_temperature":30,"weather_code":0,"relative_humidity_2m":65,"wind_speed_10m":8},
            "daily":{"time":["2026-10-25"],"temperature_2m_max":[30],"temperature_2m_min":[24],"weather_code":[2],"sunrise":["2026-10-25T06:00"],"sunset":["2026-10-25T18:00"]},
            "hourly":{"time":["2026-10-25T12:00","2026-10-25T13:00"],"temperature_2m":[28,null],"weather_code":[0,1]}});
        let w = parse_weather(&j,"Hanoi".into()).unwrap();
        assert_eq!(w.hourly.len(),1); assert_eq!(w.daily.len(),1);
        assert_eq!(chrono::DateTime::from_timestamp(w.hourly[0].at,0).unwrap().format("%H:%M").to_string(),"05:00");
        assert_eq!(w.sunset.unwrap() - w.sunrise.unwrap(),43200);
    }
}

/// "lat,lon" is used directly; anything else is geocoded (and cached).
fn resolve(location: &str) -> Option<Place> {
    if let Some((a, b)) = location.split_once(',') {
        if let (Ok(lat), Ok(lon)) = (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
            if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) {
                return Some(Place { lat, lon, name: location.to_owned() });
            }
        }
    }

    let mut cache = GEO_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, place)) = cache.iter().find(|(k, _)| k == location) {
        return Some(place.clone());
    }
    let j = get_json(
        "https://geocoding-api.open-meteo.com/v1/search",
        &[("name", location), ("count", "1"), ("language", "vi")],
    )?;
    let r = j.pointer("/results/0")?;
    let place = Place {
        lat: r.get("latitude")?.as_f64()?,
        lon: r.get("longitude")?.as_f64()?,
        name: r.get("name")?.as_str()?.to_owned(),
    };
    if cache.len() >= 32 { cache.remove(0); }
    cache.push((location.to_owned(), place.clone()));
    Some(place)
}

fn get_json(url: &str, query: &[(&str, &str)]) -> Option<Value> {
    // Must use the shared TLS-backed agent; bare `ureq::get` has no TLS when
    // default-features are off and only the `native-tls` feature is enabled.
    let mut req = crate::data::http::agent().get(url);
    for (k, v) in query {
        req = req.query(k, v);
    }
    req.call().ok()?.into_json().ok()
}

fn num(v: Option<&Value>) -> Option<f32> {
    v?.as_f64().map(|x| x as f32)
}

fn code(v: Option<&Value>) -> Option<u32> {
    v?.as_u64().map(|x| x as u32)
}
