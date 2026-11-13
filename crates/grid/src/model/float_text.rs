//! `{:.2}` for `f64` without `core::fmt`.
//!
//! [`display_text`](super::display_text) formats every visible float on
//! every frame, and a quick filter rebuild formats every float of the
//! searched columns of every row. `write!(buffer, "{:.2}")` goes through
//! `core::fmt` and its exact decimal conversion; [`push_two_decimals`]
//! writes the same text about four times faster.

use std::fmt::Write as _;

/// The decimal digit pairs "00" to "99": pair `n` starts at `2 * n`.
pub(super) const DIGIT_PAIRS: &[u8; 200] = b"\
    0001020304050607080910111213141516171819\
    2021222324252627282930313233343536373839\
    4041424344454647484950515253545556575859\
    6061626364656667686970717273747576777879\
    8081828384858687888990919293949596979899";

/// The longest text of the fast path: a sign, the 20 digits of `u64::MAX`,
/// the point and two decimals.
const MAX_LEN: usize = 24;

/// The biased exponent at which the lowest of the 53 mantissa bits is
/// worth 1.
const UNIT_EXPONENT: u64 = 1075;

/// Appends `value` to `buffer` exactly as `write!(buffer, "{value:.2}")`
/// does: the exact binary value rounded half to even at the second decimal,
/// with a `-` for every negative value, `-0.0` and values that round to
/// `-0.00` included.
///
/// A finite `|value|` is `mantissa × 2^-shift` with a 53-bit mantissa, so
/// `100 × |value|` is `100 × mantissa`, which fits in 60 bits, shifted right
/// by `shift`. Rounding compares the bits shifted out with one half and, on
/// a tie, rounds up only an odd quotient, whose parity is that of the second
/// decimal, as `core::fmt` does.
///
/// Values below 2^64 in magnitude take this path. NaN, the infinities and
/// larger values fall back to `core::fmt`.
pub(super) fn push_two_decimals(value: f64, buffer: &mut String) {
    let bits = value.to_bits();
    let biased_exponent = (bits >> 52) & 0x7ff;
    let mantissa = (bits & ((1 << 52) - 1)) | (1 << 52);
    let (whole, cents) = if biased_exponent < UNIT_EXPONENT {
        let shift = UNIT_EXPONENT - biased_exponent;
        if shift < 64 {
            let scaled = mantissa * 100;
            let quotient = scaled >> shift;
            let rest = scaled & ((1 << shift) - 1);
            let half = 1 << (shift - 1);
            let hundredths = quotient + u64::from(rest + (quotient & 1) > half);
            (hundredths / 100, (hundredths % 100) as usize)
        } else {
            // Below 2^-11, which rounds to zero. Subnormals land here too,
            // with a wrong but unused mantissa.
            (0, 0)
        }
    } else if biased_exponent < UNIT_EXPONENT + 12 {
        // An integer below 2^64.
        (mantissa << (biased_exponent - UNIT_EXPONENT), 0)
    } else {
        let _ = write!(buffer, "{value:.2}");
        return;
    };

    // Right to left: the decimals, the point, the whole part, the sign.
    let mut text = [0; MAX_LEN];
    let mut start = MAX_LEN - 3;
    text[start] = b'.';
    text[start + 1..].copy_from_slice(&DIGIT_PAIRS[cents * 2..cents * 2 + 2]);
    let mut whole = whole;
    while whole >= 100 {
        let pair = (whole % 100) as usize * 2;
        whole /= 100;
        start -= 2;
        text[start..start + 2].copy_from_slice(&DIGIT_PAIRS[pair..pair + 2]);
    }
    if whole >= 10 {
        let pair = whole as usize * 2;
        start -= 2;
        text[start..start + 2].copy_from_slice(&DIGIT_PAIRS[pair..pair + 2]);
    } else {
        start -= 1;
        text[start] = b'0' + whole as u8;
    }
    if bits >> 63 == 1 {
        start -= 1;
        text[start] = b'-';
    }
    buffer.push_str(std::str::from_utf8(&text[start..]).expect("ASCII digits"));
}

/// Powers of ten up to the most decimals the fast path of [`push_fixed`]
/// writes.
const POWERS_OF_TEN: [u64; 10] = [
    1,
    10,
    100,
    1_000,
    10_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
];

