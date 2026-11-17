//! Deterministic formatting for explicit English regional profiles.
//!
//! Dates use the Gregorian calendar. Short dates retain four-digit years.
//! Times are local clock values. No OS locale, clock, or time zone is consulted.

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};

use super::Error;

/// Packaged English profiles. Unsupported locales are errors during decoding.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnglishLocale {
    #[default]
    #[serde(rename = "en-US")]
    Us,
    #[serde(rename = "en-GB")]
    Gb,
    #[serde(rename = "en-CA")]
    Ca,
    #[serde(rename = "en-AU")]
    Au,
    #[serde(rename = "en-IN")]
    In,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NumberOptions {
    pub locale: EnglishLocale,
    pub grouping: bool,
    #[serde(deserialize_with = "fraction_digits")]
    pub fraction_digits: Option<u8>,
}

fn fraction_digits<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<u8>, D::Error> {
    let value = Option::<serde_json::Number>::deserialize(deserializer)?;
    value
        .map(|value| {
            let value = value
                .as_f64()
                .ok_or_else(|| serde::de::Error::custom("fraction digits must be an integer from zero to nine"))?;
            if value.is_finite() && (0.0..=9.0).contains(&value) && value.fract() == 0.0 {
                Ok(value as u8)
            } else {
                Err(serde::de::Error::custom(
                    "fraction digits must be an integer from zero to nine",
                ))
            }
        })
        .transpose()
}

