//! Weather from Open-Meteo (<https://open-meteo.com>): free, no API key.
//! The configured place (or coordinates) is sent to Open-Meteo, nothing
//! else.

use std::time::Duration;

use serde::Deserialize;
use sidebar_core::Source;
use sidebar_core::source::{Sky, Weather, WeatherDay, WeatherHour};

use crate::http::{Http, encode};
use crate::poll;

/// How often the forecast is refreshed; Open-Meteo updates current
/// conditions every 15 minutes.
const REFRESH: Duration = Duration::from_secs(15 * 60);
/// Retry delay after a failed fetch.
const RETRY: Duration = Duration::from_secs(2 * 60);
/// Hours shown, and the spacing between them.
const HOURS: usize = 5;
const HOUR_STEP: u8 = 3;
const DAYS: usize = 6;

/// Where to get the weather for.
#[derive(Debug, Clone, PartialEq)]
pub enum Location {
    /// A place name, looked up once: "Berlin", or "Springfield, Illinois"
    /// to pick among places of the same name.
    Place(String),
    Coordinates { latitude: f64, longitude: f64 },
}

impl Location {
    /// "Berlin", or "52.52, 13.41" for coordinates.
    pub fn parse(text: &str) -> Location {
        let coordinates = text.split_once(',').and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?)));
        match coordinates {
            Some((latitude, longitude)) => Location::Coordinates { latitude, longitude },
            None => Location::Place(text.trim().to_string()),
        }
    }
}

/// The latest weather: the newest forecast, or the error if no fetch has
/// succeeded yet.
pub type Report = Result<Weather, String>;

/// Starts fetching the weather for `location` in the background.
pub fn open_meteo(location: Location, fahrenheit: bool) -> Box<dyn Source<Report>> {
    let mut place: Option<Place> = None;
    let mut client: Option<Client> = None;
    poll::spawn("weather", REFRESH, RETRY, move || {
        let client = client.get_or_insert_with(|| Client::new(fahrenheit));
        // The place is looked up once.
        let p = match &place {
            Some(p) => p.clone(),
            None => place.insert(client.locate(&location)?).clone(),
        };
        client.forecast(&p)
    })
}

/// A resolved location.
#[derive(Debug, Clone, PartialEq)]
struct Place {
    name: String,
    latitude: f64,
    longitude: f64,
}

struct Client {
    http: Http,
    fahrenheit: bool,
}

impl Client {
    fn new(fahrenheit: bool) -> Self {
        Self { http: Http::new(), fahrenheit }
    }

    fn get(&self, url: &str) -> Result<String, String> {
        self.http.get(url)
    }

    fn locate(&self, location: &Location) -> Result<Place, String> {
        match location {
            Location::Coordinates { latitude, longitude } => {
                Ok(Place { name: format!("{latitude:.2}, {longitude:.2}"), latitude: *latitude, longitude: *longitude })
            }
            Location::Place(query) => {
                let (name, qualifier) = match query.split_once(',') {
                    Some((name, q)) => (name.trim(), Some(q.trim())),
                    None => (query.trim(), None),
                };
                let url = format!(
                    "https://geocoding-api.open-meteo.com/v1/search?name={}&count=10&language=en&format=json",
                    encode(name)
                );
                pick_place(&self.get(&url)?, qualifier).ok_or_else(|| format!("place not found: {query}"))
            }
        }
    }

    fn forecast(&self, place: &Place) -> Result<Weather, String> {
        let unit = if self.fahrenheit { "&temperature_unit=fahrenheit" } else { "" };
        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}\
             &current=temperature_2m,weather_code,is_day&hourly=temperature_2m,weather_code,is_day\
             &daily=weather_code,temperature_2m_max,temperature_2m_min&timezone=auto\
             &forecast_days={DAYS}&forecast_hours={}{unit}",
            place.latitude,
            place.longitude,
            HOURS * HOUR_STEP as usize + HOUR_STEP as usize,
        );
        let unit = if self.fahrenheit { "°F" } else { "°C" };
        parse_forecast(&self.get(&url)?, &place.name, unit)
    }
}

#[derive(Deserialize)]
struct GeoResults {
    #[serde(default)]
    results: Vec<GeoResult>,
}

#[derive(Deserialize)]
struct GeoResult {
    name: String,
    latitude: f64,
    longitude: f64,
    #[serde(default)]
    country: String,
    #[serde(default)]
    country_code: String,
    #[serde(default)]
    admin1: String,
}

