//! Saving payees safely. Adding or changing a contact is what turns "can only
//! pay known people" into "can pay anyone", so it is treated like a bank's new
//! payee: the agent may only ask, the owner confirms with an authenticator code,
//! and the contact can be paid only after a cooling-off period, with a notice
//! and a Cancel button in between. The active contacts are the policy's
//! recipient allowlist; every change to them writes a new policy version.
//! Removing a contact is immediate and needs no code.

use std::sync::Arc;

use agile_money_agent_core::tools::{ContactBook, ContactDesk};
use agile_money_domain::ports::ReadModel;
use agile_money_domain::{Address, AgentId, Policy, Recipient, Rule, SolAddress, UserId};
use agile_money_policy::fingerprint;
use agile_money_store::{Contact, ContactSource, ContactState, PgStore, PolicySource};
use async_trait::async_trait;
use jiff::{SignedDuration, Timestamp};
use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
use uuid::Uuid;

use crate::config;
use crate::security::Guard;

/// How long a request waits for the owner's code.
pub const PROPOSAL_TTL: SignedDuration = SignedDuration::from_mins(15);
const MAX_LABEL: usize = 40;

/// Every contact change goes through here, one at a time.
pub struct Desk {
    store: PgStore,
    agent: AgentId,
    owner: UserId,
    book: ContactBook,
    guard: Arc<Guard>,
    cooling_off: SignedDuration,
    bot: Bot,
    chat: ChatId,
    /// Serialises changes, so two can't write the same policy version.
    lock: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for Desk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Desk")
            .field("cooling_off", &self.cooling_off)
            .finish_non_exhaustive()
    }
}

/// A checked request, not stored yet.
struct Draft {
    label: String,
    address: Address,
    replaces: Option<Contact>,
    warnings: Vec<String>,
}