impl Default for NumberOptions {
    fn default() -> Self {
        Self {
            locale: EnglishLocale::Us,
            grouping: true,
            fraction_digits: None,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateStyle {
    Short,
    #[default]
    Long,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DateOptions {
    pub locale: EnglishLocale,
    pub style: DateStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HourCycle {
    H12,
    H24,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TimeOptions {
    pub locale: EnglishLocale,
    pub hour_cycle: Option<HourCycle>,
    pub seconds: bool,
}

const MAX_DIGITS: usize = 4096;
const MONTHS: &[&str] = &[
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Formats an exact decimal string with optional exponent notation.
///
/// Precision rounds half away from zero and pads to the requested number of
/// decimal places. Without explicit precision, trailing fractional zeros are
/// omitted. Negative zero is rendered as zero. Expanded input is bounded to
/// 4096 digits so exponent notation cannot request an unbounded allocation.
pub fn format_number(value: &str, options: &NumberOptions) -> Result<String, Error> {
    if options.fraction_digits.is_some_and(|digits| digits > 9) {
        return invalid("fraction_digits", "fraction digits must be between zero and nine");
    }
    let (negative, mut integer, mut fraction) = decimal_parts(value)?;
    if let Some(digits) = options.fraction_digits {
        let digits = usize::from(digits);
        if fraction.len() > digits {
            let round = fraction.as_bytes()[digits] >= b'5';
            fraction.truncate(digits);
            if round {
                let mut all = format!("{integer}{fraction}").into_bytes();
                let mut carry = true;
                for digit in all.iter_mut().rev() {
                    if *digit < b'9' {
                        *digit += 1;
                        carry = false;
                        break;
                    }
                    *digit = b'0';
                }
                if carry {
                    all.insert(0, b'1');
                }
                let all = String::from_utf8(all).expect("decimal digits");
                let split = all.len() - digits;
                integer = all[..split].into();
                fraction = all[split..].into();
            }
        } else {
            fraction.extend(std::iter::repeat_n('0', digits - fraction.len()));
        }
    } else {
        fraction.truncate(fraction.trim_end_matches('0').len());
    }
    let is_zero = integer.bytes().chain(fraction.bytes()).all(|digit| digit == b'0');
    let integer = if options.grouping {
        group_integer(&integer, options.locale)
    } else {
        integer
    };
    let sign = if negative && !is_zero { "-" } else { "" };
    Ok(if fraction.is_empty() {
        format!("{sign}{integer}")
    } else {
        format!("{sign}{integer}.{fraction}")
    })
}

fn decimal_parts(value: &str) -> Result<(bool, String, String), Error> {
    if value.is_empty() || value.len() > MAX_DIGITS || !value.is_ascii() {
        return invalid("number", "expected a bounded decimal string");
    }
    let (negative, unsigned) = match value.as_bytes()[0] {
        b'-' => (true, &value[1..]),
        b'+' => (false, &value[1..]),
        _ => (false, value),
    };
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(index) => {
            let exponent = &unsigned[index + 1..];
            let magnitude = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
            if magnitude.is_empty() || !magnitude.bytes().all(|digit| digit.is_ascii_digit()) {
                return invalid("number", "expected a decimal exponent");
            }
            let exponent = exponent.parse::<i32>().map_err(|_| Error::InvalidField {
                field: "number",
                reason: "decimal exponent is out of range",
            })?;
            if exponent.unsigned_abs() > MAX_DIGITS as u32 {
                return invalid("number", "decimal exponent is out of range");
            }
            (&unsigned[..index], exponent)
        }
        None => (unsigned, 0),
    };
    let (integer, fraction) = match mantissa.split_once('.') {
        Some((integer, fraction)) if !fraction.is_empty() => (integer, fraction),
        Some(_) => return invalid("number", "expected digits after the decimal point"),
        None => (mantissa, ""),
    };
    if integer.is_empty() || !integer.bytes().chain(fraction.bytes()).all(|digit| digit.is_ascii_digit()) {
        return invalid("number", "expected decimal digits");
    }
    let digits = format!("{integer}{fraction}");
    let point = integer.len() as i32 + exponent;
    let (integer, fraction) = if point <= 0 {
        let zeros = (-point) as usize;
        if 1 + zeros + digits.len() > MAX_DIGITS {
            return invalid("number", "expanded decimal exceeds 4096 digits");
        }
        ("0".into(), format!("{}{digits}", "0".repeat(zeros)))
    } else if point as usize >= digits.len() {
        let point = point as usize;
        if point > MAX_DIGITS {
            return invalid("number", "expanded decimal exceeds 4096 digits");
        }
        (format!("{digits}{}", "0".repeat(point - digits.len())), String::new())
    } else {
        (digits[..point as usize].into(), digits[point as usize..].into())
    };
    let trimmed = integer.trim_start_matches('0');
    Ok((
        negative,
        if trimmed.is_empty() { "0".into() } else { trimmed.into() },
        fraction,
    ))
}

fn group_integer(integer: &str, locale: EnglishLocale) -> String {
    if integer.len() <= 3 {
        return integer.into();
    }
    let mut pieces = Vec::new();
    let mut end = integer.len();
    let mut width = 3;
    while end > 0 {
        let start = end.saturating_sub(width);
        pieces.push(&integer[start..end]);
        end = start;
        if locale == EnglishLocale::In {
            width = 2;
        }
    }
    pieces.reverse();
    pieces.join(",")
}

/// Formats a strict `YYYY-MM-DD` Gregorian date without time-zone conversion.
pub fn format_date(value: &str, options: &DateOptions) -> Result<String, Error> {
    if value.len() != 10
        || !value.is_ascii()
        || &value[4..5] != "-"
        || &value[7..8] != "-"
        || !value
            .bytes()
            .enumerate()
            .all(|(index, digit)| matches!(index, 4 | 7) || digit.is_ascii_digit())
    {
        return invalid("date", "expected a Gregorian date in YYYY-MM-DD form");
    }
    let year = value[..4].parse::<i32>().expect("validated year");
    let month = value[5..7].parse::<u32>().expect("validated month");
    let day = value[8..].parse::<u32>().expect("validated day");
    let date = NaiveDate::from_ymd_opt(year, month, day)
        .filter(|date| date.year() >= 1)
        .ok_or(Error::InvalidField {
            field: "date",
            reason: "invalid Gregorian date",
        })?;
    Ok(match options.style {
        DateStyle::Short => match options.locale {
            EnglishLocale::Us => format!("{:02}/{:02}/{:04}", date.month(), date.day(), date.year()),
            EnglishLocale::Ca => format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day()),
            _ => format!("{:02}/{:02}/{:04}", date.day(), date.month(), date.year()),
        },
        DateStyle::Long => {
            let month = MONTHS[date.month0() as usize];
            match options.locale {
                EnglishLocale::Us | EnglishLocale::Ca => format!("{month} {}, {}", date.day(), date.year()),
                _ => format!("{} {month} {}", date.day(), date.year()),
            }
        }
    })
}

/// Formats a strict `HH:MM` or `HH:MM:SS` local clock value.
///
/// Regional hour-cycle defaults follow CLDR time preferences. An explicit
/// `hour_cycle` overrides the region without changing the supplied clock time.
pub fn format_time(value: &str, options: &TimeOptions) -> Result<String, Error> {
    if !matches!(value.len(), 5 | 8)
        || !value.is_ascii()
        || !value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 2 | 5) {
                byte == b':'
            } else {
                byte.is_ascii_digit()
            }
        })
    {
        return invalid("time", "expected a local time in HH:MM or HH:MM:SS form");
    }
    let hour = value[..2].parse::<u8>().expect("validated hour");
    let minute = value[3..5].parse::<u8>().expect("validated minute");
    let second = if value.len() == 8 {
        value[6..8].parse::<u8>().expect("validated second")
    } else {
        0
    };
    if hour > 23 || minute > 59 || second > 59 {
        return invalid("time", "invalid local clock time");
    }
    // https://github.com/unicode-org/cldr/blob/release-48/common/supplemental/supplementalData.xml
    let cycle = options.hour_cycle.unwrap_or(if options.locale == EnglishLocale::Gb {
        HourCycle::H24
    } else {
        HourCycle::H12
    });
    let seconds = if options.seconds {
        format!(":{second:02}")
    } else {
        String::new()
    };
    Ok(match cycle {
        HourCycle::H24 => format!("{hour:02}:{minute:02}{seconds}"),
        HourCycle::H12 => format!(
            "{}:{minute:02}{seconds} {}",
            if hour % 12 == 0 { 12 } else { hour % 12 },
            if hour < 12 { "AM" } else { "PM" }
        ),
    })
}

fn invalid<T>(field: &'static str, reason: &'static str) -> Result<T, Error> {
    Err(Error::InvalidField { field, reason })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_decimal_rounding_handles_carry_exponents_and_negative_zero() {
        for (source, precision, expected) in [
            ("2.675", Some(2), "2.68"),
            ("-2.675", Some(2), "-2.68"),
            ("999.995", Some(2), "1,000.00"),
            ("-0.004", Some(2), "0.00"),
            ("1.2345e3", None, "1,234.5"),
            ("5e-3", Some(2), "0.01"),
            ("00012.3400", None, "12.34"),
            ("10", Some(3), "10.000"),
            ("-5.5", Some(0), "-6"),
            ("0e100", None, "0"),
        ] {
            let options = NumberOptions {
                fraction_digits: precision,
                ..Default::default()
            };
            assert_eq!(format_number(source, &options).unwrap(), expected, "{source}");
        }
        assert_eq!(
            format_number(
                "12345678.9",
                &NumberOptions {
                    locale: EnglishLocale::In,
                    ..Default::default()
                }
            )
            .unwrap(),
            "1,23,45,678.9"
        );
        assert_eq!(
            format_number(
                "12345678.9",
                &NumberOptions {
                    grouping: false,
                    ..Default::default()
                }
            )
            .unwrap(),
            "12345678.9"
        );
    }

    #[test]
    fn regional_date_profiles_are_explicit_and_calendar_checked() {
        for (locale, short, long) in [
            (EnglishLocale::Us, "11/10/2026", "November 10, 2026"),
            (EnglishLocale::Ca, "2026-11-10", "November 10, 2026"),
            (EnglishLocale::Gb, "10/11/2026", "10 November 2026"),
            (EnglishLocale::Au, "10/11/2026", "10 November 2026"),
            (EnglishLocale::In, "10/11/2026", "10 November 2026"),
        ] {
            assert_eq!(
                format_date(
                    "2026-11-10",
                    &DateOptions {
                        locale,
                        style: DateStyle::Short
                    }
                )
                .unwrap(),
                short
            );
            assert_eq!(
                format_date(
                    "2026-11-10",
                    &DateOptions {
                        locale,
                        style: DateStyle::Long
                    }
                )
                .unwrap(),
                long
            );
        }
        assert_eq!(
            format_date("2000-02-29", &DateOptions::default()).unwrap(),
            "February 29, 2000"
        );
        for source in [
            "1900-02-29",
            "2025-02-29",
            "2026-04-31",
            "0000-01-01",
            "2026-1-01",
            "2026-11-10Z",
            "２０２６-11-10",
        ] {
            assert!(format_date(source, &DateOptions::default()).is_err(), "{source}");
        }
    }

    #[test]
    fn hour_cycles_preserve_midnight_noon_and_seconds() {
        assert_eq!(format_time("00:05", &TimeOptions::default()).unwrap(), "12:05 AM");
        assert_eq!(
            format_time(
                "12:05:09",
                &TimeOptions {
                    seconds: true,
                    ..Default::default()
                }
            )
            .unwrap(),
            "12:05:09 PM"
        );
        assert_eq!(
            format_time(
                "23:59",
                &TimeOptions {
                    locale: EnglishLocale::Gb,
                    ..Default::default()
                }
            )
            .unwrap(),
            "23:59"
        );
        assert_eq!(
            format_time(
                "23:59",
                &TimeOptions {
                    locale: EnglishLocale::Gb,
                    hour_cycle: Some(HourCycle::H12),
                    seconds: true
                }
            )
            .unwrap(),
            "11:59:00 PM"
        );
        for source in ["24:00", "12:60", "12:00:60", "1:00", "12:00Z", "12:00:00.1", "12-00"] {
            assert!(format_time(source, &TimeOptions::default()).is_err(), "{source}");
        }
    }

    #[test]
    fn invalid_options_and_unbounded_decimals_fail() {
        for source in [
            "",
            "NaN",
            "Infinity",
            "1e",
            "1e999999999",
            "1e4096",
            "1e-4096",
            "1,000",
            ".5",
            "1.",
            "1.2.3",
            "--1",
            " 1",
            "١",
        ] {
            assert!(format_number(source, &NumberOptions::default()).is_err(), "{source}");
        }
        assert!(
            format_number(
                "1",
                &NumberOptions {
                    fraction_digits: Some(10),
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert!(serde_json::from_str::<NumberOptions>(r#"{"locale":"fr-FR"}"#).is_err());
        for value in ["1", "1.0", "1e0"] {
            let options: NumberOptions = serde_json::from_str(&format!("{{\"fraction_digits\":{value}}}")).unwrap();
            assert_eq!(format_number("1.25", &options).unwrap(), "1.3");
        }
        for value in ["true", "1.5", "-1", "10", "\"2\""] {
            assert!(serde_json::from_str::<NumberOptions>(&format!("{{\"fraction_digits\":{value}}}")).is_err());
        }
        assert!(serde_json::from_str::<TimeOptions>(r#"{"time_zone":"UTC"}"#).is_err());
    }
}