/// US state and Canadian province abbreviations, as in "Burbank, CA".
const REGIONS: [(&str, &str); 64] = [
    ("AL", "Alabama"), ("AK", "Alaska"), ("AZ", "Arizona"), ("AR", "Arkansas"), ("CA", "California"),
    ("CO", "Colorado"), ("CT", "Connecticut"), ("DE", "Delaware"), ("DC", "Washington, D.C."),
    ("FL", "Florida"), ("GA", "Georgia"), ("HI", "Hawaii"), ("ID", "Idaho"), ("IL", "Illinois"),
    ("IN", "Indiana"), ("IA", "Iowa"), ("KS", "Kansas"), ("KY", "Kentucky"), ("LA", "Louisiana"),
    ("ME", "Maine"), ("MD", "Maryland"), ("MA", "Massachusetts"), ("MI", "Michigan"), ("MN", "Minnesota"),
    ("MS", "Mississippi"), ("MO", "Missouri"), ("MT", "Montana"), ("NE", "Nebraska"), ("NV", "Nevada"),
    ("NH", "New Hampshire"), ("NJ", "New Jersey"), ("NM", "New Mexico"), ("NY", "New York"),
    ("NC", "North Carolina"), ("ND", "North Dakota"), ("OH", "Ohio"), ("OK", "Oklahoma"), ("OR", "Oregon"),
    ("PA", "Pennsylvania"), ("RI", "Rhode Island"), ("SC", "South Carolina"), ("SD", "South Dakota"),
    ("TN", "Tennessee"), ("TX", "Texas"), ("UT", "Utah"), ("VT", "Vermont"), ("VA", "Virginia"),
    ("WA", "Washington"), ("WV", "West Virginia"), ("WI", "Wisconsin"), ("WY", "Wyoming"),
    ("PR", "Puerto Rico"), ("AB", "Alberta"), ("BC", "British Columbia"), ("MB", "Manitoba"),
    ("NB", "New Brunswick"), ("NL", "Newfoundland and Labrador"), ("NS", "Nova Scotia"),
    ("NT", "Northwest Territories"), ("NU", "Nunavut"), ("ON", "Ontario"), ("PE", "Prince Edward Island"),
    ("QC", "Quebec"), ("SK", "Saskatchewan"),
];

/// The first result, or the first whose country, country code or region
/// matches `qualifier` ("Illinois", "US", "Germany", or a US state /
/// Canadian province code such as "CA" for California).
fn pick_place(json: &str, qualifier: Option<&str>) -> Option<Place> {
    let results: GeoResults = serde_json::from_str(json).ok()?;
    let region = qualifier.and_then(|q| REGIONS.iter().find(|(code, _)| code.eq_ignore_ascii_case(q)).map(|(_, name)| *name));
    let matches = |r: &GeoResult| {
        qualifier.is_none_or(|q| {
            [&r.country, &r.country_code, &r.admin1].iter().any(|f| f.eq_ignore_ascii_case(q))
                || region.is_some_and(|name| r.admin1.eq_ignore_ascii_case(name))
        })
    };
    let r = results.results.into_iter().find(matches)?;
    Some(Place { name: r.name, latitude: r.latitude, longitude: r.longitude })
}

#[derive(Deserialize)]
struct Forecast {
    current: Current,
    hourly: Hourly,
    daily: Daily,
}

#[derive(Deserialize)]
struct Current {
    temperature_2m: f32,
    weather_code: u8,
    is_day: u8,
}

#[derive(Deserialize)]
struct Hourly {
    time: Vec<String>,
    temperature_2m: Vec<Option<f32>>,
    weather_code: Vec<Option<u8>>,
    is_day: Vec<Option<u8>>,
}

#[derive(Deserialize)]
struct Daily {
    time: Vec<String>,
    weather_code: Vec<Option<u8>>,
    temperature_2m_max: Vec<Option<f32>>,
    temperature_2m_min: Vec<Option<f32>>,
}

fn parse_forecast(json: &str, place: &str, unit: &'static str) -> Result<Weather, String> {
    let f: Forecast = serde_json::from_str(json).map_err(|e| format!("unexpected forecast: {e}"))?;
    let hours = (0..f.hourly.time.len())
        .filter_map(|i| {
            let hour = hour_of(&f.hourly.time[i])?;
            Some(WeatherHour {
                hour,
                temperature: (*f.hourly.temperature_2m.get(i)?)?,
                sky: sky((*f.hourly.weather_code.get(i)?)?),
                daylight: f.hourly.is_day.get(i).copied().flatten() == Some(1),
            })
        })
        // On the hour marks after now: 06:00, 09:00, ... (the first entry
        // is the current hour).
        .skip(1)
        .filter(|h| h.hour % HOUR_STEP == 0)
        .take(HOURS)
        .collect();
    let days = (0..f.daily.time.len())
        .filter_map(|i| {
            Some(WeatherDay {
                weekday: weekday_of(&f.daily.time[i])?,
                sky: sky((*f.daily.weather_code.get(i)?)?),
                high: (*f.daily.temperature_2m_max.get(i)?)?,
                low: (*f.daily.temperature_2m_min.get(i)?)?,
            })
        })
        .take(DAYS)
        .collect();
    Ok(Weather {
        place: place.to_string(),
        unit,
        temperature: f.current.temperature_2m,
        sky: sky(f.current.weather_code),
        daylight: f.current.is_day == 1,
        hours,
        days,
    })
}

