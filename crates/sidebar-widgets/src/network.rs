use std::collections::VecDeque;

use sidebar_core::source::{LinkKind, NetSample};
use sidebar_core::text::line_height;
use sidebar_core::{Color, DrawCtx, MeasureCtx, Orientation, Rect, Source, UpdateCtx, Widget};

use crate::meter::{self, COMPACT_GRAPH, GAP, GRAPH_HEIGHT, HISTORY, ROW_SIZE, SECTION_GAP};
use crate::{CARD_PAD, Every, SAMPLE_INTERVAL, card, header, metric_extent};

/// Info rows under the details: IPv4, IPv6, gateway.
const INFO_ROWS: usize = 3;
/// Smallest full scale of the traffic chart, so an idle link reads as idle.
const MIN_SCALE: f64 = 100_000.0;

/// Wi-Fi signal in `0.0..=1.0`: -100 dBm is nothing, -50 dBm or better is
/// full (the mapping NetworkManager uses).
fn signal_fraction(dbm: i32) -> f32 {
    ((dbm + 100) as f32 / 50.0).clamp(0.0, 1.0)
}

/// Green when strong, orange when fair, red when weak.
fn signal_color(fraction: f32) -> Color {
    match fraction {
        f if f >= 0.6 => meter::color(2),
        f if f >= 0.35 => meter::color(1),
        _ => meter::color(6),
    }
}

/// The smallest 1, 2 or 5 × 10ⁿ at or above `v` (and [`MIN_SCALE`]).
fn nice_scale(v: f64) -> f64 {
    let v = v.max(MIN_SCALE);
    let decade = 10f64.powf(v.log10().floor());
    [1.0, 2.0, 5.0, 10.0].into_iter().map(|m| m * decade).find(|s| *s >= v).unwrap_or(v)
}

/// "↓4.1M ↑49K" for the header.
fn rates(sample: Option<&NetSample>) -> String {
    let rate = |r: Option<f64>| r.map_or("-".into(), meter::format_bytes);
    match sample {
        Some(s) => format!("↓{} ↑{}", rate(s.down_rate), rate(s.up_rate)),
        None => "offline".into(),
    }
}

/// The connection (Wi-Fi network, link speed, addresses, signal), traffic
/// now, this session and since the interface came up, and a chart of
/// download and upload rates. Laid out like the CPU and GPU widgets, with
/// the same compact fallback.
pub struct NetGraph {
    source: Box<dyn Source<NetSample>>,
    every: Every,
    last: Option<NetSample>,
    /// (down, up) rates, oldest first; `None` while offline.
    history: VecDeque<(Option<f64>, Option<f64>)>,
}

impl NetGraph {
    pub fn new(source: Box<dyn Source<NetSample>>) -> Self {
        Self { source, every: Every::new(SAMPLE_INTERVAL), last: None, history: VecDeque::with_capacity(HISTORY) }
    }

    fn full_height(&self) -> f32 {
        let header = line_height(crate::HEADER_SIZE) + GAP;
        let info = INFO_ROWS as f32 * meter::title_height() + meter::row_height() + SECTION_GAP;
        let traffic = meter::table_height(2) + SECTION_GAP;
        header + meter::details_height(2) + info + traffic + meter::chart_height(GRAPH_HEIGHT) + 2.0 * CARD_PAD
    }

    fn peak(&self) -> f64 {
        self.history.iter().flat_map(|(d, u)| [*d, *u]).flatten().fold(0.0, f64::max)
    }

    /// Download as an area with a line, upload as a line, scaled to the
    /// recent peak.
    fn draw_traffic(&self, ctx: &mut DrawCtx<'_>, graph: Rect) {
        let scale = nice_scale(self.peak());
        let fraction = |r: Option<f64>| r.map(|r| (r / scale) as f32);
        let down: Vec<Option<f32>> = self.history.iter().map(|(d, _)| fraction(*d)).collect();
        let up: Vec<Option<f32>> = self.history.iter().map(|(_, u)| fraction(*u)).collect();
        meter::load_chart(ctx, graph, &down, &[(meter::line_color(0), down.clone()), (meter::line_color(1), up)]);
    }

