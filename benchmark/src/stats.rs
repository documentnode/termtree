//! Median, quartiles, and IQR (spec FR-12: "median and interquartile
//! range -- never a single number or a percentage-delta claim").
//!
//! **There is deliberately no percentage-delta function anywhere in this
//! module or this crate.** FR-12 forbids "up to X% faster" phrasing in the
//! published output; the cheapest way to guarantee that is for the
//! capability not to exist in the codebase (design §5.8).

use crate::result::{Aggregate, Sample};
use std::collections::BTreeMap;

/// Linear-interpolation quartiles (the common definition; matches e.g.
/// NumPy's default `'linear'` method), computed over a **sorted** copy of
/// `values`. Returns `None` for an empty slice.
pub fn median(values: &[f64]) -> Option<f64> {
  percentile(values, 0.5)
}

pub fn q1(values: &[f64]) -> Option<f64> {
  percentile(values, 0.25)
}

pub fn q3(values: &[f64]) -> Option<f64> {
  percentile(values, 0.75)
}

pub fn iqr(values: &[f64]) -> Option<f64> {
  Some(q3(values)? - q1(values)?)
}

fn percentile(values: &[f64], fraction: f64) -> Option<f64> {
  if values.is_empty() {
    return None;
  }
  let mut sorted: Vec<f64> = values.to_vec();
  sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
  if sorted.len() == 1 {
    return Some(sorted[0]);
  }
  let rank = fraction * (sorted.len() - 1) as f64;
  let lower = rank.floor() as usize;
  let upper = rank.ceil() as usize;
  if lower == upper {
    return Some(sorted[lower]);
  }
  let weight = rank - lower as f64;
  Some(sorted[lower] * (1.0 - weight) + sorted[upper] * weight)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
  pub median: f64,
  pub q1: f64,
  pub q3: f64,
  pub iqr: f64,
  pub n: u32,
}

pub fn summarize(values: &[f64]) -> Option<Summary> {
  Some(Summary {
    median: median(values)?,
    q1: q1(values)?,
    q3: q3(values)?,
    iqr: iqr(values)?,
    n: values.len() as u32,
  })
}

/// The named metrics this crate publishes per (subject, tier) sample --
/// design §5.10's column list, plus the N-session split. Extracted here,
/// once, so [`compute_aggregates`] and any future consumer agree on which
/// field of a [`Sample`] each metric name reads.
fn metrics_of(sample: &Sample) -> Vec<(&'static str, f64)> {
  let mut metrics = Vec::new();
  if let Some(memory) = &sample.memory {
    metrics.push((
      "memPhysFootprintBytes",
      memory.mem_phys_footprint_bytes as f64,
    ));
    metrics.push((
      "memPhysFootprintProcessSumBytes",
      memory.mem_phys_footprint_process_sum_bytes as f64,
    ));
    metrics.push((
      "sharedPageDoubleCountBytes",
      memory.shared_page_double_count_bytes as f64,
    ));
    metrics.push(("freeRamDeltaBytes", memory.free_ram_delta_bytes as f64));
    metrics.push((
      "hostMemoryUsedDeltaBytes",
      memory.host_memory_used_delta_bytes as f64,
    ));
    metrics.push((
      "orchestratorAttributableBytes",
      memory.orchestrator_attributable_bytes as f64,
    ));
    metrics.push((
      "agentCliAttributableBytes",
      memory.agent_cli_attributable_bytes as f64,
    ));
  }
  if let Some(cold_start) = &sample.cold_start {
    if let Some(value) = cold_start.main_window_visible_ms {
      metrics.push(("mainWindowVisibleMs", value as f64));
    }
    if let Some(value) = cold_start.app_window_ready_ms {
      metrics.push(("appWindowReadyMs", value as f64));
    }
    if let Some(value) = cold_start.splash_close_ms {
      metrics.push(("splashCloseMs", value as f64));
    }
  }
  if let Some(idle_cpu) = &sample.idle_cpu {
    metrics.push((
      "idleCpuPercentOfOneCore",
      idle_cpu.idle_cpu_percent_of_one_core_median,
    ));
  }
  metrics
}

