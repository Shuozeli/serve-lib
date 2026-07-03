use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ServeError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DurationSpec {
    seconds: u64,
}

impl Serialize for DurationSpec {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&format_duration(self.seconds))
    }
}

impl<'de> Deserialize<'de> for DurationSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

fn format_duration(seconds: u64) -> String {
    if seconds % (24 * 60 * 60) == 0 {
        format!("{}d", seconds / (24 * 60 * 60))
    } else if seconds % (60 * 60) == 0 {
        format!("{}h", seconds / (60 * 60))
    } else if seconds % 60 == 0 {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

impl DurationSpec {
    pub fn from_seconds(seconds: u64) -> Result<Self, ServeError> {
        if seconds == 0 {
            return Err(ServeError::InvalidDuration(
                "duration must be greater than zero".to_string(),
            ));
        }
        Ok(Self { seconds })
    }

    pub fn as_duration(self) -> Duration {
        Duration::from_secs(self.seconds)
    }

    pub fn as_seconds(self) -> u64 {
        self.seconds
    }
}

impl FromStr for DurationSpec {
    type Err = ServeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        if value.is_empty() {
            return Err(ServeError::InvalidDuration(
                "duration cannot be empty".to_string(),
            ));
        }

        let (number, unit) = split_number_and_unit(value)?;
        let amount = number
            .parse::<u64>()
            .map_err(|_| ServeError::InvalidDuration(value.to_string()))?;
        if amount == 0 {
            return Err(ServeError::InvalidDuration(
                "duration must be greater than zero".to_string(),
            ));
        }

        let multiplier = match unit {
            "s" => 1,
            "m" => 60,
            "h" => 60 * 60,
            "d" => 24 * 60 * 60,
            _ => return Err(ServeError::InvalidDuration(value.to_string())),
        };

        amount
            .checked_mul(multiplier)
            .ok_or_else(|| ServeError::InvalidDuration(value.to_string()))
            .and_then(DurationSpec::from_seconds)
    }
}

fn split_number_and_unit(value: &str) -> Result<(&str, &str), ServeError> {
    let split_at = value
        .find(|ch: char| !ch.is_ascii_digit())
        .ok_or_else(|| ServeError::InvalidDuration(value.to_string()))?;
    let (number, unit) = value.split_at(split_at);
    if number.is_empty() || unit.is_empty() {
        return Err(ServeError::InvalidDuration(value.to_string()));
    }
    Ok((number, unit))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_units() {
        assert_eq!("30s".parse::<DurationSpec>().unwrap().as_seconds(), 30);
        assert_eq!("10m".parse::<DurationSpec>().unwrap().as_seconds(), 600);
        assert_eq!("2h".parse::<DurationSpec>().unwrap().as_seconds(), 7200);
        assert_eq!("1d".parse::<DurationSpec>().unwrap().as_seconds(), 86400);
    }

    #[test]
    fn rejects_invalid_durations() {
        assert!("".parse::<DurationSpec>().is_err());
        assert!("0s".parse::<DurationSpec>().is_err());
        assert!("15".parse::<DurationSpec>().is_err());
        assert!("1w".parse::<DurationSpec>().is_err());
    }
}
