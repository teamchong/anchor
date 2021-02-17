//! The voting program facilitates on chain governance where votes are
//! substantiated by stake. That is, in order to vote, one must own staking pool
//! tokens. There are two different voting mechanisms.
//!
//! 1. Polling. One can create a simple poll to ask stakers a question.
//! 2. Proposals. One can create a governance proposal that stores a transaction
//!    that can be executed. Communities can use this to put code execution to
//!    to a vote, and if passsed, to execute the proposed transaction, signed
//!    by this program. This can be used for governing security sensitive keys
//!    that have "authority" access to priviledged instructions.

#![feature(proc_macro_hygiene)]

use anchor_lang::prelude::*;
use anchor_lang::solana_program;
use anchor_lang::solana_program::hash;
use anchor_lang::solana_program::instruction::Instruction;
use anchor_spl::token::{self, TokenAccount, Transfer};
use registry::{Member, Registrar};

#[program]
pub mod voting {
    use super::*;

    #[state]
    pub struct Voting {
        // Key with the ability to change the registry program.
        authority: Pubkey,
        // Staking registry program doing all stake weight accounting.
        registry_program: Pubkey,
    }

    impl Voting {
        pub fn new(ctx: Context<Ctor>, registry_program: Pubkey) -> Result<Self> {
            Ok(Self {
                authority: *ctx.accounts.authority.key,
                registry_program,
            })
        }
    }

    #[access_control(CreateGovernor::accounts(&ctx, nonce))]
    pub fn create_governor(
        ctx: Context<CreateGovernor>,
        mint: Pubkey,
        time: i64,
        nonce: u8,
        poll_price: u64,
        proposal_price: u64,
        q_len: u32,
    ) -> Result<()> {
        let governor = &mut ctx.accounts.governor;
        governor.registrar = *ctx.accounts.registrar.to_account_info().key;
        governor.nonce = nonce;
        governor.time = time;
        governor.poll_q = *ctx.accounts.poll_q.to_account_info().key;
        governor.proposal_q = *ctx.accounts.proposal_q.to_account_info().key;
        governor.poll_price = poll_price;
        governor.proposal_price = proposal_price;
        governor.mint = mint;

        let poll_q = &mut ctx.accounts.poll_q;
        poll_q.proposals.resize(q_len as usize, Default::default());
        let proposal_q = &mut ctx.accounts.proposal_q;
        proposal_q
            .proposals
            .resize(q_len as usize, Default::default());

        Ok(())
    }

    pub fn update_governor(
        ctx: Context<UpdateGovernor>,
        price: Option<u64>,
        time: Option<i64>,
    ) -> Result<()> {
        if let Some(price) = price {
            ctx.accounts.governor.proposal_price = price;
        }
        if let Some(time) = time {
            ctx.accounts.governor.time = time;
        }
        Ok(())
    }

    pub fn create_poll<'a, 'b, 'c, 'info>(
        ctx: Context<'a, 'b, 'c, 'info, CreatePoll<'info>>,
        msg: String,
        options: Vec<String>,
        end_ts: i64,
        nonce: u8,
    ) -> Result<()> {
        // Deserialize the Poll to remove from the queue, in the event the queue
        // is full.
        let tail_poll = {
            if ctx.remaining_accounts.len() > 0 {
                let acc_info = &ctx.remaining_accounts[0];
                let poll: ProgramAccount<'info, Poll> = ProgramAccount::try_from(acc_info)?;
                Some(poll)
            } else {
                None
            }
        };

        // Create the poll.
        let mut vote_weights = Vec::new();
        vote_weights.resize(options.len(), 0);
        let poll = &mut ctx.accounts.poll;
        poll.vote_weights = vote_weights;
        poll.msg = msg;
        poll.options = options;
        poll.start_ts = ctx.accounts.clock.unix_timestamp;
        poll.end_ts = end_ts;
        poll.governor = *ctx.accounts.governor.to_account_info().key;
        poll.nonce = nonce;
        poll.vault = *ctx.accounts.vault.to_account_info().key;

        // Add poll to the queue.
        ctx.accounts.poll_q.append_if_free(
            *poll.to_account_info().key,
            &ctx.accounts.clock,
            tail_poll,
        )?;

        // Transfer poll deposit to the program.
        token::transfer(ctx.accounts.into(), ctx.accounts.governor.poll_price)?;

        Ok(())
    }

