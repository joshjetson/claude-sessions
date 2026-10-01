//! How busy the machine is, for the QA queue.
//!
//! A run that starts every free lane at once can bring a laptop to a crawl:
//! each QA session is a `claude` process, a browser and a test server. With
//! `qa.healthGate` on, the dashboard reads four vitals before it starts a QA
//! session and holds the queue while any of them is red.
//!
//! The vitals and their thresholds are the ones the machine-health plugin's
//! governor uses, read here directly rather than by running the plugin: four
//! commands, about 20 ms together, against its 23 commands and about a second.
//!
//! | Vital                         | Amber      | Red        |
//! |-------------------------------|------------|------------|
//! | 5-minute load per CPU core    | above 1.0  | above 2.0  |
//! | Memory free (`memory_pressure`) | below 20% | below 10%  |
//! | Swap used and free            | over 2 GB used and under 35% free | over 8 GB used and under 20% free |
//! | CPU speed limit (thermal)     | below 100  | below 70   |
//!
//! Only red holds the queue. Amber is shown, because it explains a slow
//! machine, but a laptop doing ordinary work sits at amber often enough that
//! holding on it would stop QA for most of the day.

use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::term::Exec;

/// How a reading rates the machine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthLevel {
    /// Nothing could be read: a platform without these commands, or every
    /// one failed. The queue is never held on this.
    #[default]
    Unknown,
    Green,
    Amber,
    Red,
}

/// One reading.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Vitals {
    pub load_per_core: Option<f64>,
    pub mem_free_pct: Option<u8>,
    pub swap_used_mb: Option<f64>,
    pub swap_free_pct: Option<f64>,
    /// `CPU_Speed_Limit` from `pmset -g therm`. 100 when macOS reports no
    /// limit, which is what it prints when nothing is throttled.
    pub cpu_speed_limit: Option<u8>,
    pub level: HealthLevel,
    /// One short phrase per vital that is amber or red, worst first.
    pub reasons: Vec<String>,
    /// When it was taken, RFC 3339.
    pub sampled_at: String,
}

/// `{ 1.28 1.32 1.44 }` → the 5-minute figure.
pub fn parse_loadavg(out: &str) -> Option<f64> {
    out.trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// `System-wide memory free percentage: 65%` → 65.
pub fn parse_memory_pressure(out: &str) -> Option<u8> {
    out.lines()
        .find_map(|line| line.split_once("memory free percentage:"))
        .and_then(|(_, rest)| rest.trim().trim_end_matches('%').trim().parse().ok())
}

/// `total = 4096.00M  used = 2976.38M  free = 1119.62M  (encrypted)` → used
/// MB and free percent. `None` when swap is off (a total of zero).
pub fn parse_swapusage(out: &str) -> Option<(f64, f64)> {
    let field = |name: &str| -> Option<f64> {
        let rest = out.split_once(&format!("{name} = "))?.1;
        let value = rest.split_whitespace().next()?;
        let (number, unit) = value.split_at(value.len().checked_sub(1)?);
        let number: f64 = number.parse().ok()?;
        Some(match unit {
            "G" => number * 1024.0,
            "K" => number / 1024.0,
            _ => number,
        })
    };
    let (total, used, free) = (field("total")?, field("used")?, field("free")?);
    (total > 0.0).then(|| (used, free / total * 100.0))
}

/// `CPU_Speed_Limit = 80` → 80. macOS prints no such line when nothing is
/// throttled, and that reads as 100.
pub fn parse_therm(out: &str) -> u8 {
    out.lines()
        .find_map(|line| line.split_once("CPU_Speed_Limit"))
        .and_then(|(_, rest)| rest.trim().trim_start_matches('=').trim().parse().ok())
        .unwrap_or(100)
}

/// Rate a reading: the worst of its vitals, and why.
pub fn classify(vitals: &mut Vitals) {
    let mut found: Vec<(HealthLevel, String)> = Vec::new();
    if let Some(load) = vitals.load_per_core {
        let level = if load > 2.0 {
            HealthLevel::Red
        } else if load > 1.0 {
            HealthLevel::Amber
        } else {
            HealthLevel::Green
        };
        found.push((level, format!("load {load:.1} per core")));
    }
    if let Some(free) = vitals.mem_free_pct {
        let level = if free < 10 {
            HealthLevel::Red
        } else if free < 20 {
            HealthLevel::Amber
        } else {
            HealthLevel::Green
        };
        found.push((level, format!("memory {free}% free")));
    }
    if let (Some(used), Some(free)) = (vitals.swap_used_mb, vitals.swap_free_pct) {
        let level = if used > 8.0 * 1024.0 && free < 20.0 {
            HealthLevel::Red
        } else if used > 2.0 * 1024.0 && free < 35.0 {
            HealthLevel::Amber
        } else {
            HealthLevel::Green
        };
        found.push((
            level,
            format!("swap {:.1} GB used, {free:.0}% free", used / 1024.0),
        ));
    }
    if let Some(limit) = vitals.cpu_speed_limit {
        let level = if limit < 70 {
            HealthLevel::Red
        } else if limit < 100 {
            HealthLevel::Amber
        } else {
            HealthLevel::Green
        };
        found.push((level, format!("CPU held to {limit}% by heat")));
    }
    vitals.level = found
        .iter()
        .map(|(level, _)| *level)
        .max()
        .unwrap_or(HealthLevel::Unknown);
    found.sort_by_key(|(level, _)| std::cmp::Reverse(*level));
    vitals.reasons = found
        .into_iter()
        .filter(|(level, _)| *level >= HealthLevel::Amber)
        .map(|(_, reason)| reason)
        .collect();
}

/// Take a reading. Each command that fails leaves its vital unread, and a
/// reading with nothing read is [`HealthLevel::Unknown`].
pub fn sample(exec: &Exec) -> Vitals {
    const TIMEOUT: Duration = Duration::from_secs(3);
    let run = |program: &str, args: &[&str]| {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        let output = exec.run(program, &args, TIMEOUT);
        output.ok.then_some(output.stdout)
    };
    let cores = std::thread::available_parallelism()
        .map(|n| n.get() as f64)
        .unwrap_or(1.0);
    let swap = run("sysctl", &["-n", "vm.swapusage"])
        .as_deref()
        .and_then(parse_swapusage);
    let mut vitals = Vitals {
        load_per_core: run("sysctl", &["-n", "vm.loadavg"])
            .as_deref()
            .and_then(parse_loadavg)
            .map(|load| load / cores),
        mem_free_pct: run("memory_pressure", &["-Q"])
            .as_deref()
            .and_then(parse_memory_pressure),
        swap_used_mb: swap.map(|(used, _)| used),
        swap_free_pct: swap.map(|(_, free)| free),
        cpu_speed_limit: run("pmset", &["-g", "therm"]).as_deref().map(parse_therm),
        sampled_at: Utc::now().to_rfc3339(),
        ..Vitals::default()
    };
    classify(&mut vitals);
    vitals
}

#[cfg(test)]
mod tests;
