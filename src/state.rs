//! Proposal state derived from a room's messages.
//!
//! A proposal is a message posted with `--propose`. Decisions are votes posted
//! with `--re <proposal id>` by a participant other than the proposal's author;
//! per voter only the latest vote counts. Everything else that carries a vote is
//! informational and never changes state. Completion and supersession are
//! explicit: `--complete --re P`, or a new `--propose --re P` by P's author or
//! the executor.

use crate::room::{Message, RoomFile};
use serde::Serialize;
use std::collections::BTreeMap;

pub const POLICY: &str = "approved by one, blocked by any reject";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Pending,
    Approved,
    Rejected,
    Abstained,
    Superseded,
    Completed,
}

impl State {
    pub fn is_terminal(self) -> bool {
        matches!(self, State::Superseded | State::Completed)
    }
    pub fn label(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Approved => "approved",
            State::Rejected => "rejected",
            State::Abstained => "abstained",
            State::Superseded => "superseded",
            State::Completed => "completed",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Vote {
    pub agent: String,
    pub decision: String,
    pub id: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Proposal {
    pub id: u64,
    pub author: String,
    pub timestamp: String,
    /// First line of the proposed action (or of the thoughts when the action is None).
    pub summary: String,
    pub state: State,
    pub votes: Vec<Vote>,
    /// Participants (other than the author) who have not voted yet.
    pub waiting_for: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_by: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Analysis {
    pub executor: String,
    pub policy: &'static str,
    pub proposals: Vec<Proposal>,
    /// Votes that do not count: unlinked, self-votes, unknown authors, or on non-proposals.
    pub informational_votes: usize,
}

impl Analysis {
    pub fn open(&self) -> impl Iterator<Item = &Proposal> {
        self.proposals.iter().filter(|p| !p.state.is_terminal())
    }
    pub fn get(&self, id: u64) -> Option<&Proposal> {
        self.proposals.iter().find(|p| p.id == id)
    }
}

fn summary_of(m: &Message) -> String {
    let src = if m.action.trim().is_empty() || m.action.trim().eq_ignore_ascii_case("None") {
        &m.thoughts
    } else {
        &m.action
    };
    let line = src.lines().next().unwrap_or("").trim();
    let s: String = line.chars().take(100).collect();
    if s.len() < line.len() {
        format!("{s}...")
    } else {
        s
    }
}

/// The executor of a room, falling back to the first participant for rooms whose
/// front matter predates roles (the same rule `cowork prompt` uses).
pub fn executor_of(rf: &RoomFile, participants: &[String]) -> String {
    rf.front
        .as_ref()
        .map(|f| f.executor.clone())
        .filter(|e| !e.is_empty())
        .or_else(|| participants.first().cloned())
        .unwrap_or_default()
}

pub fn analyze(rf: &RoomFile, participants: &[String]) -> Analysis {
    let executor = executor_of(rf, participants);
    let mut by_id: BTreeMap<u64, Proposal> = BTreeMap::new();
    let mut informational = 0usize;

    for m in &rf.messages {
        if m.proposal {
            if let Some(id) = m.id {
                by_id.insert(
                    id,
                    Proposal {
                        id,
                        author: m.agent.clone(),
                        timestamp: m.timestamp.clone(),
                        summary: summary_of(m),
                        state: State::Pending,
                        votes: Vec::new(),
                        waiting_for: Vec::new(),
                        superseded_by: None,
                        completed_by: None,
                    },
                );
            }
        }
    }

    for m in &rf.messages {
        let Some(mid) = m.id else {
            if !m.vote.is_empty() {
                informational += 1;
            }
            continue;
        };
        let target = m.re.and_then(|re| by_id.get(&re).map(|p| (p.author.clone(), re)));
        // Supersession: a new proposal replying to one of your own (or, as executor, anyone's).
        if m.proposal {
            if let Some((author, re)) = &target {
                if (m.agent == *author || m.agent == executor) && re != &mid {
                    if let Some(p) = by_id.get_mut(re) {
                        p.superseded_by = Some(mid);
                    }
                }
            }
        }
        // Explicit completion.
        if let Some(c) = m.completes {
            if let Some(p) = by_id.get_mut(&c) {
                if m.agent == p.author || m.agent == executor {
                    p.completed_by = Some(mid);
                }
            }
        }
        // Votes.
        if !m.vote.is_empty() {
            let counted = match (&target, m.decision()) {
                (Some((author, re)), Some(d)) if m.agent != *author && participants.iter().any(|a| a == &m.agent) => {
                    if let Some(p) = by_id.get_mut(re) {
                        p.votes.retain(|v| v.agent != m.agent);
                        p.votes.push(Vote { agent: m.agent.clone(), decision: d.to_string(), id: mid });
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            };
            if !counted {
                informational += 1;
            }
        }
    }

    let mut proposals: Vec<Proposal> = by_id.into_values().collect();
    for p in &mut proposals {
        p.state = if p.completed_by.is_some() {
            State::Completed
        } else if p.superseded_by.is_some() {
            State::Superseded
        } else if p.votes.iter().any(|v| v.decision == "reject") {
            State::Rejected
        } else if p.votes.iter().any(|v| v.decision == "approve") {
            State::Approved
        } else if !p.votes.is_empty() {
            State::Abstained
        } else {
            State::Pending
        };
        p.waiting_for = participants
            .iter()
            .filter(|a| **a != p.author && !p.votes.iter().any(|v| &v.agent == *a))
            .cloned()
            .collect();
    }
    Analysis { executor, policy: POLICY, proposals, informational_votes: informational }
}

/// Ids of messages that belong to an unresolved proposal thread: the proposal
/// itself and every message that replies to it. `cowork archive` keeps these.
pub fn live_thread_ids(rf: &RoomFile, participants: &[String]) -> Vec<u64> {
    let a = analyze(rf, participants);
    let open: Vec<u64> = a.open().map(|p| p.id).collect();
    rf.messages
        .iter()
        .filter_map(|m| {
            let id = m.id?;
            let in_thread = open.contains(&id) || m.re.map(|r| open.contains(&r)).unwrap_or(false);
            in_thread.then_some(id)
        })
        .collect()
}

/// "14m ago" style age for a room timestamp.
pub fn age(ts: &str) -> String {
    use chrono::{NaiveDateTime, Utc};
    let Ok(t) = NaiveDateTime::parse_from_str(ts.trim_end_matches(" UTC"), "%Y-%m-%d %H:%M:%S") else {
        return String::new();
    };
    let secs = (Utc::now().naive_utc() - t).num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}
