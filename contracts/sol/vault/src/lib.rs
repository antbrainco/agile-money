//! Tilly's vault: holds the agent's SOL and pays out only within limits the
//! owner set. The bot's hot key is the vault's `agent`: it can ask for a
//! payment, and this program refuses one that is over the per-payment cap,
//! over the daily cap, or to an address not on the owner's list. Changing the
//! limits or withdrawing needs the owner's key, which the bot never holds.
//!
//! So a stolen bot key can still spend, but only what the owner already allowed.
//!
//! One vault per owner, at the PDA `["vault", owner]`. Anyone can fund it with
//! a plain system transfer. Instructions, as a tag byte then little-endian fields:
//!
//! - 0 `init`      [owner (signer, writable), vault (writable), system program]
//!   agent: 32, max_per_tx: u64, max_per_day: u64, count: u8, recipients: count × 32
//! - 1 `configure` [owner (signer), vault (writable)], same data as `init`
//! - 2 `pay`       [agent (signer), vault (writable), recipient (writable), ...]
//!   amount: u64. Accounts past the recipient are ignored, so a Solana Pay
//!   reference can ride along.
//! - 3 `withdraw`  [owner (signer, writable), vault (writable)], amount: u64
//!
//! The daily cap counts a fixed 24-hour window that starts at the first payment
//! after the last window closed.

use solana_program::account_info::{AccountInfo, next_account_info};
use solana_program::clock::Clock;
use solana_program::entrypoint::ProgramResult;
use solana_program::instruction::{AccountMeta, Instruction};
use solana_program::msg;
use solana_program::program::invoke_signed;
use solana_program::program_error::ProgramError;
use solana_program::pubkey::Pubkey;
use solana_program::rent::Rent;
use solana_program::sysvar::Sysvar;

#[cfg(target_os = "solana")]
solana_program::entrypoint!(process_instruction);

pub const SEED: &[u8] = b"vault";
pub const MAX_RECIPIENTS: usize = 16;
pub const DAY: i64 = 24 * 60 * 60;
/// 11111111111111111111111111111111
const SYSTEM_PROGRAM: Pubkey = Pubkey::new_from_array([0; 32]);

const DISCRIMINATOR: [u8; 8] = *b"tillyvlt";
/// discriminator, owner, agent, max_per_tx, max_per_day, window_start, spent,
/// bump, count, recipients.
pub const SPACE: usize = 8 + 32 + 32 + 8 + 8 + 8 + 8 + 1 + 1 + 32 * MAX_RECIPIENTS;

/// Why a request was refused, as the custom error code the transaction fails with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum VaultError {
    NotOwner = 1,
    NotAgent = 2,
    UnknownRecipient = 3,
    OverPerPayment = 4,
    OverDaily = 5,
    Insufficient = 6,
    BadData = 7,
    WrongVault = 8,
    TooManyRecipients = 9,
}

impl From<VaultError> for ProgramError {
    fn from(e: VaultError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

/// The vault's stored state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vault {
    pub owner: Pubkey,
    pub agent: Pubkey,
    pub max_per_tx: u64,
    pub max_per_day: u64,
    pub window_start: i64,
    pub spent: u64,
    pub bump: u8,
    pub recipients: Vec<Pubkey>,
}

impl Vault {
    pub fn unpack(data: &[u8]) -> Result<Self, ProgramError> {
        if data.len() < SPACE || data[..8] != DISCRIMINATOR {
            return Err(VaultError::WrongVault.into());
        }
        let mut r = Reader(&data[8..]);
        let owner = r.key()?;
        let agent = r.key()?;
        let (max_per_tx, max_per_day) = (r.u64()?, r.u64()?);
        let (window_start, spent) = (r.u64()? as i64, r.u64()?);
        let (bump, count) = (r.u8()?, r.u8()? as usize);
        if count > MAX_RECIPIENTS {
            return Err(VaultError::BadData.into());
        }
        let recipients = (0..count).map(|_| r.key()).collect::<Result<_, _>>()?;
        Ok(Self {
            owner,
            agent,
            max_per_tx,
            max_per_day,
            window_start,
            spent,
            bump,
            recipients,
        })
    }