/// WMO weather interpretation code (as used by Open-Meteo) to [`Sky`].
fn sky(code: u8) -> Sky {
    match code {
        0 => Sky::Clear,
        1 | 2 => Sky::PartlyCloudy,
        45 | 48 => Sky::Fog,
        51..=57 => Sky::Drizzle,
        61..=67 | 80..=82 => Sky::Rain,
        71..=77 | 85 | 86 => Sky::Snow,
        95..=99 => Sky::Thunder,
        _ => Sky::Cloudy,
    }
}

/// Hour from "2026-11-05T06:00".
fn hour_of(time: &str) -> Option<u8> {
    time.split_once('T')?.1.get(..2)?.parse().ok()
}

/// Weekday (0 = Monday) from "2026-11-05".
fn weekday_of(date: &str) -> Option<u8> {
    let mut parts = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    // 1970-01-01 was a Thursday.
    Some((days + 3).rem_euclid(7) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_locations() {
        assert_eq!(Location::parse(" New York "), Location::Place("New York".into()));
        assert_eq!(Location::parse("52.52, 13.41"), Location::Coordinates { latitude: 52.52, longitude: 13.41 });
        assert_eq!(Location::parse("Springfield, Illinois"), Location::Place("Springfield, Illinois".into()));
        assert_eq!(encode("São Paulo"), "S%C3%A3o%20Paulo");
    }

    #[test]
    fn picks_places() {
        let json = r#"{"results":[
            {"name":"Springfield","latitude":37.2,"longitude":-93.3,"country":"United States","country_code":"US","admin1":"Missouri"},
            {"name":"Springfield","latitude":39.8,"longitude":-89.6,"country":"United States","country_code":"US","admin1":"Illinois"}]}"#;
        assert_eq!(pick_place(json, None).map(|p| p.latitude), Some(37.2));
        assert_eq!(pick_place(json, Some("illinois")).map(|p| p.latitude), Some(39.8));
        assert_eq!(pick_place(json, Some("Canada")), None);
        assert_eq!(pick_place(json, Some("IL")).map(|p| p.latitude), Some(39.8));

        // "CA" is California, and also Canada's country code.
        let json = r#"{"results":[
            {"name":"Burbank","latitude":41.74,"longitude":-87.79,"country":"United States","country_code":"US","admin1":"Illinois"},
            {"name":"Burbank","latitude":34.18,"longitude":-118.31,"country":"United States","country_code":"US","admin1":"California"},
            {"name":"Toronto","latitude":43.7,"longitude":-79.4,"country":"Canada","country_code":"CA","admin1":"Ontario"}]}"#;
        assert_eq!(pick_place(json, Some("CA")).map(|p| p.latitude), Some(34.18));
        assert_eq!(pick_place(r#"{"generationtime_ms":0.1}"#, None), None);
    }

    #[test]
    fn dates_and_codes() {
        assert_eq!(hour_of("2026-11-05T06:00"), Some(6));
        assert_eq!(weekday_of("2026-11-05"), Some(3)); // Thursday
        assert_eq!(weekday_of("2000-02-29"), Some(1)); // Tuesday
        assert_eq!(weekday_of("1970-01-01"), Some(3));
        assert_eq!(sky(0), Sky::Clear);
        assert_eq!(sky(3), Sky::Cloudy);
        assert_eq!(sky(81), Sky::Rain);
        assert_eq!(sky(96), Sky::Thunder);
    }

    #[test]
    fn parses_forecast() {
        let json = r#"{
            "current":{"time":"2026-11-05T00:15","temperature_2m":16.7,"weather_code":3,"is_day":0},
            "hourly":{"time":["2026-11-05T00:00","2026-11-05T01:00","2026-11-05T02:00","2026-11-05T03:00",
                              "2026-11-05T04:00","2026-11-05T05:00","2026-11-05T06:00"],
                      "temperature_2m":[16.7,16.9,17.3,17.0,16.0,15.5,15.3],
                      "weather_code":[2,45,3,1,1,0,61],
                      "is_day":[0,0,0,0,0,0,1]},
            "daily":{"time":["2026-11-05","2026-11-06"],"weather_code":[61,null],
                     "temperature_2m_max":[22.2,18.5],"temperature_2m_min":[15.0,14.4]}}"#;
        let w = parse_forecast(json, "New York", "°C").unwrap();
        assert_eq!((w.temperature, w.sky, w.daylight), (16.7, Sky::Cloudy, false));
        assert_eq!(
            w.hours,
            [
                WeatherHour { hour: 3, temperature: 17.0, sky: Sky::PartlyCloudy, daylight: false },
                WeatherHour { hour: 6, temperature: 15.3, sky: Sky::Rain, daylight: true },
            ]
        );
        // A day with a missing value is dropped rather than shown wrong.
        assert_eq!(w.days, [WeatherDay { weekday: 3, sky: Sky::Rain, high: 22.2, low: 15.0 }]);
        assert!(parse_forecast("{}", "x", "°C").is_err());
    }
}
