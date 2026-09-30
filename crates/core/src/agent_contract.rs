//! Typed admission, schedule, and result contracts for always-on agents.

use crate::routines::{next_fire_after, parse_cron, parse_timezone};
use chrono::{TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};

/// Versioned trigger specification stored in `config_agents.trigger_json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AgentTriggerSpec {
    pub event_only: bool,
    pub channel: Option<String>,
    pub group_only: bool,
    pub group_ids: Vec<String>,
    pub group_titles: Vec<String>,
    pub keywords: Vec<String>,
    pub priority: i32,
    pub observer: bool,
    pub schedule: Option<AgentScheduleSpec>,
}

/// The event fields an agent may use for deterministic trigger admission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentEvent {
    pub source: String,
    pub id: String,
    pub channel: String,
    pub recipient: String,
    pub group_id: Option<String>,
    pub group_name: Option<String>,
    pub text: String,
    pub occurred_at: i64,
}

/// How a scheduled fire interacts with a still-running agent.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentOverlapPolicy {
    #[default]
    Skip,
    BufferOne,
}

/// Quiet hours in the schedule's IANA timezone, using 24-hour local times.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentQuietHours {
    pub start: String,
    pub end: String,
}

/// Calendar schedule for an always-on agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentScheduleSpec {
    pub cron: String,
    pub timezone: String,
    #[serde(default)]
    pub overlap: AgentOverlapPolicy,
    #[serde(default = "default_catchup_secs")]
    pub catchup_secs: u32,
    #[serde(default)]
    pub quiet_hours: Option<AgentQuietHours>,
    #[serde(default)]
    pub target_conversation_id: Option<String>,
}

fn default_catchup_secs() -> u32 {
    3600
}

impl AgentScheduleSpec {
    /// Validate schedule syntax and policy bounds before activation.
    pub fn validate(&self) -> Result<(), String> {
        parse_cron(&self.cron).map_err(|error| error.to_string())?;
        parse_timezone(&self.timezone).map_err(|error| error.to_string())?;
        if self.catchup_secs > 7 * 24 * 3600 {
            return Err("agent schedule catchup_secs exceeds seven days".into());
        }
        if let Some(hours) = &self.quiet_hours {
            parse_local_minute(&hours.start)?;
            parse_local_minute(&hours.end)?;
        }
        Ok(())
    }

    /// Calculate the next fire strictly after a UTC Unix timestamp.
    pub fn next_fire_after(&self, after: i64) -> Result<Option<i64>, String> {
        self.validate()?;
        let schedule = parse_cron(&self.cron).map_err(|error| error.to_string())?;
        let timezone = parse_timezone(&self.timezone).map_err(|error| error.to_string())?;
        let after = Utc
            .timestamp_opt(after, 0)
            .single()
            .ok_or("invalid agent schedule timestamp")?;
        Ok(next_fire_after(&schedule, timezone, after).map(|fire| fire.timestamp()))
    }

    /// Return whether a scheduled fire falls inside configured quiet hours.
    pub fn is_quiet_at(&self, at: i64) -> Result<bool, String> {
        let Some(hours) = &self.quiet_hours else {
            return Ok(false);
        };
        let timezone = parse_timezone(&self.timezone).map_err(|error| error.to_string())?;
        let at = Utc
            .timestamp_opt(at, 0)
            .single()
            .ok_or("invalid agent schedule timestamp")?;
        let local = at.with_timezone(&timezone);
        let minute = local.hour() as u16 * 60 + local.minute() as u16;
        let start = parse_local_minute(&hours.start)?;
        let end = parse_local_minute(&hours.end)?;
        Ok(if start < end {
            minute >= start && minute < end
        } else if start > end {
            minute >= start || minute < end
        } else {
            false
        })
    }
}

fn parse_local_minute(value: &str) -> Result<u16, String> {
    let (hour_text, minute_text) = value.split_once(':').ok_or("quiet hours must use HH:MM")?;
    let hour: u16 = hour_text.parse().map_err(|_| "invalid quiet-hours hour")?;
    let minute: u16 = minute_text
        .parse()
        .map_err(|_| "invalid quiet-hours minute")?;
    if hour > 23 || minute > 59 || hour_text.len() != 2 || minute_text.len() != 2 {
        return Err("quiet hours must use HH:MM".into());
    }
    Ok(hour * 60 + minute)
}

