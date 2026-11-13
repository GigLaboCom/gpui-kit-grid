//! The page's URL parameters (`?rate=300&theme=heat`), as the example
//! reads them in its setup.

use wasm_bindgen::JsValue;
use web_sys::UrlSearchParams;
use web_time::{SystemTime, UNIX_EPOCH};

/// The parameters of the page's URL, read by the browser's own parser.
pub struct PageParams {
    params: Option<UrlSearchParams>,
}

impl PageParams {
    /// The parameters of `query`, the page's `location.search`.
    pub fn new(query: &str) -> Self {
        Self {
            params: UrlSearchParams::new_with_str(query).ok(),
        }
    }

    /// The first value of `name`, if the URL has it.
    pub fn get(&self, name: &str) -> Option<String> {
        self.params.as_ref()?.get(name)
    }

    /// The value of `name` as a number, as JavaScript reads one: `Number`,
    /// then wrapped into 32 bits as `>>> 0` does (`Number('')` is 0, `-1`
    /// wraps, anything else that is not a number is 0). `None` if the URL
    /// does not have it.
    pub fn number(&self, name: &str) -> Option<u32> {
        let text = self.get(name)?;
        let value = JsValue::from_str(&text).unchecked_into_f64();
        Some(if value.is_finite() {
            value.trunc().rem_euclid(4_294_967_296.) as u32
        } else {
            0
        })
    }

    /// `?seed=`, or one from the clock: `fastrand` has no entropy in this
    /// build, and a fixed seed gives the same simulated market every run.
    pub fn seed(&self) -> u64 {
        self.number("seed").map_or_else(
            || {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64
            },
            u64::from,
        )
    }
}