    #[access_control(
        VotePoll::accounts(&ctx, selector)
        poll_active(&ctx)
    )]
    pub fn vote_poll(ctx: Context<VotePoll>, selector: u32) -> Result<()> {
        let vote = &mut ctx.accounts.vote;
        let poll = &mut ctx.accounts.poll;

        vote.account = *poll.to_account_info().key;
        vote.selector = selector;
        vote.burned = true;
        vote.member = *ctx.accounts.stake.member.to_account_info().key;

        poll.vote_weights[selector as usize] +=
            ctx.accounts.stake.member_spt.amount + ctx.accounts.stake.member_spt_locked.amount;

        Ok(())
    }

    #[access_control(poll_over(&ctx))]
    pub fn close_vote(ctx: Context<CloseVote>) -> Result<()> {
        let vote_lamports = ctx.accounts.vote.to_account_info().lamports();
        let to_lamports = ctx.accounts.to.lamports();
        **ctx.accounts.to.lamports.borrow_mut() = to_lamports
            .checked_add(vote_lamports)
            .ok_or(ErrorCode::Overflow)?;
        **ctx.accounts.vote.to_account_info().lamports.borrow_mut() = 0;
        Ok(())
    }

    #[access_control(CreateProposal::accounts(&ctx, nonce))]
    pub fn create_proposal<'info>(
        ctx: Context<'_, '_, '_, 'info, CreateProposal<'info>>,
        msg: String,
        tx: Transaction,
        nonce: u8,
    ) -> Result<()> {
        // Only needs to be provided if the queue is full.
        let tail_proposal = {
            if ctx.remaining_accounts.len() > 0 {
                let acc_info = &ctx.remaining_accounts[0];
                let proposal: ProgramAccount<'info, Proposal> = ProgramAccount::try_from(acc_info)?;
                Some(proposal)
            } else {
                None
            }
        };

        let proposal = &mut ctx.accounts.proposal;
        let proposal_q = &mut ctx.accounts.proposal_q;

        // Create proposal.
        proposal.governor = *ctx.accounts.governor.to_account_info().key;
        proposal.msg = msg;
        proposal.start_ts = ctx.accounts.clock.unix_timestamp;
        proposal.end_ts = ctx.accounts.clock.unix_timestamp + ctx.accounts.governor.time;
        proposal.nonce = nonce;
        proposal.vault = *ctx.accounts.vault.to_account_info().key;
        proposal.tx = tx;

        // Add proposal to the queue.
        proposal_q.append_if_free(
            *proposal.to_account_info().key,
            &ctx.accounts.clock,
            tail_proposal,
        )?;

        // Transfer proposal deposit to the program.
        token::transfer(ctx.accounts.into(), ctx.accounts.governor.proposal_price)?;

        Ok(())
    }

    #[access_control(proposal_active(&ctx))]
    pub fn vote_proposal(ctx: Context<VoteProposal>, yes: bool) -> Result<()> {
        if yes {
            ctx.accounts.proposal.vote_yes += 1;
        } else {
            ctx.accounts.proposal.vote_no += 1;
        }
        Ok(())
    }

    #[access_control(proposal_over(&ctx))]
    pub fn execute_proposal(ctx: Context<ExecuteProposal>) -> Result<()> {
        if ctx.accounts.proposal.burned {
            return Err(ErrorCode::ProposalBurned.into());
        }
        if ctx.accounts.clock.unix_timestamp < ctx.accounts.proposal.end_ts {
            return Err(ErrorCode::VotingPeriodActive.into());
        }

        let total_votes = ctx.accounts.proposal.vote_yes + ctx.accounts.proposal.vote_no;

        if total_votes != 0 {
            // Adjust to avoid floating point.
            let adjusted = ctx.accounts.proposal.vote_yes * 100;
            if (adjusted / total_votes) > 60 {
                // 60% of the total vote has voted in favor. Execute proposal.
                execute_transaction(&ctx)?;
            }
        }

        ctx.accounts.proposal.burned = true;

        Ok(())
    }
}

#[derive(Accounts)]
pub struct Ctor<'info> {
    #[account(signer)]
    authority: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct CreatePoll<'info> {
    #[account(init)]
    poll: ProgramAccount<'info, Poll>,
    #[account(has_one = poll_q)]
    governor: ProgramAccount<'info, Governor>,
    #[account(mut)]
    poll_q: ProgramAccount<'info, GovQueue>,
    #[account(mut)]
    depositor: AccountInfo<'info>,
    #[account(signer)]
    depositor_authority: AccountInfo<'info>,
    #[account("&vault.owner == poll_signer.key", "vault.mint == governor.mint")]
    vault: CpiAccount<'info, TokenAccount>,
    poll_signer: AccountInfo<'info>,
    rent: Sysvar<'info, Rent>,
    clock: Sysvar<'info, Clock>,
    token_program: AccountInfo<'info>,
}