impl AgentTriggerSpec {
    /// Decode and validate a stored trigger specification.
    pub fn from_value(value: &serde_json::Value) -> Result<Self, String> {
        let trigger: Self =
            serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
        trigger.validate()?;
        Ok(trigger)
    }

    /// Validate filters and the optional schedule.
    pub fn validate(&self) -> Result<(), String> {
        if self
            .channel
            .as_deref()
            .is_some_and(|channel| channel.trim().is_empty())
        {
            return Err("agent trigger channel must not be empty".into());
        }
        if self.group_ids.iter().any(|id| id.trim().is_empty())
            || self
                .group_titles
                .iter()
                .any(|title| title.trim().is_empty())
            || self
                .keywords
                .iter()
                .any(|keyword| keyword.trim().is_empty())
        {
            return Err("agent trigger filters must not contain empty values".into());
        }
        if let Some(schedule) = &self.schedule {
            schedule.validate()?;
        }
        Ok(())
    }

    /// Explain admission using only stable event metadata and text.
    pub fn match_reason(&self, event: &AgentEvent) -> Result<Option<&'static str>, String> {
        self.validate()?;
        if self.channel.is_none()
            && self.group_ids.is_empty()
            && self.group_titles.is_empty()
            && self.keywords.is_empty()
        {
            return Ok(None);
        }
        if self
            .channel
            .as_deref()
            .is_some_and(|channel| !channel.eq_ignore_ascii_case(&event.channel))
        {
            return Ok(None);
        }
        if self.group_only && event.group_id.is_none() {
            return Ok(None);
        }
        if !self.group_ids.is_empty()
            && !self
                .group_ids
                .iter()
                .any(|id| event.group_id.as_deref() == Some(id.as_str()))
        {
            return Ok(None);
        }
        if !self.group_titles.is_empty()
            && !self.group_titles.iter().any(|title| {
                event
                    .group_name
                    .as_deref()
                    .is_some_and(|name| title.eq_ignore_ascii_case(name))
            })
        {
            return Ok(None);
        }
        if self.keywords.is_empty() {
            return Ok(Some("source_filters"));
        }
        let haystack = if self.group_titles.is_empty() {
            format!(
                "{} {}",
                event.text,
                event.group_name.as_deref().unwrap_or("")
            )
        } else {
            event.text.clone()
        }
        .to_lowercase();
        Ok(self
            .keywords
            .iter()
            .any(|keyword| haystack.contains(&keyword.to_lowercase()))
            .then_some("keyword"))
    }
}

/// One effect-free trigger preview decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentPreviewDecision {
    pub source: String,
    pub event_id: String,
    pub matched: bool,
    pub reason: String,
}

/// Evaluate captured or synthetic events without queuing work or calling a model.
pub fn preview_events(
    trigger: &AgentTriggerSpec,
    events: &[AgentEvent],
) -> Result<Vec<AgentPreviewDecision>, String> {
    trigger.validate()?;
    events
        .iter()
        .map(|event| {
            if event.source.trim().is_empty() || event.id.trim().is_empty() {
                return Err("preview events require a stable source and id".into());
            }
            let reason = trigger.match_reason(event)?;
            Ok(AgentPreviewDecision {
                source: event.source.clone(),
                event_id: event.id.clone(),
                matched: reason.is_some(),
                reason: reason.unwrap_or("filtered").into(),
            })
        })
        .collect()
}

/// Validated result category for an always-on agent run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentOutcome {
    Irrelevant,
    DraftReady {
        review: String,
        suggested_reply: String,
        #[serde(default)]
        evidence_refs: Vec<String>,
    },
    NeedsInput {
        question: String,
    },
    Report {
        text: String,
        #[serde(default)]
        evidence_refs: Vec<String>,
    },
}

