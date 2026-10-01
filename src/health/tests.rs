//! The parsers against real output from a Mac, and the thresholds.

use super::*;

#[test]
fn the_parsers_read_what_macos_prints() {
    assert_eq!(parse_loadavg("{ 1.28 1.32 1.44 }\n"), Some(1.32));
    assert_eq!(
        parse_memory_pressure(
            "The system has 17179869184 (1048576 pages with a page size of 16384).\n\
             System-wide memory free percentage: 65%\n"
        ),
        Some(65)
    );
    let (used, free) =
        parse_swapusage("total = 4096.00M  used = 2976.38M  free = 1119.62M  (encrypted)")
            .expect("swap");
    assert!((used - 2976.38).abs() < 0.01);
    assert!((free - 27.33).abs() < 0.01);
    assert_eq!(
        parse_swapusage("total = 0.00M  used = 0.00M  free = 0.00M  (encrypted)"),
        None
    );
    assert_eq!(
        parse_therm("Note: No thermal warning level has been recorded\n"),
        100
    );
    assert_eq!(
        parse_therm("CPU_Scheduler_Limit \t= 100\nCPU_Speed_Limit \t= 64\n"),
        64
    );
}

#[test]
fn garbage_reads_as_unread_rather_than_as_a_number() {
    assert_eq!(parse_loadavg(""), None);
    assert_eq!(parse_memory_pressure("nothing here"), None);
    assert_eq!(parse_swapusage("swap is off"), None);
}

fn rated(vitals: Vitals) -> Vitals {
    let mut vitals = vitals;
    classify(&mut vitals);
    vitals
}

#[test]
fn the_worst_vital_rates_the_machine_and_leads_the_reasons() {
    let calm = rated(Vitals {
        load_per_core: Some(0.4),
        mem_free_pct: Some(65),
        swap_used_mb: Some(100.0),
        swap_free_pct: Some(90.0),
        cpu_speed_limit: Some(100),
        ..Vitals::default()
    });
    assert_eq!(calm.level, HealthLevel::Green);
    assert!(calm.reasons.is_empty());

    // This Mac on 2026-09-30: swap amber, everything else fine.
    let swapping = rated(Vitals {
        load_per_core: Some(0.17),
        mem_free_pct: Some(65),
        swap_used_mb: Some(2976.0),
        swap_free_pct: Some(27.0),
        cpu_speed_limit: Some(100),
        ..Vitals::default()
    });
    assert_eq!(swapping.level, HealthLevel::Amber);
    assert_eq!(swapping.reasons, ["swap 2.9 GB used, 27% free"]);

    let starved = rated(Vitals {
        load_per_core: Some(1.5),
        mem_free_pct: Some(8),
        ..Vitals::default()
    });
    assert_eq!(starved.level, HealthLevel::Red);
    assert_eq!(starved.reasons, ["memory 8% free", "load 1.5 per core"]);
}

#[test]
fn each_vital_turns_red_at_its_own_threshold() {
    let red = |vitals: Vitals| rated(vitals).level == HealthLevel::Red;
    assert!(red(Vitals {
        load_per_core: Some(2.1),
        ..Vitals::default()
    }));
    assert!(!red(Vitals {
        load_per_core: Some(2.0),
        ..Vitals::default()
    }));
    assert!(red(Vitals {
        mem_free_pct: Some(9),
        ..Vitals::default()
    }));
    assert!(!red(Vitals {
        mem_free_pct: Some(10),
        ..Vitals::default()
    }));
    assert!(red(Vitals {
        swap_used_mb: Some(9000.0),
        swap_free_pct: Some(10.0),
        ..Vitals::default()
    }));
    assert!(!red(Vitals {
        swap_used_mb: Some(9000.0),
        swap_free_pct: Some(25.0),
        ..Vitals::default()
    }));
    assert!(red(Vitals {
        cpu_speed_limit: Some(60),
        ..Vitals::default()
    }));
}

#[test]
fn nothing_read_is_unknown() {
    assert_eq!(rated(Vitals::default()).level, HealthLevel::Unknown);
}