impl<'info> CreatePoll<'info> {
    pub fn accounts(ctx: &Context<'_, '_, '_, 'info, CreatePoll<'info>>, nonce: u8) -> Result<()> {
        let signer = Pubkey::create_program_address(
            &[ctx.accounts.poll.to_account_info().key.as_ref(), &[nonce]],
            ctx.program_id,
        )
        .map_err(|_| ErrorCode::InvalidNonce)?;
        if &signer != ctx.accounts.poll_signer.key {
            return Err(ErrorCode::InvalidSigner.into());
        }
        Ok(())
    }
}

#[derive(Accounts)]
pub struct VotePoll<'info> {
    governor: ProgramAccount<'info, Governor>,
    #[account(mut, belongs_to = governor)]
    poll: ProgramAccount<'info, Poll>,
    #[account(init)]
    vote: ProgramAccount<'info, Vote>,
    #[account("governor.registrar == stake.member.registrar")]
    stake: StakeMember<'info>,
    rent: Sysvar<'info, Rent>,
    clock: Sysvar<'info, Clock>,
}

#[derive(Accounts)]
pub struct StakeMember<'info> {
    member: CpiAccount<'info, Member>,
    #[account("&member.balances.spt == member_spt.to_account_info().key")]
    member_spt: CpiAccount<'info, TokenAccount>,
    #[account("&member.balances_locked.spt == member_spt_locked.to_account_info().key")]
    member_spt_locked: CpiAccount<'info, TokenAccount>,
}

impl<'info> VotePoll<'info> {
    pub fn accounts(ctx: &Context<VotePoll>, selector: u32) -> Result<()> {
        let seed = hash::hashv(&[
            ctx.accounts.stake.member.to_account_info().key.as_ref(),
            ctx.accounts.poll.to_account_info().key.as_ref(),
        ])
        .to_string();
        let mut i = ctx
            .accounts
            .stake
            .member
            .to_account_info()
            .key
            .as_ref()
            .to_vec();
        i.append(&mut ctx.accounts.poll.to_account_info().key.as_ref().to_vec());
        msg!(
            "Seed: {:?}",
            hash::hashv(&[
                ctx.accounts.stake.member.to_account_info().key.as_ref(),
                ctx.accounts.poll.to_account_info().key.as_ref(),
            ])
            .to_bytes()
        );
        msg!("I: {:?}", hash::hashv(&[&[0]]).to_bytes());
        let expected_vote = Pubkey::create_with_seed(
            &ctx.accounts.stake.member.beneficiary,
            &seed,
            ctx.program_id,
        )
        .map_err(|_| ErrorCode::InvalidVoteAccount)?;
        if &expected_vote != ctx.accounts.vote.to_account_info().key {
            return Err(ErrorCode::InvalidVoteAddress.into());
        }

        if selector as usize >= ctx.accounts.poll.vote_weights.len() {
            return Err(ErrorCode::InvalidPollOption.into());
        }
        Ok(())
    }
}

#[derive(Accounts)]
pub struct CloseVote<'info> {
    // The poll that was voted on.
    poll: ProgramAccount<'info, Poll>,
    // The vote being closed.
    #[account(has_one = member)]
    vote: ProgramAccount<'info, Vote>,
    // The stake account that voted.
    #[account(has_one = beneficiary)]
    member: CpiAccount<'info, Member>,
    // Owner of the stake account.
    #[account(signer)]
    beneficiary: AccountInfo<'info>,
    // The spl token account to send tokens to.
    to: AccountInfo<'info>,
    clock: Sysvar<'info, Clock>,
}

#[derive(Accounts)]
pub struct CreateGovernor<'info> {
    #[account(init)]
    governor: ProgramAccount<'info, Governor>,
    #[account(init)]
    poll_q: ProgramAccount<'info, GovQueue>,
    #[account(init)]
    proposal_q: ProgramAccount<'info, GovQueue>,
    registrar: CpiAccount<'info, Registrar>,
    rent: Sysvar<'info, Rent>,
}

