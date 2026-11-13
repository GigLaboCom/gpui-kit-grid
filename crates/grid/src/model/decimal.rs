//! Exact decimal values (`rust_decimal::Decimal`, the `decimal` feature):
//! their display text without `core::fmt`, their exact order against floats,
//! and the integer or float each one equals, so that equal [`OwnedValue`]s
//! hash alike.

use std::{cmp::Ordering, fmt};

use rust_decimal::Decimal;
use serde::{Serialize, Serializer, de};

use super::{OwnedValue, cmp_int_float, float_text::DIGIT_PAIRS};

/// The digits of the largest mantissa, 2^96 - 1.
const MAX_DIGITS: usize = 29;

/// The longest text: a sign, the digits and the point.
const MAX_LEN: usize = MAX_DIGITS + 2;

/// The powers of ten up to the largest scale, 28.
const POW10: [u128; 29] = powers(10);

/// The powers of five up to the largest scale: 10^s = 5^s × 2^s.
const POW5: [u128; 29] = powers(5);

const fn powers(base: u128) -> [u128; 29] {
    let mut powers = [1; 29];
    let mut ix = 1;
    while ix < powers.len() {
        powers[ix] = powers[ix - 1] * base;
        ix += 1;
    }
    powers
}

/// Appends `value` to `buffer` exactly as `write!(buffer, "{value}")` does:
/// the mantissa's digits with the point placed by the scale, so the scale
/// shows (`12.30`, `0.05`, `0.00`), and a `-` whenever the sign is negative,
/// a negative zero (`-0`) included.
///
/// `Display` divides the 96-bit mantissa by ten per digit and pads through
/// `core::fmt`; this writes digit pairs of a `u64` (one `u128` division for
/// a mantissa above `u64::MAX`) on the stack: 1.5 to 2 times faster.
pub(super) fn push_decimal(value: Decimal, buffer: &mut String) {
    let scale = value.scale() as usize;
    // The digits of the mantissa, right-aligned over zeros: at least
    // `scale + 1` of them show, so a fraction gets its leading `0.`.
    let mut digits = [b'0'; MAX_DIGITS];
    let written = write_digits(value.mantissa().unsigned_abs(), &mut digits);
    let shown = &digits[MAX_DIGITS - written.max(scale + 1)..];
    let (whole, fraction) = shown.split_at(shown.len() - scale);

    let mut text = [0; MAX_LEN];
    let mut len = usize::from(value.is_sign_negative());
    text[0] = b'-';
    text[len..len + whole.len()].copy_from_slice(whole);
    len += whole.len();
    if scale > 0 {
        text[len] = b'.';
        text[len + 1..len + 1 + scale].copy_from_slice(fraction);
        len += 1 + scale;
    }
    buffer.push_str(std::str::from_utf8(&text[..len]).expect("ASCII digits"));
}

/// Writes the digits of `value`, below 2^96, right-aligned into `digits`
/// (nothing for zero) and returns how many it wrote.
fn write_digits(value: u128, digits: &mut [u8; MAX_DIGITS]) -> usize {
    const TEN_POW_19: u128 = 10_000_000_000_000_000_000;
    match u64::try_from(value) {
        Ok(value) => write_u64(value, digits),
        Err(_) => {
            // Below 2^96 the quotient fits in a `u64`. The low 19 digits keep
            // their leading zeros: `digits` is filled with zeros.
            write_u64((value % TEN_POW_19) as u64, digits);
            let high = (value / TEN_POW_19) as u64;
            19 + write_u64(high, &mut digits[..MAX_DIGITS - 19])
        }
    }
}

/// Writes the digits of `value` at the end of `digits`, nothing for zero,
/// and returns how many it wrote.
fn write_u64(mut value: u64, digits: &mut [u8]) -> usize {
    let mut start = digits.len();
    while value >= 100 {
        let pair = (value % 100) as usize * 2;
        value /= 100;
        start -= 2;
        digits[start..start + 2].copy_from_slice(&DIGIT_PAIRS[pair..pair + 2]);
    }
    if value >= 10 {
        let pair = value as usize * 2;
        start -= 2;
        digits[start..start + 2].copy_from_slice(&DIGIT_PAIRS[pair..pair + 2]);
    } else if value > 0 {
        start -= 1;
        digits[start] = b'0' + value as u8;
    }
    digits.len() - start
}