    /// Network name (Wi-Fi) or link type, then interface and link speed.
    fn details(&self) -> [String; 2] {
        let Some(s) = &self.last else { return ["Offline".into(), "no default route".into()] };
        let name = match (s.kind, &s.ssid) {
            (LinkKind::Wireless, Some(ssid)) => match s.frequency_mhz {
                Some(mhz) => format!("{ssid} ({:.0} GHz)", (mhz as f32 / 1000.0).floor()),
                None => ssid.clone(),
            },
            (LinkKind::Wireless, None) => "Wi-Fi".into(),
            (LinkKind::Wired, _) => "Ethernet".into(),
            (LinkKind::Other, _) => "Network".into(),
        };
        let link = match s.link_mbps {
            Some(mbps) => format!("{} · {mbps:.0} Mb/s", s.interface),
            None => s.interface.clone(),
        };
        [name, link]
    }
}

impl Widget for NetGraph {
    fn name(&self) -> &str {
        "network"
    }

    fn measure(&self, _ctx: &MeasureCtx<'_>, orientation: Orientation, _cross_extent: f32) -> f32 {
        match orientation {
            Orientation::Vertical => self.full_height(),
            Orientation::Horizontal => metric_extent(orientation, COMPACT_GRAPH),
        }
    }

    fn update(&mut self, ctx: &UpdateCtx) -> bool {
        if !self.every.due(ctx.now) {
            return false;
        }
        self.last = self.source.sample();
        if self.history.len() == HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(self.last.as_ref().map_or((None, None), |s| (s.down_rate, s.up_rate)));
        true
    }

