//! Instrumentation des modes de mesure : défilement automatisé et latence du thread d'interface.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Sonde de latence : un thread soumet une tâche vide au thread d'interface toutes les 50 ms et
/// mesure le délai avant son exécution. Un délai élevé signifie que le thread d'interface était
/// occupé (ou bloqué) pendant ce temps.
pub struct LatencyProbe {
    samples: Mutex<Vec<f64>>,
    stop: AtomicBool,
}

pub const PROBE_PERIOD: Duration = Duration::from_millis(50);

impl LatencyProbe {
    pub fn start() -> Arc<Self> {
        let probe = Arc::new(Self {
            samples: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let p = probe.clone();
        let _ = std::thread::Builder::new()
            .name("fx-latency".into())
            .spawn(move || {
                while !p.stop.load(Ordering::Relaxed) {
                    std::thread::sleep(PROBE_PERIOD);
                    let sent = Instant::now();
                    let p2 = p.clone();
                    if slint::invoke_from_event_loop(move || p2.record(sent.elapsed())).is_err() {
                        break;
                    }
                }
            });
        probe
    }

    fn record(&self, d: Duration) {
        if let Ok(mut s) = self.samples.lock() {
            s.push(d.as_secs_f64() * 1000.0);
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// « latence du thread d'interface : N mesures, p50 …, p99 …, max …, > 50 ms : k ».
    pub fn report(&self) -> String {
        let mut s = self.samples.lock().map(|s| s.clone()).unwrap_or_default();
        if s.is_empty() {
            return "latence du thread d'interface : aucune mesure\n".into();
        }
        s.sort_by(f64::total_cmp);
        let over50 = s.iter().filter(|&&v| v > 50.0).count();
        let over100 = s.iter().filter(|&&v| v > 100.0).count();
        format!(
            "latence du thread d'interface : {} mesures, p50 {:.1} ms, p99 {:.1} ms, max {:.1} ms, > 50 ms : {over50}, > 100 ms : {over100}\n",
            s.len(),
            percentile(&s, 0.5),
            percentile(&s, 0.99),
            s.last().copied().unwrap_or(f64::NAN),
        )
    }
}

pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Scénarios de défilement : (nom, durée en s, vitesse en px/s ; 0 = sauts aléatoires).
pub const SCENARIOS: [(&str, f64, f64); 3] = [
    ("molette", 4.0, 1_500.0),
    ("rapide", 4.0, 15_000.0),
    ("sauts", 4.0, 0.0),
];

pub struct Phase {
    pub name: &'static str,
    pub start: Instant,
    pub end: Instant,
    pub cpu_start_ms: f64,
    pub cpu_end_ms: f64,
}

/// Défilement automatisé en cours.
pub struct ScrollRun {
    pub started: Instant,
    pub phases: Vec<Phase>,
    /// Début de chaque image (`RedrawRequested`).
    pub frames: Vec<Instant>,
    pub rng: u64,
    /// Minuterie qui pilote le défilement ; gardée vivante tant que la mesure dure.
    pub _driver: slint::Timer,
}

impl ScrollRun {
    /// Position de défilement à l'instant présent, ou `None` quand les scénarios sont finis.
    pub fn position(&mut self, max: f64) -> Option<f64> {
        let t = self.started.elapsed().as_secs_f64();
        let mut acc = 0.0;
        for (name, dur, speed) in SCENARIOS {
            if t < acc + dur {
                let now = Instant::now();
                let cpu = fx_win::process::cpu_ms().unwrap_or(f64::NAN);
                if self.phases.last().is_none_or(|p| p.name != name) {
                    self.phases.push(Phase {
                        name,
                        start: now,
                        end: now,
                        cpu_start_ms: cpu,
                        cpu_end_ms: cpu,
                    });
                }
                if let Some(p) = self.phases.last_mut() {
                    p.end = now;
                    p.cpu_end_ms = cpu;
                }
                return Some(if speed > 0.0 {
                    ((t - acc) * speed) % max.max(1.0)
                } else {
                    let mut x = self.rng;
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    self.rng = x;
                    (x % 1_000_000) as f64 / 1_000_000.0 * max
                });
            }
            acc += dur;
        }
        None
    }

    pub fn report(&self) -> String {
        let mut out = String::new();
        for ph in &self.phases {
            let frames: Vec<Instant> = self
                .frames
                .iter()
                .copied()
                .filter(|t| *t >= ph.start && *t <= ph.end)
                .collect();
            let mut iv: Vec<f64> = frames
                .windows(2)
                .map(|w| (w[1] - w[0]).as_secs_f64() * 1000.0)
                .collect();
            iv.sort_by(f64::total_cmp);
            let dur = (ph.end - ph.start).as_secs_f64().max(1e-6);
            let cpu = (ph.cpu_end_ms - ph.cpu_start_ms) / (dur * 1000.0) * 100.0;
            out += &format!(
                "  {:<8} {:>6.1} i/s · intervalle p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms · > 16,7 ms : {} · CPU {:.0} % d'un cœur\n",
                ph.name,
                frames.len() as f64 / dur,
                percentile(&iv, 0.5),
                percentile(&iv, 0.99),
                iv.last().copied().unwrap_or(f64::NAN),
                iv.iter().filter(|&&v| v > 16.7).count(),
                cpu,
            );
        }
        out
    }
}
