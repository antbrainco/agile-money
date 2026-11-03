//! The agent loop with a scripted model, the real tools and the in-memory store.

#![allow(clippy::unwrap_used)] // helpers outside #[test] fns, which clippy.toml does not cover

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use jiff::Timestamp;
use serde_json::json;
use uuid::Uuid;
use warden_agent_core::tools::{ListContacts, PaymentStatus, ProposePayment};
use warden_agent_core::{
    Agent, DynTool, Error, Llm, Message, Reply, RunObserver, ToolCall, ToolSpec,
};
use warden_domain::memory::MemStore;
use warden_domain::ports::{FixedClock, OutboxCommand, SequentialIds};
use warden_domain::*;
use warden_policy::{Approver, Payments, ProposeOutcome};

fn sol() -> Asset {
    Asset {
        rail: RailId::Sol,
        code: "SOL".parse().unwrap(),
        decimals: 9,
        contract: None,
    }
}

fn acme() -> Recipient {
    Recipient {
        label: "Acme".into(),
        address: Address::SolPubkey(SolAddress::from_bytes([1; 32])),
    }
}

fn agent_id() -> AgentId {
    AgentId::new(Uuid::from_u128(2))
}

/// Replays canned replies and records what it was sent.
struct Scripted {
    replies: Mutex<VecDeque<Reply>>,
    seen: Mutex<Vec<Vec<Message>>>,
}

#[async_trait]
impl Llm for Scripted {
    async fn complete(&self, messages: &[Message], _: &[ToolSpec]) -> Result<Reply, Error> {
        self.seen.lock().unwrap().push(messages.to_vec());
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| Error::Llm("script ran out".into()))
    }
}

struct NoApprover;

#[async_trait]
impl Approver for NoApprover {
    async fn request(
        &self,
        _: &Approval,
        _: &Intent<Proposed>,
    ) -> Result<(), warden_policy::Error> {
        Ok(())
    }
}

fn call(name: &str, args: serde_json::Value) -> Reply {
    Reply::ToolCalls {
        calls: vec![ToolCall {
            id: format!("call-{name}").parse().unwrap(),
            name: name.into(),
            args,
        }],
        native: None,
    }
}

fn setup(replies: Vec<Reply>) -> (Agent, Arc<MemStore>, Arc<Scripted>) {
    let store = Arc::new(MemStore::with_policy(Policy {
        id: PolicyId::new(Uuid::from_u128(9)),
        version: 1,
        agent: agent_id(),
        rules: vec![
            Rule::AllowAssets {
                assets: vec![sol()],
            },
            Rule::AllowRecipients {
                addresses: vec![acme().address],
            },
            Rule::ApprovalAbove {
                amount: Amount::new(1_000_000_000, sol()),
            },
        ],
    }));
    let ids: Arc<SequentialIds> = Arc::new(SequentialIds::default());
    let clock = Arc::new(FixedClock::new(Timestamp::UNIX_EPOCH));
    let payments = Payments::new(
        store.clone(),
        store.clone(),
        Arc::new(NoApprover),
        clock.clone(),
        ids.clone(),
    );
    let tools: Vec<Arc<dyn DynTool>> = vec![
        Arc::new(ProposePayment::new(vec![acme()], vec![sol()], ids.clone())),
        Arc::new(ListContacts::new(vec![acme()])),
        Arc::new(PaymentStatus::new(payments.clone())),
    ];
    let llm = Arc::new(Scripted {
        replies: Mutex::new(replies.into()),
        seen: Mutex::default(),
    });
    let agent = Agent::new(llm.clone(), tools, payments, agent_id(), clock, ids);
    (agent, store, llm)
}

#[tokio::test]
async fn pays_a_contact_within_policy() {
    let (agent, store, llm) = setup(vec![
        call(
            "propose_payment",
            json!({ "to": "acme", "amount": "0.5", "asset": "SOL" }),
        ),
        Reply::Final {
            text: "Sending 0.5 SOL to Acme.".into(),
        },
    ]);
    let report = agent
        .run(&[], Untrusted::new("pay acme half a sol".into()))
        .await
        .unwrap();

    assert_eq!(report.state, RunState::Done);
    assert_eq!(report.steps, 2);
    let [ProposeOutcome::Queued { intent }] = report.proposals[..] else {
        panic!("expected one queued proposal, got {:?}", report.proposals);
    };
    assert_eq!(
        store.pending_commands(),
        [OutboxCommand::SubmitPayment { intent }]
    );
    // The model saw the policy's answer.
    let seen = llm.seen.lock().unwrap();
    let Message::ToolResult { content, .. } = seen[1].last().unwrap() else {
        panic!("tool result last");
    };
    assert_eq!(content["status"], "allowed");
}

/// Records what a run streams.
#[derive(Default)]
struct Seen(Mutex<Vec<String>>);

#[async_trait]
impl RunObserver for Seen {
    async fn text(&self, step: u32, delta: &str) {
        self.0.lock().unwrap().push(format!("{step}:{delta}"));
    }

    async fn tool_call(&self, step: u32, name: &str) {
        self.0.lock().unwrap().push(format!("{step}:call {name}"));
    }
}

#[tokio::test]
async fn observer_sees_each_steps_text() {
    let (agent, _, _) = setup(vec![
        call("list_contacts", json!({})),
        Reply::Final {
            text: "Acme is your only contact.".into(),
        },
    ]);
    let seen = Seen::default();
    agent
        .run_observed(&[], Untrusted::new("who can I pay?".into()), &seen)
        .await
        .unwrap();
    // A model that cannot stream still reports its final text once.
    assert_eq!(*seen.0.lock().unwrap(), ["2:Acme is your only contact."]);
}

#[tokio::test]
async fn large_payment_waits_for_approval() {
    let (agent, store, _) = setup(vec![
        call(
            "propose_payment",
            json!({ "to": "Acme", "amount": "2", "asset": "SOL" }),
        ),
        Reply::Final {
            text: "Waiting on you.".into(),
        },
    ]);
    let report = agent
        .run(&[], Untrusted::new("pay acme 2 sol".into()))
        .await
        .unwrap();
    assert!(matches!(
        report.proposals[..],
        [ProposeOutcome::AwaitingApproval { .. }]
    ));
    assert!(store.pending_commands().is_empty());
}

#[tokio::test]
async fn bad_arguments_go_back_to_the_model() {
    let (agent, _, llm) = setup(vec![
        call(
            "propose_payment",
            json!({ "to": "Acme", "amount": "lots", "asset": "SOL" }),
        ),
        Reply::Final {
            text: "How much?".into(),
        },
    ]);
    let report = agent
        .run(&[], Untrusted::new("pay acme".into()))
        .await
        .unwrap();
    assert!(report.proposals.is_empty());
    let seen = llm.seen.lock().unwrap();
    let Message::ToolResult { content, .. } = seen[1].last().unwrap() else {
        panic!("tool result last");
    };
    assert!(content["error"].as_str().unwrap().contains("not a decimal"));
}

#[tokio::test]
async fn step_limit_fails_the_run() {
    let loops = (0..3)
        .map(|_| call("list_contacts", json!({})))
        .collect::<Vec<_>>();
    let (agent, _, _) = setup(loops);
    let report = agent
        .with_step_limit(3)
        .run(&[], Untrusted::new("loop".into()))
        .await
        .unwrap();
    assert_eq!(report.state, RunState::Failed);
}