impl<'info> CreateGovernor<'info> {
    pub fn accounts(ctx: &Context<CreateGovernor>, nonce: u8) -> Result<()> {
        Pubkey::create_program_address(
            &[
                ctx.accounts.governor.to_account_info().key.as_ref(),
                &[nonce],
            ],
            ctx.program_id,
        )
        .map_err(|_| ErrorCode::InvalidNonce)?;
        Ok(())
    }
}

#[derive(Accounts)]
pub struct UpdateGovernor<'info> {
    #[account(mut)]
    governor: ProgramAccount<'info, Governor>,
    #[account(signer, seeds = [
        governor.to_account_info().key.as_ref(),
        &[governor.nonce],
    ])]
    governor_signer: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct CreateProposal<'info> {
    #[account(init)]
    proposal: ProgramAccount<'info, Proposal>,
    #[account(has_one = proposal_q)]
    governor: ProgramAccount<'info, Governor>,
    #[account(mut)]
    proposal_q: ProgramAccount<'info, GovQueue>,
    #[account("&vault.owner == proposal_signer.key", "vault.mint == governor.mint")]
    vault: CpiAccount<'info, TokenAccount>,
    proposal_signer: AccountInfo<'info>,
    #[account(mut)]
    depositor: AccountInfo<'info>,
    #[account(signer)]
    depositor_authority: AccountInfo<'info>,
    rent: Sysvar<'info, Rent>,
    clock: Sysvar<'info, Clock>,
    token_program: AccountInfo<'info>,
}

impl<'info> CreateProposal<'info> {
    pub fn accounts(
        ctx: &Context<'_, '_, '_, 'info, CreateProposal<'info>>,
        nonce: u8,
    ) -> Result<()> {
        let signer = Pubkey::create_program_address(
            &[
                ctx.accounts.proposal.to_account_info().key.as_ref(),
                &[nonce],
            ],
            ctx.program_id,
        )
        .map_err(|_| ErrorCode::InvalidNonce)?;
        if &signer != ctx.accounts.proposal_signer.key {
            return Err(ErrorCode::InvalidSigner.into());
        }
        Ok(())
    }
}

#[derive(Accounts)]
pub struct VoteProposal<'info> {
    governor: ProgramAccount<'info, Governor>,
    #[account(mut, belongs_to = governor)]
    proposal: ProgramAccount<'info, Proposal>,
    #[account(init)]
    vote: ProgramAccount<'info, Vote>,
    #[account("stake.member.registrar == governor.registrar")]
    stake: StakeMember<'info>,
    rent: Sysvar<'info, Rent>,
    clock: Sysvar<'info, Clock>,
}

#[derive(Accounts)]
pub struct ExecuteProposal<'info> {
    governor: ProgramAccount<'info, Governor>,
    #[account(mut, belongs_to = governor)]
    proposal: ProgramAccount<'info, Proposal>,
    clock: Sysvar<'info, Clock>,
}

// The Governor account is effectively a multisig wallet that will sign
// transactions in the event a Proposal is passed. It's not actually a multisig.
#[account]
pub struct Governor {
    // The staking registrar defining who is allowed to vote, i.e., anyone
    // with staking pool tokens form this registrar.
    pub registrar: Pubkey,
    // Bump seed for the governor signer.
    pub nonce: u8,
    // Address of the poll queue account.
    pub poll_q: Pubkey,
    // The amount of `mint` tokens that must be deposited to create a poll.
    pub poll_price: u64,
    // Address of the proposal queue account.
    pub proposal_q: Pubkey,
    // The amount of `mint` tokens that must be deposited to create a
    // proposal.
    pub proposal_price: u64,
    // The token mint that must be used for creating a proposal or poll.
    pub mint: Pubkey,
    // The amount of time governance proposals last before expiry.
    pub time: i64,
}

#[account]
pub struct Poll {
    // The governor defining who is allowed to vote, i.e., anyone
    // with staking pool tokens from its associated registrar.
    pub governor: Pubkey,
    // UI message to display to voters.
    pub msg: String,
    // Unix timestamp when the poll started.
    pub start_ts: i64,
    // Unix timestamp when the poll ended.
    pub end_ts: i64,
    // The options to vote for. Each entry is a String for a UI label
    pub options: Vec<String>,
    // The current vote tallies for each option.
    pub vote_weights: Vec<u64>,
    // Bump seed for the poll signer.
    pub nonce: u8,
    // Deposit vault holding the funds required to create the Poll.
    pub vault: Pubkey,
}

