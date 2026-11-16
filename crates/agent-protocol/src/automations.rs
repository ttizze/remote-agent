//! A Host-owned schedule submits through the normal idempotent conversation API.
use crate::{models::ModelRef, session::SessionRef};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Leave room in the 16 MiB RPC frame for worst-case JSON escaping and metadata.
pub const AUTOMATION_LIMIT: i64 = 16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Schedule {
    Once {
        at_ms: i64,
    },
    Interval {
        every_ms: u64,
    },
    /// Explicit fixed offset; the schedule does not silently change with Host TZ.
    FixedTime {
        minute_of_day: u16,
        weekdays: Vec<u8>,
        utc_offset_minutes: i16,
    },
}
impl Schedule {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Once { at_ms } if *at_ms <= 0 => Err("execution time must be positive"),
            Self::Interval { every_ms } if !(60_000..=31_536_000_000).contains(every_ms) => {
                Err("interval must be between one minute and one year")
            }
            Self::FixedTime {
                minute_of_day,
                weekdays,
                utc_offset_minutes,
            } if *minute_of_day >= 1440
                || weekdays.len() > 7
                || weekdays.iter().any(|day| *day > 6)
                || !(-840..=840).contains(utc_offset_minutes) =>
            {
                Err("invalid fixed-time schedule")
            }
            _ => Ok(()),
        }
    }
    pub fn next_after(&self, now: i64) -> Option<i64> {
        if self.validate().is_err() {
            return None;
        }
        match self {
            Self::Once { at_ms } => (*at_ms > now).then_some(*at_ms),
            Self::Interval { every_ms } => now.checked_add(*every_ms as i64),
            Self::FixedTime {
                minute_of_day,
                weekdays,
                utc_offset_minutes,
            } => {
                let offset = i64::from(*utc_offset_minutes) * 60_000;
                let day = now.checked_add(offset)?.div_euclid(86_400_000);
                (0..=7).find_map(|delta| {
                    let day = day.checked_add(delta)?;
                    let weekday = (day + 4).rem_euclid(7) as u8;
                    if !weekdays.is_empty() && !weekdays.contains(&weekday) {
                        return None;
                    }
                    let at = day
                        .checked_mul(86_400_000)?
                        .checked_add(i64::from(*minute_of_day) * 60_000)?
                        .checked_sub(offset)?;
                    (at > now).then_some(at)
                })
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Definition {
    pub id: Uuid,
    pub session: SessionRef,
    pub title: String,
    pub prompt: String,
    pub model: Option<ModelRef>,
    pub effort: Option<String>,
    pub service_tier: Option<String>,
    pub schedule: Schedule,
    pub enabled: bool,
}
impl Definition {
    pub fn next_after(&self, now: i64) -> Option<i64> {
        self.enabled
            .then(|| self.schedule.next_after(now))
            .flatten()
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        self.session.validate()?;
        if self.title.trim().is_empty()
            || self.title.len() > 1024
            || self.prompt.trim().is_empty()
            || self.prompt.len() > 128 * 1024
        {
            return Err("automation requires a title and bounded nonempty prompt");
        }
        if self
            .model
            .as_ref()
            .is_some_and(|model| model.id.trim().is_empty() || model.id.len() > 1024)
            || self.effort.as_ref().is_some_and(|value| value.len() > 128)
            || self
                .service_tier
                .as_ref()
                .is_some_and(|value| value.len() > 128)
        {
            return Err("invalid automation model options");
        }
        self.schedule.validate()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RunState {
    Sending,
    Accepted,
    Unknown,
    Failed { message: String },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: Uuid,
    pub at_ms: i64,
    pub state: RunState,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Automation {
    pub definition: Definition,
    pub next_at_ms: Option<i64>,
    pub last_run: Option<Run>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Action {
    /// Explicit recovery reuses an uncertain dispatch's operation ID.
    Retry {
        id: Uuid,
    },
    List,
    Save {
        definition: Box<Definition>,
    },
    RunNow {
        id: Uuid,
    },
    Pause {
        id: Uuid,
    },
    Resume {
        id: Uuid,
    },
    Remove {
        id: Uuid,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub session: SessionRef,
    pub action: Action,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub automations: Vec<Automation>,
}
impl Request {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.session.validate()?;
        if let Action::Save { definition } = &self.action {
            definition.validate()?;
            if definition.session != self.session {
                return Err("automation belongs to a different conversation");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn fixed_times_obey_the_requested_local_clock_and_weekday(
            now in 0i64..250_000_000_000_000,
            minute in 0u16..1440,
            offset in -840i16..=840,
            weekdays in proptest::collection::vec(0u8..7, 0..8),
        ) {
            let schedule = Schedule::FixedTime { minute_of_day: minute, weekdays: weekdays.clone(), utc_offset_minutes: offset };
            let at = schedule.next_after(now).unwrap();
            prop_assert!(at > now && at - now <= 7 * 86_400_000);
            let local = chrono::DateTime::from_timestamp_millis(at + i64::from(offset) * 60_000).unwrap();
            prop_assert_eq!(local.hour() * 60 + local.minute(), u32::from(minute));
            prop_assert_eq!(local.second(), 0);
            prop_assert!(weekdays.is_empty() || weekdays.contains(&(local.weekday().num_days_from_sunday() as u8)));
        }
    }
    #[test]
    fn fixed_times_are_strictly_future_and_preserve_weekday_and_offset_across_midnight() {
        let schedule = Schedule::FixedTime {
            minute_of_day: 0,
            weekdays: vec![5],
            utc_offset_minutes: 540,
        };
        // Epoch Thursday UTC: Friday midnight in UTC+9 is Thursday 15:00 UTC.
        assert_eq!(schedule.next_after(0), Some(15 * 3_600_000));
        assert_eq!(
            schedule.next_after(15 * 3_600_000),
            Some(15 * 3_600_000 + 7 * 86_400_000)
        );
        assert_eq!(
            Schedule::Interval { every_ms: 60_000 }.next_after(i64::MAX),
            None
        );
        assert_eq!(Schedule::Once { at_ms: 5 }.next_after(5), None);
        assert!(Schedule::Interval { every_ms: 59_999 }.validate().is_err());
    }
}
