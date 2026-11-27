//! A provider-supplied reset instant, never an inferred retry deadline.
use core::fmt;

/// Whole Unix seconds in the UTC calendar years 1970 through 9999.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProviderLimitReset(u64);

impl ProviderLimitReset {
    pub fn from_unix_seconds(seconds: u64) -> Option<Self> {
        (seconds <= 253_402_300_799).then_some(Self(seconds))
    }

    pub fn unix_seconds(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ProviderLimitReset {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut days = self.0 / 86_400;
        let mut year = 1970_u32;
        let leap = |year: u32| {
            year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
        };
        loop {
            let year_days = if leap(year) { 366 } else { 365 };
            if days < year_days {
                break;
            }
            days -= year_days;
            year += 1;
        }
        let months = [
            31,
            if leap(year) { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ];
        let seconds = self.0 % 86_400;
        for (month, length) in months.into_iter().enumerate() {
            if days < length {
                return write!(
                    formatter,
                    "{year:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
                    month + 1,
                    days + 1,
                    seconds / 3600,
                    seconds / 60 % 60,
                    seconds % 60,
                );
            }
            days -= length;
        }
        Err(fmt::Error)
    }
}