    fn pack(&self, data: &mut [u8]) -> Result<(), ProgramError> {
        if data.len() < SPACE || self.recipients.len() > MAX_RECIPIENTS {
            return Err(VaultError::BadData.into());
        }
        let mut out = Vec::with_capacity(SPACE);
        out.extend_from_slice(&DISCRIMINATOR);
        out.extend_from_slice(self.owner.as_ref());
        out.extend_from_slice(self.agent.as_ref());
        out.extend_from_slice(&self.max_per_tx.to_le_bytes());
        out.extend_from_slice(&self.max_per_day.to_le_bytes());
        out.extend_from_slice(&self.window_start.to_le_bytes());
        out.extend_from_slice(&self.spent.to_le_bytes());
        out.push(self.bump);
        out.push(self.recipients.len() as u8);
        for key in &self.recipients {
            out.extend_from_slice(key.as_ref());
        }
        out.resize(SPACE, 0);
        data[..SPACE].copy_from_slice(&out);
        Ok(())
    }

    /// Checks a payment against the limits and, if it fits, counts it.
    pub fn spend(&mut self, to: &Pubkey, amount: u64, now: i64) -> Result<(), VaultError> {
        if !self.recipients.contains(to) {
            return Err(VaultError::UnknownRecipient);
        }
        if amount > self.max_per_tx {
            return Err(VaultError::OverPerPayment);
        }
        if now.saturating_sub(self.window_start) >= DAY {
            self.window_start = now;
            self.spent = 0;
        }
        let spent = self
            .spent
            .checked_add(amount)
            .ok_or(VaultError::OverDaily)?;
        if spent > self.max_per_day {
            return Err(VaultError::OverDaily);
        }
        self.spent = spent;
        Ok(())
    }
}

/// The owner's settings, as `init` and `configure` carry them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    pub agent: Pubkey,
    pub max_per_tx: u64,
    pub max_per_day: u64,
    pub recipients: Vec<Pubkey>,
}

impl Limits {
    fn parse(data: &[u8]) -> Result<Self, ProgramError> {
        let mut r = Reader(data);
        let agent = r.key()?;
        let (max_per_tx, max_per_day) = (r.u64()?, r.u64()?);
        let count = r.u8()? as usize;
        if count > MAX_RECIPIENTS {
            return Err(VaultError::TooManyRecipients.into());
        }
        let recipients = (0..count).map(|_| r.key()).collect::<Result<_, _>>()?;
        if !r.0.is_empty() {
            return Err(VaultError::BadData.into());
        }
        Ok(Self {
            agent,
            max_per_tx,
            max_per_day,
            recipients,
        })
    }

    fn encode(&self) -> Vec<u8> {
        let mut data = Vec::new();
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

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], ProgramError> {
        let (head, rest) = self
            .0
            .split_first_chunk::<N>()
            .ok_or(ProgramError::from(VaultError::BadData))?;
        self.0 = rest;
        Ok(*head)
    }
    fn key(&mut self) -> Result<Pubkey, ProgramError> {
        self.take::<32>().map(Pubkey::new_from_array)
    }
    fn u64(&mut self) -> Result<u64, ProgramError> {
        self.take::<8>().map(u64::from_le_bytes)
    }
    fn u8(&mut self) -> Result<u8, ProgramError> {
        self.take::<1>().map(|[b]| b)
    }
}

/// The vault address for `owner`, and its bump.
pub fn vault_address(program: &Pubkey, owner: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED, owner.as_ref()], program)
}