impl Burnable for Poll {
    // As a convenience, we allow the Poll to be removed from the queue if it
    // expires. If needed, one can still  fetch the account to view the results.
    // This is because there's no burn event as there is with a proposal (which
    // is code execution of hte proposal).
    fn burned<'info>(&self, clock: &Sysvar<'info, Clock>) -> bool {
        self.end_ts < clock.unix_timestamp
    }
}

// Proposal is a binary proposal for executing a transaction signed by this
// program.
#[account]
pub struct Proposal {
    // The governor account this proposal belongs to.
    pub governor: Pubkey,
    // The address that created the proposal.
    pub proposer: Pubkey,
    // UI message to display to voters.
    pub msg: String,
    // Unix timestamp when the poll started.
    pub start_ts: i64,
    // Unix timestamp when the poll ended.
    pub end_ts: i64,
    // The current vote tallies for the proposal.
    pub vote_yes: u64,
    // The current vote tallies against the proposal.
    pub vote_no: u64,
    // The transaction to execute if this proposal is passed.
    pub tx: Transaction,
    // The vault holding the proposal deposit.
    pub vault: Pubkey,
    // The bump seed for the proposal signer owning the vualt.
    pub nonce: u8,
    // One time token for proposal execution.
    pub burned: bool,
}

impl Burnable for Proposal {
    fn burned<'info>(&self, _clock: &Sysvar<'info, Clock>) -> bool {
        self.burned
    }
}

#[account]
pub struct Vote {
    // The staking member who created this vote.
    pub member: Pubkey,
    // The account this vote is being used for.
    pub account: Pubkey,
    // The index of the poll item being voted for.
    pub selector: u32,
    // True if the vote has been used. Ensures one time voting.
    pub burned: bool,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct Transaction {
    // Target program to execute against.
    program_id: Pubkey,
    // Boolean ensuring one time execution.
    did_execute: bool,
    // Accounts requried for the transaction.
    accounts: Vec<TransactionAccount>,
    // Instruction data for the transaction.
    data: Vec<u8>,
    // signers[index] is true iff multisig.owners[index] signed the transaction.
    signers: Vec<bool>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct TransactionAccount {
    pubkey: Pubkey,
    is_signer: bool,
    is_writable: bool,
}

impl From<&Transaction> for Instruction {
    fn from(tx: &Transaction) -> Instruction {
        Instruction {
            program_id: tx.program_id,
            accounts: tx.accounts.clone().into_iter().map(Into::into).collect(),
            data: tx.data.clone(),
        }
    }
}

impl From<TransactionAccount> for AccountMeta {
    fn from(account: TransactionAccount) -> AccountMeta {
        match account.is_writable {
            false => AccountMeta::new_readonly(account.pubkey, account.is_signer),
            true => AccountMeta::new(account.pubkey, account.is_signer),
        }
    }
}

impl<'a, 'b, 'c, 'info> From<&mut CreatePoll<'info>>
    for CpiContext<'a, 'b, 'c, 'info, Transfer<'info>>
{
    fn from(accounts: &mut CreatePoll<'info>) -> CpiContext<'a, 'b, 'c, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: accounts.depositor.clone(),
            to: accounts.vault.to_account_info(),
            authority: accounts.depositor_authority.clone(),
        };
        let cpi_program = accounts.token_program.clone();
        CpiContext::new(cpi_program, cpi_accounts)
    }
}

impl<'a, 'b, 'c, 'info> From<&mut CreateProposal<'info>>
    for CpiContext<'a, 'b, 'c, 'info, Transfer<'info>>
{
    fn from(
        accounts: &mut CreateProposal<'info>,
    ) -> CpiContext<'a, 'b, 'c, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: accounts.depositor.clone(),
            to: accounts.vault.to_account_info(),
            authority: accounts.depositor_authority.clone(),
        };
        let cpi_program = accounts.token_program.clone();
        CpiContext::new(cpi_program, cpi_accounts)
    }
}

