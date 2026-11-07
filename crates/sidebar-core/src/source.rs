//! System data for widgets.
//!
//! Widgets read system state (CPU, memory, battery, time zone) only through
//! [`Source`]. Implementations are per-OS and live outside the core (see
//! the `sidebar-sysinfo` crate), so widgets stay portable and testable
//! with fake sources.

/// Produces the current value of some system quantity.
pub trait Source<T> {
    /// `None` when the value is unavailable right now (e.g. a rate that
    /// needs a second sample, or a device that went away).
    fn sample(&mut self) -> Option<T>;
}

/// Any closure works as a source, which keeps tests and fakes short.
impl<T, F: FnMut() -> Option<T>> Source<T> for F {
    fn sample(&mut self) -> Option<T> {
        self()
    }
}

/// CPU usage since the previous sample. Loads are the fraction of time
/// spent busy, in `0.0..=1.0`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CpuSample {
    /// Over all cores.
    pub total: f32,
    /// One entry per logical core, in OS order.
    pub cores: Vec<f32>,
    /// Current clock averaged over cores, if the OS reports it.
    pub mhz: Option<f32>,
    /// Package temperature in °C, if there is a sensor.
    pub celsius: Option<f32>,
}

/// CPU facts that do not change while running.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CpuInfo {
    /// Marketing name, e.g. "Intel(R) Core(TM) i7-13700".
    pub model: Option<String>,
    /// Logical cores.
    pub cores: usize,
    /// Core types on hybrid CPUs (P-cores and E-cores); empty when all
    /// cores are alike or the OS does not say.
    pub groups: Vec<CoreGroup>,
    /// Whether samples carry a temperature.
    pub temperature: bool,
}

/// Cores of one type, e.g. the E-cores of an Intel hybrid CPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreGroup {
    pub name: String,
    /// Indices into [`CpuSample::cores`].
    pub cores: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Memory {
    pub used_bytes: u64,
    pub total_bytes: u64,
    /// Swap (page file); both 0 when there is none.
    pub swap_used_bytes: u64,
    pub swap_total_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Battery {
    /// Charge in `0.0..=1.0`.
    pub level: f32,
    pub charging: bool,
}

/// Offset of local time from UTC, in seconds (east positive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UtcOffset(pub i32);

/// Fixed facts about one GPU.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GpuInfo {
    /// e.g. "NVIDIA GeForce RTX 5060 Ti".
    pub name: String,
    /// Whether samples carry a temperature.
    pub temperature: bool,
}

/// A GPU's state at one moment. Loads are fractions in `0.0..=1.0`;
/// `None` where the driver does not report a value.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GpuSample {
    /// Busy time of the graphics/compute engine.
    pub load: f32,
    /// Busy time of the memory controller (bandwidth use).
    pub memory_load: Option<f32>,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub encoder_load: Option<f32>,
    pub decoder_load: Option<f32>,
    pub fan: Option<f32>,
    pub power_watts: Option<f32>,
    pub power_limit_watts: Option<f32>,
    pub core_mhz: Option<f32>,
    pub memory_mhz: Option<f32>,
    pub celsius: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Wired,
    Wireless,
    /// VPNs, bridges, tunnels.
    Other,
}

/// The state of one network interface (the one with the default route,
/// unless configured otherwise). Byte counts are in bytes, rates in bytes
/// per second.
#[derive(Debug, Clone, PartialEq)]
pub struct NetSample {
    pub interface: String,
    pub kind: LinkKind,
    /// Wi-Fi network name.
    pub ssid: Option<String>,
    /// Wi-Fi channel frequency.
    pub frequency_mhz: Option<u32>,
    pub signal_dbm: Option<i32>,
    /// Negotiated link speed (Ethernet) or transmit bitrate (Wi-Fi).
    pub link_mbps: Option<f32>,
    pub ipv4: Option<std::net::Ipv4Addr>,
    /// A global address, if the interface has one.
    pub ipv6: Option<std::net::Ipv6Addr>,
    pub gateway: Option<std::net::Ipv4Addr>,
    /// Since the previous sample; `None` on the first one.
    pub down_rate: Option<f64>,
    pub up_rate: Option<f64>,
    /// Since the source was created.
    pub session_down: u64,
    pub session_up: u64,
    /// The interface's own counters (since it came up).
    pub total_down: u64,
    pub total_up: u64,
}

/// Weather conditions, coarse enough to pick an icon and a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sky {
    Clear,
    PartlyCloudy,
    Cloudy,
    Fog,
    Drizzle,
    Rain,
    Snow,
    Thunder,
}

impl Sky {
    pub fn describe(self) -> &'static str {
        match self {
            Sky::Clear => "Clear",
            Sky::PartlyCloudy => "Partly cloudy",
            Sky::Cloudy => "Cloudy",
            Sky::Fog => "Fog",
            Sky::Drizzle => "Drizzle",
            Sky::Rain => "Rain",
            Sky::Snow => "Snow",
            Sky::Thunder => "Thunderstorm",
        }
    }
}

/// One hour of the forecast, in the forecast location's local time.
#[derive(Debug, Clone, PartialEq)]
pub struct WeatherHour {
    pub hour: u8,
    pub temperature: f32,
    pub sky: Sky,
    pub daylight: bool,
}

/// One day of the forecast.
#[derive(Debug, Clone, PartialEq)]
pub struct WeatherDay {
    /// 0 = Monday.
    pub weekday: u8,
    pub sky: Sky,
    pub high: f32,
    pub low: f32,
}

/// Current weather and forecast for one place. Temperatures are in
/// `unit` ("°C" or "°F").
#[derive(Debug, Clone, PartialEq)]
pub struct Weather {
    pub place: String,
    pub unit: &'static str,
    pub temperature: f32,
    pub sky: Sky,
    pub daylight: bool,
    /// Upcoming hours, soonest first.
    pub hours: Vec<WeatherHour>,
    /// Today first.
    pub days: Vec<WeatherDay>,
}

/// A quote for a stock, ETF, index or currency pair.
#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    /// As configured, e.g. "AAPL", "^GSPC", "EURUSD=X".
    pub symbol: String,
    /// ISO code from the exchange, e.g. "USD" (or "GBp" for pence).
    pub currency: String,
    pub price: f64,
    pub previous_close: f64,
    /// Prices through the current (or most recent) regular session, oldest
    /// first, one per [`Quote::session_points`] interval.
    pub intraday: Vec<f64>,
    /// Points in a full session, so a session in progress can be drawn to
    /// scale (78 for 6.5 hours at 5 minutes).
    pub session_points: usize,
    pub market_open: bool,
}

impl Quote {
    pub fn change(&self) -> f64 {
        self.price - self.previous_close
    }

    /// Change since the previous close, in percent.
    pub fn change_percent(&self) -> f64 {
        if self.previous_close == 0.0 { 0.0 } else { self.change() / self.previous_close * 100.0 }
    }
}
