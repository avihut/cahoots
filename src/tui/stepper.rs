//! A number changed without typing: ← and → step it through round values (5%
//! at a time, a minute, a million tokens), so a held key crosses the range in
//! a moment, and a value between two of them steps to the next. `r` puts back
//! the default. No text and no terminal: `view` draws it.

use super::keys::Key;

/// What a number counts, which says how it steps and reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Percent,
    Seconds,
    Tokens,
    Count,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stepper {
    /// `None`: not set, and nothing is in its place.
    pub value: Option<i64>,
    pub unit: Unit,
    pub default: Option<i64>,
    stops: Vec<i64>,
    /// Where stepping starts from nothing.
    start: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stepped {
    Open,
    /// Enter: this is the answer.
    Answered(Option<i64>),
    /// Esc: no answer.
    Left,
}

/// The most a number steps up to when it has no ceiling of its own.
const CEILING: i64 = 10_000_000_000;

impl Stepper {
    pub fn new(
        value: Option<i64>,
        range: (i64, i64),
        unit: Unit,
        default: Option<i64>,
        start: i64,
    ) -> Stepper {
        let (min, max) = range;
        Stepper {
            value,
            unit,
            default,
            stops: stops(min, max, unit),
            start: start.clamp(min, max),
        }
    }

    pub fn press(&mut self, key: Key) -> Stepped {
        match key {
            Key::Right | Key::Up => {
                self.value = Some(match self.value {
                    None => self.start,
                    Some(now) => self.stops.iter().copied().find(|s| *s > now).unwrap_or(now),
                });
                Stepped::Open
            }
            Key::Left | Key::Down => {
                self.value = Some(match self.value {
                    None => self.start,
                    Some(now) => self
                        .stops
                        .iter()
                        .rev()
                        .copied()
                        .find(|s| *s < now)
                        .unwrap_or(now),
                });
                Stepped::Open
            }
            Key::Char('r') => {
                self.value = self.default;
                Stepped::Open
            }
            Key::Enter => Stepped::Answered(self.value),
            Key::Cancel | Key::Quit => Stepped::Left,
            _ => Stepped::Open,
        }
    }
}

/// The values a number stops at from `min` to `max`: every 5 for a percent,
/// every one for a short count, and otherwise round values — 1, 2, 3 … 10,
/// 12, 15, 20, 25, 30, 40, 50, 60, 80 of each power of ten, a minute or an
/// hour for seconds. The two ends are always among them.
pub fn stops(min: i64, max: i64, unit: Unit) -> Vec<i64> {
    let top = max.min(CEILING);
    let ladder: Vec<i64> = match unit {
        Unit::Percent => (0..=100).step_by(5).collect(),
        Unit::Count if top - min <= 20 => (min..=top).collect(),
        Unit::Count => round(1, top),
        Unit::Tokens => round(100_000, top),
        Unit::Seconds => vec![
            1, 2, 3, 5, 10, 15, 20, 30, 45, 60, 90, 120, 180, 300, 600, 900, 1200, 1800, 2700,
            3600, 5400, 7200, 10_800, 14_400, 21_600, 43_200, 86_400,
        ],
    };
    let mut stops: Vec<i64> = ladder
        .into_iter()
        .filter(|stop| (min..=top).contains(stop))
        .chain([min, top])
        .collect();
    stops.sort_unstable();
    stops.dedup();
    stops
}

/// Round values from `from` to `to`.
fn round(from: i64, to: i64) -> Vec<i64> {
    let mut values = Vec::new();
    let mut decade = from;
    while decade <= to {
        for tenths in [10, 12, 15, 20, 25, 30, 40, 50, 60, 80] {
            values.push(decade * tenths / 10);
        }
        decade = decade.saturating_mul(10);
    }
    values
}

/// A number as a person reads it: `60%`, `90s`, `30m`, `1h 30m`, `300M
/// tokens`, `3`.
pub fn number(n: i64, unit: Unit) -> String {
    match unit {
        Unit::Percent => format!("{n}%"),
        Unit::Count => n.to_string(),
        Unit::Seconds => match n {
            ..120 => format!("{n}s"),
            _ if n < 3600 && n % 60 == 0 => format!("{}m", n / 60),
            _ if n < 3600 => format!("{}m {}s", n / 60, n % 60),
            _ if n % 3600 == 0 => format!("{}h", n / 3600),
            _ => format!("{}h {}m", n / 3600, n % 3600 / 60),
        },
        Unit::Tokens => {
            let (scale, suffix) = match n {
                1_000_000_000.. => (1e9, "B"),
                1_000_000.. => (1e6, "M"),
                _ => (1e3, "k"),
            };
            let short = n as f64 / scale;
            if short.fract() == 0.0 {
                format!("{short:.0}{suffix} tokens")
            } else {
                format!("{short:.1}{suffix} tokens")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stepper(value: Option<i64>, range: (i64, i64), unit: Unit) -> Stepper {
        Stepper::new(value, range, unit, Some(75), 50)
    }

    #[test]
    fn a_percent_steps_by_five_and_stops_at_its_ends() {
        let mut cap = stepper(Some(75), (1, 100), Unit::Percent);
        cap.press(Key::Left);
        cap.press(Key::Left);
        assert_eq!(cap.value, Some(65));
        for _ in 0..30 {
            cap.press(Key::Right);
        }
        assert_eq!(cap.value, Some(100));
        let mut odd = stepper(Some(72), (1, 100), Unit::Percent);
        odd.press(Key::Right);
        assert_eq!(odd.value, Some(75), "between two stops, to the next");
        let mut stop = stepper(Some(80), (76, 100), Unit::Percent);
        stop.press(Key::Left);
        stop.press(Key::Left);
        assert_eq!(stop.value, Some(76), "the bottom of its own range");
    }

    #[test]
    fn nothing_steps_from_its_start_and_r_puts_the_default_back() {
        let mut limit = Stepper::new(None, (100_000, i64::MAX), Unit::Tokens, None, 50_000_000);
        limit.press(Key::Right);
        assert_eq!(limit.value, Some(50_000_000));
        limit.press(Key::Right);
        assert_eq!(limit.value, Some(60_000_000));
        limit.press(Key::Char('r'));
        assert_eq!(limit.value, None);
        assert_eq!(limit.press(Key::Enter), Stepped::Answered(None));
        assert_eq!(limit.press(Key::Cancel), Stepped::Left);
    }

    #[test]
    fn a_long_range_steps_through_round_values() {
        assert_eq!(
            stops(0, 600, Unit::Count)[..14],
            [0, 1, 2, 3, 4, 5, 6, 8, 10, 12, 15, 20, 25, 30]
        );
        assert_eq!(stops(1, 8, Unit::Count), [1, 2, 3, 4, 5, 6, 7, 8]);
        let tokens = stops(100_000, i64::MAX, Unit::Tokens);
        assert_eq!(tokens[..4], [100_000, 120_000, 150_000, 200_000]);
        assert!(tokens.contains(&300_000_000));
        assert_eq!(*tokens.last().unwrap(), CEILING);
        assert!(
            tokens.len() < 60,
            "a held key crosses it in a moment: {}",
            tokens.len()
        );
        assert_eq!(stops(30, 14_400, Unit::Seconds)[..5], [30, 45, 60, 90, 120]);
    }

    #[test]
    fn numbers_read_in_their_units() {
        assert_eq!(number(60, Unit::Percent), "60%");
        assert_eq!(number(90, Unit::Seconds), "90s");
        assert_eq!(number(1800, Unit::Seconds), "30m");
        assert_eq!(number(150, Unit::Seconds), "2m 30s");
        assert_eq!(number(5400, Unit::Seconds), "1h 30m");
        assert_eq!(number(86_400, Unit::Seconds), "24h");
        assert_eq!(number(300_000_000, Unit::Tokens), "300M tokens");
        assert_eq!(number(1_500_000, Unit::Tokens), "1.5M tokens");
        assert_eq!(number(150_000, Unit::Tokens), "150k tokens");
        assert_eq!(number(2_000_000_000, Unit::Tokens), "2B tokens");
        assert_eq!(number(3, Unit::Count), "3");
    }
}
