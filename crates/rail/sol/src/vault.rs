//! Client side of the vault program (`contracts/sol/vault`). The vault holds
//! the SOL and pays out only within limits the owner signed for, so the bot's
//! key can ask for a payment but can't raise a cap, add a recipient or drain it.
//! The instruction layout mirrors the program's; see its crate docs.

use serde::Deserialize;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;

pub const SEED: &[u8] = b"vault";
pub const MAX_RECIPIENTS: usize = 16;
// The tag predates the rename; accounts on chain carry it.
const DISCRIMINATOR: &[u8; 8] = b"tillyvlt";

/// `[rail.sol.vault]`: where the vault lives. Both are public keys.
#[derive(Debug, Clone, Deserialize)]
pub struct VaultConfig {
    pub program: SolKey,
    pub owner: SolKey,
}

/// A base58 public key from config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SolKey(pub Pubkey);

impl<'de> Deserialize<'de> for SolKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map(SolKey).map_err(serde::de::Error::custom)
    }
}

/// A vault: its program and the PDA for its owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vault {
    pub program: Pubkey,
    pub owner: Pubkey,
    pub address: Pubkey,
}

impl Vault {
    pub fn new(program: Pubkey, owner: Pubkey) -> Self {
        let (address, _) = Pubkey::find_program_address(&[SEED, owner.as_ref()], &program);
        Self {
            program,
            owner,
            address,
        }
    }

    pub fn from_config(config: &VaultConfig) -> Self {
        Self::new(config.program.0, config.owner.0)
    }

    /// The agent asks the vault to pay `to`. `extra` accounts (a Solana Pay
    /// reference) are ignored by the program but indexed with the transaction.
    pub fn pay(
        &self,
        agent: &Pubkey,
        to: &Pubkey,
        amount: u64,
        extra: &[AccountMeta],
    ) -> Instruction {
        let mut accounts = vec![
            AccountMeta::new_readonly(*agent, true),
            AccountMeta::new(self.address, false),
            AccountMeta::new(*to, false),
        ];
        accounts.extend_from_slice(extra);
        Instruction::new_with_bytes(self.program, &tagged(2, &amount.to_le_bytes()), accounts)
    }

    /// Opens the vault. Signed by the owner.
    pub fn init(&self, limits: &Limits) -> Instruction {
        Instruction::new_with_bytes(
            self.program,
            &tagged(0, &limits.encode()),
            vec![
                AccountMeta::new(self.owner, true),
                AccountMeta::new(self.address, false),
                AccountMeta::new_readonly(solana_system_interface::program::ID, false),
            ],
        )
    }

    /// Replaces the agent, caps and recipients. Signed by the owner.
    pub fn configure(&self, limits: &Limits) -> Instruction {
        Instruction::new_with_bytes(
            self.program,
            &tagged(1, &limits.encode()),
            self.owner_accounts(),
        )
    }

    /// Moves lamports back to the owner. Signed by the owner.
    pub fn withdraw(&self, amount: u64) -> Instruction {
        Instruction::new_with_bytes(
            self.program,
            &tagged(3, &amount.to_le_bytes()),
            self.owner_accounts(),
        )
    }

    fn owner_accounts(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.owner, true),
            AccountMeta::new(self.address, false),
        ]
    }
}

fn tagged(tag: u8, fields: &[u8]) -> Vec<u8> {
    [&[tag][..], fields].concat()
}

/// What the owner allows: who may ask, how much, and to whom. Amounts in lamports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    pub agent: Pubkey,
    pub max_per_tx: u64,
    pub max_per_day: u64,
    pub recipients: Vec<Pubkey>,
}

impl Limits {
    fn encode(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(49 + 32 * self.recipients.len());
        data.extend_from_slice(self.agent.as_ref());
        data.extend_from_slice(&self.max_per_tx.to_le_bytes());
        data.extend_from_slice(&self.max_per_day.to_le_bytes());
        data.push(self.recipients.len() as u8);
        for key in &self.recipients {
            data.extend_from_slice(key.as_ref());
        }
        data
    }
}

/// The vault account's state, as the program stores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub owner: Pubkey,
    pub limits: Limits,
    /// Unix seconds; the daily cap counts from here for 24 hours.
    pub window_start: i64,
    /// Lamports paid in the current window.
    pub spent: u64,
}

impl State {
    pub fn decode(data: &[u8]) -> Option<Self> {
        let rest = data.strip_prefix(DISCRIMINATOR)?;
        let key = |at: usize| {
            Some(Pubkey::new_from_array(
                rest.get(at..at + 32)?.try_into().ok()?,
            ))
        };
        let int = |at: usize| Some(u64::from_le_bytes(rest.get(at..at + 8)?.try_into().ok()?));
        let count = usize::from(*rest.get(97)?);
        if count > MAX_RECIPIENTS {
            return None;
        }
        Some(Self {
            owner: key(0)?,
            limits: Limits {
                agent: key(32)?,
                max_per_tx: int(64)?,
                max_per_day: int(72)?,
                recipients: (0..count)
                    .map(|i| key(98 + 32 * i))
                    .collect::<Option<_>>()?,
            },
            window_start: int(80)? as i64,
            spent: int(88)?,
        })
    }
}

/// Why the program refused, from its custom error code.
pub fn refusal(code: u32) -> Option<&'static str> {
    Some(match code {
        1 => "only the vault's owner can do that",
        2 => "this key is not the vault's agent",
        3 => "the vault doesn't list this recipient",
        4 => "over the vault's per-payment cap",
        5 => "over the vault's daily cap",
        6 => "the vault doesn't hold enough",
        7 => "the vault couldn't read the request",
        8 => "not this program's vault",
        9 => "too many recipients for the vault",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pay_carries_the_amount_and_extra_accounts() {
        let vault = Vault::new(Pubkey::new_unique(), Pubkey::new_unique());
        let (agent, to, reference) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        let ix = vault.pay(
            &agent,
            &to,
            5,
            &[AccountMeta::new_readonly(reference, false)],
        );
        assert_eq!(ix.data, [2, 5, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(ix.accounts[1].pubkey, vault.address);
        assert!(ix.accounts[0].is_signer && !ix.accounts[0].is_writable);
        assert!(ix.accounts[2].is_writable);
        assert_eq!(ix.accounts[3].pubkey, reference);
    }

    #[test]
    fn state_decodes_the_program_layout() {
        let (owner, agent, to) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        let mut data = DISCRIMINATOR.to_vec();
        data.extend_from_slice(owner.as_ref());
        data.extend_from_slice(agent.as_ref());
        for n in [100u64, 250, 86_400, 40] {
            data.extend_from_slice(&n.to_le_bytes());
        }
        data.extend_from_slice(&[254, 1]);
        data.extend_from_slice(to.as_ref());
        data.resize(8 + 98 + 32 * MAX_RECIPIENTS, 0);
        let state = State::decode(&data).unwrap();
        assert_eq!(state.owner, owner);
        assert_eq!(state.limits.agent, agent);
        assert_eq!(
            (state.limits.max_per_tx, state.limits.max_per_day),
            (100, 250)
        );
        assert_eq!((state.window_start, state.spent), (86_400, 40));
        assert_eq!(state.limits.recipients, [to]);
        assert!(State::decode(b"nope").is_none());
    }
}