impl Desk {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: PgStore,
        agent: AgentId,
        owner: UserId,
        book: ContactBook,
        guard: Arc<Guard>,
        cooling_off: SignedDuration,
        bot: Bot,
        chat: ChatId,
    ) -> Self {
        Self {
            store,
            agent,
            owner,
            book,
            guard,
            cooling_off,
            bot,
            chat,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn guard(&self) -> &Guard {
        &self.guard
    }

    pub fn cooling_off(&self) -> SignedDuration {
        self.cooling_off
    }

    pub async fn list(&self) -> Result<Vec<Contact>, String> {
        self.store
            .contacts(self.agent)
            .await
            .map_err(|e| e.to_string())
    }

    /// Makes `[[contacts]]` in agile-money.toml active at once (whoever edits the
    /// file already holds the keys), then brings the policy in line.
    pub async fn seed(&self, contacts: &[config::Contact]) -> anyhow::Result<()> {
        let _lock = self.lock.lock().await;
        let now = Timestamp::now();
        let existing = self.store.contacts(self.agent).await?;
        for c in contacts {
            let address = Address::SolPubkey(c.address);
            let active = existing.iter().filter(|e| e.state == ContactState::Active);
            if active.clone().any(|e| e.address == address) {
                continue;
            }
            if let Some(clash) = active.clone().find(|e| same_label(&e.label, &c.label)) {
                tracing::warn!(label = %c.label, saved = %clash.address, "config contact skipped: the name is saved with another address");
                continue;
            }
            self.store
                .insert_contact(
                    self.agent,
                    &Contact {
                        id: Uuid::now_v7(),
                        label: c.label.clone(),
                        address,
                        state: ContactState::Active,
                        source: ContactSource::Config,
                        replaces: None,
                        proposed_at: now,
                        confirmed_at: Some(now),
                        active_at: Some(now),
                        closed_at: None,
                    },
                )
                .await?;
            tracing::info!(label = %c.label, "contact added from config");
        }
        self.sync().await.map_err(anyhow::Error::msg)
    }

    /// Checks a request against the saved contacts.
    async fn draft(&self, label: &str, address: &str) -> Result<Draft, String> {
        let label = label.trim();
        if label.is_empty() || label.chars().count() > MAX_LABEL {
            return Err(format!("A name needs 1 to {MAX_LABEL} characters."));
        }
        if label.chars().any(char::is_control) || label.starts_with('/') {
            return Err("That name has characters I can't use.".into());
        }
        let address: SolAddress = address
            .trim()
            .parse()
            .map_err(|_| "That isn't a Solana address.".to_owned())?;
        let address = Address::SolPubkey(address);
        let saved = self.list().await?;
        let active: Vec<&Contact> = saved
            .iter()
            .filter(|c| c.state == ContactState::Active)
            .collect();
        if let Some(same) = active.iter().find(|c| c.address == address) {
            return Err(format!("That address is already saved as {}.", same.label));
        }
        let replaces = active
            .iter()
            .find(|c| same_label(&c.label, label))
            .map(|c| (*c).clone());
        let mut warnings = Vec::new();
        if let Some(old) = &replaces {
            warnings.push(format!(
                "ADDRESS CHANGE: this replaces the saved address for {} ({}). Check with them another way first: \"our address changed\" is a common scam.",
                old.label,
                fingerprint(&old.address)
            ));
        }
        if let Some(twin) = active
            .iter()
            .find(|c| !same_label(&c.label, label) && skeleton(&c.label) == skeleton(label))
        {
            warnings.push(format!(
                "This name looks like your saved contact {}.",
                twin.label
            ));
        }
        if !label.is_ascii() {
            warnings.push("The name has non-Latin letters, which can imitate others.".into());
        }
        Ok(Draft {
            label: label.to_owned(),
            address,
            replaces,
            warnings,
        })
    }

    async fn insert(&self, draft: &Draft, source: ContactSource) -> Result<Contact, String> {
        let contact = Contact {
            id: Uuid::now_v7(),
            label: draft.label.clone(),
            address: draft.address.clone(),
            state: ContactState::Proposed,
            source,
            replaces: draft.replaces.as_ref().map(|c| c.id),
            proposed_at: Timestamp::now(),
            confirmed_at: None,
            active_at: None,
            closed_at: None,
        };
        self.store
            .insert_contact(self.agent, &contact)
            .await
            .map_err(|e| e.to_string())?;
        Ok(contact)
    }

    /// Records a request and asks the owner for their code in Telegram. Only
    /// one request waits for a code at a time: a new one replaces it.
    pub async fn propose(
        &self,
        label: &str,
        address: &str,
        source: ContactSource,
    ) -> Result<Contact, String> {
        if !self.guard.enabled() {
            return Err(crate::security::CodeError::NotSetUp.to_string());
        }
        let _lock = self.lock.lock().await;
        let draft = self.draft(label, address).await?;
        let now = Timestamp::now();
        for open in self.list().await? {
            if open.state == ContactState::Proposed {
                self.close(
                    open.id,
                    &[ContactState::Proposed],
                    ContactState::Cancelled,
                    now,
                )
                .await?;
            }
        }
        let contact = self.insert(&draft, source).await?;
        let from = match source {
            ContactSource::Agent => " (asked for in chat)",
            ContactSource::Dashboard => " (from the dashboard)",
            _ => "",
        };
        let mut text = format!(
            "New payee request{from}\n\nName: {}\nAddress: {}\nCheck: {}\n",
            contact.label,
            contact.address,
            fingerprint(&contact.address)
        );
        for w in &draft.warnings {
            text.push_str(&format!("\n⚠️ {w}\n"));
        }
        text.push_str(&format!(
            "\nTo save it, send /confirm and the 6-digit code from your authenticator app within {}. \
             It can be paid {} after you confirm, and its first payment will still need your OK.",
            human(PROPOSAL_TTL),
            human(self.cooling_off)
        ));
        self.notify(text, Some(contact.id)).await;
        Ok(contact)
    }

    /// Confirms the request waiting for a code. Returns what to tell the owner
    /// and, while it isn't live yet, the contact to offer a Cancel button for.
    pub async fn confirm(&self, code: &str) -> Result<(String, Option<Uuid>), String> {
        let _lock = self.lock.lock().await;
        let now = Timestamp::now();
        let waiting = self
            .list()
            .await?
            .into_iter()
            .filter(|c| c.state == ContactState::Proposed && c.proposed_at + PROPOSAL_TTL > now)
            .max_by_key(|c| c.proposed_at)
            .ok_or("Nothing is waiting for a code.")?;
        self.guard.check(code, now).map_err(|e| e.to_string())?;
        self.confirmed(waiting, now).await
    }

    /// Adds a contact from the dashboard, where the code comes with the request.
    /// The owner still gets a notice in Telegram with a Cancel button.
    pub async fn add_with_code(
        &self,
        label: &str,
        address: &str,
        code: &str,
    ) -> Result<String, String> {
        let _lock = self.lock.lock().await;
        let draft = self.draft(label, address).await?;
        let now = Timestamp::now();
        self.guard.check(code, now).map_err(|e| e.to_string())?;
        let contact = self.insert(&draft, ContactSource::Dashboard).await?;
        let (note, cancel) = self.confirmed(contact, now).await?;
        let warnings: String = draft
            .warnings
            .iter()
            .map(|w| format!("\n\n⚠️ {w}"))
            .collect();
        self.notify(
            format!("Added on the dashboard: {note}{warnings}\n\nNot you? Cancel it and change the dashboard password."),
            cancel,
        )
        .await;
        Ok(note)
    }

    async fn confirmed(
        &self,
        contact: Contact,
        now: Timestamp,
    ) -> Result<(String, Option<Uuid>), String> {
        let live = now + self.cooling_off;
        let moved = self
            .store
            .move_contact(
                contact.id,
                &[ContactState::Proposed],
                ContactState::Confirmed,
                Some(live),
                now,
            )
            .await
            .map_err(|e| e.to_string())?;
        if !moved {
            return Err("That request was already closed.".into());
        }
        tracing::info!(label = %contact.label, %live, "contact confirmed");
        if self.cooling_off.is_zero() {
            self.activate(&contact, now).await?;
            let note = format!(
                "{} ({}) is saved and can be paid now. Its first payment will still need your OK.",
                contact.label,
                fingerprint(&contact.address)
            );
            return Ok((note, None));
        }
        let note = format!(
            "{} ({}) can be paid from {} UTC, in {}. Until then you can cancel it.",
            contact.label,
            fingerprint(&contact.address),
            live.strftime("%b %-d %H:%M"),
            human(self.cooling_off)
        );
        Ok((note, Some(contact.id)))
    }

    /// Cancels a request or a confirmed contact that isn't live yet. No code:
    /// it only takes away.
    pub async fn cancel(&self, id: Uuid) -> Result<String, String> {
        let _lock = self.lock.lock().await;
        let contact = self
            .store
            .contact(id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("No such contact.")?;
        let open = [ContactState::Proposed, ContactState::Confirmed];
        if !self
            .close(id, &open, ContactState::Cancelled, Timestamp::now())
            .await?
        {
            return Err("That's already closed.".into());
        }
        Ok(format!("Cancelled. {} was not saved.", contact.label))
    }

    /// Removes a saved contact at once, with any change waiting for it.
    pub async fn remove(&self, id: Uuid) -> Result<String, String> {
        let _lock = self.lock.lock().await;
        let now = Timestamp::now();
        let contact = self
            .store
            .contact(id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("No such contact.")?;
        if !self
            .close(id, &[ContactState::Active], ContactState::Removed, now)
            .await?
        {
            return Err("That contact isn't saved.".into());
        }
        for pending in self.list().await? {
            if pending.replaces == Some(id) {
                let open = [ContactState::Proposed, ContactState::Confirmed];
                self.close(pending.id, &open, ContactState::Cancelled, now)
                    .await?;
            }
        }
        self.sync().await?;
        Ok(format!(
            "Removed {}. It can't be paid any more.",
            contact.label
        ))
    }

    /// The saved contact called `label`, for /removecontact.
    pub async fn find_active(&self, label: &str) -> Result<Option<Contact>, String> {
        Ok(self
            .list()
            .await?
            .into_iter()
            .find(|c| c.state == ContactState::Active && same_label(&c.label, label)))
    }

    /// Expires requests nobody confirmed and brings due contacts live.
    pub async fn tick(&self) {
        let _lock = self.lock.lock().await;
        let now = Timestamp::now();
        let contacts = match self.list().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "contact sweep failed");
                return;
            }
        };
        for c in contacts {
            let result = match c.state {
                ContactState::Proposed if c.proposed_at + PROPOSAL_TTL <= now => self
                    .close(c.id, &[ContactState::Proposed], ContactState::Expired, now)
                    .await
                    .map(|_| ()),
                ContactState::Confirmed if c.active_at.is_some_and(|t| t <= now) => {
                    match self.activate(&c, now).await {
                        Ok(()) => {
                            self.notify(
                                format!(
                                    "{} ({}) can now be paid. Its first payment will still need your OK.",
                                    c.label,
                                    fingerprint(&c.address)
                                ),
                                None,
                            )
                            .await;
                            Ok(())
                        }
                        Err(e) => Err(e),
                    }
                }
                _ => Ok(()),
            };
            if let Err(e) = result {
                tracing::warn!(error = %e, label = %c.label, "contact not updated");
            }
        }
    }

    async fn activate(&self, contact: &Contact, now: Timestamp) -> Result<(), String> {
        if let Some(old) = contact.replaces {
            self.close(old, &[ContactState::Active], ContactState::Removed, now)
                .await?;
        }
        let moved = self
            .store
            .move_contact(
                contact.id,
                &[ContactState::Confirmed],
                ContactState::Active,
                None,
                now,
            )
            .await
            .map_err(|e| e.to_string())?;
        if moved {
            tracing::info!(label = %contact.label, "contact active");
        }
        self.sync().await
    }

    async fn close(
        &self,
        id: Uuid,
        from: &[ContactState],
        to: ContactState,
        now: Timestamp,
    ) -> Result<bool, String> {
        self.store
            .move_contact(id, from, to, None, now)
            .await
            .map_err(|e| e.to_string())
    }

    /// Writes the active contacts into the policy's recipient allowlist when
    /// they differ, and into the agent's contact book.
    async fn sync(&self) -> Result<(), String> {
        let active: Vec<Recipient> = self
            .list()
            .await?
            .into_iter()
            .filter(|c| c.state == ContactState::Active)
            .map(|c| Recipient {
                label: c.label,
                address: c.address,
            })
            .collect();
        let addresses: Vec<Address> = active.iter().map(|r| r.address.clone()).collect();
        let current = self
            .store
            .policy(self.agent)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(current) = current {
            let listed = current.rules.iter().find_map(|r| match r {
                Rule::AllowRecipients { addresses } => Some(addresses),
                _ => None,
            });
            let same = listed.is_some_and(|l| {
                l.len() == addresses.len() && l.iter().all(|a| addresses.contains(a))
            });
            if !same {
                let next = with_recipients(&current, addresses);
                self.store
                    .save_policy(&next, self.owner, PolicySource::Contacts)
                    .await
                    .map_err(|e| e.to_string())?;
                tracing::info!(version = next.version, "policy updated: contacts changed");
            }
        }
        self.book.replace(active);
        Ok(())
    }

    /// A message to the owner, with a Cancel button for a contact not live yet.
    pub async fn notify(&self, text: String, cancel: Option<Uuid>) {
        let mut message = self.bot.send_message(self.chat, text);
        if let Some(id) = cancel {
            message = message.reply_markup(InlineKeyboardMarkup::new([[
                InlineKeyboardButton::callback("Cancel", format!("cx:{}", id.simple())),
            ]]));
        }
        if let Err(e) = message.await {
            tracing::warn!(error = %e, "contact notice not delivered");
        }
    }
}