#[error]
pub enum ErrorCode {
    #[msg("The given vote account has an invalid address.")]
    InvalidVoteAddress,
    #[msg("Voting is closed.")]
    VotingIsOver,
    #[msg("Please provide a valid poll option.")]
    InvalidPollOption,
    #[msg("The nonce provided does not create a valid program derived address.")]
    InvalidNonce,
    #[msg("Voting for the given proposal has not closed, yet.")]
    VotingPeriodActive,
    #[msg("The proposal has already been burned.")]
    ProposalBurned,
    #[msg("Integer overflow.")]
    Overflow,
    #[msg("Proposal ring buffer is full.")]
    RingFull,
    #[msg("Proposal queue can't be garbage not full.")]
    ProposalQNotFull,
    #[msg("A tail proposal must be provided when adding to a full proposal queue.")]
    TailProposalNotProvided,
    #[msg("Tail proposal provided does not match the address in the queue.")]
    InvalidTailProposal,
    #[msg("Please burn the proposal before garbage collecting it.")]
    ProposalNotBurned,
    #[msg("Signer given does not match the signer derived.")]
    InvalidSigner,
    #[msg("The given vote account does not match the expected address.")]
    InvalidVoteAccount,
    Unknown,
}

fn poll_active(ctx: &Context<VotePoll>) -> Result<()> {
    if ctx.accounts.clock.unix_timestamp < ctx.accounts.poll.end_ts {
        return Err(ErrorCode::VotingIsOver.into());
    }
    Ok(())
}

fn poll_over(ctx: &Context<CloseVote>) -> Result<()> {
    if ctx.accounts.clock.unix_timestamp >= ctx.accounts.poll.end_ts {
        return Err(ErrorCode::VotingIsOver.into());
    }
    Ok(())
}

fn proposal_active(ctx: &Context<VoteProposal>) -> Result<()> {
    if ctx.accounts.clock.unix_timestamp < ctx.accounts.proposal.end_ts {
        return Err(ErrorCode::VotingIsOver.into());
    }
    Ok(())
}

fn proposal_over(ctx: &Context<ExecuteProposal>) -> Result<()> {
    if ctx.accounts.clock.unix_timestamp >= ctx.accounts.proposal.end_ts {
        return Err(ErrorCode::VotingIsOver.into());
    }
    Ok(())
}

fn execute_transaction(ctx: &Context<ExecuteProposal>) -> Result<()> {
    // Execute the multisig transaction.
    let ix: Instruction = (&ctx.accounts.proposal.tx).into();
    let seeds = &[
        ctx.accounts.governor.to_account_info().key.as_ref(),
        &[ctx.accounts.governor.nonce],
    ];
    let signer = &[&seeds[..]];
    let accounts = ctx.remaining_accounts;
    solana_program::program::invoke_signed(&ix, &accounts, signer)?;
    Ok(())
}

#[account]
pub struct GovQueue {
    // Invariant: index is position of the next available slot.
    head: u32,
    // Invariant: index is position of the first (oldest) taken slot.
    // Invariant: head == tail => queue is initialized.
    // Invariant: index_of(head + 1) == index_of(tail) => queue is full.
    tail: u32,
    // Although a vec is used, the size is immutable. All entries are proposal
    // account addresses.
    proposals: Vec<Pubkey>,
}

impl GovQueue {
    // Errors if the queue is full.
    pub fn append_if_free<'info, T: Burnable>(
        &mut self,
        proposal: Pubkey,
        clock: &Sysvar<'info, Clock>,
        tail_proposal: Option<ProgramAccount<'info, T>>,
    ) -> Result<u32> {
        let cursor = self.head;

        // The queue is full, so assert the tail's proposal has expired. If so,
        // discard it.
        if self.is_full() {
            let proposal = tail_proposal.ok_or(ErrorCode::TailProposalNotProvided)?;
            if self.get_tail() != proposal.to_account_info().key {
                return Err(ErrorCode::InvalidTailProposal.into());
            }
            if !proposal.burned(clock) {
                return Err(ErrorCode::ProposalNotBurned.into());
            }
            self.tail += 1;
        }

        // Insert into next available slot.
        let h_idx = self.index_of(self.head);
        self.proposals[h_idx] = proposal;
        self.head += 1;

        Ok(cursor)
    }

    pub fn get_tail(&self) -> &Pubkey {
        &self.proposals[self.tail as usize % self.capacity()]
    }

    pub fn is_full(&self) -> bool {
        self.index_of(self.head + 1) == self.index_of(self.tail)
    }

    fn index_of(&self, counter: u32) -> usize {
        counter as usize % self.capacity()
    }

    fn capacity(&self) -> usize {
        self.proposals.len()
    }
}

pub trait Burnable: AccountSerialize + AccountDeserialize + Clone {
    fn burned<'info>(&self, clock: &Sysvar<'info, Clock>) -> bool;
}
