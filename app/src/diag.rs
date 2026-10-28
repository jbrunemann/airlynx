//! Battery and wakeup diagnostics for judging what listening costs. Shown on the Settings page and
//! optionally logged once a minute, so a test can run with the screen off and be read back afterwards.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use airlynx_core::receive::DEFAULT_POLL_INTERVAL;
use airlynx_core::stats::{self, Stats};

use crate::android::{self, Keep, PowerInfo};

const LOG_INTERVAL: Duration = Duration::from_secs(60);
const REFRESH_INTERVAL: Duration = Duration::from_secs(2);
/// The battery level moves in 1% steps, so a rate measured over less than this is mostly noise.
const MIN_DRAIN_WINDOW: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
struct Sample {
    at: Instant,
    power: Option<PowerInfo>,
    cpu_seconds: Option<f64>,
    stats: Stats,
}

impl Sample {
    fn now() -> Self {
        Self {
            at: Instant::now(),
            power: android::power_info(),
            cpu_seconds: stats::process_cpu_seconds(),
            stats: stats::snapshot(),
        }
    }
}

/// Things to switch off one at a time to find out which of them costs battery. The defaults are how the
/// app normally runs. Every log line records the combination, so one file can hold several runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Experiments {
    /// Off is the control run: no receiver, service or locks.
    pub receiver: bool,
    pub wake_lock: bool,
    pub wifi_lock: bool,
    pub multicast_lock: bool,
    pub slow_poll: bool,
}

impl Default for Experiments {
    fn default() -> Self {
        Self { receiver: true, wake_lock: true, wifi_lock: true, multicast_lock: true, slow_poll: false }
    }
}

impl Experiments {
    pub fn keep(&self) -> Keep {
        Keep { wake_lock: self.wake_lock, wifi_lock: self.wifi_lock, multicast_lock: self.multicast_lock }
    }

    pub fn poll_interval(&self) -> Duration {
        if self.slow_poll { Duration::from_secs(1) } else { DEFAULT_POLL_INTERVAL }
    }

    /// Compact and free of spaces, so a log line stays easy to split.
    fn label(&self) -> String {
        let flag = |on: bool| u8::from(on);
        format!(
            "rx:{},wake:{},wifi:{},mcast:{},poll:{}",
            flag(self.receiver),
            flag(self.wake_lock),
            flag(self.wifi_lock),
            flag(self.multicast_lock),
            self.poll_interval().as_millis()
        )
    }
}

pub struct Diagnostics {
    /// Shared with the logging thread, which tags every line with it.
    experiments: Arc<Mutex<Experiments>>,
    /// Shared with the logging thread, which reports everything relative to it.
    baseline: Arc<Mutex<Sample>>,
    current: Sample,
    /// Set while the logging thread should keep running.
    logging: Option<Arc<AtomicBool>>,
    /// The log file of the current or last run.
    log_path: Option<PathBuf>,
    /// Why logging could not start.
    pub log_error: Option<String>,
}

impl Diagnostics {
    pub fn new() -> Self {
        let now = Sample::now();
        Self {
            experiments: Arc::default(),
            baseline: Arc::new(Mutex::new(now)),
            current: now, logging: None,
            log_path: None,
            log_error: None,
        }
    }

    /// Takes a new sample if the last one is old, so it is cheap to call every frame.
    pub fn refresh(&mut self) {
        if self.current.at.elapsed() >= REFRESH_INTERVAL {
            self.current = Sample::now();
        }
    }

    /// Starts measuring from now.
    pub fn reset(&mut self) {
        self.current = Sample::now();
        *self.baseline.lock().unwrap() = self.current;
    }

    pub fn experiments(&self) -> Experiments {
        *self.experiments.lock().unwrap()
    }

    /// Records the new combination and starts measuring from now, so each combination is its own segment.
    /// The caller applies it (restarting the receiver and its locks).
    pub fn set_experiments(&mut self, experiments: Experiments) {
        *self.experiments.lock().unwrap() = experiments;
        self.reset();
    }

    pub fn logging(&self) -> bool {
        self.logging.is_some()
    }

    pub fn log_path(&self) -> Option<&Path> {
        self.log_path.as_deref()
    }

    /// Appends one line a minute to `path` until turned off, so a test can run with the phone locked and
    /// the file be sent off the device afterwards. Turning logging on also resets the baseline, so each
    /// run in the file is self-contained.
    pub fn set_logging(&mut self, on: bool, path: &Path) {
        if let Some(flag) = self.logging.take() {
            flag.store(false, Ordering::Relaxed);
        }
        if !on {
            return;
        }
        let mut file = match OpenOptions::new().create(true).append(true).open(path) {
            Ok(file) => file,
            Err(e) => {
                self.log_error = Some(format!("Cannot write {}: {e}", path.display()));
                return;
            }
        };
        self.log_error = None;
        self.log_path = Some(path.to_path_buf());
        self.reset();
        let header = format!("# AirLynx {} on {}", env!("CARGO_PKG_VERSION"), android::device_name());
        if let Err(e) = writeln!(file, "{header}") {
            self.log_error = Some(format!("Cannot write {}: {e}", path.display()));
            return;
        }
        let flag = Arc::new(AtomicBool::new(true));
        self.logging = Some(flag.clone());
        let baseline = self.baseline.clone();
        let experiments = self.experiments.clone();
        thread::spawn(move || {
            while flag.load(Ordering::Relaxed) {
                let base = *baseline.lock().unwrap();
                let exp = *experiments.lock().unwrap();
                let line = format!("ts={} {}", unix_seconds(), log_line(&base, &Sample::now(), &exp));
                log::info!("diag {line}");
                // Flushed every line, so little is lost if the process is killed.
                if let Err(e) = writeln!(file, "{line}").and_then(|()| file.flush()) {
                    log::warn!("diag log write failed: {e}");
                }
                thread::sleep(LOG_INTERVAL);
            }
        });
    }

