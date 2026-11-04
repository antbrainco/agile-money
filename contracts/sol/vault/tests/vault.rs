//! Runs the built program (`cargo build-sbf` first) in litesvm.

use litesvm::LiteSVM;
use solana_address::Address;
use solana_clock::Clock;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction::Transaction;
use tilly_vault::{DAY, Limits, VaultError, data};

const SOL: u64 = 1_000_000_000;

fn key(a: &Address) -> solana_program::pubkey::Pubkey {
    solana_program::pubkey::Pubkey::new_from_array(a.to_bytes())
}

fn addr(k: &solana_program::pubkey::Pubkey) -> Address {
    Address::new_from_array(k.to_bytes())
}

struct World {
    svm: LiteSVM,
    program: Address,
    owner: Keypair,
    agent: Keypair,
    vault: Address,
    friend: Address,
}

impl World {
    /// A vault holding 5 SOL: 1 SOL per payment, 2 SOL a day, to `friend` only.
    fn new() -> Self {
        let mut svm = LiteSVM::new();
        let program = Address::new_unique();
        svm.add_program_from_file(program, "target/deploy/tilly_vault.so")
            .expect("run `cargo build-sbf` first");
        let (owner, agent) = (Keypair::new(), Keypair::new());
        svm.airdrop(&owner.pubkey(), 10 * SOL).unwrap();
        svm.airdrop(&agent.pubkey(), SOL).unwrap();
        let vault = addr(&tilly_vault::vault_address(&key(&program), &key(&owner.pubkey())).0);
        let mut world = Self {
            svm,
            program,
            owner,
            agent,
            vault,
            friend: Address::new_unique(),
        };
        world.set_time(DAY * 100);
        let limits = world.limits(SOL, 2 * SOL);
        world.as_owner(data::init(&limits), true).unwrap();
        let fund = solana_system_interface::instruction::transfer(
            &world.owner.pubkey(),
            &world.vault,
            5 * SOL,
        );
        world.send(fund, &[]).unwrap();
        world
    }

    fn limits(&self, per_tx: u64, per_day: u64) -> Limits {
        Limits {
            agent: key(&self.agent.pubkey()),
            max_per_tx: per_tx,
            max_per_day: per_day,
            recipients: vec![key(&self.friend)],
        }
    }

    fn set_time(&mut self, unix: i64) {
        let mut clock: Clock = self.svm.get_sysvar();
        clock.unix_timestamp = unix;
        self.svm.set_sysvar(&clock);
    }

    fn send(&mut self, ix: Instruction, extra: &[&Keypair]) -> Result<(), String> {
        self.svm.expire_blockhash();
        let mut signers = vec![&self.owner];
        signers.extend_from_slice(extra);
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.owner.pubkey()),
            &signers,
            self.svm.latest_blockhash(),
        );
        self.svm
            .send_transaction(tx)
            .map(|_| ())
            .map_err(|e| format!("{:?}", e.err))
    }

    fn as_owner(&mut self, data: Vec<u8>, init: bool) -> Result<(), String> {
        let mut accounts = vec![
            AccountMeta::new(self.owner.pubkey(), true),
            AccountMeta::new(self.vault, false),
        ];
        if init {
            accounts.push(AccountMeta::new_readonly(Address::default(), false));
        }
        let ix = Instruction::new_with_bytes(self.program, &data, accounts);
        self.send(ix, &[])
    }

    /// The agent asks the vault to pay `to`; the owner only pays the fee.
    fn pay_as(&mut self, signer: &Keypair, to: Address, amount: u64) -> Result<(), String> {
        let ix = Instruction::new_with_bytes(
            self.program,
            &data::pay(amount),
            vec![
                AccountMeta::new_readonly(signer.pubkey(), true),
                AccountMeta::new(self.vault, false),
                AccountMeta::new(to, false),
                // A Solana Pay reference rides along and is ignored.
                AccountMeta::new_readonly(Address::new_unique(), false),
            ],
        );
        let signer = signer.insecure_clone();
        self.send(ix, &[&signer])
    }

    fn pay(&mut self, amount: u64) -> Result<(), String> {
        let agent = self.agent.insecure_clone();
        self.pay_as(&agent, self.friend, amount)
    }

    fn balance(&self, a: &Address) -> u64 {
        self.svm.get_balance(a).unwrap_or(0)
    }
}

fn refused(e: VaultError) -> String {
    format!("InstructionError(0, Custom({}))", e as u32)
}