#[async_trait]
impl ContactDesk for Desk {
    async fn propose(&self, label: &str, address: &str) -> Result<String, String> {
        let contact = Desk::propose(self, label, address, ContactSource::Agent).await?;
        Ok(format!(
            "The owner has a request to save {} and must confirm it with their authenticator code; it can be paid {} after that. It is not a contact yet.",
            contact.label,
            human(self.cooling_off)
        ))
    }
}

/// The policy with its recipient allowlist set to `addresses`, as the next version.
fn with_recipients(current: &Policy, addresses: Vec<Address>) -> Policy {
    let mut rules: Vec<Rule> = current
        .rules
        .iter()
        .filter(|r| !matches!(r, Rule::AllowRecipients { .. }))
        .cloned()
        .collect();
    // After the asset allowlist, where the bot seeds it.
    let at = usize::from(matches!(rules.first(), Some(Rule::AllowAssets { .. })));
    rules.insert(at, Rule::AllowRecipients { addresses });
    Policy {
        version: current.version + 1,
        rules,
        ..current.clone()
    }
}

fn same_label(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// A name with look-alike characters folded together, so "B0b" matches "Bob".
fn skeleton(label: &str) -> String {
    label
        .to_lowercase()
        .replace("rn", "m")
        .replace("vv", "w")
        .chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| match c {
            '0' => 'o',
            '1' | 'i' | '|' => 'l',
            '3' => 'e',
            '5' => 's',
            '8' => 'b',
            c => c,
        })
        .collect()
}