    fn draw(&self, ctx: &mut DrawCtx<'_>) {
        let compact = ctx.bounds.height + 0.5 < self.full_height();
        let area = card(ctx);
        let rest = header(ctx, area, "NET", &rates(self.last.as_ref()));
        if compact {
            self.draw_traffic(ctx, rest);
            return;
        }
        let fg = ctx.theme.foreground;
        let [name, link] = self.details();
        let mut y = meter::details(ctx, rest, vec![(name, meter::color(2)), (link, fg.with_alpha(200))]);
        let at = |y| Rect::new(rest.x, y, rest.width, 0.0);

        let last = self.last.as_ref();
        let text = |v: Option<String>| v.unwrap_or_else(|| "-".into());
        y = meter::title(ctx, at(y), "IPv4", text(last.and_then(|s| s.ipv4).map(|a| a.to_string())), fg);
        y = meter::title(ctx, at(y), "IPv6", text(last.and_then(|s| s.ipv6).map(|a| a.to_string())), fg);
        y = meter::title(ctx, at(y), "Gateway", text(last.and_then(|s| s.gateway).map(|a| a.to_string())), fg);
        let signal = last.and_then(|s| s.signal_dbm).map(signal_fraction);
        let label_w = ctx.text.measure("Signal", ROW_SIZE).width;
        let color = signal.map_or(fg.with_alpha(170), signal_color);
        meter::bar_row(ctx, meter::row(rest, y), label_w, "Signal".into(), signal, meter::percent(signal), color);
        y += meter::row_height() + SECTION_GAP;

        let bytes = |v: Option<u64>| v.map_or("-".into(), |b| meter::format_bytes(b as f64));
        let rate = |v: Option<f64>| v.map_or("-".into(), meter::format_rate);
        let rows = [
            ("↓".to_string(), meter::color(0), [rate(last.and_then(|s| s.down_rate)), bytes(last.map(|s| s.session_down)), bytes(last.map(|s| s.total_down))]),
            ("↑".to_string(), meter::color(1), [rate(last.and_then(|s| s.up_rate)), bytes(last.map(|s| s.session_up)), bytes(last.map(|s| s.total_up))]),
        ];
        let label_w = ctx.text.measure("↓", ROW_SIZE).width;
        y = meter::table(ctx, at(y), label_w, "Data", ["Now", "Session", "Total"], &rows) + SECTION_GAP;

        y = meter::title(ctx, at(y), "Traffic", format!("peak {}", meter::format_rate(self.peak())), fg);
        self.draw_traffic(ctx, Rect::new(rest.x, y, rest.width, GRAPH_HEIGHT));
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::meter::{area_color, line_color};
    use crate::testing::{drawn_text, rects_in};
    use sidebar_core::Theme;

    fn sample(down: f64, up: f64) -> NetSample {
        NetSample {
            interface: "wlan0".into(),
            kind: LinkKind::Wireless,
            ssid: Some("HomeNet".into()),
            frequency_mhz: Some(5180),
            signal_dbm: Some(-52),
            link_mbps: Some(780.0),
            ipv4: Some("192.168.1.20".parse().unwrap()),
            ipv6: None,
            gateway: Some("192.168.1.1".parse().unwrap()),
            down_rate: Some(down),
            up_rate: Some(up),
            session_down: 3_100_000_000,
            session_up: 55_000_000,
            total_down: 17_000_000_000,
            total_up: 578_000_000,
        }
    }

    #[test]
    fn shows_connection_and_traffic() {
        let mut samples = [Some(sample(4_310_000.0, 50_484.0)), None].into_iter();
        let mut net = NetGraph::new(Box::new(move || samples.next().flatten()));
        let h = net.full_height();
        let t0 = SystemTime::UNIX_EPOCH;
        assert!(net.update(&UpdateCtx { now: t0 }));
        let text = drawn_text(&net, 148.0, h);
        assert_eq!(
            text,
            [
                "NET", "↓4.1M ↑49K", "HomeNet (5 GHz)", "wlan0 · 780 Mb/s",
                "IPv4", "192.168.1.20", "IPv6", "-", "Gateway", "192.168.1.1", "Signal", "96%",
                "Data", "Now", "Session", "Total", "↓", "4.1M/s", "2.9G", "16G", "↑", "49K/s", "52M", "551M",
                "Traffic", "peak 4.1M/s",
            ]
        );
        // Download fills the chart and is drawn as a line, as is upload.
        assert_eq!(rects_in(&net, 148.0, h, area_color(Theme::default().accent)).len(), 1);
        assert!(!rects_in(&net, 148.0, h, line_color(1)).is_empty());

        // Offline: no source sample.
        assert!(net.update(&UpdateCtx { now: t0 + Duration::from_secs(1) }));
        assert_eq!(drawn_text(&net, 148.0, h)[..4], ["NET", "offline", "Offline", "no default route"]);
    }

    #[test]
    fn wired_and_compact() {
        let wired = NetSample { kind: LinkKind::Wired, ssid: None, frequency_mhz: None, signal_dbm: None, link_mbps: Some(1000.0), interface: "enp4s0".into(), ..sample(0.0, 0.0) };
        let mut net = NetGraph::new(Box::new(move || Some(wired.clone())));
        net.update(&UpdateCtx { now: SystemTime::UNIX_EPOCH });
        let text = drawn_text(&net, 148.0, net.full_height());
        assert_eq!(text[2..4], ["Ethernet", "enp4s0 · 1000 Mb/s"]);
        assert_eq!(text[10..12], ["Signal", "-"]);
        assert_eq!(drawn_text(&net, 120.0, 40.0), ["NET", "↓0B ↑0B"]);
    }

    #[test]
    fn scales_and_signal() {
        assert_eq!(nice_scale(0.0), MIN_SCALE);
        assert_eq!(nice_scale(4_310_000.0), 5_000_000.0);
        assert_eq!(nice_scale(2_000_000.0), 2_000_000.0);
        assert_eq!(signal_fraction(-52), 0.96);
        assert_eq!(signal_fraction(-30), 1.0);
        assert_eq!(signal_fraction(-110), 0.0);
    }
}