#[test]
fn the_agent_pays_a_listed_recipient_within_limits() {
    let mut w = World::new();
    w.pay(SOL / 2).unwrap();
    assert_eq!(w.balance(&w.friend.clone()), SOL / 2);
}

#[test]
fn over_the_per_payment_cap_is_refused() {
    let mut w = World::new();
    assert_eq!(w.pay(SOL + 1), Err(refused(VaultError::OverPerPayment)));
    assert_eq!(w.balance(&w.friend.clone()), 0);
}

#[test]
fn the_daily_cap_holds_until_the_window_closes() {
    let mut w = World::new();
    w.pay(SOL).unwrap();
    w.pay(SOL).unwrap();
    assert_eq!(w.pay(1), Err(refused(VaultError::OverDaily)));
    w.set_time(DAY * 101);
    w.pay(SOL).unwrap();
    assert_eq!(w.balance(&w.friend.clone()), 3 * SOL);
}

#[test]
fn an_unlisted_address_is_refused() {
    let mut w = World::new();
    let agent = w.agent.insecure_clone();
    let stranger = Address::new_unique();
    assert_eq!(
        w.pay_as(&agent, stranger, 1),
        Err(refused(VaultError::UnknownRecipient))
    );
}

#[test]
fn only_the_agent_key_can_pay() {
    let mut w = World::new();
    let thief = Keypair::new();
    let friend = w.friend;
    assert_eq!(
        w.pay_as(&thief, friend, 1),
        Err(refused(VaultError::NotAgent))
    );
}

#[test]
fn the_agent_cannot_change_limits_or_withdraw() {
    let mut w = World::new();
    let agent = w.agent.insecure_clone();
    let limits = w.limits(100 * SOL, 100 * SOL);
    let configure = Instruction::new_with_bytes(
        w.program,
        &data::configure(&limits),
        vec![
            AccountMeta::new_readonly(agent.pubkey(), true),
            AccountMeta::new(w.vault, false),
        ],
    );
    assert_eq!(
        w.send(configure, &[&agent]),
        Err(refused(VaultError::NotOwner))
    );
    let withdraw = Instruction::new_with_bytes(
        w.program,
        &data::withdraw(SOL),
        vec![
            AccountMeta::new(agent.pubkey(), true),
            AccountMeta::new(w.vault, false),
        ],
    );
    assert_eq!(
        w.send(withdraw, &[&agent]),
        Err(refused(VaultError::NotOwner))
    );
}

#[test]
fn the_owner_changes_limits_and_withdraws() {
    let mut w = World::new();
    let limits = w.limits(3 * SOL, 4 * SOL);
    w.as_owner(data::configure(&limits), false).unwrap();
    w.pay(3 * SOL).unwrap();
    let before = w.balance(&w.owner.pubkey());
    w.as_owner(data::withdraw(SOL), false).unwrap();
    assert!(w.balance(&w.owner.pubkey()) > before);
}

#[test]
fn the_vault_keeps_its_rent() {
    let mut w = World::new();
    let limits = w.limits(10 * SOL, 10 * SOL);
    w.as_owner(data::configure(&limits), false).unwrap();
    assert_eq!(w.pay(5 * SOL + 1), Err(refused(VaultError::Insufficient)));
    w.pay(5 * SOL).unwrap();
}

#[test]
fn a_vault_opens_once() {
    let mut w = World::new();
    let limits = w.limits(SOL, SOL);
    assert!(w.as_owner(data::init(&limits), true).is_err());
}

#[test]
fn funding_the_address_first_does_not_block_opening() {
    let mut svm = LiteSVM::new();
    let program = Address::new_unique();
    svm.add_program_from_file(program, "target/deploy/tilly_vault.so")
        .unwrap();
    let owner = Keypair::new();
    svm.airdrop(&owner.pubkey(), 10 * SOL).unwrap();
    let vault = addr(&tilly_vault::vault_address(&key(&program), &key(&owner.pubkey())).0);
    svm.airdrop(&vault, 1_000_000).unwrap();
    let limits = Limits {
        agent: key(&Address::new_unique()),
        max_per_tx: 1,
        max_per_day: 1,
        recipients: vec![],
    };
    let ix = Instruction::new_with_bytes(
        program,
        &data::init(&limits),
        vec![
            AccountMeta::new(owner.pubkey(), true),
            AccountMeta::new(vault, false),
            AccountMeta::new_readonly(Address::default(), false),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&owner.pubkey()),
        &[&owner],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).unwrap();
    assert_eq!(svm.get_account(&vault).unwrap().owner, program);
}