pub fn process_instruction(
    program: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let (&tag, rest) = data
        .split_first()
        .ok_or(ProgramError::from(VaultError::BadData))?;
    match tag {
        0 => init(program, accounts, Limits::parse(rest)?),
        1 => configure(program, accounts, Limits::parse(rest)?),
        2 => pay(program, accounts, amount(rest)?),
        3 => withdraw(program, accounts, amount(rest)?),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

fn amount(data: &[u8]) -> Result<u64, ProgramError> {
    let bytes: [u8; 8] = data.try_into().map_err(|_| VaultError::BadData)?;
    Ok(u64::from_le_bytes(bytes))
}

fn init(program: &Pubkey, accounts: &[AccountInfo], limits: Limits) -> ProgramResult {
    let iter = &mut accounts.iter();
    let owner = next_account_info(iter)?;
    let vault = next_account_info(iter)?;
    let system_program = next_account_info(iter)?;
    if !owner.is_signer {
        return Err(VaultError::NotOwner.into());
    }
    let (address, bump) = vault_address(program, owner.key);
    if *vault.key != address || *system_program.key != SYSTEM_PROGRAM {
        return Err(VaultError::WrongVault.into());
    }
    if vault.owner == program {
        return Err(ProgramError::AccountAlreadyInitialized);
    }
    // Someone may have funded the address first, and create_account refuses an
    // account that holds lamports, so top up, allocate and assign instead.
    let seeds: &[&[u8]] = &[SEED, owner.key.as_ref(), &[bump]];
    let rent = Rent::get()?.minimum_balance(SPACE);
    let short = rent.saturating_sub(vault.lamports());
    if short > 0 {
        invoke_signed(
            &system(
                0x02,
                vec![
                    AccountMeta::new(*owner.key, true),
                    AccountMeta::new(*vault.key, false),
                ],
                &short.to_le_bytes(),
            ),
            &[owner.clone(), vault.clone(), system_program.clone()],
            &[],
        )?;
    }
    invoke_signed(
        &system(
            0x08,
            vec![AccountMeta::new(*vault.key, true)],
            &(SPACE as u64).to_le_bytes(),
        ),
        &[vault.clone(), system_program.clone()],
        &[seeds],
    )?;
    invoke_signed(
        &system(
            0x01,
            vec![AccountMeta::new(*vault.key, true)],
            program.as_ref(),
        ),
        &[vault.clone(), system_program.clone()],
        &[seeds],
    )?;
    let state = Vault {
        owner: *owner.key,
        agent: limits.agent,
        max_per_tx: limits.max_per_tx,
        max_per_day: limits.max_per_day,
        window_start: 0,
        spent: 0,
        bump,
        recipients: limits.recipients,
    };
    state.pack(&mut vault.try_borrow_mut_data()?)?;
    msg!("vault opened");
    Ok(())
}

/// The vault's state, checked to be this program's vault for the owner it names.
fn load(program: &Pubkey, vault: &AccountInfo) -> Result<Vault, ProgramError> {
    if vault.owner != program || !vault.is_writable {
        return Err(VaultError::WrongVault.into());
    }
    let state = Vault::unpack(&vault.try_borrow_data()?)?;
    let expected =
        Pubkey::create_program_address(&[SEED, state.owner.as_ref(), &[state.bump]], program)?;
    if expected != *vault.key {
        return Err(VaultError::WrongVault.into());
    }
    Ok(state)
}

fn configure(program: &Pubkey, accounts: &[AccountInfo], limits: Limits) -> ProgramResult {
    let iter = &mut accounts.iter();
    let owner = next_account_info(iter)?;
    let vault = next_account_info(iter)?;
    let mut state = load(program, vault)?;
    if !owner.is_signer || *owner.key != state.owner {
        return Err(VaultError::NotOwner.into());
    }
    state.agent = limits.agent;
    state.max_per_tx = limits.max_per_tx;
    state.max_per_day = limits.max_per_day;
    state.recipients = limits.recipients;
    state.pack(&mut vault.try_borrow_mut_data()?)?;
    msg!("vault limits changed");
    Ok(())
}

fn pay(program: &Pubkey, accounts: &[AccountInfo], amount: u64) -> ProgramResult {
    let iter = &mut accounts.iter();
    let agent = next_account_info(iter)?;
    let vault = next_account_info(iter)?;
    let to = next_account_info(iter)?;
    let mut state = load(program, vault)?;
    if !agent.is_signer || *agent.key != state.agent {
        return Err(VaultError::NotAgent.into());
    }
    let now = Clock::get()?.unix_timestamp;
    if let Err(e) = state.spend(to.key, amount, now) {
        msg!("refused: {:?}", e);
        return Err(e.into());
    }
    move_lamports(vault, to, amount)?;
    state.pack(&mut vault.try_borrow_mut_data()?)?;
    msg!("paid {} lamports", amount);
    Ok(())
}

fn withdraw(program: &Pubkey, accounts: &[AccountInfo], amount: u64) -> ProgramResult {
    let iter = &mut accounts.iter();
    let owner = next_account_info(iter)?;
    let vault = next_account_info(iter)?;
    let state = load(program, vault)?;
    if !owner.is_signer || *owner.key != state.owner {
        return Err(VaultError::NotOwner.into());
    }
    move_lamports(vault, owner, amount)
}

/// Debits the vault directly, which it may since this program owns it, and
/// never below what keeps it rent-exempt.
fn move_lamports(vault: &AccountInfo, to: &AccountInfo, amount: u64) -> ProgramResult {
    let floor = Rent::get()?.minimum_balance(vault.data_len());
    let left = vault
        .lamports()
        .checked_sub(amount)
        .filter(|left| *left >= floor)
        .ok_or(VaultError::Insufficient)?;
    let credited = to
        .lamports()
        .checked_add(amount)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    **vault.try_borrow_mut_lamports()? = left;
    **to.try_borrow_mut_lamports()? = credited;
    Ok(())
}

/// A system program instruction, encoded by hand: its bincode is a u32 tag
/// (1 assign, 2 transfer, 8 allocate) then the fields.
fn system(tag: u32, accounts: Vec<AccountMeta>, fields: &[u8]) -> Instruction {
    Instruction {
        program_id: SYSTEM_PROGRAM,
        accounts,
        data: [&tag.to_le_bytes()[..], fields].concat(),
    }
}

/// Instruction data, for clients.
pub mod data {
    use super::Limits;

    pub fn init(limits: &Limits) -> Vec<u8> {
        [vec![0], limits.encode()].concat()
    }
    pub fn configure(limits: &Limits) -> Vec<u8> {
        [vec![1], limits.encode()].concat()
    }
    pub fn pay(amount: u64) -> Vec<u8> {
        [vec![2], amount.to_le_bytes().to_vec()].concat()
    }
    pub fn withdraw(amount: u64) -> Vec<u8> {
        [vec![3], amount.to_le_bytes().to_vec()].concat()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(to: Pubkey) -> Vault {
        Vault {
            owner: Pubkey::new_unique(),
            agent: Pubkey::new_unique(),
            max_per_tx: 100,
            max_per_day: 250,
            window_start: 0,
            spent: 0,
            bump: 255,
            recipients: vec![to],
        }
    }

    #[test]
    fn state_round_trips() {
        let v = vault(Pubkey::new_unique());
        let mut buf = vec![0; SPACE];
        v.pack(&mut buf).unwrap();
        assert_eq!(Vault::unpack(&buf).unwrap(), v);
    }

    #[test]
    fn limits_hold_and_the_window_resets() {
        let to = Pubkey::new_unique();
        let mut v = vault(to);
        assert_eq!(
            v.spend(&Pubkey::new_unique(), 1, 0),
            Err(VaultError::UnknownRecipient)
        );
        assert_eq!(v.spend(&to, 101, DAY), Err(VaultError::OverPerPayment));
        v.spend(&to, 100, DAY).unwrap();
        v.spend(&to, 100, DAY + 1).unwrap();
        assert_eq!(v.spend(&to, 51, DAY + 2), Err(VaultError::OverDaily));
        v.spend(&to, 50, DAY + 3).unwrap();
        v.spend(&to, 100, 2 * DAY).unwrap();
        assert_eq!(v.spent, 100);
    }

    #[test]
    fn limits_parse_back() {
        let limits = Limits {
            agent: Pubkey::new_unique(),
            max_per_tx: 7,
            max_per_day: 9,
            recipients: vec![Pubkey::new_unique(), Pubkey::new_unique()],
        };
        assert_eq!(Limits::parse(&limits.encode()).unwrap(), limits);
        let mut long = limits.encode();
        long.push(0);
        assert!(Limits::parse(&long).is_err());
    }
}
