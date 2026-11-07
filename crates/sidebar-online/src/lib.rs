//! Online data for widgets: weather (Open-Meteo) and stock quotes (Yahoo
//! Finance).
//!
//! Every source polls on its own background thread, so a slow or missing
//! network never blocks the UI; widgets read the latest result through a
//! `sidebar_core::Source`.

mod http;
mod poll;
mod stocks;
mod weather;

pub use stocks::{QuotesReport, yahoo_quotes};
pub use weather::{Location, Report, open_meteo};
