/* src/experiments.rs */
use std::time::Duration;
use quiche::h3::Priority;

use crate::quic::{multi_get_with_priorities, Req, Stat};

/* Tunables */
const TRIALS: usize = 1;            // repetitions per experiment
const UPDATE_DELAY_MS: u64 = 100;  // PRIORITY_UPDATE delay

/// Experiment A: different initial urgencies, repeated.
pub fn experiment_initial_priorities(
    base: &str,
    path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let p = path.as_bytes().to_vec();

    let template = vec![
        Req { path: p.clone(), urgency: 7, incremental: false, tag: "u7" },
        Req { path: p.clone(), urgency: 6, incremental: false, tag: "u6" },
        Req { path: p.clone(), urgency: 3, incremental: false, tag: "u3" },
        Req { path: p.clone(), urgency: 0, incremental: false, tag: "u0" },
    ];

    let mut total_pairs = 0usize;
    let mut total_inversions = 0usize;
    let mut trials_zero_inv = 0usize;

    println!("\n=== Experiment A: initial priorities over {TRIALS} trials ===");

    for t in 1..=TRIALS {
        println!("\n[A] Trial {t}/{TRIALS}");
        let res = multi_get_with_priorities(base, &template, None)?;
        let (pairs, inversions) = finish_order_vs_urgency(&res.rows);
        total_pairs += pairs;
        total_inversions += inversions;
        if inversions == 0 && pairs > 0 { trials_zero_inv += 1; }

        if let Some((tag, urg)) = first_finisher(&res.rows) {
            println!("[A] first finished: {tag} (u={urg})");
        }
        println!("[A] trial verdict: {}  (inversions {inversions}/{pairs})",
                 verdict_from_ratio(pairs, inversions));
    }

    let overall = verdict_from_ratio(total_pairs, total_inversions);
    let ratio = if total_pairs > 0 {
        total_inversions as f64 / total_pairs as f64
    } else { 1.0 };

    println!("\n[A] OVERALL: inversions {total_inversions}/{total_pairs}  (ratio {:.2})", ratio);
    println!("[A] Trials with zero inversions: {trials_zero_inv}/{TRIALS}");
    println!("[A] Conclusion: {overall}");

    Ok(())
}
pub fn experiment_priority_update(
    base: &str,
    path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let p = path.as_bytes().to_vec();

    // Initial order we want: A (u=0) < B (u=3) < C (u=7)
    let reqs = vec![
        Req { path: p.clone(), urgency: 0, incremental: true, tag: "A" },
        Req { path: p.clone(), urgency: 3, incremental: true, tag: "B" },
        Req { path: p.clone(), urgency: 7, incremental: true, tag: "C" },
    ];

    println!("\n=== Experiment B: reorder A,B,C -> C,B,A via PRIORITY_UPDATE over {TRIALS} trials ===");

    let mut matches_cba = 0usize;

    for t in 1..=TRIALS {
        println!("\n[B] Trial {t}/{TRIALS}");

        // After UPDATE_DELAY_MS, flip urgencies to C,B,A:
        //   A -> 7 (least important)
        //   B -> 3 (middle)   (same as initial, but send for completeness)
        //   C -> 0 (most important)
        let updates = vec![
            (0usize, Priority::new(7, true), Duration::from_millis(UPDATE_DELAY_MS)), // A
            (1usize, Priority::new(3, true), Duration::from_millis(UPDATE_DELAY_MS)), // B
            (2usize, Priority::new(0, true), Duration::from_millis(UPDATE_DELAY_MS)), // C
        ];

        let res = multi_get_with_priorities(base, &reqs, Some(updates))?;

        // Evaluate final finish order (by t_fin) -> we expect C,B,A
        let mut rows = res.rows.clone();
        rows.retain(|r| r.t_fin.is_some());
        rows.sort_by(|a, b| a.t_fin.unwrap().partial_cmp(&b.t_fin.unwrap()).unwrap());

        let order: Vec<&'static str> = rows.iter().map(|r| r.tag).collect();
        println!("[B] finish order: {:?}", order);

        if order.starts_with(&["C", "B", "A"]) {
            matches_cba += 1;
            println!("[B] verdict: matches target C,B,A");
        } else {
            println!("[B] verdict: does not match C,B,A");
        }
    }

    println!(
        "\n[B] OVERALL: matched target order C,B,A in {}/{} trials.",
        matches_cba, TRIALS
    );

    Ok(())
}


/* ---------------- helpers ---------------- */

/// Count inversions of finish order vs ascending urgency.
/// Lower `u` should finish earlier if priorities are honored.
fn finish_order_vs_urgency(rows: &[Stat]) -> (usize, usize) {
    // Build (urgency, finish_delta_ms) where finish_delta_ms is time from send to finish.
    let mut finished: Vec<(u8, f64)> = rows
        .iter()
        .filter_map(|r| r.t_fin.map(|tf| (r.urgency, (tf - r.t_sent).as_secs_f64() * 1000.0)))
        .collect();

    // Sort by finish time asc.
    finished.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

    let n = finished.len();
    let mut inversions = 0usize;
    for i in 0..n {
        for j in i + 1..n {
            if finished[i].0 > finished[j].0 {
                inversions += 1;
            }
        }
    }
    let pairs = n * (n - 1) / 2;
    (pairs, inversions)
}

fn verdict_from_ratio(pairs: usize, inversions: usize) -> &'static str {
    if pairs == 0 { return "insufficient data"; }
    if inversions == 0 { return "priorities honored"; }
    let r = inversions as f64 / pairs as f64;
    if r <= 0.25 { "mostly honored" }
    else if r <= 0.5 { "mixed" }
    else { "not honored" }
}

/// Return the first finisher’s (tag, urgency), if any.
fn first_finisher(rows: &[Stat]) -> Option<(&'static str, u8)> {
    let mut v: Vec<_> = rows.iter().filter(|r| r.t_fin.is_some()).collect();
    if v.is_empty() { return None; }
    v.sort_by(|a, b| a.t_fin.unwrap().partial_cmp(&b.t_fin.unwrap()).unwrap());
    let r = v[0];
    Some((r.tag, r.urgency))
}