impl AgentOutcome {
    /// Parse a typed JSON result or a legacy Markdown review without trusting prose as status.
    pub fn parse(text: &str, draft_required: bool) -> Result<Self, String> {
        if text.trim().is_empty() {
            return Err("agent returned no visible output".into());
        }
        let outcome = if text.trim_start().starts_with('{') {
            serde_json::from_str::<Self>(text)
                .map_err(|error| format!("invalid structured agent outcome: {error}"))?
        } else if text.trim() == "NOT_APPLICABLE"
            || markdown_section(text, "Relevance") == Some("NOT_APPLICABLE")
        {
            Self::Irrelevant
        } else if let Some(reply) = markdown_section(text, "Suggested reply") {
            Self::DraftReady {
                review: text.trim().into(),
                suggested_reply: reply.into(),
                evidence_refs: Vec::new(),
            }
        } else if draft_required {
            return Err("agent output has no Suggested reply".into());
        } else {
            Self::Report {
                text: text.trim().into(),
                evidence_refs: Vec::new(),
            }
        };
        match &outcome {
            Self::DraftReady {
                review,
                suggested_reply,
                ..
            } if review.trim().is_empty() || suggested_reply.trim().is_empty() => {
                Err("agent draft is empty".into())
            }
            Self::NeedsInput { question } if question.trim().is_empty() => {
                Err("agent clarification is empty".into())
            }
            Self::Report { text, .. } if text.trim().is_empty() => {
                Err("agent report is empty".into())
            }
            Self::Report { .. } if draft_required => {
                Err("agent did not produce a required draft".into())
            }
            _ => Ok(outcome),
        }
    }

    /// Stable persisted status independent of model wording.
    pub fn status(&self) -> &'static str {
        match self {
            Self::Irrelevant => "irrelevant",
            Self::DraftReady { .. } => "draft_ready",
            Self::NeedsInput { .. } => "needs_input",
            Self::Report { .. } => "report_ready",
        }
    }
}

fn markdown_section<'a>(report: &'a str, name: &str) -> Option<&'a str> {
    let mut start = None;
    let mut end = report.len();
    let mut offset = 0;
    for line in report.split_inclusive('\n') {
        let heading = line.trim();
        if start.is_some() && heading.starts_with("## ") {
            end = offset;
            break;
        }
        if heading
            .strip_prefix("## ")
            .is_some_and(|value| value.eq_ignore_ascii_case(name))
        {
            start = Some(offset + line.len());
        }
        offset += line.len();
    }
    report
        .get(start?..end)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_preview_is_effect_free_and_group_scoped() {
        let trigger = AgentTriggerSpec {
            channel: Some("whatsapp".into()),
            group_only: true,
            group_ids: vec!["camper@g.us".into()],
            keywords: vec!["camper".into()],
            ..Default::default()
        };
        let event = AgentEvent {
            source: "whatsapp".into(),
            id: "message-1".into(),
            channel: "whatsapp".into(),
            recipient: "camper@g.us".into(),
            group_id: Some("camper@g.us".into()),
            group_name: None,
            text: "Can we rent the camper?".into(),
            occurred_at: 10,
        };
        let matched = preview_events(&trigger, &[event.clone()]).unwrap();
        assert_eq!(matched[0].reason, "keyword");
        let other = AgentEvent {
            group_id: Some("other@g.us".into()),
            ..event
        };
        assert!(!preview_events(&trigger, &[other]).unwrap()[0].matched);
    }

    #[test]
    fn structured_outcome_rejects_empty_draft_and_preserves_legacy_markdown() {
        assert_eq!(
            AgentOutcome::parse("", true).unwrap_err(),
            "agent returned no visible output"
        );
        assert!(
            AgentOutcome::parse(
                r#"{"kind":"draft_ready","review":"x","suggested_reply":""}"#,
                true
            )
            .is_err()
        );
        let legacy = "## Suggested reply\nHello.\n\n## Review notes\nCheck price.";
        assert!(
            matches!(AgentOutcome::parse(legacy, true).unwrap(), AgentOutcome::DraftReady { suggested_reply, .. } if suggested_reply == "Hello.")
        );
    }

    #[test]
    fn schedule_validates_timezone_and_quiet_hours() {
        let schedule = AgentScheduleSpec {
            cron: "0 8 * * *".into(),
            timezone: "America/Vancouver".into(),
            overlap: AgentOverlapPolicy::Skip,
            catchup_secs: 3600,
            quiet_hours: Some(AgentQuietHours {
                start: "22:00".into(),
                end: "07:00".into(),
            }),
            target_conversation_id: None,
        };
        schedule.validate().unwrap();
        assert!(schedule.next_fire_after(1_790_000_000).unwrap().is_some());
        assert!(
            AgentScheduleSpec {
                timezone: "Mars/Olympus".into(),
                ..schedule
            }
            .validate()
            .is_err()
        );
    }
}
