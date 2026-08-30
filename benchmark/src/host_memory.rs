//! `vm_stat` invocation and its pure parser, plus the two derived host
//! memory quantities the free-RAM path publishes (design §5.3.2).
//!
//! A naive free-page delta understates consumption on a compressing OS:
//! memory an app "consumes" can land in the compressor without ever moving
//! the free-page count. `HostMemorySample` records the raw counters;
//! `free_ram_bytes`/`host_memory_used_bytes` are the two derived readings
//! whose *delta* the harness publishes side by side (`freeRamDeltaBytes`,
//! `hostMemoryUsedDeltaBytes`).

use crate::exec::{run_capture, ExecError};
use std::collections::BTreeMap;

pub const VM_STAT_PROGRAM: &str = "/usr/bin/vm_stat";

#[derive(Debug, Clone, PartialEq)]
pub struct HostMemorySample {
  pub page_size_bytes: u64,
  pub counters: BTreeMap<String, u64>,
}

pub fn invoke_vm_stat() -> Result<String, ExecError> {
  let output = run_capture(VM_STAT_PROGRAM, &[])?;
  Ok(output.stdout)
}

/// Parses the whole `vm_stat` capture. The page size is read from the
/// header line rather than assumed to be 4096 or 16384 -- Apple Silicon
/// hosts use 16384, but the parser must not hard-code it (design §5.3.2).
pub fn parse_vm_stat(text: &str) -> Option<HostMemorySample> {
  let mut lines = text.lines();
  let header = lines.next()?;
  let page_size_bytes = header
    .split("page size of ")
    .nth(1)?
    .split(' ')
    .next()?
    .parse::<u64>()
    .ok()?;

  let mut counters = BTreeMap::new();
  for line in lines {
    // Every data line is `"Key":  <value>.` or `Key:  <value>.`; the quoted
    // form (`"Translation faults":`) must not confuse a colon-based split,
    // since the label itself never contains a colon.
    let (label, value) = line.split_once(':')?;
    let label = label.trim().trim_matches('"').to_string();
    let value = value.trim().trim_end_matches('.');
    if let Ok(parsed) = value.parse::<u64>() {
      counters.insert(label, parsed);
    }
  }

  Some(HostMemorySample {
    page_size_bytes,
    counters,
  })
}

impl HostMemorySample {
  fn counter(&self, key: &str) -> u64 {
    self.counters.get(key).copied().unwrap_or(0)
  }

  /// `(Pages free + Pages speculative) * pageSize` (design §5.3.2).
  pub fn free_ram_bytes(&self) -> u64 {
    (self.counter("Pages free") + self.counter("Pages speculative"))
      * self.page_size_bytes
  }

  /// `(Anonymous pages + Pages wired down + Pages occupied by compressor -
  /// Pages purgeable) * pageSize` -- the compression-aware companion to
  /// `free_ram_bytes` (design §5.3.2).
  pub fn host_memory_used_bytes(&self) -> u64 {
    let used_pages = self.counter("Anonymous pages")
      + self.counter("Pages wired down")
      + self.counter("Pages occupied by compressor");
    used_pages.saturating_sub(self.counter("Pages purgeable"))
      * self.page_size_bytes
  }

  pub fn compressor_occupied_bytes(&self) -> u64 {
    self.counter("Pages occupied by compressor") * self.page_size_bytes
  }

  pub fn swapouts(&self) -> u64 {
    self.counter("Swapouts")
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;

  fn fixture() -> HostMemorySample {
    let text = fs::read_to_string(concat!(
      env!("CARGO_MANIFEST_DIR"),
      "/fixtures/vm_stat.txt"
    ))
    .unwrap();
    parse_vm_stat(&text).unwrap()
  }

  #[test]
  fn page_size_is_read_from_the_header_not_assumed() {
    let sample = fixture();
    assert_eq!(sample.page_size_bytes, 16384);
  }

  #[test]
  fn every_documented_counter_is_read() {
    let sample = fixture();
    for key in [
      "Pages free",
      "Pages active",
      "Pages inactive",
      "Pages speculative",
      "Pages throttled",
      "Pages wired down",
      "Pages purgeable",
      "File-backed pages",
      "Anonymous pages",
      "Pages occupied by compressor",
      "Swapouts",
      "Compressions",
    ] {
      assert!(sample.counters.contains_key(key), "missing {key}");
    }
  }

  #[test]
  fn quoted_translation_faults_key_does_not_confuse_the_splitter() {
    let sample = fixture();
    assert_eq!(
      sample.counters.get("Translation faults"),
      Some(&28_748_548_688)
    );
  }

  #[test]
  fn trailing_dot_is_stripped_from_every_value() {
    let sample = fixture();
    assert_eq!(sample.counters.get("Pages free"), Some(&3920));
  }

  #[test]
  fn derived_quantities_use_the_documented_formulas() {
    let sample = fixture();
    let expected_free = (3920 + 6005) * 16384;
    assert_eq!(sample.free_ram_bytes(), expected_free);

    let expected_used = (177087u64 + 308016 + 410934 - 940) * 16384;
    assert_eq!(sample.host_memory_used_bytes(), expected_used);

    assert_eq!(sample.swapouts(), 112_106_478);
  }
}
