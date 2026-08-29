//! `CGWindowListCopyWindowInfo` -- the one root-free way to observe "a
//! window of this PID is on screen" (spec FR-4, design §5.4). This is the
//! crate's only platform-FFI module.
//!
//! Only `kCGWindowOwnerPID`, `kCGWindowLayer`, and `kCGWindowBounds` are
//! read. `kCGWindowName` is deliberately never read: without Screen
//! Recording permission macOS redacts window titles, but owner PID, layer,
//! and bounds are returned regardless -- so the probe never needs a
//! permission this crate's NFR-4 (no required root/extra permissions) would
//! otherwise be at odds with.

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::window::{
  copy_window_info, kCGNullWindowID, kCGWindowBounds, kCGWindowLayer,
  kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly,
  kCGWindowOwnerPID,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OnScreenWindow {
  pub owner_pid: u32,
  pub layer: i64,
  /// Width * height, in points, as read from `kCGWindowBounds`.
  pub area_pt: f64,
}

/// Queries every on-screen, non-desktop-chrome window and returns the ones
/// owned by `pid`. There is no fixture for this call -- it talks to the
/// live window server, so it is exercised through `doctor` and the smoke
/// sweep rather than a unit test (design §11: "not unit-tested, by design").
pub fn on_screen_windows_owned_by(pid: u32) -> Vec<OnScreenWindow> {
  let option =
    kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements;
  let Some(array) = copy_window_info(option, kCGNullWindowID) else {
    return Vec::new();
  };
  // `copy_window_info` returns an untyped `CFArray<*const c_void>`; retype
  // it (via the get rule, which retains rather than consumes) to iterate
  // strongly-typed dictionary entries instead of hand-walking void
  // pointers.
  let typed: CFArray<CFDictionary<CFString, CFType>> =
    unsafe { CFArray::wrap_under_get_rule(array.as_concrete_TypeRef()) };
  typed
    .iter()
    .filter_map(|dict| parse_window_dict(&dict))
    .filter(|w| w.owner_pid == pid)
    .collect()
}

fn parse_window_dict(
  dict: &CFDictionary<CFString, CFType>,
) -> Option<OnScreenWindow> {
  // Reading an `extern "C"` static needs `unsafe`; the subsequent
  // `dict.find` calls do not, so the unsafe block is scoped tightly to the
  // static reads alone.
  let (owner_pid_key, layer_key, bounds_key) =
    unsafe { (kCGWindowOwnerPID, kCGWindowLayer, kCGWindowBounds) };

  let owner_pid = dict_number_by_static_key(dict, owner_pid_key)? as u32;
  let layer = dict_number_by_static_key(dict, layer_key).unwrap_or(0.0) as i64;
  // `downcast` only accepts a `ConcreteCFType`, which `CFDictionary` is
  // solely at its default (untyped) parameterization -- so downcast to
  // that first, then retype (get rule: retains, does not consume) to the
  // strongly-typed dictionary the rest of this module works with.
  let bounds_dict: Option<CFDictionary<CFString, CFType>> = dict
    .find(bounds_key)
    .and_then(|value| value.downcast::<CFDictionary>())
    .map(|untyped| unsafe {
      CFDictionary::<CFString, CFType>::wrap_under_get_rule(
        untyped.as_concrete_TypeRef(),
      )
    });
  let area_pt = bounds_dict.map(bounds_area_pt).unwrap_or(0.0);

  Some(OnScreenWindow {
    owner_pid,
    layer,
    area_pt,
  })
}

/// `kCGWindowBounds`'s value is itself a dictionary with plain string keys
/// `"X"`/`"Y"`/`"Width"`/`"Height"` (no `CFStringRef` constant is exported
/// for these by Apple; they are literal dictionary keys from
/// `CGRectCreateDictionaryRepresentation`), so the keys are constructed
/// locally and kept alive for the lookup rather than converted to a raw
/// `CFStringRef` and dropped -- doing the latter would leave a dangling
/// pointer.
fn bounds_area_pt(bounds: CFDictionary<CFString, CFType>) -> f64 {
  let width_key = CFString::from_static_string("Width");
  let height_key = CFString::from_static_string("Height");
  let width = dict_number_by_owned_key(&bounds, &width_key).unwrap_or(0.0);
  let height = dict_number_by_owned_key(&bounds, &height_key).unwrap_or(0.0);
  width * height
}

fn dict_number_by_static_key(
  dict: &CFDictionary<CFString, CFType>,
  key: CFStringRef,
) -> Option<f64> {
  let value = dict.find(key)?;
  value.downcast::<CFNumber>()?.to_f64()
}

fn dict_number_by_owned_key(
  dict: &CFDictionary<CFString, CFType>,
  key: &CFString,
) -> Option<f64> {
  let value = dict.find(key)?;
  value.downcast::<CFNumber>()?.to_f64()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn on_screen_windows_owned_by_an_impossible_pid_is_empty() {
    // PID 0 owns no windows on a live system; this is a smoke check that
    // the FFI call itself does not crash, not a substitute for a fixture --
    // there is no fixture for live window-server output (design §11).
    let windows = on_screen_windows_owned_by(0);
    assert!(windows.is_empty());
  }
}