/// Appends `value` to `buffer` exactly as `write!(buffer, "{value:.*}",
/// decimals)` does: [`push_two_decimals`] for two decimals, the same
/// arithmetic in 128 bits for up to nine, `core::fmt` beyond and for the
/// values [`push_two_decimals`] leaves to it.
pub(crate) fn push_fixed(value: f64, decimals: u8, buffer: &mut String) {
    if decimals == 2 {
        push_two_decimals(value, buffer);
        return;
    }
    let Some(&scale) = POWERS_OF_TEN.get(usize::from(decimals)) else {
        let _ = write!(buffer, "{value:.*}", usize::from(decimals));
        return;
    };
    let bits = value.to_bits();
    let biased_exponent = (bits >> 52) & 0x7ff;
    let mantissa = (bits & ((1 << 52) - 1)) | (1 << 52);
    let (whole, fraction) = if biased_exponent < UNIT_EXPONENT {
        let shift = UNIT_EXPONENT - biased_exponent;
        if shift < 128 {
            let scale = u128::from(scale);
            let scaled = u128::from(mantissa) * scale;
            let quotient = scaled >> shift;
            let rest = scaled & ((1 << shift) - 1);
            let half = 1 << (shift - 1);
            let units = quotient + u128::from(rest + (quotient & 1) > half);
            ((units / scale) as u64, (units % scale) as u64)
        } else {
            // Below 2^-52, which rounds to zero at nine decimals; the
            // subnormals land here too.
            (0, 0)
        }
    } else if biased_exponent < UNIT_EXPONENT + 12 {
        (mantissa << (biased_exponent - UNIT_EXPONENT), 0)
    } else {
        let _ = write!(buffer, "{value:.*}", usize::from(decimals));
        return;
    };
    if bits >> 63 == 1 {
        buffer.push('-');
    }
    let _ = write!(buffer, "{whole}");
    if decimals > 0 {
        let _ = write!(buffer, ".{fraction:0width$}", width = usize::from(decimals));
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::{push_fixed, push_two_decimals};

    /// Checks [`push_fixed`] against `core::fmt` for every count of
    /// decimals up to 12, the slow ones included.
    #[test]
    fn test_push_fixed_matches_core_fmt() {
        let mut rng = fastrand::Rng::with_seed(0xf1_7ed);
        let (mut fast, mut reference) = (String::new(), String::new());
        let mut check = |value: f64, decimals: u8| {
            fast.clear();
            reference.clear();
            push_fixed(value, decimals, &mut fast);
            let _ = write!(reference, "{value:.*}", usize::from(decimals));
            assert_eq!(fast, reference, "{value:e} at {decimals} decimals");
        };
        let edges = [
            0.0,
            -0.0,
            0.5,
            1.5,
            2.5,
            -0.49,
            -0.489_999_999_999_994_9,
            0.000_5,
            0.000_15,
            123_456.789_012_345,
            9.999_999_999,
            1e15 + 0.125,
            9_007_199_254_740_992.0,
            18_446_744_073_709_549_568.0,
            1e20,
            5e-324,
            f64::MIN_POSITIVE,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ];
        for decimals in 0..=12 {
            for value in edges {
                check(value, decimals);
                check(-value, decimals);
            }
            for exponent in -12..=22 {
                let power = 10f64.powi(exponent);
                for value in [power, power.next_down(), power.next_up()] {
                    check(value, decimals);
                }
            }
            for _ in 0..20_000 {
                check(
                    f64::from_bits(rng.u64(..) & !(0x7ff << 52) | (rng.u64(900..=1_090) << 52)),
                    decimals,
                );
                check(rng.i64(-10_000_000..10_000_000) as f64 / 1_000.0, decimals);
                check((rng.f64() - 0.5) * 2e4, decimals);
            }
        }
    }

    /// Random values checked against `core::fmt` by the property test.
    /// Debug builds check fewer to keep `cargo test` fast.
    const RANDOM_VALUES: usize = if cfg!(debug_assertions) {
        300_000
    } else {
        12_000_000
    };

    /// Compares `value` with `format!("{value:.2}")`.
    struct Checker {
        fast: String,
        reference: String,
        checked: usize,
    }

    impl Checker {
        fn new() -> Self {
            Self {
                fast: String::with_capacity(64),
                reference: String::with_capacity(64),
                checked: 0,
            }
        }

        #[track_caller]
        fn check(&mut self, value: f64) {
            self.fast.clear();
            self.reference.clear();
            push_two_decimals(value, &mut self.fast);
            let _ = write!(self.reference, "{value:.2}");
            assert_eq!(
                self.fast,
                self.reference,
                "{value:e} (bits {:#018x})",
                value.to_bits()
            );
            self.checked += 1;
        }

        /// Checks `value`, its neighbours and their negations.
        #[track_caller]
        fn check_around(&mut self, value: f64) {
            for value in [value.next_down(), value, value.next_up()] {
                self.check(value);
                self.check(-value);
            }
        }
    }

    /// A random `f64` with every bit pattern of the given biased exponents.
    fn random_bits(rng: &mut fastrand::Rng, exponents: std::ops::RangeInclusive<u64>) -> f64 {
        let sign_and_mantissa = rng.u64(..) & !(0x7ff << 52);
        f64::from_bits(sign_and_mantissa | (rng.u64(exponents) << 52))
    }

    #[test]
    fn test_push_two_decimals_edge_cases() {
        let expected = [
            (0.0, "0.00"),
            (-0.0, "-0.00"),
            (-0.001, "-0.00"),
            (-0.004_999, "-0.00"),
            (5e-324, "0.00"),
            (-5e-324, "-0.00"),
            (f64::MIN_POSITIVE, "0.00"),
            // 0.005 is a little above its decimal, 1.005 a little below.
            (0.005, "0.01"),
            (-0.005, "-0.01"),
            (1.005, "1.00"),
            (2.675, "2.67"),
            // Exact ties round to an even second decimal.
            (0.125, "0.12"),
            (0.375, "0.38"),
            (0.625, "0.62"),
            (0.875, "0.88"),
            (-0.125, "-0.12"),
            (1e15 + 0.125, "1000000000000000.12"),
            (1e15 + 0.375, "1000000000000000.38"),
            // Rounding carries into the whole part.
            (9.999, "10.00"),
            (-99.999, "-100.00"),
            (0.999, "1.00"),
            (12.5, "12.50"),
            (187.25, "187.25"),
            (-1234.567, "-1234.57"),
            // Integers, up to the largest of the fast path.
            (9_007_199_254_740_992.0, "9007199254740992.00"),
            (18_446_744_073_709_549_568.0, "18446744073709549568.00"),
            // Fallbacks.
            (18_446_744_073_709_551_616.0, "18446744073709551616.00"),
            (1e20, "100000000000000000000.00"),
            (f64::NAN, "NaN"),
            (-f64::NAN, "NaN"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
        ];
        let mut text = String::new();
        for (value, expected) in expected {
            text.clear();
            push_two_decimals(value, &mut text);
            assert_eq!(text, expected, "{value:e}");
        }

        let mut checker = Checker::new();
        // NaN payloads and signs.
        for bits in [
            0x7ff8_0000_0000_0001,
            0xfff0_0000_0000_0001,
            0xffff_ffff_ffff_ffff,
        ] {
            checker.check(f64::from_bits(bits));
        }
        for value in [f64::MAX, f64::MIN, f64::MIN_POSITIVE, f64::EPSILON] {
            checker.check_around(value);
        }
        // Every power of two, which covers each exponent and both ends of
        // the fast path (2^-11 and 2^64), and the smallest subnormals.
        for exponent in -1074..=1023i64 {
            let bits = if exponent < -1022 {
                1 << (exponent + 1074)
            } else {
                ((exponent + 1023) as u64) << 52
            };
            checker.check_around(f64::from_bits(bits));
        }
        for bits in 1..1_000 {
            checker.check(f64::from_bits(bits));
        }
        // Around powers of ten, where the number of digits changes, and just
        // below them, where rounding carries into a new digit.
        for exponent in -5..=22 {
            let power = 10f64.powi(exponent);
            checker.check_around(power);
            checker.check_around(power - 0.005);
            checker.check_around(power - 0.01);
        }
        // Ties: every multiple of 1/8 in a range, and odd thousandths.
        for k in -100_000..100_000 {
            checker.check(f64::from(k) / 8.0);
            checker.check_around(f64::from(2 * k + 1) / 200.0);
        }
        assert!(checker.checked > 1_000_000);
    }

    #[test]
    fn test_push_two_decimals_matches_core_fmt_on_random_values() {
        let mut rng = fastrand::Rng::with_seed(0x2d_ec1a_15);
        let mut checker = Checker::new();
        for _ in 0..RANDOM_VALUES / 6 {
            // Any finite bit pattern up to just above the fast path, the
            // subnormals included.
            checker.check(random_bits(&mut rng, 0..=1_090));
            // Prices: hundredths and their binary neighbours.
            let hundredths = rng.i64(-100_000_000_000..100_000_000_000);
            let price = hundredths as f64 / 100.0;
            checker.check(price);
            checker.check(if rng.bool() {
                price.next_up()
            } else {
                price.next_down()
            });
            // Thousandths, half of which are decimal ties.
            checker.check(rng.i64(-100_000_000..100_000_000) as f64 / 1_000.0);
            // Uniform values of a few scales, as in the benchmarks.
            checker.check(rng.f64() * 500.0);
            checker.check((rng.f64() - 0.5) * 2e6);
        }
        // Any bit pattern. Most are huge and format slowly, so fewer.
        for _ in 0..RANDOM_VALUES / 1_000 {
            checker.check(f64::from_bits(rng.u64(..)));
        }
        assert!(checker.checked >= RANDOM_VALUES);
    }

    /// Every price below 100 000.00 and its binary neighbours, both signs:
    /// 60 million values. Run with `cargo test --release -p gpui-kit-grid
    /// --lib -- --ignored test_push_two_decimals_every_hundredth`.
    #[test]
    #[ignore = "60M values; run in release with --ignored"]
    fn test_push_two_decimals_every_hundredth() {
        let mut checker = Checker::new();
        for k in 0..10_000_000u32 {
            checker.check_around(f64::from(k) / 100.0);
        }
        assert_eq!(checker.checked, 60_000_000);
    }

    /// Nanoseconds per value of `core::fmt` and of [`push_two_decimals`].
    /// Run with `cargo test --release -p gpui-kit-grid --lib -- --ignored
    /// --nocapture test_push_two_decimals_timing`.
    #[test]
    #[ignore = "timing; run in release with --ignored --nocapture"]
    #[cfg(not(target_family = "wasm"))]
    fn test_push_two_decimals_timing() {
        use std::{hint::black_box, time::Instant};

        fn nanos_per_value(values: &[f64], format: impl Fn(f64, &mut String)) -> f64 {
            let mut buffer = String::with_capacity(64);
            (0..5)
                .map(|_| {
                    let started = Instant::now();
                    for &value in values {
                        buffer.clear();
                        format(black_box(value), &mut buffer);
                        black_box(buffer.len());
                    }
                    started.elapsed().as_nanos() as f64 / values.len() as f64
                })
                .fold(f64::MAX, f64::min)
        }

        const COUNT: usize = 1_000_000;
        let mut rng = fastrand::Rng::with_seed(7);
        let data: [(&str, Vec<f64>); 4] = [
            (
                "prices k/100 below 100 000",
                (0..COUNT)
                    .map(|_| f64::from(rng.u32(..10_000_000)) / 100.0)
                    .collect(),
            ),
            (
                "uniform 0..500",
                (0..COUNT).map(|_| rng.f64() * 500.0).collect(),
            ),
            (
                "uniform 0..1e6",
                (0..COUNT).map(|_| rng.f64() * 1e6).collect(),
            ),
            (
                "random bits below 2^64",
                (0..COUNT)
                    .map(|_| random_bits(&mut rng, 1_000..=1_086))
                    .collect(),
            ),
        ];
        println!("| Values | core::fmt {{:.2}} | push_two_decimals |");
        println!("|---|---|---|");
        for (name, values) in &data {
            let reference = nanos_per_value(values, |value, buffer| {
                let _ = write!(buffer, "{value:.2}");
            });
            let fast = nanos_per_value(values, push_two_decimals);
            println!("| {name} | {reference:.1} ns | {fast:.1} ns |");
        }
    }
}