    /// Label and value pairs for the Settings card.
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        let base = *self.baseline.lock().unwrap();
        let now = &self.current;
        let elapsed = now.at.duration_since(base.at);
        let secs = elapsed.as_secs_f64().max(0.001);
        let per_second = |now: u64, base: u64| now.saturating_sub(base) as f64 / secs;

        let mut rows = vec![("Measuring for", duration(elapsed))];
        match now.power {
            Some(p) => {
                let state = if p.charging { "charging" } else { "on battery" };
                rows.push(("Battery", format!("{}%, {state}, {:.1} C", p.percent, p.temperature_c)));
                rows.push(("Drain", drain(&base, now)));
                let doze = if p.doze { "in Doze" } else { "not in Doze" };
                rows.push(("Screen", format!("{}, {doze}", if p.screen_on { "on" } else { "off" })));
            }
            None => rows.push(("Battery", "unknown".into())),
        }
        let held = android::held_locks();
        rows.push(("Locks", if held.is_empty() { "none held".into() } else { held.join(", ") }));
        if let Some(path) = &self.log_path {
            let size = fs::metadata(path).map_or(0, |m| m.len());
            let state = if self.logging() { "writing" } else { "stopped" };
            rows.push(("Log", format!("{state}, {} KB", size.div_ceil(1024))));
        }

        let threads = stats::thread_count().map_or(String::new(), |n| format!(", {n} threads"));
        rows.push((
            "CPU",
            match (base.cpu_seconds, now.cpu_seconds) {
                (Some(b), Some(n)) => format!("{:.1} s used, {:.2}% average{threads}", n - b, (n - b) / secs * 100.0),
                _ => "unknown".into(),
            },
        ));
        rows.push((
            "Loop wakeups",
            format!(
                "accept {:.1}/s, discovery {:.1}/s",
                per_second(now.stats.accept_polls, base.stats.accept_polls),
                per_second(now.stats.discovery_polls, base.stats.discovery_polls),
            ),
        ));
        rows.push((
            "Activity",
            format!(
                "{} discovery replies, {} sessions",
                now.stats.discovery_replies.saturating_sub(base.stats.discovery_replies),
                now.stats.sessions.saturating_sub(base.stats.sessions)
            ),
        ));
        rows
    }
}

impl Drop for Diagnostics {
    fn drop(&mut self) {
        if let Some(flag) = &self.logging {
            flag.store(false, Ordering::Relaxed);
        }
    }
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self::new()
    }
}

fn drain(base: &Sample, now: &Sample) -> String {
    let (Some(b), Some(n)) = (base.power, now.power) else { return "unknown".into() };
    if b.charging || n.charging {
        return "not measured while charging".into();
    }
    let elapsed = now.at.duration_since(base.at);
    let percent = b.percent - n.percent;
    if elapsed < MIN_DRAIN_WINDOW {
        return format!("{percent}% so far");
    }
    let hours = elapsed.as_secs_f64() / 3600.0;
    match (b.charge_uah, n.charge_uah) {
        (Some(b), Some(n)) => format!("{:.0} mAh/h ({percent}% in {})", (b - n) as f64 / 1000.0 / hours, duration(elapsed)),
        _ => format!("{:.1}%/h ({percent}% in {})", percent as f64 / hours, duration(elapsed)),
    }
}

/// One line with everything needed to work out the drain later, even if the app was restarted.
fn log_line(base: &Sample, now: &Sample, exp: &Experiments) -> String {
    fn opt<T: std::fmt::Display>(value: Option<T>) -> String {
        value.map_or("-".into(), |v| v.to_string())
    }
    let power = now.power;
    let held = android::held_locks();
    let locks = if held.is_empty() { "none".into() } else { held.join("+") };
    format!(
        "up_s={} start_pct={} start_uah={} pct={} uah={} current_raw={} charging={} screen={} doze={} temp_c={} \
         cpu_s={} threads={} accept_polls={} discovery_polls={} replies={} sessions={} locks={} exp={}",
        now.at.duration_since(base.at).as_secs(),
        opt(base.power.map(|p| p.percent)),
        opt(base.power.and_then(|p| p.charge_uah)),
        opt(power.map(|p| p.percent)),
        opt(power.and_then(|p| p.charge_uah)),
        opt(power.and_then(|p| p.current_raw)),
        opt(power.map(|p| p.charging)),
        opt(power.map(|p| p.screen_on)),
        opt(power.map(|p| p.doze)),
        opt(power.map(|p| format!("{:.1}", p.temperature_c))),
        opt(match (base.cpu_seconds, now.cpu_seconds) {
            (Some(b), Some(n)) => Some(format!("{:.2}", n - b)),
            _ => None,
        }),
        opt(stats::thread_count()),
        now.stats.accept_polls.saturating_sub(base.stats.accept_polls),
        now.stats.discovery_polls.saturating_sub(base.stats.discovery_polls),
        now.stats.discovery_replies.saturating_sub(base.stats.discovery_replies),
        now.stats.sessions.saturating_sub(base.stats.sessions),
        locks,
        exp.label(),
    )
}

fn unix_seconds() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn duration(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}