/// "24 hours", "15 minutes".
pub fn human(d: SignedDuration) -> String {
    let mins = d.as_mins();
    let (n, unit) = if mins >= 60 && mins % 60 == 0 {
        (mins / 60, "hour")
    } else if mins >= 1 {
        (mins, "minute")
    } else {
        (d.as_secs(), "second")
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agile_money_domain::PolicyId;

    #[test]
    fn look_alike_names_share_a_skeleton() {
        assert_eq!(skeleton("Bob"), skeleton("B0b"));
        assert_eq!(skeleton("Acme Design"), skeleton("Acrne Design"));
        assert_ne!(skeleton("Bob"), skeleton("Rob"));
    }

    #[test]
    fn durations_read_as_words() {
        assert_eq!(human(SignedDuration::from_hours(24)), "24 hours");
        assert_eq!(human(SignedDuration::from_mins(15)), "15 minutes");
        assert_eq!(human(SignedDuration::from_mins(1)), "1 minute");
    }

    #[test]
    fn the_allowlist_is_swapped_in_place_as_a_new_version() {
        let a = Address::SolPubkey(SolAddress::from_bytes([1; 32]));
        let b = Address::SolPubkey(SolAddress::from_bytes([2; 32]));
        let current = Policy {
            id: PolicyId::new(Uuid::nil()),
            version: 4,
            agent: AgentId::new(Uuid::nil()),
            rules: vec![
                Rule::AllowAssets { assets: vec![] },
                Rule::AllowRecipients {
                    addresses: vec![a.clone()],
                },
                Rule::ApproveFirstPayment,
            ],
        };
        let next = with_recipients(&current, vec![a, b]);
        assert_eq!(next.version, 5);
        assert!(
            matches!(&next.rules[1], Rule::AllowRecipients { addresses } if addresses.len() == 2)
        );
        assert_eq!(next.rules.len(), 3);
    }
}