/// Aggregates every valid, non-calibration sample into one [`Aggregate`]
/// per (subject, tier, metric): median/q1/q3/iqr over that group's values,
/// plus `n` and a `discardedCount`/`discardedReasons` histogram scoped to
/// the same (subject, tier) (spec FR-12, design §5.8).
///
/// Recomputed from the full sample list on every write, so an interrupted
/// run still has valid partial aggregates (design §7). Pure -- takes the
/// sample list already collected, never touches the filesystem itself.
pub fn compute_aggregates(samples: &[Sample]) -> Vec<Aggregate> {
  let mut discarded_count: BTreeMap<(String, String), u32> = BTreeMap::new();
  let mut discarded_reasons: BTreeMap<(String, String), BTreeMap<String, u32>> =
    BTreeMap::new();
  for sample in samples {
    if !sample.is_valid {
      let key = (sample.subject_id.clone(), sample.tier.clone());
      *discarded_count.entry(key.clone()).or_insert(0) += 1;
      if let Some(reason) = &sample.invalid_reason {
        *discarded_reasons
          .entry(key)
          .or_default()
          .entry(reason.clone())
          .or_insert(0) += 1;
      }
    }
  }

  let mut groups: BTreeMap<(String, String, &'static str), Vec<f64>> =
    BTreeMap::new();
  for sample in samples {
    if !sample.is_valid || sample.is_calibration {
      continue;
    }
    for (metric, value) in metrics_of(sample) {
      groups
        .entry((sample.subject_id.clone(), sample.tier.clone(), metric))
        .or_default()
        .push(value);
    }
  }

  groups
    .into_iter()
    .filter_map(|((subject_id, tier, metric), values)| {
      let summary = summarize(&values)?;
      let key = (subject_id.clone(), tier.clone());
      Some(Aggregate {
        subject_id,
        tier,
        metric: metric.to_string(),
        median: summary.median,
        q1: summary.q1,
        q3: summary.q3,
        iqr: summary.iqr,
        n: summary.n,
        discarded_count: discarded_count.get(&key).copied().unwrap_or(0),
        discarded_reasons: discarded_reasons
          .get(&key)
          .cloned()
          .unwrap_or_default(),
        derivation: "measured".to_string(),
      })
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn median_of_odd_length() {
    assert_eq!(median(&[1.0, 3.0, 2.0]), Some(2.0));
  }

  #[test]
  fn median_of_even_length_interpolates() {
    assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), Some(2.5));
  }

  #[test]
  fn median_of_single_element() {
    assert_eq!(median(&[42.0]), Some(42.0));
  }

  #[test]
  fn median_of_empty_is_none() {
    assert_eq!(median(&[]), None);
    assert_eq!(summarize(&[]), None);
  }

  #[test]
  fn quartiles_and_iqr_on_a_known_set() {
    let values = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0];
    assert_eq!(median(&values), Some(5.0));
    assert_eq!(q1(&values), Some(3.0));
    assert_eq!(q3(&values), Some(7.0));
    assert_eq!(iqr(&values), Some(4.0));
  }

  #[test]
  fn summarize_reports_n() {
    let summary = summarize(&[1.0, 2.0, 3.0]).unwrap();
    assert_eq!(summary.n, 3);
    assert_eq!(summary.median, 2.0);
  }

  #[test]
  fn unsorted_input_is_handled() {
    assert_eq!(median(&[9.0, 1.0, 5.0]), Some(5.0));
  }

  // --- compute_aggregates --------------------------------------------

  fn memory_record(
    mem_phys_footprint_bytes: u64,
  ) -> crate::result::MemoryRecord {
    crate::result::MemoryRecord {
      mem_phys_footprint_bytes,
      mem_phys_footprint_process_sum_bytes: mem_phys_footprint_bytes,
      shared_page_double_count_bytes: 0,
      cross_partition_shared_bytes: 0,
      mem_rss_bytes: 0,
      mem_rss_method: "naive-per-process-sum".into(),
      orchestrator_attributable_bytes: mem_phys_footprint_bytes,
      agent_cli_attributable_bytes: 0,
      core_process_bytes: None,
      render_helper_bytes: None,
      free_ram_before_bytes: 0,
      free_ram_after_bytes: 0,
      free_ram_delta_bytes: 0,
      free_ram_delta_sign: "positive-means-consumed".into(),
      host_memory_used_before_bytes: 0,
      host_memory_used_after_bytes: 0,
      host_memory_used_delta_bytes: 0,
      compressor_occupied_delta_bytes: 0,
      swapouts_delta: 0,
      orchestrator_free_ram_delta_bytes: None,
      agent_cli_free_ram_delta_bytes: None,
      free_ram_split_derivation: None,
    }
  }

  fn sample(
    subject_id: &str,
    tier: &str,
    repetition: u32,
    is_calibration: bool,
    is_valid: bool,
    invalid_reason: Option<&str>,
    mem_phys_footprint_bytes: Option<u64>,
  ) -> Sample {
    Sample {
      sample_id: format!("{subject_id}/{tier}/{repetition:03}"),
      subject_id: subject_id.to_string(),
      tier: tier.to_string(),
      session_count: 0,
      repetition,
      is_calibration,
      sampled_at: "2026-08-25T02:31:07Z".into(),
      is_valid,
      invalid_reason: invalid_reason.map(str::to_string),
      attribution: None,
      memory: mem_phys_footprint_bytes.map(memory_record),
      cold_start: None,
      idle_cpu: None,
      quiesce: None,
      warm_helper_count: None,
      helper_kill_count: None,
    }
  }

  #[test]
  fn aggregates_only_valid_non_calibration_samples() {
    let samples = vec![
      sample(
        "termtree",
        "fresh-launch",
        0,
        true,
        false,
        Some("calibration-discard"),
        Some(999),
      ),
      sample("termtree", "fresh-launch", 1, false, true, None, Some(100)),
      sample("termtree", "fresh-launch", 2, false, true, None, Some(200)),
      sample(
        "termtree",
        "fresh-launch",
        3,
        false,
        false,
        Some("warm-webview"),
        Some(500),
      ),
    ];
    let aggregates = compute_aggregates(&samples);
    let footprint_row = aggregates
      .iter()
      .find(|a| a.metric == "memPhysFootprintBytes")
      .unwrap();
    assert_eq!(footprint_row.n, 2);
    assert_eq!(footprint_row.median, 150.0);
    assert_eq!(footprint_row.discarded_count, 2);
    assert_eq!(
      footprint_row.discarded_reasons.get("calibration-discard"),
      Some(&1)
    );
    assert_eq!(
      footprint_row.discarded_reasons.get("warm-webview"),
      Some(&1)
    );
  }

  #[test]
  fn aggregates_are_scoped_per_subject_and_tier() {
    let samples = vec![
      sample("termtree", "fresh-launch", 1, false, true, None, Some(100)),
      sample(
        "collaborator",
        "fresh-launch",
        1,
        false,
        true,
        None,
        Some(9000),
      ),
    ];
    let aggregates = compute_aggregates(&samples);
    let termtree_row = aggregates
      .iter()
      .find(|a| {
        a.subject_id == "termtree" && a.metric == "memPhysFootprintBytes"
      })
      .unwrap();
    let collaborator_row = aggregates
      .iter()
      .find(|a| {
        a.subject_id == "collaborator" && a.metric == "memPhysFootprintBytes"
      })
      .unwrap();
    assert_eq!(termtree_row.median, 100.0);
    assert_eq!(collaborator_row.median, 9000.0);
  }

  #[test]
  fn a_sample_with_no_memory_record_contributes_no_memory_metric() {
    let samples = vec![sample(
      "termtree",
      "fresh-launch",
      1,
      false,
      true,
      None,
      None,
    )];
    let aggregates = compute_aggregates(&samples);
    assert!(aggregates
      .iter()
      .all(|a| a.metric != "memPhysFootprintBytes"));
  }

  #[test]
  fn empty_sample_list_produces_no_aggregates() {
    assert!(compute_aggregates(&[]).is_empty());
  }
}