/// Compares a decimal with a float exactly, consistent with
/// [`cmp_int_float`] and `f64::total_cmp`: a NaN sorts by its sign beyond
/// every number, and a zero decimal of any scale or sign compares as
/// `Int(0)`, above -0.0 and equal to 0.0.
///
/// Converting either side loses precision (`0.1` has no exact `f64`; a
/// float below 10^-28 or with more than 28 decimals has no exact `Decimal`)
/// and would break transitivity, which sorting relies on.
pub(super) fn cmp_decimal_float(decimal: Decimal, float: f64) -> Ordering {
    if decimal.is_zero() {
        return cmp_int_float(0, float);
    }
    let negative = decimal.is_sign_negative();
    if float.is_nan() {
        return if float.is_sign_negative() {
            Ordering::Greater
        } else {
            Ordering::Less
        };
    }
    // Different signs, a float zero of either sign included.
    if float == 0.0 || negative != (float < 0.0) {
        return if negative {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }
    let magnitude = cmp_magnitude(
        decimal.mantissa().unsigned_abs(),
        decimal.scale(),
        float.abs(),
    );
    if negative {
        magnitude.reverse()
    } else {
        magnitude
    }
}

/// Compares `mantissa / 10^scale` with a positive `float`, exactly: whole
/// parts first, then the fractions.
fn cmp_magnitude(mantissa: u128, scale: u32, float: f64) -> Ordering {
    if float.is_infinite() {
        return Ordering::Less;
    }
    // `float` is `significand × 2^exponent`.
    let bits = float.to_bits();
    let biased_exponent = (bits >> 52) as i32;
    let fraction = bits & ((1 << 52) - 1);
    let (significand, exponent) = if biased_exponent == 0 {
        (fraction, -1074)
    } else {
        (fraction | 1 << 52, biased_exponent - 1075)
    };
    let power = POW10[scale as usize];
    let (whole, rest) = (mantissa / power, mantissa % power);
    // The float's whole part and its fraction `numerator / 2^bits`.
    let (float_whole, numerator, bits) = if exponent >= 0 {
        // At least 2^(52 + exponent): from 2^96 on, above every decimal.
        if exponent >= 44 {
            return Ordering::Less;
        }
        (u128::from(significand) << exponent, 0, 0)
    } else {
        let shift = exponent.unsigned_abs();
        if shift >= 64 {
            (0, significand, shift)
        } else {
            (
                u128::from(significand >> shift),
                significand & ((1 << shift) - 1),
                shift,
            )
        }
    };
    whole
        .cmp(&float_whole)
        .then_with(|| cmp_fraction(rest, scale, numerator, bits))
}

/// Compares `rest / 10^scale` with `numerator / 2^bits`, both below 1:
/// `rest × 2^bits` with `numerator × 5^scale × 2^scale`, in `u128`.
fn cmp_fraction(rest: u128, scale: u32, numerator: u64, bits: u32) -> Ordering {
    if numerator == 0 {
        return rest.cmp(&0);
    }
    if rest == 0 {
        return Ordering::Less;
    }
    // Below 2^53 × 5^28 < 2^119.
    let odd = u128::from(numerator) * POW5[scale as usize];
    if bits >= scale {
        let shift = bits - scale;
        if shift >= 128 || rest.leading_zeros() < shift {
            // `rest × 2^shift` is at least 2^128.
            Ordering::Greater
        } else {
            (rest << shift).cmp(&odd)
        }
    } else {
        // `numerator < 2^bits`, so this is below 10^scale: no overflow.
        rest.cmp(&(odd << (scale - bits)))
    }
}

/// The `Int` or `Float` that [`CellValue::cmp_total`](super::CellValue::cmp_total)
/// calls equal to `value`, if there is one: an integral value in the `i64`
/// range is an `Int`, any other value an `f64` holds exactly (`0.5`, 2^70)
/// a `Float`. A zero of any sign or scale is `Int(0)`.
pub(super) fn equal_int_or_float(value: Decimal) -> Option<OwnedValue> {
    if value.is_zero() {
        return Some(OwnedValue::Int(0));
    }
    // Without trailing zeros: a scale above zero means a fraction.
    let normal = value.normalize();
    let (mantissa, scale) = (normal.mantissa(), normal.scale());
    if scale == 0
        && let Ok(int) = i64::try_from(mantissa)
    {
        return Some(OwnedValue::Int(int));
    }
    // `mantissa / 10^scale` is `(mantissa / 5^scale) / 2^scale` when 5^scale
    // divides the mantissa, and an `f64` holds it when that quotient has
    // at most 53 significant bits.
    let magnitude = mantissa.unsigned_abs();
    let power = POW5[scale as usize];
    if magnitude % power != 0 {
        return None;
    }
    let odd = magnitude / power;
    if 128 - odd.leading_zeros() - odd.trailing_zeros() > f64::MANTISSA_DIGITS {
        return None;
    }
    // Both steps are exact: the quotient has at most 53 significant bits,
    // and the divisor is a power of two.
    let float = odd as f64 / (1u64 << scale) as f64;
    Some(OwnedValue::Float(if mantissa < 0 { -float } else { float }))
}

/// A decimal serialized as its text, which keeps the scale: `"12.30"`.
pub(super) struct DecimalText(pub(super) Decimal);

impl Serialize for DecimalText {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

/// Reads the text of `{"decimal": "12.30"}`. The text must be exact: a
/// value with more digits than a `Decimal` holds is an error, not rounded.
pub(super) fn read_decimal<E: de::Error>(text: &str) -> Result<Decimal, E> {
    Decimal::from_str_exact(text)
        .map_err(|_| E::invalid_value(de::Unexpected::Str(text), &ExactDecimal))
}

struct ExactDecimal;

impl de::Expected for ExactDecimal {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("an exact decimal number")
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    /// A decimal from its mantissa and scale.
    fn dec(mantissa: i128, scale: u32) -> Decimal {
        Decimal::from_i128_with_scale(mantissa, scale)
    }

    /// A random mantissa below 2^96, of any bit length, so every digit count
    /// and both paths of `write_digits` are common.
    fn random_mantissa(rng: &mut fastrand::Rng) -> i128 {
        let bits = rng.u32(0..=96);
        let magnitude = if bits == 0 {
            0
        } else {
            rng.u128(..) >> (128 - bits)
        };
        if rng.bool() {
            -(magnitude as i128)
        } else {
            magnitude as i128
        }
    }

    #[test]
    fn test_push_decimal_matches_display() {
        let mut negative_zero = dec(0, 2);
        negative_zero.set_sign_negative(true);
        let expected = [
            (dec(1230, 2), "12.30"),
            (dec(5, 2), "0.05"),
            (dec(-5, 2), "-0.05"),
            (dec(0, 0), "0"),
            (dec(0, 2), "0.00"),
            (negative_zero, "-0.00"),
            (-Decimal::ZERO, "-0"),
            (dec(1, 28), "0.0000000000000000000000000001"),
            (Decimal::MAX, "79228162514264337593543950335"),
            (Decimal::MIN, "-79228162514264337593543950335"),
            (
                dec(Decimal::MAX.mantissa(), 28),
                "7.9228162514264337593543950335",
            ),
            (dec(u64::MAX.into(), 0), "18446744073709551615"),
            (dec(i128::from(u64::MAX) + 1, 0), "18446744073709551616"),
            (dec(i128::from(u64::MAX) + 1, 20), "0.18446744073709551616"),
            (dec(10_000_000_000_000_000_000, 19), "1.0000000000000000000"),
            (dec(100, 0), "100"),
            (dec(-7, 0), "-7"),
        ];
        let mut text = String::new();
        for (value, expected) in expected {
            text.clear();
            push_decimal(value, &mut text);
            assert_eq!(text, expected, "{value:?}");
            assert_eq!(text, value.to_string(), "{value:?}");
        }

        // Any mantissa, scale and sign, against `Display`.
        let (mut fast, mut reference) = (String::new(), String::new());
        let mut rng = fastrand::Rng::with_seed(0xdec);
        for _ in 0..200_000 {
            let mut value = dec(random_mantissa(&mut rng), rng.u32(0..=28));
            if rng.u8(0..20) == 0 {
                value.set_sign_negative(true);
            }
            fast.clear();
            reference.clear();
            push_decimal(value, &mut fast);
            let _ = write!(reference, "{value}");
            assert_eq!(fast, reference, "{value:?}, scale {}", value.scale());
        }
    }

    /// Nanoseconds per value of `Display` and of [`push_decimal`]. Run with
    /// `cargo test --release -p gpui-kit-grid --features decimal --lib --
    /// --ignored --nocapture test_push_decimal_timing`.
    #[test]
    #[ignore = "timing; run in release with --ignored --nocapture"]
    #[cfg(not(target_family = "wasm"))]
    fn test_push_decimal_timing() {
        use std::{hint::black_box, time::Instant};

        fn nanos_per_value(values: &[Decimal], format: impl Fn(Decimal, &mut String)) -> f64 {
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
        let data: [(&str, Vec<Decimal>); 3] = [
            (
                "money k/100 below 100 000",
                (0..COUNT)
                    .map(|_| dec(rng.i128(0..10_000_000), 2))
                    .collect(),
            ),
            (
                "quantities, scale 0..=8",
                (0..COUNT)
                    .map(|_| {
                        dec(
                            rng.i128(-1_000_000_000_000..1_000_000_000_000),
                            rng.u32(0..=8),
                        )
                    })
                    .collect(),
            ),
            (
                "any mantissa below 2^96",
                (0..COUNT)
                    .map(|_| dec(random_mantissa(&mut rng), rng.u32(0..=28)))
                    .collect(),
            ),
        ];
        println!("| Values | Display | push_decimal |");
        println!("|---|---|---|");
        for (name, values) in &data {
            let reference = nanos_per_value(values, |value, buffer| {
                let _ = write!(buffer, "{value}");
            });
            let fast = nanos_per_value(values, push_decimal);
            println!("| {name} | {reference:.1} ns | {fast:.1} ns |");
        }
    }

    /// The exact order of the magnitudes `a` and `b` (decimal text without a
    /// sign, as `Display` writes a `Decimal` and `{:.1074}` an `f64`): the
    /// reference for [`cmp_decimal_float`].
    fn cmp_decimal_texts(a: &str, b: &str) -> Ordering {
        let split = |text: &str| {
            let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
            (
                whole.trim_start_matches('0').to_owned(),
                fraction.trim_end_matches('0').to_owned(),
            )
        };
        let ((a_whole, a_fraction), (b_whole, b_fraction)) = (split(a), split(b));
        a_whole
            .len()
            .cmp(&b_whole.len())
            .then_with(|| a_whole.cmp(&b_whole))
            .then_with(|| a_fraction.cmp(&b_fraction))
    }

    #[test]
    fn test_cmp_decimal_float_is_exact() {
        let mut rng = fastrand::Rng::with_seed(0xf10a7);
        // Floats near 0.1, binary fractions, values around the decimal
        // range's ends and arbitrary bit patterns; decimals equal to some of
        // them, their neighbours, and arbitrary decimals.
        let random_float = |rng: &mut fastrand::Rng| -> f64 {
            let float = match rng.u8(0..6) {
                0 => f64::from(rng.i32(-1_000..1_000)) / 8.0,
                1 => rng.i64(..) as f64 / 10f64.powi(rng.i32(0..=30)),
                2 => 2f64.powi(rng.i32(-100..=100)),
                3 => 7.922_816_251_426_434e28 * rng.f64(),
                _ => f64::from_bits(rng.u64(..)),
            };
            if rng.bool() { float.next_up() } else { float }
        };
        let mut checked = 0;
        for _ in 0..300_000 {
            let float = random_float(&mut rng);
            if !float.is_finite() || float == 0.0 {
                continue;
            }
            let decimal = match rng.u8(0..4) {
                0 => Decimal::from_f64_retain(float).unwrap_or(Decimal::ONE),
                1 => {
                    let retained = Decimal::from_f64_retain(float).unwrap_or(Decimal::ONE);
                    let step = dec(1, retained.scale());
                    if rng.bool() {
                        retained.checked_add(step).unwrap_or(retained)
                    } else {
                        retained.checked_sub(step).unwrap_or(retained)
                    }
                }
                _ => dec(random_mantissa(&mut rng), rng.u32(0..=28)),
            };
            if decimal.is_zero() {
                continue;
            }
            let expected = match (decimal.is_sign_negative(), float < 0.0) {
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                (negative, _) => {
                    let decimal_text = decimal.abs().to_string();
                    let float_text = format!("{:.1074}", float.abs());
                    let magnitude = cmp_decimal_texts(&decimal_text, &float_text);
                    if negative {
                        magnitude.reverse()
                    } else {
                        magnitude
                    }
                }
            };
            assert_eq!(
                cmp_decimal_float(decimal, float),
                expected,
                "{decimal} against {float:e}"
            );
            checked += 1;
        }
        assert!(checked > 250_000);
    }

    #[test]
    fn test_equal_int_or_float() {
        let mut negative_zero = dec(0, 3);
        negative_zero.set_sign_negative(true);
        let p63 = 9_223_372_036_854_775_808.0;
        for (value, expected) in [
            (dec(0, 0), Some(OwnedValue::Int(0))),
            (negative_zero, Some(OwnedValue::Int(0))),
            (dec(100, 2), Some(OwnedValue::Int(1))),
            (dec(-1230, 1), Some(OwnedValue::Int(-123))),
            (dec(i64::MIN.into(), 0), Some(OwnedValue::Int(i64::MIN))),
            (
                dec(i128::from(i64::MAX) + 1, 0),
                Some(OwnedValue::Float(p63)),
            ),
            (dec(1 << 70, 0), Some(OwnedValue::Float(2f64.powi(70)))),
            (dec(50, 2), Some(OwnedValue::Float(0.5))),
            (dec(-1_250, 3), Some(OwnedValue::Float(-1.25))),
            (dec(1, 28), None),
            (dec(1, 1), None),
            (dec(1230, 2), None),
            (dec((1 << 53) + 1, 1), None),
            (Decimal::MAX, None),
        ] {
            let equal = equal_int_or_float(value);
            assert!(
                matches!(
                    (&equal, &expected),
                    (Some(OwnedValue::Int(a)), Some(OwnedValue::Int(b))) if a == b
                ) || matches!(
                    (&equal, &expected),
                    (Some(OwnedValue::Float(a)), Some(OwnedValue::Float(b))) if a.to_bits() == b.to_bits()
                ) || (equal.is_none() && expected.is_none()),
                "{value}: {equal:?}, expected {expected:?}"
            );
        }

        // A float found is equal under the exact comparison; none found
        // means no float is: the nearest ones compare unequal.
        let mut rng = fastrand::Rng::with_seed(0xe9);
        for _ in 0..100_000 {
            let value = match rng.u8(0..3) {
                0 => dec(random_mantissa(&mut rng), rng.u32(0..=28)),
                1 => dec(i128::from(rng.i64(-1_000..1_000)) * 125, rng.u32(0..=4)),
                _ => Decimal::from_f64_retain(f64::from_bits(rng.u64(..))).unwrap_or(Decimal::ONE),
            };
            match equal_int_or_float(value) {
                Some(OwnedValue::Int(int)) => assert_eq!(value, Decimal::from(int)),
                Some(OwnedValue::Float(float)) => {
                    assert_eq!(cmp_decimal_float(value, float), Ordering::Equal, "{value}")
                }
                Some(other) => panic!("{value}: {other:?}"),
                None => {
                    let near = rust_decimal::prelude::ToPrimitive::to_f64(&value).unwrap();
                    for float in [near.next_down(), near, near.next_up()] {
                        assert_ne!(cmp_decimal_float(value, float), Ordering::Equal, "{value}");
                    }
                }
            }
        }
    }
}
