//! One agent run: a single user message handled end to end.

use serde::{Deserialize, Serialize};

use crate::Error;

/// Model round-trips allowed before a run fails.
pub const STEP_LIMIT: u32 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunState {
    Started,
    Thinking,
    CallingTools,
    WaitingOnHuman,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunTrigger {
    ToolCalls,
    ToolResults,
    NeedsApproval,
    ApprovalResolved,
    FinalAnswer,
    StepLimit,
    Error,
}

impl RunState {
    pub fn transition(self, trigger: RunTrigger) -> Result<RunState, Error> {
        use RunState as S;
        use RunTrigger as T;
        match (self, trigger) {
            (S::Done | S::Failed, t) => Err(Error::illegal("run", self, t)),
            (_, T::StepLimit | T::Error) => Ok(S::Failed),
            (S::Started | S::Thinking, T::ToolCalls) => Ok(S::CallingTools),
            (S::CallingTools, T::ToolResults) => Ok(S::Thinking),
            (S::CallingTools, T::NeedsApproval) => Ok(S::WaitingOnHuman),
            (S::WaitingOnHuman, T::ApprovalResolved) => Ok(S::Thinking),
            (S::Started | S::Thinking, T::FinalAnswer) => Ok(S::Done),
            (from, trigger) => Err(Error::illegal("run", from, trigger)),
        }
    }
}
